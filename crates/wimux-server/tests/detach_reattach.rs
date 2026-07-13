//! Test d'intégration du jalon J2 : une session survit au détachement du client.
//!
//! Scénario : créer une session, y exécuter une commande dont la sortie est
//! reconnaissable, fermer la connexion (= détachement), se reconnecter et
//! s'attacher à la même session, puis vérifier qu'elle est toujours vivante et
//! affiche le résultat produit avant le détachement.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use wimux_protocol::transport::{PipeConn, connect};
use wimux_protocol::{
    ClientMessage, Frame, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, recv, send,
};
use wimux_server::daemon;

fn frame_text(frame: &Frame) -> String {
    let mut lines = Vec::new();
    for row in 0..frame.rows {
        let start = row as usize * frame.cols as usize;
        let mut line = String::new();
        for cell in &frame.cells[start..start + frame.cols as usize] {
            if cell.width != 0 {
                line.push(cell.ch);
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn start_daemon(pipe: &str) {
    let listener_pipe = pipe.to_string();
    std::thread::spawn(move || {
        let _ = daemon::run_on(&listener_pipe);
    });
    // Laisser le thread créer la première instance de pipe ; `connect_retry`
    // couvre le reste de la latence de démarrage.
    std::thread::sleep(Duration::from_millis(150));
}

fn connect_retry(pipe: &str) -> PipeConn {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match connect(pipe) {
            Ok(conn) => return conn,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("connexion impossible : {e}"),
        }
    }
}

fn handshake(conn: &PipeConn) {
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
    match recv::<_, ServerMessage>(&mut r).unwrap() {
        ServerMessage::Hello(HelloReply::Ok { .. }) => {}
        other => panic!("handshake inattendu : {other:?}"),
    }
}

/// Lance un thread qui draine les messages serveur dans un canal.
fn spawn_reader(conn: Arc<PipeConn>) -> Receiver<ServerMessage> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut r: &PipeConn = &conn;
        while let Ok(msg) = recv::<_, ServerMessage>(&mut r) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    rx
}

/// Attend un message `SetClipboard`, dans la limite du délai.
fn wait_for_clipboard(rx: &Receiver<ServerMessage>, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(ServerMessage::SetClipboard(text)) = rx.recv_timeout(Duration::from_millis(200)) {
            return Some(text);
        }
    }
    None
}

/// Attend qu'une frame contienne `marker`, dans la limite du délai. Renvoie le
/// dernier texte de frame observé.
fn wait_for_marker(
    rx: &Receiver<ServerMessage>,
    marker: &str,
    timeout: Duration,
) -> (bool, String) {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::Frame(frame)) => {
                last = frame_text(&frame);
                if last.contains(marker) {
                    return (true, last);
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    (false, last)
}

#[test]
fn session_survit_au_detachement() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-detach", std::process::id());
    start_daemon(&pipe);

    // --- Client 1 : créer la session et exécuter une commande ---
    let conn1 = Arc::new(connect_retry(&pipe));
    handshake(&conn1);

    {
        let mut w: &PipeConn = &conn1;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("dev".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }

    let rx1 = spawn_reader(Arc::clone(&conn1));

    // Attendre l'attachement + l'invite PowerShell.
    let session_name = {
        // Le premier message doit être Attached.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx1.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::Attached { name }) => break name,
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas d'Attached reçu"),
            }
        }
    };
    assert_eq!(session_name, "dev");

    // Attendre que l'invite PowerShell soit prête (plutôt qu'un délai fixe, pour
    // éviter la flakiness sur un runner lent), puis envoyer une commande dont la
    // sortie « CANARI7788 » n'apparaît QUE si la commande s'exécute (la ligne
    // tapée montre la concaténation, pas le résultat).
    let (prompt_ready, prompt_screen) = wait_for_marker(&rx1, "PS", Duration::from_secs(15));
    assert!(
        prompt_ready,
        "l'invite PowerShell n'est pas apparue.\nÉcran :\n{prompt_screen}"
    );
    {
        let mut w: &PipeConn = &conn1;
        send(
            &mut w,
            &ClientMessage::Input(b"Write-Output ('CANARI' + '7788')\r".to_vec()),
        )
        .unwrap();
    }

    let (found, screen) = wait_for_marker(&rx1, "CANARI7788", Duration::from_secs(12));
    assert!(
        found,
        "la sortie de la commande n'est pas apparue.\nÉcran :\n{screen}"
    );

    // --- Détachement : on ferme la connexion du client 1 ---
    drop(rx1);
    drop(conn1);
    std::thread::sleep(Duration::from_millis(300));

    // --- Client 2 : se rattacher à la même session ---
    let conn2 = Arc::new(connect_retry(&pipe));
    handshake(&conn2);
    {
        let mut w: &PipeConn = &conn2;
        send(
            &mut w,
            &ClientMessage::Attach {
                name: "dev".into(),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let rx2 = spawn_reader(Arc::clone(&conn2));

    // La session doit toujours exister ET son écran doit encore montrer la
    // sortie produite AVANT le détachement.
    let (still_there, screen2) = wait_for_marker(&rx2, "CANARI7788", Duration::from_secs(5));
    assert!(
        still_there,
        "après reattach, la session ne montre plus son contenu.\nÉcran :\n{screen2}"
    );

    // Nettoyage : tuer la session (évite de laisser un PowerShell orphelin).
    let mut w: &PipeConn = &conn2;
    send(&mut w, &ClientMessage::Kill { name: "dev".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn liste_reflete_les_sessions() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-list", std::process::id());
    start_daemon(&pipe);

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);

    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("alpha".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&conn));
    // Consommer l'Attached.
    let _ = rx.recv_timeout(Duration::from_secs(5));

    // Nouvelle connexion pour lister (le client 1 reste attaché).
    let lister = connect_retry(&pipe);
    handshake(&lister);
    {
        let mut w: &PipeConn = &lister;
        send(&mut w, &ClientMessage::List).unwrap();
    }
    let mut r: &PipeConn = &lister;
    let sessions = match recv::<_, ServerMessage>(&mut r).unwrap() {
        ServerMessage::Sessions(s) => s,
        other => panic!("réponse inattendue : {other:?}"),
    };

    assert!(
        sessions.iter().any(|s| s.name == "alpha" && s.attached),
        "la session « alpha » attachée devrait apparaître : {sessions:?}"
    );

    let mut w: &PipeConn = &lister;
    send(
        &mut w,
        &ClientMessage::Kill {
            name: "alpha".into(),
        },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn split_compose_deux_volets_avec_bordure() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-split", std::process::id());
    start_daemon(&pipe);

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("s".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&conn));

    // Consommer l'Attached et attendre l'invite du premier volet.
    let _ = rx.recv_timeout(Duration::from_secs(5));
    let (ready, _) = wait_for_marker(&rx, "PS", Duration::from_secs(15));
    assert!(ready, "invite du premier volet absente");

    // Barre de statut : la dernière frame doit montrer « [s] ».
    let (status, _) = wait_for_marker(&rx, "[s]", Duration::from_secs(3));
    assert!(status, "la barre de statut ne montre pas le nom de session");

    // Découpe verticale (Ctrl-b %). Une bordure « │ » doit apparaître.
    {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::Input(b"\x02%".to_vec())).unwrap();
    }
    let (has_border, screen) = wait_for_marker(&rx, "│", Duration::from_secs(8));
    assert!(
        has_border,
        "après split, aucune bordure verticale dans la composition.\nÉcran :\n{screen}"
    );

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Kill { name: "s".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn mode_copie_selectionne_et_copie() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-copie", std::process::id());
    start_daemon(&pipe);

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("c".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&conn));
    let _ = rx.recv_timeout(Duration::from_secs(5)); // Attached

    let (ready, _) = wait_for_marker(&rx, "PS", Duration::from_secs(15));
    assert!(ready, "invite absente");

    // Produire une sortie reconnaissable (n'apparaît qu'à l'exécution).
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::Input(b"Write-Output ('COPY' + 'MARK99')\r".to_vec()),
        )
        .unwrap();
    }
    let (printed, _) = wait_for_marker(&rx, "COPYMARK99", Duration::from_secs(10));
    assert!(printed, "la sortie n'est pas apparue");

    // Entrer en mode copie (Ctrl-b [). La barre de statut doit montrer « COPIE ».
    {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::Input(b"\x02[".to_vec())).unwrap();
    }
    let (in_copy, _) = wait_for_marker(&rx, "COPIE", Duration::from_secs(5));
    assert!(in_copy, "l'indicateur de mode copie n'apparaît pas");

    // Sélectionner tout le buffer et copier :
    // g (haut), 0 (début), espace (ancre), G (bas), $ (fin), y (copier).
    for key in *b"g0 G$y" {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::Input(vec![key])).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }

    let clip = wait_for_clipboard(&rx, Duration::from_secs(5));
    let clip = clip.expect("aucun message SetClipboard reçu après la copie");
    assert!(
        clip.contains("COPYMARK99"),
        "le presse-papiers ne contient pas le marqueur.\nContenu :\n{clip}"
    );

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Kill { name: "c".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn mode_copie_recherche_puis_copie() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-search", std::process::id());
    start_daemon(&pipe);

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("r".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&conn));
    let _ = rx.recv_timeout(Duration::from_secs(5)); // Attached

    let (ready, _) = wait_for_marker(&rx, "PS", Duration::from_secs(15));
    assert!(ready, "invite absente");

    // Produire une ligne contenant « NEEDLE42 » (uniquement à l'exécution).
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::Input(b"Write-Output ('NEED' + 'LE42')\r".to_vec()),
        )
        .unwrap();
    }
    let (printed, _) = wait_for_marker(&rx, "NEEDLE42", Duration::from_secs(10));
    assert!(printed, "la ligne cible n'est pas apparue");

    // Mode copie, aller en haut, rechercher « NEEDLE42 ».
    {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::Input(b"\x02[".to_vec())).unwrap();
    }
    let (in_copy, _) = wait_for_marker(&rx, "COPIE", Duration::from_secs(5));
    assert!(in_copy, "mode copie non actif");

    {
        let mut w: &PipeConn = &conn;
        // g (haut), /NEEDLE42<Entrée> (recherche).
        send(&mut w, &ClientMessage::Input(b"g/NEEDLE42\r".to_vec())).unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));

    // Copier la ligne trouvée : 0 (début), espace (ancre), $ (fin), y (copier).
    for key in *b"0 $y" {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::Input(vec![key])).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }

    let clip = wait_for_clipboard(&rx, Duration::from_secs(5))
        .expect("aucun SetClipboard après la recherche+copie");
    assert!(
        clip.contains("NEEDLE42"),
        "la recherche n'a pas positionné le curseur sur la cible.\nContenu :\n{clip}"
    );

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Kill { name: "r".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn send_keys_injecte_dans_une_session() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-sendkeys", std::process::id());
    start_daemon(&pipe);

    // Client attaché à la session.
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some("sk".into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&conn));
    let _ = rx.recv_timeout(Duration::from_secs(5)); // Attached
    let (ready, _) = wait_for_marker(&rx, "PS", Duration::from_secs(15));
    assert!(ready, "invite absente");

    // Connexion SÉPARÉE (scriptable) qui injecte une commande via send-keys.
    let scripter = connect_retry(&pipe);
    handshake(&scripter);
    {
        let mut w: &PipeConn = &scripter;
        send(
            &mut w,
            &ClientMessage::SendKeys {
                session: "sk".into(),
                keys: b"Write-Output ('SENT' + 'KEYS')\r".to_vec(),
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &scripter;
    match recv::<_, ServerMessage>(&mut r).unwrap() {
        ServerMessage::Ok => {}
        other => panic!("send-keys : réponse inattendue {other:?}"),
    }

    // La commande injectée doit s'exécuter dans la session attachée.
    let (executed, screen) = wait_for_marker(&rx, "SENTKEYS", Duration::from_secs(10));
    assert!(
        executed,
        "la commande injectée via send-keys ne s'est pas exécutée.\nÉcran :\n{screen}"
    );

    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::Kill { name: "sk".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}
