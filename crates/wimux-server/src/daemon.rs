//! Démon : boucle d'acceptation des clients sur le Named Pipe, gestion des
//! sessions et dialogue avec chaque client (un thread par connexion, plus un
//! thread émetteur de frames par attachement).
//!
//! Le **préfixe** `Ctrl-b` et sa table de commandes sont interprétés ici, côté
//! serveur (modèle tmux) : le client transmet toutes les frappes, et le serveur
//! décide de les router vers le volet actif ou d'exécuter une commande (découpe,
//! navigation, détachement...).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use wimux_protocol::transport::{PipeConn, PipeListener, user_pipe_name};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, SessionInfo, recv, send,
};

use crate::config::{Action, Config};
use crate::pane::CopyAction;
use crate::session::Session;
use crate::window::{Move, SplitDir};

pub struct Server {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    config: Config,
}

impl Server {
    fn new() -> Arc<Server> {
        Arc::new(Server {
            sessions: Mutex::new(HashMap::new()),
            config: Config::load(),
        })
    }

    fn reap(&self) {
        self.sessions.lock().unwrap().retain(|_, s| s.is_alive());
    }

    fn get(&self, name: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(name).cloned()
    }

    fn list(&self) -> Vec<SessionInfo> {
        self.reap();
        let sessions = self.sessions.lock().unwrap();
        let mut infos: Vec<SessionInfo> = sessions
            .values()
            .map(|s| SessionInfo {
                name: s.name.clone(),
                windows: s.window_count() as u32,
                attached: s.attached_count() > 0,
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    }

    fn kill(&self, name: &str) -> bool {
        let session = self.sessions.lock().unwrap().remove(name);
        match session {
            Some(s) => {
                s.kill();
                true
            }
            None => false,
        }
    }

    fn create_session(
        &self,
        name: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Session>, String> {
        self.reap();
        let mut sessions = self.sessions.lock().unwrap();

        let name = match name {
            Some(n) => {
                if sessions.contains_key(&n) {
                    return Err(format!("la session « {n} » existe déjà"));
                }
                n
            }
            None => {
                let mut i = 0;
                loop {
                    let candidate = i.to_string();
                    if !sessions.contains_key(&candidate) {
                        break candidate;
                    }
                    i += 1;
                }
            }
        };

        let session = Session::new(name.clone(), cols, rows, &self.config.default_shell)
            .map_err(|e| format!("création de la session : {e}"))?;
        sessions.insert(name, Arc::clone(&session));
        Ok(session)
    }
}

/// Lance le démon sur le pipe de l'utilisateur courant.
pub fn run() -> Result<()> {
    run_on(&user_pipe_name())
}

/// Lance le démon sur un pipe nommé donné (utile pour les tests isolés).
pub fn run_on(pipe_name: &str) -> Result<()> {
    let server = Server::new();
    let listener = PipeListener::bind(pipe_name);

    loop {
        match listener.accept() {
            Ok(conn) => {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(server, conn) {
                        eprintln!("wimux-server : client terminé sur erreur : {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("wimux-server : échec d'acceptation : {e}");
            }
        }
    }
}

/// Lien actif client<->session : garde d'attachement + thread émetteur de frames.
struct Attachment {
    keep_going: Arc<AtomicBool>,
    sender: Option<JoinHandle<()>>,
    session: Arc<Session>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.keep_going.store(false, Ordering::Relaxed);
        self.session.notifier().notify();
        if let Some(handle) = self.sender.take() {
            let _ = handle.join();
        }
        self.session.decr_attached();
    }
}

fn attach(session: Arc<Session>, conn: Arc<PipeConn>) -> Attachment {
    session.incr_attached();
    let keep_going = Arc::new(AtomicBool::new(true));

    let sender = {
        let session = Arc::clone(&session);
        let keep_going = Arc::clone(&keep_going);
        std::thread::spawn(move || sender_loop(session, conn, keep_going))
    };

    Attachment {
        keep_going,
        sender: Some(sender),
        session,
    }
}

/// Boucle émettrice : envoie une frame composée à chaque changement.
fn sender_loop(session: Arc<Session>, conn: Arc<PipeConn>, keep_going: Arc<AtomicBool>) {
    let notifier = session.notifier();
    let mut last_gen = 0u64;
    loop {
        let generation = notifier.wait_change(last_gen, &keep_going);
        if !keep_going.load(Ordering::Relaxed) {
            break;
        }
        last_gen = generation;

        let frame = session.composite();
        let mut w: &PipeConn = &conn;
        if send(&mut w, &ServerMessage::Frame(frame)).is_err() {
            break;
        }
        if !session.is_alive() {
            let mut w: &PipeConn = &conn;
            let _ = send(&mut w, &ServerMessage::PaneExited { code: 0 });
            break;
        }
    }
}

/// État du décodage du préfixe pour un client.
#[derive(Default)]
struct PrefixState {
    armed: bool,
}

fn handle_client(server: Arc<Server>, conn: PipeConn) -> Result<()> {
    let conn = Arc::new(conn);

    // Handshake de version.
    let mut rd: &PipeConn = &conn;
    let hello: ClientMessage = match recv(&mut rd) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let ok = matches!(
        &hello,
        ClientMessage::Hello(Hello { client_version, .. })
            if client_version.is_compatible_with(PROTOCOL_VERSION)
    );
    let mut wr: &PipeConn = &conn;
    if !ok {
        let _ = send(
            &mut wr,
            &ServerMessage::Hello(HelloReply::VersionMismatch {
                server_version: PROTOCOL_VERSION,
                reason: "version de protocole incompatible".into(),
            }),
        );
        return Ok(());
    }
    send(
        &mut wr,
        &ServerMessage::Hello(HelloReply::Ok {
            server_version: PROTOCOL_VERSION,
        }),
    )?;

    let mut attachment: Option<Attachment> = None;
    let mut prefix = PrefixState::default();

    loop {
        let mut rd: &PipeConn = &conn;
        let msg = match recv::<_, ClientMessage>(&mut rd) {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            ClientMessage::NewSession { name, cols, rows } => {
                match server.create_session(name, cols, rows) {
                    Ok(session) => {
                        let mut wr: &PipeConn = &conn;
                        send(
                            &mut wr,
                            &ServerMessage::Attached {
                                name: session.name.clone(),
                            },
                        )?;
                        attachment = Some(attach(session, Arc::clone(&conn)));
                    }
                    Err(e) => {
                        let mut wr: &PipeConn = &conn;
                        send(&mut wr, &ServerMessage::Error(e))?;
                    }
                }
            }
            ClientMessage::Attach { name, cols, rows } => match server.get(&name) {
                Some(session) => {
                    session.resize(cols, rows);
                    let mut wr: &PipeConn = &conn;
                    send(
                        &mut wr,
                        &ServerMessage::Attached {
                            name: session.name.clone(),
                        },
                    )?;
                    attachment = Some(attach(session, Arc::clone(&conn)));
                }
                None => {
                    let mut wr: &PipeConn = &conn;
                    send(
                        &mut wr,
                        &ServerMessage::Error(format!("session introuvable : {name}")),
                    )?;
                }
            },
            ClientMessage::List => {
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Sessions(server.list()))?;
            }
            ClientMessage::Kill { name } => {
                server.kill(&name);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::SendKeys { session, keys } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        s.send_input(&keys);
                        ServerMessage::Ok
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Command { session, command } => {
                let reply = match server.get(&session) {
                    Some(s) => match crate::commands::run(&s, &command) {
                        crate::commands::CommandResult::Text(t) => ServerMessage::CommandResult(t),
                        crate::commands::CommandResult::None => {
                            ServerMessage::CommandResult(String::new())
                        }
                    },
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Input(bytes) => {
                if let Some(a) = &attachment {
                    let outcome = route_input(&a.session, &server.config, &mut prefix, &bytes);
                    if let Some(text) = outcome.clipboard {
                        let mut wr: &PipeConn = &conn;
                        let _ = send(&mut wr, &ServerMessage::SetClipboard(text));
                    }
                    if outcome.detach {
                        let mut wr: &PipeConn = &conn;
                        let _ = send(&mut wr, &ServerMessage::Detached);
                        attachment = None;
                        break;
                    }
                }
            }
            ClientMessage::Resize { cols, rows } => {
                if let Some(a) = &attachment {
                    a.session.resize(cols, rows);
                }
            }
            ClientMessage::Detach => {
                attachment = None;
                break;
            }
            ClientMessage::Shutdown => {
                for info in server.list() {
                    server.kill(&info.name);
                }
                std::process::exit(0);
            }
            ClientMessage::Ping => {
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Pong)?;
            }
            ClientMessage::Hello(_) => {}
        }
    }

    drop(attachment);
    Ok(())
}

/// Résultat du routage d'un fragment d'entrée.
#[derive(Default)]
struct RouteOutcome {
    detach: bool,
    clipboard: Option<String>,
}

/// Exécute une action de préfixe. Renvoie `true` pour un détachement.
fn execute_action(session: &Session, action: Action) -> bool {
    match action {
        Action::Detach => return true,
        Action::SplitH => session.split(SplitDir::LeftRight),
        Action::SplitV => session.split(SplitDir::TopBottom),
        Action::NextPane => session.next_pane(),
        Action::SelectLeft => session.select(Move::Left),
        Action::SelectDown => session.select(Move::Down),
        Action::SelectUp => session.select(Move::Up),
        Action::SelectRight => session.select(Move::Right),
        Action::KillPane => session.close_active_pane(),
        Action::NewWindow => session.new_window(),
        Action::NextWindow => session.next_window(),
        Action::PrevWindow => session.prev_window(),
        Action::CopyMode => session.enter_copy_mode(),
        Action::Paste => session.paste(),
    }
    false
}

/// Décode le flux d'entrée : mode copie, commande de préfixe (selon la config),
/// ou frappe vers le volet actif.
fn route_input(
    session: &Session,
    config: &Config,
    prefix: &mut PrefixState,
    bytes: &[u8],
) -> RouteOutcome {
    let mut outcome = RouteOutcome::default();

    // En mode copie, les touches pilotent la vue et ne vont pas au shell.
    if session.active_in_copy_mode() {
        for &b in bytes {
            if let CopyAction::Copied(text) = session.copy_key(b) {
                outcome.clipboard = Some(text);
            }
        }
        return outcome;
    }

    // Invite de commande : les touches construisent la ligne ; Entrée l'exécute.
    if session.in_command_prompt() {
        for &b in bytes {
            if let Some(cmd) = session.command_key(b) {
                let _ = crate::commands::run(session, &cmd);
            }
        }
        return outcome;
    }

    let mut forward: Vec<u8> = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        if prefix.armed {
            prefix.armed = false;
            // Vider ce qui précède avant d'exécuter la commande.
            if !forward.is_empty() {
                session.send_input(&forward);
                forward.clear();
            }
            if byte == config.prefix {
                forward.push(config.prefix); // préfixe deux fois -> préfixe littéral
            } else if byte == b':' {
                session.enter_command_prompt();
            } else if byte.is_ascii_digit() {
                session.select_window((byte - b'0') as usize);
            } else if let Some(&action) = config.bindings.get(&byte) {
                if execute_action(session, action) {
                    outcome.detach = true;
                    return outcome;
                }
            }
        } else if byte == config.prefix {
            prefix.armed = true;
        } else {
            forward.push(byte);
        }
    }

    if !forward.is_empty() {
        session.send_input(&forward);
    }
    outcome
}
