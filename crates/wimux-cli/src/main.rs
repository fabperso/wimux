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

mod agent {
    use std::io;
    use wimux_protocol::SplitDir;

    /// Arguments analysés de `wimux agent spawn`.
    pub struct SpawnArgs {
        pub session: Option<String>,
        pub from_pane: Option<u64>,
        pub dir: SplitDir,
        pub cwd: Option<String>,
        pub program: String,
        pub program_args: Vec<String>,
    }

    /// Analyse `wimux agent spawn [--dir h|v] [--cwd DIR] [-t SESSION] [--from-pane ID] -- <prog...>`.
    pub fn parse_spawn(args: &[String]) -> io::Result<SpawnArgs> {
        let mut session = None;
        let mut from_pane = None;
        let mut dir = SplitDir::LeftRight; // défaut : côte à côte
        let mut cwd = None;
        let mut i = 0;
        let mut rest: Vec<String> = Vec::new();
        while i < args.len() {
            match args[i].as_str() {
                "--" => {
                    rest = args[i + 1..].to_vec();
                    break;
                }
                "-t" | "--target" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                "--from-pane" | "-p" => {
                    from_pane = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                "--cwd" => {
                    cwd = args.get(i + 1).cloned();
                    i += 2;
                }
                "--dir" => {
                    dir = match args.get(i + 1).map(String::as_str) {
                        Some("v") | Some("vertical") => SplitDir::TopBottom,
                        _ => SplitDir::LeftRight,
                    };
                    i += 2;
                }
                _ => i += 1,
            }
        }
        let program = rest.first().cloned().ok_or_else(|| {
            io::Error::other("usage : wimux agent spawn [flags] -- <commande...>")
        })?;
        Ok(SpawnArgs {
            session,
            from_pane,
            dir,
            cwd,
            program,
            program_args: rest[1..].to_vec(),
        })
    }

    /// Extrait `(session, pane)` de flags `-t SESSION -p PANE` (capture/logs/send/kill).
    pub fn parse_target_pane(args: &[String]) -> (Option<String>, Option<u64>, Vec<String>) {
        let mut session = None;
        let mut pane = None;
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-t" | "--target" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                "-p" | "--pane" => {
                    pane = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                other => {
                    rest.push(other.to_string());
                    i += 1;
                }
            }
        }
        (session, pane, rest)
    }

    /// Échappe une chaîne pour l'insérer dans du JSON (backslash + guillemets + contrôles simples).
    pub fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out
    }

    /// Retire les séquences CSI (`ESC[..lettre`) et OSC (`ESC]..BEL|ST`) pour rendre
    /// un journal lisible. Suffisant pour un flux ligne à ligne.
    pub fn strip_ansi(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(s.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'[' => {
                        i += 2;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                        i += 1;
                    }
                    b']' => {
                        i += 2;
                        while i < bytes.len() && bytes[i] != 0x07 {
                            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                        i += 1;
                    }
                    _ => i += 2,
                }
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

mod batch {
    use std::io;

    /// Arguments analysés de `wimux batch create`.
    pub struct CreateArgs {
        pub repo: String,
        pub template: String,
        pub prompt: String,
        pub count: u32,
    }

    /// Analyse `wimux batch create --repo <p> --template <t> --prompt "…" [--count N]`.
    pub fn parse_create(args: &[String]) -> io::Result<CreateArgs> {
        let (mut repo, mut template, mut prompt) = (None, None, None);
        let mut count = 2u32;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--repo" => {
                    repo = args.get(i + 1).cloned();
                    i += 2;
                }
                "--template" => {
                    template = args.get(i + 1).cloned();
                    i += 2;
                }
                "--prompt" => {
                    prompt = args.get(i + 1).cloned();
                    i += 2;
                }
                "--count" => {
                    count = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(2);
                    i += 2;
                }
                _ => i += 1,
            }
        }
        match (repo, template, prompt) {
            (Some(repo), Some(template), Some(prompt)) => Ok(CreateArgs {
                repo,
                template,
                prompt,
                count,
            }),
            _ => Err(io::Error::other(
                "usage : wimux batch create --repo <chemin> --template <nom> --prompt \"…\" [--count N]",
            )),
        }
    }

    /// Extrait `-g <group>` et `-i <index>` (ou `-s <session>`).
    ///
    /// Non utilisée avant la Task 8 (verbes `review`/`diff`/`pr`) : autorisée à
    /// rester non appelée dans cette task pour que le module compile déjà avec
    /// sa forme finale.
    #[allow(dead_code)]
    pub fn parse_target(
        args: &[String],
    ) -> (Option<String>, Option<u32>, Option<String>, Vec<String>) {
        let (mut group, mut index, mut session) = (None, None, None);
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-g" | "--group" => {
                    group = args.get(i + 1).cloned();
                    i += 2;
                }
                "-i" | "--index" => {
                    index = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                "-s" | "--session" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                other => {
                    rest.push(other.to_string());
                    i += 1;
                }
            }
        }
        (group, index, session, rest)
    }
}

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
        Some("agent") => cmd_agent(&args[1..]),
        Some("batch") => cmd_batch(&args[1..]),
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

/// Ouvre une connexion + handshake, ou erreur si aucun serveur.
fn connected() -> io::Result<PipeConn> {
    let conn =
        connect(&user_pipe_name()).map_err(|_| io::Error::other("aucun serveur en cours"))?;
    handshake(&conn)?;
    Ok(conn)
}

/// Session par défaut : `-t` explicite, sinon `$WIMUX_SESSION`.
fn default_session(explicit: Option<String>) -> io::Result<String> {
    explicit
        .or_else(|| std::env::var("WIMUX_SESSION").ok())
        .ok_or_else(|| {
            io::Error::other("aucune session : passez -t <session> ou lancez depuis un volet wimux")
        })
}

/// Pane par défaut : `-p` explicite, sinon `$WIMUX_PANE`.
fn default_pane(explicit: Option<u64>) -> io::Result<u64> {
    explicit
        .or_else(|| {
            std::env::var("WIMUX_PANE")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .ok_or_else(|| {
            io::Error::other("aucun volet : passez -p <pane> ou lancez depuis un volet wimux")
        })
}

fn cmd_agent(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("spawn") => agent_spawn(&args[1..]),
        Some("list") => agent_list(&args[1..]),
        Some("capture") => agent_capture(&args[1..]),
        Some("logs") => agent_logs(&args[1..]),
        Some("send") => agent_send(&args[1..]),
        Some("kill") => agent_kill(&args[1..]),
        Some("whoami") => agent_whoami(),
        _ => Err(io::Error::other(
            "usage : wimux agent <spawn|list|logs|capture|send|kill|whoami> ...",
        )),
    }
}

fn agent_spawn(args: &[String]) -> io::Result<()> {
    let a = agent::parse_spawn(args)?;
    let session = default_session(a.session)?;
    let from_pane = a.from_pane.or_else(|| {
        std::env::var("WIMUX_PANE")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::SpawnPane {
            session,
            from_pane,
            dir: a.dir,
            cwd: a.cwd,
            program: a.program,
            args: a.program_args,
        },
    )?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneSpawned { pane_id } => {
            println!("{{\"pane_id\":{pane_id}}}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_list(args: &[String]) -> io::Result<()> {
    let (session, _pane, _rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ListPanes { session })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneList(panes) => {
            // JSON array manuel (pas de dépendance serde_json).
            let items: Vec<String> = panes
                .iter()
                .map(|p| {
                    let cwd = p.cwd.as_deref().map(|c| format!("\"{}\"", agent::json_escape(c))).unwrap_or_else(|| "null".into());
                    let log = p.log_path.as_deref().map(|c| format!("\"{}\"", agent::json_escape(c))).unwrap_or_else(|| "null".into());
                    let ec = p.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "null".into());
                    format!(
                        "{{\"pane_id\":{},\"running\":{},\"exit_code\":{},\"cwd\":{},\"log_path\":{}}}",
                        p.pane_id, p.running, ec, cwd, log
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_capture(args: &[String]) -> io::Result<()> {
    let (session, pane, _rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::CapturePane { session, pane })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneCapture(text) => {
            println!("{text}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_send(args: &[String]) -> io::Result<()> {
    let (session, pane, rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    if rest.is_empty() {
        return Err(io::Error::other("aucune touche à envoyer"));
    }
    let keys = translate_keys(&rest);
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::SendKeysPane {
            session,
            pane,
            keys,
        },
    )?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_kill(args: &[String]) -> io::Result<()> {
    let (session, pane, _rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::KillPane { session, pane })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_whoami() -> io::Result<()> {
    let session = std::env::var("WIMUX_SESSION").unwrap_or_default();
    let pane = std::env::var("WIMUX_PANE").unwrap_or_default();
    let pipe = std::env::var("WIMUX_PIPE").unwrap_or_else(|_| user_pipe_name());
    println!(
        "{{\"session\":\"{}\",\"pane\":\"{}\",\"pipe\":\"{}\"}}",
        agent::json_escape(&session),
        agent::json_escape(&pane),
        agent::json_escape(&pipe)
    );
    Ok(())
}

/// Lecture (et suivi) du journal d'un volet, avec dé-ANSI par défaut.
fn agent_logs(args: &[String]) -> io::Result<()> {
    let (session, pane, rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    let mut tail: Option<usize> = None;
    let mut follow = false;
    let mut raw = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--tail" => {
                tail = rest.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--follow" | "-f" => {
                follow = true;
                i += 1;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    // Résoudre le chemin du journal via ListPanes.
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::ListPanes {
            session: session.clone(),
        },
    )?;
    let mut r: &PipeConn = &conn;
    let path = match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneList(panes) => panes
            .into_iter()
            .find(|p| p.pane_id == pane)
            .and_then(|p| p.log_path)
            .ok_or_else(|| io::Error::other(format!("volet {pane} sans journal")))?,
        ServerMessage::Error(e) => return Err(io::Error::other(e)),
        _ => return Err(io::Error::other("réponse inattendue du serveur")),
    };

    let render = |content: &str| -> String {
        let text = if raw {
            content.to_string()
        } else {
            agent::strip_ansi(content)
        };
        match tail {
            Some(n) => {
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            }
            None => text,
        }
    };

    // Lecture initiale.
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    println!("{}", render(&content));
    let mut last_len = content.len() as u64;

    if !follow {
        return Ok(());
    }
    // Imprime les octets ajoutés depuis `last_len` (dé-ANSI par bloc si `!raw`)
    // et avance `last_len`. Factorisé car appelé à chaque tour ET une dernière
    // fois après détection de la fin de l'agent.
    let print_new = |last_len: &mut u64| {
        let content = std::fs::read(&path).unwrap_or_default();
        if (content.len() as u64) > *last_len {
            let slice = &content[*last_len as usize..];
            let chunk = String::from_utf8_lossy(slice);
            let text = if raw {
                chunk.to_string()
            } else {
                agent::strip_ansi(&chunk)
            };
            print!("{text}");
            let _ = io::stdout().flush();
            *last_len = content.len() as u64;
        }
    };

    // Suivi : relire les octets ajoutés au fichier (best-effort, dé-ANSI par
    // bloc). S'arrête dès que le volet n'est plus « running » (agent terminé) :
    // sans TTY côté appelant (Claude), un `--follow` sans fin bloquerait pour
    // toujours faute de Ctrl-C possible.
    loop {
        std::thread::sleep(Duration::from_millis(300));
        print_new(&mut last_len);

        // Re-interroger l'état du volet via une connexion courte (comme le
        // reste du CLI) : absent ou non-running -> volet terminé, dernière
        // lecture puis sortie de boucle.
        let still_running = (|| -> io::Result<bool> {
            let conn = connected()?;
            let mut w: &PipeConn = &conn;
            send(
                &mut w,
                &ClientMessage::ListPanes {
                    session: session.clone(),
                },
            )?;
            let mut r: &PipeConn = &conn;
            match recv::<_, ServerMessage>(&mut r)? {
                ServerMessage::PaneList(panes) => Ok(panes
                    .into_iter()
                    .find(|p| p.pane_id == pane)
                    .map(|p| p.running)
                    .unwrap_or(false)),
                _ => Ok(false),
            }
        })()
        .unwrap_or(false);

        if !still_running {
            // Dernière lecture pour ne rien perdre entre la dernière boucle et
            // la fin effective de l'écriture dans le journal.
            print_new(&mut last_len);
            return Ok(());
        }
    }
}

fn cmd_batch(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("create") => batch_create(&args[1..]),
        Some("list") => batch_list(),
        Some("review") => batch_review(&args[1..]),
        Some("diff") => batch_diff(&args[1..]),
        Some("pr") => batch_pr(&args[1..]),
        _ => Err(io::Error::other(
            "usage : wimux batch <create|list|review|diff|pr> …",
        )),
    }
}

fn batch_create(args: &[String]) -> io::Result<()> {
    let a = batch::parse_create(args)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::CreateAgentBatch {
            template: a.template,
            prompt: a.prompt,
            base_repo: a.repo,
            count: a.count,
        },
    )?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BatchCreated { group, sessions } => {
            let names: Vec<String> = sessions
                .iter()
                .map(|s| format!("\"{}\"", agent::json_escape(s)))
                .collect();
            println!(
                "{{\"group\":\"{}\",\"sessions\":[{}]}}",
                agent::json_escape(&group),
                names.join(",")
            );
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn batch_list() -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ListBatches)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Batches(batches) => {
            let items: Vec<String> = batches
                .iter()
                .map(|b| {
                    let sessions: Vec<String> = b
                        .sessions
                        .iter()
                        .map(|s| format!("\"{}\"", agent::json_escape(s)))
                        .collect();
                    format!(
                        "{{\"group\":\"{}\",\"base_repo\":\"{}\",\"base_branch\":\"{}\",\"sessions\":[{}]}}",
                        agent::json_escape(&b.group),
                        agent::json_escape(&b.base_repo),
                        agent::json_escape(&b.base_branch),
                        sessions.join(",")
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

// TODO(Task 8) : stubs temporaires, remplacés par l'implémentation réelle de
// `review`/`diff`/`pr` (revue agrégée d'un lot d'agents).
fn batch_review(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other(
        "wimux batch review : implémenté en Task 8",
    ))
}
fn batch_diff(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("wimux batch diff : implémenté en Task 8"))
}
fn batch_pr(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("wimux batch pr : implémenté en Task 8"))
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
             agent <sous-cmd>    Orchestration d'agents (spawn/list/logs/capture/send/kill/whoami)\n    \
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

#[cfg(test)]
mod agent_tests {
    use super::agent::*;
    use wimux_protocol::SplitDir;

    #[test]
    fn parse_spawn_separe_le_programme_apres_double_tiret() {
        let a = parse_spawn(&[
            "--dir".into(),
            "v".into(),
            "-t".into(),
            "sess".into(),
            "--from-pane".into(),
            "4".into(),
            "--".into(),
            "claude".into(),
            "-p".into(),
            "fais X".into(),
        ])
        .unwrap();
        assert_eq!(a.session.as_deref(), Some("sess"));
        assert_eq!(a.from_pane, Some(4));
        assert!(matches!(a.dir, SplitDir::TopBottom));
        assert_eq!(a.program, "claude");
        assert_eq!(a.program_args, vec!["-p".to_string(), "fais X".to_string()]);
    }

    #[test]
    fn parse_spawn_defaut_dir_horizontal_sans_programme_est_erreur() {
        let a = parse_spawn(&["--".into(), "cmd.exe".into()]).unwrap();
        assert!(matches!(a.dir, SplitDir::LeftRight)); // défaut
        assert!(parse_spawn(&["--dir".into(), "h".into()]).is_err()); // pas de programme
    }

    #[test]
    fn json_escape_echappe_backslash_et_guillemets() {
        assert_eq!(json_escape("C:\\a\"b"), "C:\\\\a\\\"b");
    }

    #[test]
    fn strip_ansi_retire_csi_et_osc() {
        assert_eq!(strip_ansi("\x1b[31mrouge\x1b[0m"), "rouge");
        assert_eq!(strip_ansi("\x1b]9;notif\x07texte"), "texte");
    }
}

#[cfg(test)]
mod batch_tests {
    use super::batch::*;

    #[test]
    fn parse_create_lit_tous_les_champs() {
        let a = parse_create(&[
            "--repo".into(),
            "C:\\repo".into(),
            "--template".into(),
            "claude".into(),
            "--prompt".into(),
            "corrige le parser".into(),
            "--count".into(),
            "3".into(),
        ])
        .unwrap();
        assert_eq!(a.repo, "C:\\repo");
        assert_eq!(a.template, "claude");
        assert_eq!(a.prompt, "corrige le parser");
        assert_eq!(a.count, 3);
    }

    #[test]
    fn parse_create_exige_repo_template_prompt() {
        assert!(parse_create(&["--repo".into(), "C:\\repo".into()]).is_err());
    }

    #[test]
    fn parse_create_count_defaut_est_deux() {
        let a = parse_create(&[
            "--repo".into(),
            "r".into(),
            "--template".into(),
            "t".into(),
            "--prompt".into(),
            "p".into(),
        ])
        .unwrap();
        assert_eq!(a.count, 2, "count par défaut = 2");
    }
}
