//! Démon : boucle d'acceptation des clients sur le Named Pipe, gestion des
//! sessions et dialogue avec chaque client (un thread par connexion, plus un
//! thread émetteur de frames par attachement).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use wimux_protocol::transport::{PipeConn, PipeListener, user_pipe_name};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, SessionInfo, recv, send,
};

use crate::session::{AttachGuard, Session};

/// Shell lancé par défaut dans les nouveaux volets.
fn default_shell() -> String {
    std::env::var("WIMUX_SHELL").unwrap_or_else(|_| "powershell.exe".to_string())
}

/// État global du serveur : l'ensemble des sessions vivantes.
pub struct Server {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl Server {
    fn new() -> Arc<Server> {
        Arc::new(Server {
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Retire les sessions dont le shell est terminé.
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
                windows: 1,
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

    /// Crée une session (nom auto si absent) et la renvoie.
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

        let session = Session::new(name.clone(), cols, rows, &default_shell())
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

/// Lien actif entre un client et une session : garde d'attachement + thread
/// émetteur de frames. Le `Drop` arrête proprement l'émetteur.
struct Attachment {
    keep_going: Arc<AtomicBool>,
    sender: Option<JoinHandle<()>>,
    session: Arc<Session>,
    _guard: AttachGuard,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.keep_going.store(false, Ordering::Relaxed);
        // Réveiller l'émetteur s'il attend un changement.
        self.session.notify();
        if let Some(handle) = self.sender.take() {
            let _ = handle.join();
        }
    }
}

fn attach(session: Arc<Session>, conn: Arc<PipeConn>) -> Attachment {
    let keep_going = Arc::new(AtomicBool::new(true));
    let guard = AttachGuard::new(Arc::clone(&session));

    let sender = {
        let session = Arc::clone(&session);
        let keep_going = Arc::clone(&keep_going);
        std::thread::spawn(move || sender_loop(session, conn, keep_going))
    };

    Attachment {
        keep_going,
        sender: Some(sender),
        session,
        _guard: guard,
    }
}

/// Boucle émettrice : envoie au client une frame à chaque changement d'affichage.
fn sender_loop(session: Arc<Session>, conn: Arc<PipeConn>, keep_going: Arc<AtomicBool>) {
    let mut last_gen = 0u64;
    loop {
        let (generation, frame, exit) = session.wait_change(last_gen, &keep_going);
        if !keep_going.load(Ordering::Relaxed) {
            break;
        }
        last_gen = generation;

        let mut w: &PipeConn = &conn;
        if send(&mut w, &ServerMessage::Frame(frame)).is_err() {
            break;
        }
        if let Some(code) = exit {
            let mut w: &PipeConn = &conn;
            let _ = send(&mut w, &ServerMessage::PaneExited { code });
            break;
        }
    }
}

fn handle_client(server: Arc<Server>, conn: PipeConn) -> Result<()> {
    let conn = Arc::new(conn);

    // Handshake de version.
    let mut rd: &PipeConn = &conn;
    let hello: ClientMessage = match recv(&mut rd) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let ok = match hello {
        ClientMessage::Hello(Hello { client_version, .. }) => {
            client_version.is_compatible_with(PROTOCOL_VERSION)
        }
        _ => false,
    };
    let mut wr: &PipeConn = &conn;
    if ok {
        send(
            &mut wr,
            &ServerMessage::Hello(HelloReply::Ok {
                server_version: PROTOCOL_VERSION,
            }),
        )?;
    } else {
        let _ = send(
            &mut wr,
            &ServerMessage::Hello(HelloReply::VersionMismatch {
                server_version: PROTOCOL_VERSION,
                reason: "version de protocole incompatible".into(),
            }),
        );
        return Ok(());
    }

    // Boucle de messages. `attachment` maintient l'éventuel volet suivi.
    let mut attachment: Option<Attachment> = None;
    loop {
        let mut rd: &PipeConn = &conn;
        let msg = match recv::<_, ClientMessage>(&mut rd) {
            Ok(m) => m,
            Err(_) => break, // client déconnecté
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
            ClientMessage::Input(bytes) => {
                if let Some(a) = &attachment {
                    a.session.send_input(&bytes);
                }
            }
            ClientMessage::Resize { cols, rows } => {
                if let Some(a) = &attachment {
                    a.session.resize(cols, rows);
                }
            }
            ClientMessage::Detach => {
                attachment = None; // arrête l'émetteur, session conservée
                break;
            }
            ClientMessage::Shutdown => {
                // Terminer toutes les sessions puis quitter le processus.
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
