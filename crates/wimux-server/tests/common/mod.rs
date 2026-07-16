//! Helpers partagés par les tests d'intégration du démon (client de test minimal
//! par-dessus le protocole : démarrage, connexion, handshake, lecture asynchrone).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use wimux_protocol::transport::{PipeConn, connect};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, recv, send,
};
use wimux_server::daemon;

pub fn start_daemon(pipe: &str) {
    let p = pipe.to_string();
    std::thread::spawn(move || {
        let _ = daemon::run_on(&p);
    });
    std::thread::sleep(Duration::from_millis(150));
}

pub fn connect_retry(pipe: &str) -> PipeConn {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match connect(pipe) {
            Ok(c) => return c,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("connexion impossible : {e}"),
        }
    }
}

pub fn handshake(conn: &PipeConn) {
    let mut w: &PipeConn = conn;
    send(
        &mut w,
        &ClientMessage::Hello(Hello {
            client_version: PROTOCOL_VERSION,
            client_build: "test".into(),
        }),
    )
    .unwrap();
    let mut r: &PipeConn = conn;
    assert!(matches!(
        recv::<_, ServerMessage>(&mut r).unwrap(),
        ServerMessage::Hello(HelloReply::Ok { .. })
    ));
}

pub fn spawn_reader(conn: Arc<PipeConn>) -> Receiver<ServerMessage> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut r: &PipeConn = &conn;
        while let Ok(m) = recv::<_, ServerMessage>(&mut r) {
            if tx.send(m).is_err() {
                break;
            }
        }
    });
    rx
}

/// Attend un message satisfaisant `pred`, dans la limite du délai.
pub fn wait_for<F: Fn(&wimux_protocol::ServerMessage) -> bool>(
    rx: &std::sync::mpsc::Receiver<wimux_protocol::ServerMessage>,
    timeout: std::time::Duration,
    pred: F,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(m) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
            if pred(&m) {
                return true;
            }
        }
    }
    false
}

/// Démarre un démon de test avec une config construite depuis `conf` (contenu
/// façon `wimux.conf`, appliqué via `Config::apply`). Évite de toucher au
/// fichier utilisateur ou à des variables d'environnement globales.
pub fn start_daemon_with_config(pipe: &str, conf: &str) {
    let p = pipe.to_string();
    let mut config = wimux_server::config::Config::default();
    config.apply(conf);
    std::thread::spawn(move || {
        let _ = daemon::run_on_with_config(&p, config);
    });
    std::thread::sleep(Duration::from_millis(150));
}
