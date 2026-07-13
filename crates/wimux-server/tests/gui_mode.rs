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
fn bascule_gui_arrete_le_flux_precedent() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-switch", std::process::id());
    common::start_daemon(&pipe);

    // Créer A et B via des clients TUI (pour avoir des volets vivants).
    for name in ["A", "B"] {
        let c = std::sync::Arc::new(common::connect_retry(&pipe));
        common::handshake(&c);
        let mut w: &wimux_protocol::transport::PipeConn = &c;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::NewSession {
                name: Some(name.to_string()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
        // garder la connexion vivante un court instant pour laisser le shell démarrer
        std::thread::sleep(std::time::Duration::from_millis(800));
        // on laisse `c` tomber : la session A/B survit (détachement).
    }

    // Client GUI : attache A.
    let gui = std::sync::Arc::new(common::connect_retry(&pipe));
    common::handshake(&gui);
    {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::AttachGui {
                session: "A".into(),
            },
        )
        .unwrap();
    }
    let rx = common::spawn_reader(std::sync::Arc::clone(&gui));

    // Attendre le snapshot de A.
    common::wait_for(&rx, std::time::Duration::from_secs(5), |m| {
        matches!(m, wimux_protocol::ServerMessage::PaneSnapshot { .. })
    });

    // Basculer sur B.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::AttachGui {
                session: "B".into(),
            },
        )
        .unwrap();
    }
    // On doit recevoir un nouveau PaneSnapshot (celui de B).
    let got_b_snapshot = common::wait_for(&rx, std::time::Duration::from_secs(5), |m| {
        matches!(m, wimux_protocol::ServerMessage::PaneSnapshot { .. })
    });
    assert!(got_b_snapshot, "pas de snapshot après bascule sur B");

    // Nettoyage.
    for name in ["A", "B"] {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        let _ = wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::Kill {
                name: name.to_string(),
            },
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
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
