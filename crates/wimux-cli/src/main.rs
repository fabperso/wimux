//! Binaire `wimux` : point d'entrée utilisateur. Analyse la ligne de commande,
//! démarre le serveur détaché s'il est absent, puis lance un client TUI (attach)
//! ou envoie une commande de contrôle au serveur.

use std::io::{self, Write};
use std::process::Command;
use std::time::Duration;

use wimux_client::{ExitReason, run};
use wimux_protocol::transport::{PipeConn, connect, user_pipe_name};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, recv, send,
};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);

    let result = match cmd {
        Some("--version") | Some("-V") => {
            println!(
                "wimux {} (protocole {})",
                env!("CARGO_PKG_VERSION"),
                PROTOCOL_VERSION
            );
            Ok(())
        }
        Some("new") => cmd_new(session_name_arg(&args[1..])),
        Some("attach") | Some("a") => cmd_attach(args.get(1).cloned()),
        Some("ls") | Some("list-sessions") => cmd_list(),
        Some("send-keys") => cmd_send_keys(&args[1..]),
        Some(c @ ("split-window" | "new-window" | "list-panes" | "capture-pane")) => {
            cmd_control(c, &args[1..])
        }
        Some("kill-session") => cmd_kill(args.get(1).cloned()),
        Some("kill-server") => cmd_shutdown(),
        Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => {
            eprintln!("wimux : commande inconnue « {other} » (essayez `wimux --help`)");
            return std::process::ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wimux : {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Extrait un nom de session depuis `new [-s <nom>|<nom>]`.
fn session_name_arg(rest: &[String]) -> Option<String> {
    match rest.first().map(String::as_str) {
        Some("-s") | Some("--session") => rest.get(1).cloned(),
        Some(name) => Some(name.to_string()),
        None => None,
    }
}

fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((80, 24))
}

// --- Connexion / démarrage du serveur -------------------------------------

/// Se connecte au serveur, en le démarrant (détaché) s'il n'écoute pas encore.
fn ensure_server() -> io::Result<PipeConn> {
    let name = user_pipe_name();
    match connect(&name) {
        Ok(conn) => return Ok(conn),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    spawn_server()?;

    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        match connect(&name) {
            Ok(conn) => return Ok(conn),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "le serveur wimux n'a pas démarré à temps",
    ))
}

/// Démarre `wimux-server serve` en processus détaché (survit à la fermeture du
/// terminal courant).
fn spawn_server() -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let exe = std::env::current_exe()?;
    let server = exe.with_file_name("wimux-server.exe");
    Command::new(server)
        .arg("serve")
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn()?;
    Ok(())
}

/// Négocie la version avec le serveur.
fn handshake(conn: &PipeConn) -> io::Result<()> {
    let mut w: &PipeConn = conn;
    send(
        &mut w,
        &ClientMessage::Hello(Hello {
            client_version: PROTOCOL_VERSION,
            client_build: env!("CARGO_PKG_VERSION").to_string(),
        }),
    )?;

    let mut r: &PipeConn = conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Hello(HelloReply::Ok { .. }) => Ok(()),
        ServerMessage::Hello(HelloReply::VersionMismatch { reason, .. }) => {
            Err(io::Error::other(reason))
        }
        _ => Err(io::Error::other("réponse de handshake inattendue")),
    }
}

// --- Commandes ------------------------------------------------------------

fn cmd_new(name: Option<String>) -> io::Result<()> {
    let (cols, rows) = terminal_size();
    let conn = ensure_server()?;
    handshake(&conn)?;

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::NewSession { name, cols, rows })?;

    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Attached { name } => {
            eprintln!("wimux : session « {name} » créée");
            report(run(conn)?);
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn cmd_attach(name: Option<String>) -> io::Result<()> {
    let name = name.ok_or_else(|| io::Error::other("usage : wimux attach <nom>"))?;
    let (cols, rows) = terminal_size();
    let conn =
        connect(&user_pipe_name()).map_err(|_| io::Error::other("aucun serveur en cours"))?;
    handshake(&conn)?;

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Attach { name, cols, rows })?;

    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Attached { .. } => {
            report(run(conn)?);
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn cmd_list() -> io::Result<()> {
    let conn = match connect(&user_pipe_name()) {
        Ok(conn) => conn,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            println!("(aucun serveur wimux en cours)");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    handshake(&conn)?;

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::List)?;

    let mut r: &PipeConn = &conn;
    if let ServerMessage::Sessions(sessions) = recv::<_, ServerMessage>(&mut r)? {
        if sessions.is_empty() {
            println!("(aucune session)");
        } else {
            let mut stdout = io::stdout();
            for s in sessions {
                let flag = if s.attached { " (attachée)" } else { "" };
                writeln!(stdout, "{}: {} fenêtre(s){}", s.name, s.windows, flag)?;
            }
        }
    }
    Ok(())
}

/// `wimux send-keys -t <session> <touches...>` : injecte des frappes dans une
/// session sans s'y attacher (scriptable). Les jetons spéciaux sont traduits :
/// `Enter`, `Tab`, `Space`, `Escape`, `C-<lettre>` ; le reste est envoyé littéralement.
fn cmd_send_keys(args: &[String]) -> io::Result<()> {
    let mut session = None;
    let mut keys_args = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--target" => {
                session = args.get(i + 1).cloned();
                i += 2;
            }
            other => {
                keys_args.push(other.to_string());
                i += 1;
            }
        }
    }
    let session = session
        .ok_or_else(|| io::Error::other("usage : wimux send-keys -t <session> <touches...>"))?;
    if keys_args.is_empty() {
        return Err(io::Error::other("aucune touche à envoyer"));
    }

    let keys = translate_keys(&keys_args);
    let conn =
        connect(&user_pipe_name()).map_err(|_| io::Error::other("aucun serveur en cours"))?;
    handshake(&conn)?;

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::SendKeys { session, keys })?;

    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

/// `wimux <commande> -t <session> [flags]` : exécute une commande textuelle sur
/// une session (split-window, new-window, list-panes, capture-pane) et affiche
/// son éventuel résultat.
fn cmd_control(command: &str, args: &[String]) -> io::Result<()> {
    let mut session = None;
    let mut extra = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--target" => {
                session = args.get(i + 1).cloned();
                i += 2;
            }
            other => {
                extra.push(other.to_string());
                i += 1;
            }
        }
    }
    let session = session
        .ok_or_else(|| io::Error::other(format!("usage : wimux {command} -t <session> [flags]")))?;
    let full = if extra.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", extra.join(" "))
    };

    let conn =
        connect(&user_pipe_name()).map_err(|_| io::Error::other("aucun serveur en cours"))?;
    handshake(&conn)?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::Command {
            session,
            command: full,
        },
    )?;

    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::CommandResult(text) => {
            if !text.is_empty() {
                println!("{text}");
            }
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

/// Traduit des jetons de touches en octets.
fn translate_keys(args: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for arg in args {
        match arg.as_str() {
            "Enter" => out.push(b'\r'),
            "Tab" => out.push(b'\t'),
            "Space" => out.push(b' '),
            "Escape" => out.push(0x1b),
            s if s.len() == 3 && s.starts_with("C-") => {
                let c = s.as_bytes()[2].to_ascii_lowercase();
                if c.is_ascii_lowercase() {
                    out.push(c - b'a' + 1);
                }
            }
            s => out.extend_from_slice(s.as_bytes()),
        }
    }
    out
}

fn cmd_kill(name: Option<String>) -> io::Result<()> {
    let name = name.ok_or_else(|| io::Error::other("usage : wimux kill-session <nom>"))?;
    let conn =
        connect(&user_pipe_name()).map_err(|_| io::Error::other("aucun serveur en cours"))?;
    handshake(&conn)?;

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Kill { name: name.clone() })?;

    let mut r: &PipeConn = &conn;
    let _ = recv::<_, ServerMessage>(&mut r)?;
    println!("session « {name} » terminée");
    Ok(())
}

fn cmd_shutdown() -> io::Result<()> {
    let conn = match connect(&user_pipe_name()) {
        Ok(conn) => conn,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            println!("(aucun serveur wimux en cours)");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    handshake(&conn)?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Shutdown)?;
    println!("serveur arrêté");
    Ok(())
}

fn report(reason: ExitReason) {
    match reason {
        ExitReason::Detached => eprintln!("[détaché — la session reste active]"),
        ExitReason::PaneExited(code) => eprintln!("[session terminée (code {code})]"),
        ExitReason::ServerGone => eprintln!("[connexion au serveur perdue]"),
    }
}

fn print_help() {
    println!(
        "wimux — multiplexeur de terminal natif Windows\n\
         \n\
         USAGE :\n    \
             wimux <COMMANDE>\n\
         \n\
         COMMANDES :\n    \
             new [-s <nom>]      Crée une session et s'y attache\n    \
             attach <nom>        S'attache à une session existante (alias : a)\n    \
             ls                  Liste les sessions (alias : list-sessions)\n    \
             send-keys -t <nom> <touches...>  Injecte des frappes (scriptable)\n    \
             kill-session <nom>  Termine une session\n    \
             kill-server         Arrête le serveur et toutes les sessions\n\
         \n\
         RACCOURCIS (dans une session) :\n    \
             Ctrl-b d            Se détacher (la session survit)\n\
         \n\
         OPTIONS :\n    \
             -V, --version       Affiche la version\n    \
             -h, --help          Affiche cette aide"
    );
}
