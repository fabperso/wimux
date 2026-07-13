use std::sync::Arc;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};
use wimux_protocol::transport::{PipeConn, connect, user_pipe_name};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, recv, send,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Bridge::default())
        .invoke_handler(tauri::generate_handler![
            attach_session,
            pane_input,
            list_sessions,
            create_session,
            kill_session,
            rename_session
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de wimux-gui");
}
