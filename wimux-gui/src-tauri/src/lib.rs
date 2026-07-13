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

#[tauri::command]
fn gui_attach(session: String, app: AppHandle, bridge: State<Bridge>) -> Result<(), String> {
    let conn = Arc::new(connect(&user_pipe_name()).map_err(|_| "serveur wimux introuvable".to_string())?);
    do_handshake(&conn)?;
    {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::AttachGui { session }).map_err(|e| e.to_string())?;
    }
    *bridge.conn.lock().unwrap() = Some(Arc::clone(&conn));

    // Thread lecteur : relaye les messages serveur vers le frontend.
    let reader = Arc::clone(&conn);
    std::thread::spawn(move || {
        let mut r: &PipeConn = &reader;
        while let Ok(msg) = recv::<_, ServerMessage>(&mut r) {
            match msg {
                ServerMessage::PaneSnapshot { pane_id, bytes } => {
                    let _ = app.emit("pane-snapshot", (pane_id, bytes));
                }
                ServerMessage::PaneOutput { pane_id, bytes } => {
                    let _ = app.emit("pane-output", (pane_id, bytes));
                }
                ServerMessage::Error(msg) => {
                    let _ = app.emit("pane-error", msg);
                }
                _ => {}
            }
        }
    });
    Ok(())
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
        .invoke_handler(tauri::generate_handler![gui_attach, pane_input])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de wimux-gui");
}
