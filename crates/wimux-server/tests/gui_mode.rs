//! Test d'intégration du mode GUI : un client GUI s'attache à une session
//! existante, reçoit un `PaneSnapshot` puis un flux de `PaneOutput`, et peut
//! injecter des frappes via `PaneInput`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use wimux_protocol::transport::PipeConn;
use wimux_protocol::{ClientMessage, ServerMessage, send};

#[test]
fn attach_gui_recoit_snapshot_puis_flux() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-gui", std::process::id());
    start_daemon(&pipe);

    // Créer une session en mode TUI classique (pour avoir un volet vivant).
    let owner = Arc::new(connect_retry(&pipe));
    handshake(&owner);
    {
        let mut w: &PipeConn = &owner;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("g".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let orx = spawn_reader(Arc::clone(&owner));
    // consommer Attached + attendre l'invite
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match orx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::Frame(_)) => break, // au moins une frame => shell demarre
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de frame"),
        }
    }
    std::thread::sleep(Duration::from_millis(1500)); // laisser l'invite s'etablir

    // Client GUI : s'attacher, recevoir le snapshot, injecter une commande.
    let gui = Arc::new(connect_retry(&pipe));
    handshake(&gui);
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::AttachGui {
                session: "g".into(),
            },
        )
        .unwrap();
    }
    let grx = spawn_reader(Arc::clone(&gui));

    // Le premier message GUI doit etre un PaneSnapshot.
    let pane_id = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneSnapshot { pane_id, .. }) => break pane_id,
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de PaneSnapshot"),
            }
        }
    };

    // Injecter une commande via PaneInput ; la sortie doit revenir en PaneOutput.
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::PaneInput {
                pane_id,
                bytes: b"Write-Output ('GUI' + 'OK')\r".to_vec(),
            },
        )
        .unwrap();
    }

    let mut acc = String::new();
    let found = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneOutput { bytes, .. }) => {
                    acc.push_str(&String::from_utf8_lossy(&bytes));
                    if acc.contains("GUIOK") {
                        break true;
                    }
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => break false,
            }
        }
    };
    assert!(
        found,
        "PaneOutput ne contient pas la sortie.\nRecu :\n{acc}"
    );

    let mut w: &PipeConn = &owner;
    send(&mut w, &ClientMessage::Kill { name: "g".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn attach_gui_session_inexistante_renvoie_erreur() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-guierr", std::process::id());
    start_daemon(&pipe);
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::AttachGui {
                session: "inexistante".into(),
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&conn));
    let got_error = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::Error(_)) => break true,
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => break false,
            }
        }
    };
    assert!(
        got_error,
        "attach GUI vers session inexistante aurait dû renvoyer Error"
    );
}
