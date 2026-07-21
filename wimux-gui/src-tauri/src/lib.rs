use std::sync::Arc;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};
use wimux_protocol::transport::{connect, user_pipe_name, PipeConn};
use wimux_protocol::{
    recv, send, ClientMessage, Hello, HelloReply, ServerMessage, SplitDir, PROTOCOL_VERSION,
};

/// Connexion partagée au serveur wimux (écrivain).
#[derive(Default)]
struct Bridge {
    conn: Mutex<Option<Arc<PipeConn>>>,
    /// Sérialise TOUTES les écritures sur `conn` (connexion GUI persistante).
    /// Chaque commande Tauri (pane_input, pane_resize, web_back, list_windows,
    /// attach_session...) peut s'exécuter sur son propre thread, indépendamment
    /// des autres : sans ce verrou, deux `send` concurrents entrelaceraient
    /// leurs trames sur le même handle de pipe et désynchroniseraient le
    /// protocole côté serveur (celui-ci `break`rait alors sa boucle de lecture
    /// sur la trame illisible, coupant silencieusement tout flux futur — c'est
    /// exactement le défaut que `gui_write` évite côté serveur pour ses propres
    /// écritures ; il manquait le pendant côté client).
    write_lock: Mutex<()>,
}

/// Envoie un message sur la connexion GUI persistante, sous `write_lock`.
/// No-op silencieux si aucune connexion n'est encore établie (avant le premier
/// `attach_session`) : les commandes de volet appelées trop tôt ne doivent pas
/// échouer bruyamment, elles n'ont simplement aucun effet.
fn send_persistent(bridge: &Bridge, msg: &ClientMessage) -> Result<(), String> {
    let conn = bridge.conn.lock().unwrap().clone();
    if let Some(conn) = conn {
        let _g = bridge.write_lock.lock().unwrap();
        let mut w: &PipeConn = &conn;
        send(&mut w, msg).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn do_handshake(conn: &PipeConn) -> Result<(), String> {
    let mut w: &PipeConn = conn;
    send(
        &mut w,
        &ClientMessage::Hello(Hello {
            client_version: PROTOCOL_VERSION,
            client_build: env!("CARGO_PKG_VERSION").to_string(),
        }),
    )
    .map_err(|e| e.to_string())?;
    let mut r: &PipeConn = conn;
    match recv::<_, ServerMessage>(&mut r).map_err(|e| e.to_string())? {
        ServerMessage::Hello(HelloReply::Ok { .. }) => Ok(()),
        _ => Err("handshake refusé".into()),
    }
}

/// Ouvre une connexion de contrôle jetable, fait le handshake, envoie un
/// message et parse la réponse.
fn control<F, R>(build: impl FnOnce() -> ClientMessage, parse: F) -> Result<R, String>
where
    F: FnOnce(ServerMessage) -> Result<R, String>,
{
    let conn = connect(&user_pipe_name()).map_err(|_| "serveur wimux introuvable".to_string())?;
    do_handshake(&conn)?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &build()).map_err(|e| e.to_string())?;
    let mut r: &PipeConn = &conn;
    let msg = recv::<_, ServerMessage>(&mut r).map_err(|e| e.to_string())?;
    parse(msg)
}

#[tauri::command]
fn attach_session(session: String, app: AppHandle, bridge: State<Bridge>) -> Result<(), String> {
    let existing = bridge.conn.lock().unwrap().clone();
    // Le thread lecteur est démarré une seule fois, à la première connexion ;
    // sa valeur de retour ne sert qu'à ce `match` (elle est déjà stockée dans
    // `bridge.conn`), d'où le `_` : `send_persistent` relit `bridge.conn`.
    let _conn = match existing {
        Some(c) => c, // réutiliser : le thread lecteur tourne déjà
        None => {
            let c = Arc::new(
                connect(&user_pipe_name()).map_err(|_| "serveur wimux introuvable".to_string())?,
            );
            do_handshake(&c)?;
            *bridge.conn.lock().unwrap() = Some(Arc::clone(&c));
            // Thread lecteur (une seule fois) : relaie snapshot/output/error au frontend.
            let reader = Arc::clone(&c);
            let app2 = app.clone();
            std::thread::spawn(move || {
                let mut r: &PipeConn = &reader;
                while let Ok(msg) = recv::<_, ServerMessage>(&mut r) {
                    match msg {
                        ServerMessage::PaneSnapshot { pane_id, bytes } => {
                            let _ = app2.emit("pane-snapshot", (pane_id, bytes));
                        }
                        ServerMessage::PaneOutput { pane_id, bytes } => {
                            let _ = app2.emit("pane-output", (pane_id, bytes));
                        }
                        ServerMessage::WindowLayout { tree, active } => {
                            let _ = app2.emit("window-layout", (tree, active));
                        }
                        ServerMessage::WindowList { windows, active } => {
                            let _ = app2.emit("window-list", (windows, active));
                        }
                        ServerMessage::Error(m) => {
                            let _ = app2.emit("pane-error", m);
                        }
                        _ => {}
                    }
                }
            });
            c
        }
    };
    send_persistent(&bridge, &ClientMessage::AttachGui { session })
}

#[derive(serde::Serialize)]
struct SessionDto {
    name: String,
    attached: bool,
    activity: bool,
    bell: bool,
    agent: bool,
    agent_status: Option<String>,
    group: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    color: Option<String>,
    pinned: bool,
    layout_rev: u64,
}

/// Libellé stable d'un `AgentStatus` pour le frontend (mappé sur un glyphe côté
/// TypeScript).
fn agent_status_label(status: wimux_protocol::AgentStatus) -> String {
    use wimux_protocol::AgentStatus::*;
    match status {
        Working => "Working",
        Idle => "Idle",
        Attention => "Attention",
        Done => "Done",
        Error => "Error",
    }
    .to_string()
}

#[tauri::command]
fn list_sessions() -> Result<Vec<SessionDto>, String> {
    control(
        || ClientMessage::List,
        |msg| match msg {
            ServerMessage::Sessions(v) => Ok(v
                .into_iter()
                .map(|s| SessionDto {
                    name: s.name,
                    attached: s.attached,
                    activity: s.activity,
                    bell: s.bell,
                    agent: s.agent,
                    agent_status: s.agent_status.map(agent_status_label),
                    group: s.group,
                    cwd: s.cwd,
                    branch: s.branch,
                    color: s.color,
                    pinned: s.pinned,
                    layout_rev: s.layout_rev,
                })
                .collect()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn create_session(name: Option<String>) -> Result<String, String> {
    control(
        || ClientMessage::CreateSession { name },
        |msg| match msg {
            ServerMessage::SessionCreated { name } => Ok(name),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[derive(serde::Serialize)]
struct AgentTemplateDto {
    name: String,
}

#[tauri::command]
fn list_agent_templates() -> Result<Vec<AgentTemplateDto>, String> {
    control(
        || ClientMessage::ListAgentTemplates,
        |msg| match msg {
            ServerMessage::AgentTemplates(v) => Ok(v
                .into_iter()
                .map(|t| AgentTemplateDto { name: t.name })
                .collect()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn create_agent(
    template: String,
    prompt: String,
    cwd: Option<String>,
    name: Option<String>,
) -> Result<String, String> {
    control(
        || ClientMessage::CreateAgentSession {
            name,
            template,
            prompt,
            cwd,
        },
        |msg| match msg {
            ServerMessage::SessionCreated { name } => Ok(name),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn create_batch(
    template: String,
    prompt: String,
    base_repo: String,
    count: u32,
) -> Result<String, String> {
    control(
        || ClientMessage::CreateAgentBatch {
            template,
            prompt,
            base_repo,
            count,
        },
        |msg| match msg {
            ServerMessage::BatchCreated { group, .. } => Ok(group),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn kill_session(name: String) -> Result<(), String> {
    control(
        || ClientMessage::Kill { name },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn reorder_sessions(names: Vec<String>) -> Result<(), String> {
    control(
        || ClientMessage::ReorderSessions { names },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn set_session_color(name: String, color: Option<String>) -> Result<(), String> {
    control(
        || ClientMessage::SetSessionColor { name, color },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn set_session_pinned(name: String, pinned: bool) -> Result<(), String> {
    control(
        || ClientMessage::SetSessionPinned { name, pinned },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[derive(serde::Serialize)]
struct NotificationDto {
    session: String,
    title: Option<String>,
    body: String,
}

#[tauri::command]
fn take_notifications() -> Result<Vec<NotificationDto>, String> {
    control(
        || ClientMessage::TakeNotifications,
        |msg| match msg {
            ServerMessage::Notifications(v) => Ok(v
                .into_iter()
                .map(|n| NotificationDto {
                    session: n.session,
                    title: n.title,
                    body: n.body,
                })
                .collect()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn mark_session_read(name: String) -> Result<(), String> {
    control(
        || ClientMessage::MarkSessionRead { name },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn mark_session_unread(name: String) -> Result<(), String> {
    control(
        || ClientMessage::MarkSessionUnread { name },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn rename_session(from: String, to: String) -> Result<(), String> {
    control(
        || ClientMessage::RenameSession { from, to },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn pane_input(pane_id: u64, bytes: Vec<u8>, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::PaneInput { pane_id, bytes })
}

#[tauri::command]
fn split_pane(pane_id: u64, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    send_persistent(&bridge, &ClientMessage::SplitPane { pane_id, dir })
}

#[tauri::command]
fn close_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::ClosePane { pane_id })
}

#[tauri::command]
fn focus_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::FocusPane { pane_id })
}

#[tauri::command]
fn set_split_ratio(node_id: u32, ratio: f32, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::SetSplitRatio { node_id, ratio })
}

#[tauri::command]
fn pane_resize(pane_id: u64, cols: u16, rows: u16, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::PaneResize {
            pane_id,
            cols,
            rows,
        },
    )
}

#[tauri::command]
fn open_web_pane(url: String, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    send_persistent(
        &bridge,
        &ClientMessage::OpenWebPane {
            session: String::new(),
            from_pane: None,
            dir,
            url,
        },
    )
}

#[tauri::command]
fn web_navigate(pane_id: u64, url: String, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::WebNavigate {
            session: String::new(),
            pane: pane_id,
            url,
        },
    )
}

#[tauri::command]
fn web_back(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::WebBack {
            session: String::new(),
            pane: pane_id,
        },
    )
}

#[tauri::command]
fn web_forward(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::WebForward {
            session: String::new(),
            pane: pane_id,
        },
    )
}

#[tauri::command]
fn new_window(bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::NewWindow)
}

#[tauri::command]
fn list_windows(bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::ListWindows)
}

#[tauri::command]
fn select_window(index: u32, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::SelectWindow { index })
}

#[tauri::command]
fn close_window(index: u32, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::CloseWindow { index })
}

#[tauri::command]
fn rename_window(index: u32, name: String, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::RenameWindow { index, name })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(Bridge::default())
        .invoke_handler(tauri::generate_handler![
            attach_session,
            pane_input,
            pane_resize,
            split_pane,
            close_pane,
            focus_pane,
            set_split_ratio,
            list_sessions,
            create_session,
            kill_session,
            reorder_sessions,
            set_session_color,
            set_session_pinned,
            take_notifications,
            mark_session_read,
            mark_session_unread,
            rename_session,
            list_agent_templates,
            create_agent,
            create_batch,
            new_window,
            list_windows,
            select_window,
            close_window,
            rename_window,
            open_web_pane,
            web_navigate,
            web_back,
            web_forward
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de wimux-gui");
}
