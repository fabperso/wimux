use std::sync::Arc;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};
use wimux_protocol::transport::{PipeConn, connect, user_pipe_name};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, SplitDir, recv, send,
};

/// Connexion partagée au serveur wimux (écrivain).
#[derive(Default)]
struct Bridge {
    conn: Mutex<Option<Arc<PipeConn>>>,
}

fn do_handshake(conn: &PipeConn) -> Result<(), String> {
    let mut w: &PipeConn = conn;
    send(&mut w, &ClientMessage::Hello(Hello {
        client_version: PROTOCOL_VERSION,
        client_build: env!("CARGO_PKG_VERSION").to_string(),
    })).map_err(|e| e.to_string())?;
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
    let conn = match existing {
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
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::AttachGui { session }).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(serde::Serialize)]
struct SessionDto {
    name: String,
    attached: bool,
    activity: bool,
    bell: bool,
    agent: bool,
    agent_status: Option<String>,
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
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::PaneInput { pane_id, bytes }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn split_pane(pane_id: u64, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::SplitPane { pane_id, dir }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::ClosePane { pane_id }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn focus_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::FocusPane { pane_id }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn set_split_ratio(node_id: u32, ratio: f32, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::SetSplitRatio { node_id, ratio })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn pane_resize(pane_id: u64, cols: u16, rows: u16, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::PaneResize { pane_id, cols, rows })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            rename_session,
            list_agent_templates,
            create_agent
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de wimux-gui");
}
