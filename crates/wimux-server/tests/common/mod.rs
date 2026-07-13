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
