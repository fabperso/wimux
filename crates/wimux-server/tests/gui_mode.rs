//! Test d'intégration du mode GUI : un client GUI s'attache à une session
//! existante, reçoit un `PaneSnapshot` puis un flux de `PaneOutput`, et peut
//! injecter des frappes via `PaneInput`.

mod common;

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use common::*;
use wimux_protocol::transport::PipeConn;
use wimux_protocol::{
    ClientMessage, LayoutNode, PaneKind, ServerMessage, SessionInfo, SplitDir, WindowInfo, recv,
    send,
};

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
    // L'échéance est évaluée EN TÊTE de boucle (et non seulement dans le bras
    // `Err`) : le volet produit des `Frame` en continu, donc si on ne
    // vérifiait `deadline` qu'à l'échec du `recv_timeout`, un flux ininterrompu
    // de messages ferait tourner la boucle indéfiniment au lieu d'échouer
    // proprement (voir Fix 3 de la revue).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut got_frame = false;
    while !got_frame && Instant::now() < deadline {
        match orx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::Frame(_)) => got_frame = true, // au moins une frame => shell demarre
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(got_frame, "pas de frame");
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
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneSnapshot { pane_id, .. }) => found = Some(pane_id),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        found.expect("pas de PaneSnapshot")
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
        let mut ok = false;
        while !ok && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneOutput { bytes, .. }) => {
                    acc.push_str(&String::from_utf8_lossy(&bytes));
                    if acc.contains("GUIOK") {
                        ok = true;
                    }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        ok
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

    // Attendre le snapshot de A et capturer son pane_id.
    let pane_id_a = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(wimux_protocol::ServerMessage::PaneSnapshot { pane_id, .. }) => {
                    found = Some(pane_id)
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        found.expect("pas de PaneSnapshot pour A")
    };

    // Faire produire à A une sortie continue pendant plusieurs secondes, pour
    // pouvoir vérifier après la bascule qu'elle ne fuit plus vers le client GUI.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::PaneInput {
                pane_id: pane_id_a,
                bytes: b"1..40 | ForEach-Object { $_; Start-Sleep -Milliseconds 100 }\r".to_vec(),
            },
        )
        .unwrap();
    }

    // Attendre au moins un PaneOutput de A : preuve que le flux de A est bien
    // en train de diffuser avant la bascule.
    let a_diffuse = common::wait_for(&rx, Duration::from_secs(5), |m| {
        matches!(
            m,
            wimux_protocol::ServerMessage::PaneOutput { pane_id, .. } if *pane_id == pane_id_a
        )
    });
    assert!(a_diffuse, "A n'a produit aucun PaneOutput avant la bascule");

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
    // On doit recevoir un nouveau PaneSnapshot (celui de B) et son pane_id doit
    // différer de celui de A (volets distincts).
    let pane_id_b = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(wimux_protocol::ServerMessage::PaneSnapshot { pane_id, .. }) => {
                    found = Some(pane_id)
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        found.expect("pas de snapshot après bascule sur B")
    };
    assert_ne!(pane_id_a, pane_id_b, "A et B partagent le même pane_id");

    // Fenêtre de grâce : le design peut laisser passer un dernier chunk de A
    // « en vol » au moment du join de l'ancien thread de diffusion. On l'absorbe
    // sans l'évaluer.
    let grace_deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < grace_deadline {
        let _ = rx.recv_timeout(Duration::from_millis(50));
    }

    // A continue de produire sa sortie côté serveur (sa boucle PowerShell tourne
    // encore). On draine ensuite pendant ~1,5 s et on vérifie qu'AUCUN
    // PaneOutput { pane_id == pane_id_a } n'arrive : preuve que le flux de A a
    // bien été coupé par la bascule.
    let strict_deadline = Instant::now() + Duration::from_millis(1500);
    let mut leaked_a = None;
    while Instant::now() < strict_deadline {
        if let Ok(wimux_protocol::ServerMessage::PaneOutput { pane_id, bytes }) =
            rx.recv_timeout(Duration::from_millis(100))
            && pane_id == pane_id_a
        {
            leaked_a = Some(String::from_utf8_lossy(&bytes).into_owned());
            break;
        }
    }
    assert!(
        leaked_a.is_none(),
        "le flux de A a continué après la bascule sur B : {leaked_a:?}"
    );

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
fn create_session_cree_et_liste() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-create", std::process::id());
    common::start_daemon(&pipe);
    let conn = std::sync::Arc::new(common::connect_retry(&pipe));
    common::handshake(&conn);
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::CreateSession {
                name: Some("neuve".into()),
            },
        )
        .unwrap();
    }
    let mut r: &wimux_protocol::transport::PipeConn = &conn;
    let name = match wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r).unwrap() {
        wimux_protocol::ServerMessage::SessionCreated { name } => name,
        other => panic!("attendu SessionCreated, reçu {other:?}"),
    };
    assert_eq!(name, "neuve");

    // Elle doit apparaître dans List.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::List).unwrap();
    }
    let mut r2: &wimux_protocol::transport::PipeConn = &conn;
    let listed = matches!(
        wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r2).unwrap(),
        wimux_protocol::ServerMessage::Sessions(v) if v.iter().any(|s| s.name == "neuve"));
    assert!(listed, "la session créée n'apparaît pas dans List");

    let mut w: &wimux_protocol::transport::PipeConn = &conn;
    let _ = wimux_protocol::send(
        &mut w,
        &wimux_protocol::ClientMessage::Kill {
            name: "neuve".into(),
        },
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
}

#[test]
fn rename_session_met_a_jour_la_liste() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-rename", std::process::id());
    common::start_daemon(&pipe);
    let conn = std::sync::Arc::new(common::connect_retry(&pipe));
    common::handshake(&conn);
    // Créer "vieux".
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::CreateSession {
                name: Some("vieux".into()),
            },
        )
        .unwrap();
    }
    let mut r: &wimux_protocol::transport::PipeConn = &conn;
    let _ = wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r).unwrap(); // SessionCreated
    // Renommer.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(
            &mut w,
            &wimux_protocol::ClientMessage::RenameSession {
                from: "vieux".into(),
                to: "nouveau".into(),
            },
        )
        .unwrap();
    }
    let mut r2: &wimux_protocol::transport::PipeConn = &conn;
    assert!(
        matches!(
            wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r2).unwrap(),
            wimux_protocol::ServerMessage::Ok
        ),
        "rename doit répondre Ok"
    );
    // List reflète le nouveau nom.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::List).unwrap();
    }
    let mut r3: &wimux_protocol::transport::PipeConn = &conn;
    let ok = matches!(
        wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r3).unwrap(),
        wimux_protocol::ServerMessage::Sessions(v)
            if v.iter().any(|s| s.name == "nouveau") && !v.iter().any(|s| s.name == "vieux"));
    assert!(ok, "List devrait montrer 'nouveau' et plus 'vieux'");

    let mut w: &wimux_protocol::transport::PipeConn = &conn;
    let _ = wimux_protocol::send(
        &mut w,
        &wimux_protocol::ClientMessage::Kill {
            name: "nouveau".into(),
        },
    );
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
        let mut ok = false;
        while !ok && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::Error(_)) => ok = true,
                Ok(_) => {}
                Err(_) => {}
            }
        }
        ok
    };
    assert!(
        got_error,
        "attach GUI vers session inexistante aurait dû renvoyer Error"
    );
}

#[test]
fn rename_vers_nom_existant_renvoie_error() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-renameerr", std::process::id());
    start_daemon(&pipe);
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);

    // Créer "a".
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateSession {
                name: Some("a".into()),
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &conn;
    let _ = wimux_protocol::recv::<_, ServerMessage>(&mut r).unwrap(); // SessionCreated a

    // Créer "b".
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateSession {
                name: Some("b".into()),
            },
        )
        .unwrap();
    }
    let mut r2: &PipeConn = &conn;
    let _ = wimux_protocol::recv::<_, ServerMessage>(&mut r2).unwrap(); // SessionCreated b

    // Renommer "a" vers "b" (déjà pris) : doit renvoyer Error, pas Ok.
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::RenameSession {
                from: "a".into(),
                to: "b".into(),
            },
        )
        .unwrap();
    }
    let mut r3: &PipeConn = &conn;
    assert!(
        matches!(
            wimux_protocol::recv::<_, ServerMessage>(&mut r3).unwrap(),
            ServerMessage::Error(_)
        ),
        "rename vers un nom déjà pris doit répondre Error"
    );

    // Nettoyage.
    let mut w: &PipeConn = &conn;
    let _ = send(&mut w, &ClientMessage::Kill { name: "a".into() });
    let _ = send(&mut w, &ClientMessage::Kill { name: "b".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn create_nom_existant_renvoie_error() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-createerr", std::process::id());
    start_daemon(&pipe);
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);

    // Créer "dup" une première fois.
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateSession {
                name: Some("dup".into()),
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &conn;
    assert!(
        matches!(
            wimux_protocol::recv::<_, ServerMessage>(&mut r).unwrap(),
            ServerMessage::SessionCreated { .. }
        ),
        "la première création de 'dup' doit répondre SessionCreated"
    );

    // Recréer "dup" : doit renvoyer Error, pas Ok/SessionCreated.
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateSession {
                name: Some("dup".into()),
            },
        )
        .unwrap();
    }
    let mut r2: &PipeConn = &conn;
    assert!(
        matches!(
            wimux_protocol::recv::<_, ServerMessage>(&mut r2).unwrap(),
            ServerMessage::Error(_)
        ),
        "créer une session avec un nom déjà pris doit répondre Error"
    );

    // Nettoyage.
    let mut w: &PipeConn = &conn;
    let _ = send(&mut w, &ClientMessage::Kill { name: "dup".into() });
    std::thread::sleep(Duration::from_millis(200));
}

fn setup_attached(pipe: &str, name: &str) -> (Arc<PipeConn>, Receiver<ServerMessage>) {
    let owner = Arc::new(connect_retry(pipe));
    handshake(&owner);
    {
        let mut w: &PipeConn = &owner;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some(name.into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    let orx = spawn_reader(Arc::clone(&owner));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut got_frame = false;
    while !got_frame && Instant::now() < deadline {
        match orx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::Frame(_)) => got_frame = true,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(got_frame, "pas de frame (shell non démarré)");
    std::thread::sleep(Duration::from_millis(1000));
    // On laisse tomber `owner` : la session survit (détachée).
    let gui = Arc::new(connect_retry(pipe));
    handshake(&gui);
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::AttachGui {
                session: name.into(),
            },
        )
        .unwrap();
    }
    let grx = spawn_reader(Arc::clone(&gui));
    (gui, grx)
}

fn wait_layout(rx: &Receiver<ServerMessage>, secs: u64) -> (LayoutNode, u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowLayout { tree, active }) => return (tree, active),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    panic!("pas de WindowLayout");
}

fn wait_window_list(rx: &Receiver<ServerMessage>, secs: u64) -> (Vec<WindowInfo>, u32) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowList { windows, active }) => return (windows, active),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    panic!("pas de WindowList");
}

fn find_ratio(tree: &LayoutNode, node_id: u32) -> Option<f32> {
    match tree {
        LayoutNode::Leaf { .. } => None,
        LayoutNode::Split {
            node_id: nid,
            ratio,
            a,
            b,
            ..
        } => {
            if *nid == node_id {
                Some(*ratio)
            } else {
                find_ratio(a, node_id).or_else(|| find_ratio(b, node_id))
            }
        }
    }
}

fn wait_ratio_near(rx: &Receiver<ServerMessage>, node_id: u32, target: f32, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Ok(ServerMessage::WindowLayout { tree, .. }) =
            rx.recv_timeout(Duration::from_millis(200))
            && let Some(r) = find_ratio(&tree, node_id)
            && (r - target).abs() < 0.05
        {
            return true;
        }
    }
    false
}

#[test]
fn attach_gui_envoie_layout_et_snapshots() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3layout", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "L");

    let mut layout_pane: Option<u64> = None;
    let mut snap_pane: Option<u64> = None;
    let deadline = Instant::now() + Duration::from_secs(6);
    while (layout_pane.is_none() || snap_pane.is_none()) && Instant::now() < deadline {
        match grx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowLayout { tree, active }) => match tree {
                LayoutNode::Leaf { pane_id, .. } => {
                    assert_eq!(pane_id, active);
                    layout_pane = Some(pane_id);
                }
                _ => panic!("session neuve : arbre attendu = feuille"),
            },
            Ok(ServerMessage::PaneSnapshot { pane_id, .. }) => snap_pane = Some(pane_id),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(layout_pane.is_some(), "WindowLayout non reçu");
    assert_eq!(
        layout_pane, snap_pane,
        "layout et snapshot désignent des volets différents"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "L".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn split_pane_ajoute_une_feuille() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3split", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "S");

    let (tree, active) = wait_layout(&grx, 6);
    let leaf = match tree {
        LayoutNode::Leaf { pane_id, .. } => pane_id,
        _ => panic!("attendu une feuille"),
    };
    assert_eq!(leaf, active);

    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SplitPane {
                pane_id: leaf,
                dir: SplitDir::TopBottom,
            },
        )
        .unwrap();
    }

    let mut new_id: Option<u64> = None;
    let mut split_ids: Option<Vec<u64>> = None;
    let mut split_ok = false;
    let mut output_ok = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while (!split_ok || !output_ok) && Instant::now() < deadline {
        match grx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::PaneSnapshot { pane_id, .. }) => {
                if pane_id != leaf {
                    new_id = Some(pane_id);
                }
            }
            Ok(ServerMessage::WindowLayout { tree, .. }) => {
                if let LayoutNode::Split { a, b, .. } = tree {
                    let mut ids = Vec::new();
                    if let LayoutNode::Leaf { pane_id, .. } = *a {
                        ids.push(pane_id);
                    }
                    if let LayoutNode::Leaf { pane_id, .. } = *b {
                        ids.push(pane_id);
                    }
                    split_ids = Some(ids);
                }
            }
            Ok(ServerMessage::PaneOutput { pane_id, .. }) => {
                if Some(pane_id) == new_id {
                    output_ok = true;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
        // Réévalué après chaque message : le correctif d'ordre (WindowLayout
        // avant PaneSnapshot, cf. Défaut 2) fait arriver la disposition AVANT
        // que `new_id` ne soit connu ; ne pas dépendre de l'ordre de réception.
        if let Some(ids) = &split_ids
            && ids.contains(&leaf)
            && ids.iter().any(|&i| Some(i) == new_id)
        {
            split_ok = true;
        }
    }
    assert!(new_id.is_some(), "pas de snapshot du nouveau volet");
    assert!(split_ok, "le WindowLayout ne reflète pas le split");
    assert!(output_ok, "le nouveau volet ne diffuse pas de PaneOutput");

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "S".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn set_split_ratio_change_le_ratio() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3ratio", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "R");

    let (tree, _) = wait_layout(&grx, 6);
    let leaf = match tree {
        LayoutNode::Leaf { pane_id, .. } => pane_id,
        _ => panic!("attendu une feuille"),
    };
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SplitPane {
                pane_id: leaf,
                dir: SplitDir::LeftRight,
            },
        )
        .unwrap();
    }
    // Récupérer le node_id du split.
    let node_id = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    if let LayoutNode::Split { node_id, .. } = tree {
                        found = Some(node_id);
                    }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        found.expect("pas de WindowLayout à un split")
    };

    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SetSplitRatio {
                node_id,
                ratio: 0.75,
            },
        )
        .unwrap();
    }
    assert!(
        wait_ratio_near(&grx, node_id, 0.75, 8),
        "le ratio n'a pas été fixé à 0.75"
    );

    // Clamp : 5.0 -> 0.9.
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SetSplitRatio {
                node_id,
                ratio: 5.0,
            },
        )
        .unwrap();
    }
    assert!(
        wait_ratio_near(&grx, node_id, 0.9, 8),
        "le ratio n'a pas été borné à 0.9"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "R".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn close_pane_retire_la_feuille() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3close", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "C");

    let (tree, _) = wait_layout(&grx, 6);
    let leaf = match tree {
        LayoutNode::Leaf { pane_id, .. } => pane_id,
        _ => panic!("attendu une feuille"),
    };
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SplitPane {
                pane_id: leaf,
                dir: SplitDir::LeftRight,
            },
        )
        .unwrap();
    }
    // Capturer le nouvel id (snapshot != leaf).
    let new_id = {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneSnapshot { pane_id, .. }) if pane_id != leaf => {
                    found = Some(pane_id);
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        found.expect("pas de snapshot du nouveau volet")
    };

    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::ClosePane { pane_id: new_id }).unwrap();
    }
    // Attendre un WindowLayout redevenu une feuille == leaf.
    let back_to_leaf = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut result = None;
        while result.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    if let LayoutNode::Leaf { pane_id, .. } = tree {
                        result = Some(pane_id == leaf);
                    }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        result.unwrap_or(false)
    };
    assert!(
        back_to_leaf,
        "après fermeture, l'arbre n'est pas redevenu la feuille restante"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "C".into() });
    std::thread::sleep(Duration::from_millis(200));
}

// --- G4 : indicateurs d'activité -----------------------------------------

/// Crée une session détachée (le client est relâché ; la session survit).
fn create_detached(pipe: &str, name: &str) {
    let c = Arc::new(connect_retry(pipe));
    handshake(&c);
    {
        let mut w: &PipeConn = &c;
        send(
            &mut w,
            &ClientMessage::NewSession {
                name: Some(name.into()),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    }
    // Laisser le shell démarrer avant de relâcher la connexion.
    std::thread::sleep(Duration::from_millis(800));
}

/// Injecte des octets dans le volet actif d'une session, sans s'y attacher.
fn send_keys(pipe: &str, session: &str, keys: &[u8]) {
    let c = Arc::new(connect_retry(pipe));
    handshake(&c);
    {
        let mut w: &PipeConn = &c;
        send(
            &mut w,
            &ClientMessage::SendKeys {
                session: session.into(),
                keys: keys.to_vec(),
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &c;
    let _ = recv::<_, ServerMessage>(&mut r); // Ok / Error
}

/// Récupère la liste des sessions via une connexion de contrôle jetable.
fn fetch_list(pipe: &str) -> Vec<SessionInfo> {
    let c = Arc::new(connect_retry(pipe));
    handshake(&c);
    {
        let mut w: &PipeConn = &c;
        send(&mut w, &ClientMessage::List).unwrap();
    }
    let mut r: &PipeConn = &c;
    match recv::<_, ServerMessage>(&mut r).unwrap() {
        ServerMessage::Sessions(v) => v,
        other => panic!("attendu Sessions, reçu {other:?}"),
    }
}

/// Sonde `List` jusqu'à ce que `pred` soit vrai, dans la limite du délai.
fn poll_list_until<F: Fn(&[SessionInfo]) -> bool>(pipe: &str, secs: u64, pred: F) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if pred(&fetch_list(pipe)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Attache la GUI (connexion persistante) à une session et attend son snapshot.
fn attach_gui_persistent(pipe: &str, name: &str) -> (Arc<PipeConn>, Receiver<ServerMessage>) {
    let gui = Arc::new(connect_retry(pipe));
    handshake(&gui);
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::AttachGui {
                session: name.into(),
            },
        )
        .unwrap();
    }
    let rx = spawn_reader(Arc::clone(&gui));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_snapshot = false;
    while !got_snapshot && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::PaneSnapshot { .. }) => got_snapshot = true,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(got_snapshot, "pas de PaneSnapshot pour {name}");
    (gui, rx)
}

#[test]
fn activite_marquee_pour_session_inactive() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g4act", std::process::id());
    start_daemon(&pipe);
    create_detached(&pipe, "A");
    create_detached(&pipe, "B");

    // GUI attachée à A : A devient « vue », B reste inactive.
    let (gui, _grx) = attach_gui_persistent(&pipe, "A");

    // Injecter de la sortie dans B (session inactive).
    send_keys(&pipe, "B", b"Write-Output ABC\r");

    let ok = poll_list_until(&pipe, 15, |list| {
        let a = list.iter().find(|s| s.name == "A");
        let b = list.iter().find(|s| s.name == "B");
        matches!(a, Some(s) if !s.activity) && matches!(b, Some(s) if s.activity)
    });
    assert!(
        ok,
        "B devrait être active et A inactive : {:?}",
        fetch_list(&pipe)
    );

    // Nettoyage.
    for name in ["A", "B"] {
        let mut w: &PipeConn = &gui;
        let _ = send(&mut w, &ClientMessage::Kill { name: name.into() });
    }
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn bascule_efface_activite() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g4switch", std::process::id());
    start_daemon(&pipe);
    create_detached(&pipe, "A");
    create_detached(&pipe, "B");

    let (gui, grx) = attach_gui_persistent(&pipe, "A");

    // Rendre B active.
    send_keys(&pipe, "B", b"Write-Output XYZ\r");
    assert!(
        poll_list_until(&pipe, 15, |l| l
            .iter()
            .find(|s| s.name == "B")
            .is_some_and(|s| s.activity)),
        "B aurait dû devenir active"
    );

    // Basculer la GUI sur B (même connexion persistante).
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::AttachGui {
                session: "B".into(),
            },
        )
        .unwrap();
    }
    // Attendre le snapshot de B (bascule effective).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_snapshot = false;
    while !got_snapshot && Instant::now() < deadline {
        match grx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::PaneSnapshot { .. }) => got_snapshot = true,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(got_snapshot, "pas de snapshot après bascule sur B");

    // Regarder B efface son indicateur d'activité.
    assert!(
        poll_list_until(&pipe, 10, |l| l
            .iter()
            .find(|s| s.name == "B")
            .is_some_and(|s| !s.activity)),
        "après bascule, B ne devrait plus être active : {:?}",
        fetch_list(&pipe)
    );

    for name in ["A", "B"] {
        let mut w: &PipeConn = &gui;
        let _ = send(&mut w, &ClientMessage::Kill { name: name.into() });
    }
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn cloche_marquee_pour_session_inactive() {
    // Ce test dépend du shell : PowerShell émet un BEL en SORTIE via
    // `[Console]::Write([char]7)`. Sur un shell sans BEL, il serait à ignorer.
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g4bell", std::process::id());
    start_daemon(&pipe);
    create_detached(&pipe, "B");

    // Aucune GUI attachée : gui_viewed = None, la cloche persiste jusqu'à la vue.
    send_keys(&pipe, "B", b"[Console]::Write([char]7)\r");

    let rang = poll_list_until(&pipe, 15, |l| {
        l.iter().find(|s| s.name == "B").is_some_and(|s| s.bell)
    });
    assert!(
        rang,
        "B aurait dû être marquée cloche : {:?}",
        fetch_list(&pipe)
    );

    let c = Arc::new(connect_retry(&pipe));
    handshake(&c);
    let mut w: &PipeConn = &c;
    let _ = send(&mut w, &ClientMessage::Kill { name: "B".into() });
    std::thread::sleep(Duration::from_millis(200));
}

// --- M2 : templates d'agents et création de session agent ----------------

#[test]
fn list_agent_templates_renvoie_les_modeles() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-tpllist", std::process::id());
    common::start_daemon_with_config(
        &pipe,
        "agent-template echo cmd.exe /c echo {prompt}\nagent-template shell cmd.exe\n",
    );
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::ListAgentTemplates).unwrap();
    }
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r).unwrap() {
        ServerMessage::AgentTemplates(v) => {
            assert!(
                v.iter().any(|t| t.name == "echo"),
                "modèle 'echo' absent : {v:?}"
            );
            assert!(
                v.iter().any(|t| t.name == "shell"),
                "modèle 'shell' absent : {v:?}"
            );
        }
        other => panic!("attendu AgentTemplates, reçu {other:?}"),
    }
}

#[test]
fn create_agent_session_one_shot_apparait_agent_puis_done() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-agone", std::process::id());
    common::start_daemon_with_config(&pipe, "agent-template echo cmd.exe /c echo {prompt}\n");
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentSession {
                name: Some("bot".into()),
                template: "echo".into(),
                prompt: "salut".into(),
                cwd: None,
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &conn;
    let created = match recv::<_, ServerMessage>(&mut r).unwrap() {
        ServerMessage::SessionCreated { name } => name,
        other => panic!("attendu SessionCreated, reçu {other:?}"),
    };
    assert_eq!(created, "bot");

    // La session doit apparaître dans List, marquée agent, puis passer à Done.
    let ok = poll_list_until(&pipe, 20, |list| {
        list.iter().any(|s| {
            s.name == "bot" && s.agent && s.agent_status == Some(wimux_protocol::AgentStatus::Done)
        })
    });
    assert!(
        ok,
        "la session agent one-shot devrait être Done : {:?}",
        fetch_list(&pipe)
    );

    let mut w: &PipeConn = &conn;
    let _ = send(&mut w, &ClientMessage::Kill { name: "bot".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn create_agent_session_stdin_prompt_termine() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-agstdin", std::process::id());
    common::start_daemon_with_config(&pipe, "agent-template shell cmd.exe\n");
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentSession {
                name: Some("sh".into()),
                template: "shell".into(),
                prompt: "exit 0".into(),
                cwd: None,
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &conn;
    assert!(
        matches!(
            recv::<_, ServerMessage>(&mut r).unwrap(),
            ServerMessage::SessionCreated { .. }
        ),
        "la création de l'agent stdin doit répondre SessionCreated"
    );
    // Le prompt "exit 0" envoyé sur stdin (+ Entrée) fait sortir cmd.exe -> Done.
    let ok = poll_list_until(&pipe, 20, |list| {
        list.iter().any(|s| {
            s.name == "sh" && s.agent && s.agent_status == Some(wimux_protocol::AgentStatus::Done)
        })
    });
    assert!(
        ok,
        "l'agent stdin devrait se terminer (Done) : {:?}",
        fetch_list(&pipe)
    );

    let mut w: &PipeConn = &conn;
    let _ = send(&mut w, &ClientMessage::Kill { name: "sh".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn create_agent_session_modele_inconnu_renvoie_error() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-agunknown", std::process::id());
    common::start_daemon_with_config(&pipe, "agent-template echo cmd.exe /c echo {prompt}\n");
    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentSession {
                name: None,
                template: "absent".into(),
                prompt: "x".into(),
                cwd: None,
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &conn;
    assert!(
        matches!(
            recv::<_, ServerMessage>(&mut r).unwrap(),
            ServerMessage::Error(_)
        ),
        "un modèle inconnu doit répondre Error"
    );
}

// --- M3 : orchestration fan-out (lots d'agents en worktrees) --------------

/// Crée un dépôt git temporaire (init + commit vide) et renvoie son chemin.
/// Renvoie `None` si git est absent (le test se termine alors proprement).
fn init_temp_git_repo(label: &str) -> Option<std::path::PathBuf> {
    let git_ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("wimux-m3-repo-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    assert!(git(&["init"]), "git init a échoué");
    assert!(
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]),
        "commit initial a échoué"
    );
    Some(dir)
}

#[test]
fn create_agent_batch_cree_worktrees_puis_les_nettoie() {
    let Some(repo) = init_temp_git_repo("ok") else {
        eprintln!("git absent : test create_agent_batch_cree_worktrees_puis_les_nettoie ignoré");
        return;
    };
    let pipe = format!(r"\\.\pipe\wimux-test-{}-batchok", std::process::id());
    let root = std::env::temp_dir().join(format!("wimux-m3-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Template déterministe `echo` (= cmd.exe /c echo {prompt}) + racine de worktrees.
    let conf = format!(
        "agent-template echo cmd.exe /c echo {{prompt}}\nset agent-worktree-root {}\n",
        root.display()
    );
    common::start_daemon_with_config(&pipe, &conf);

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentBatch {
                template: "echo".into(),
                prompt: "salut".into(),
                base_repo: repo.to_string_lossy().into_owned(),
                count: 2,
            },
        )
        .unwrap();
    }
    let (group, names) = {
        let mut r: &PipeConn = &conn;
        match recv::<_, ServerMessage>(&mut r).unwrap() {
            ServerMessage::BatchCreated { group, sessions } => (group, sessions),
            other => panic!("attendu BatchCreated, reçu {other:?}"),
        }
    };
    assert_eq!(names.len(), 2, "le lot devrait avoir 2 membres : {names:?}");

    // List : 2 sessions du même group, marquées agent.
    let g = group.clone();
    let listed = poll_list_until(&pipe, 20, |list| {
        list.iter()
            .filter(|s| s.group.as_deref() == Some(g.as_str()) && s.agent)
            .count()
            == 2
    });
    assert!(
        listed,
        "List devrait montrer 2 membres agent du group {group} : {:?}",
        fetch_list(&pipe)
    );

    // Les 2 dossiers de worktree existent.
    let wt0 = root.join(format!("{group}-0"));
    let wt1 = root.join(format!("{group}-1"));
    assert!(wt0.exists(), "worktree 0 absent : {}", wt0.display());
    assert!(wt1.exists(), "worktree 1 absent : {}", wt1.display());

    // Kill des 2 membres : chacun nettoie son worktree.
    for name in &names {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::Kill { name: name.clone() }).unwrap();
        let mut r: &PipeConn = &conn;
        let _ = recv::<_, ServerMessage>(&mut r); // Ok
    }

    // Les dossiers de worktree ont disparu (nettoyage).
    let gone = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !wt0.exists() && !wt1.exists() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    assert!(
        gone,
        "les worktrees devraient avoir été nettoyés après Kill : {} / {}",
        wt0.display(),
        wt1.display()
    );

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn create_agent_batch_base_non_git_renvoie_error() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-batchnogit", std::process::id());
    common::start_daemon_with_config(&pipe, "agent-template echo cmd.exe /c echo {prompt}\n");
    // Dossier existant mais qui n'est PAS un repo git.
    let notgit = std::env::temp_dir().join(format!("wimux-m3-notgit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&notgit);
    std::fs::create_dir_all(&notgit).unwrap();

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentBatch {
                template: "echo".into(),
                prompt: "x".into(),
                base_repo: notgit.to_string_lossy().into_owned(),
                count: 2,
            },
        )
        .unwrap();
    }
    let mut r: &PipeConn = &conn;
    assert!(
        matches!(
            recv::<_, ServerMessage>(&mut r).unwrap(),
            ServerMessage::Error(_)
        ),
        "une base non-git doit répondre Error"
    );

    let _ = std::fs::remove_dir_all(&notgit);
}

#[test]
fn onglets_cycle_de_vie() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-w2tabs", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "W");

    // À l'attache : WindowList avec 1 fenêtre, active = 0.
    let (windows, active) = wait_window_list(&grx, 8);
    assert_eq!(windows.len(), 1);
    assert_eq!(active, 0);

    // Capturer le pane_id de la 1re fenêtre (via WindowLayout).
    let (tree0, _) = wait_layout(&grx, 8);
    let pane0 = match tree0 {
        LayoutNode::Leaf { pane_id, .. } => pane_id,
        _ => panic!("session neuve : arbre attendu = feuille"),
    };

    // NewWindow -> 2 fenêtres, active = 1 ; WindowLayout d'une nouvelle feuille.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::NewWindow).unwrap();
    }
    let (windows, active) = wait_window_list(&grx, 8);
    assert_eq!(windows.len(), 2);
    assert_eq!(active, 1);
    let (tree1, _) = wait_layout(&grx, 8);
    let pane1 = match tree1 {
        LayoutNode::Leaf { pane_id, .. } => pane_id,
        _ => panic!("nouvelle fenêtre : arbre attendu = feuille"),
    };
    assert_ne!(pane0, pane1, "les deux fenêtres partagent un pane_id");

    // SelectWindow { index: 0 } -> active = 0.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::SelectWindow { index: 0 }).unwrap();
    }
    let (_windows, active) = wait_window_list(&grx, 8);
    assert_eq!(active, 0);

    // RenameWindow { index: 0, name: "build" } -> WindowList reflète le nom.
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::RenameWindow {
                index: 0,
                name: "build".into(),
            },
        )
        .unwrap();
    }
    let named = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut got_list = false;
        let mut found = None;
        while !got_list && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowList { windows, .. }) => {
                    found = windows.first().and_then(|w| w.name.clone());
                    got_list = true;
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(got_list, "pas de WindowList après RenameWindow");
        found
    };
    assert_eq!(named.as_deref(), Some("build"));

    // CloseWindow { index: 1 } -> 1 fenêtre, active = 0.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::CloseWindow { index: 1 }).unwrap();
    }
    let (windows, active) = wait_window_list(&grx, 8);
    assert_eq!(windows.len(), 1);
    assert_eq!(active, 0);

    // 2e CloseWindow sur l'unique fenêtre : no-op (toujours 1 fenêtre).
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::CloseWindow { index: 0 }).unwrap();
    }
    let (windows, _) = wait_window_list(&grx, 8);
    assert_eq!(
        windows.len(),
        1,
        "fermer la dernière fenêtre doit être no-op"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "W".into() });
    std::thread::sleep(Duration::from_millis(200));
}

// --- W3 : cwd (OSC 7) + branche git dans SessionInfo ----------------------

/// Crée un dépôt git temporaire sur une branche nommée. `None` si git absent.
fn init_temp_git_repo_on_branch(label: &str, branch: &str) -> Option<std::path::PathBuf> {
    let git_ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("wimux-w3-repo-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    assert!(git(&["init"]), "git init a échoué");
    assert!(
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]),
        "commit initial a échoué"
    );
    assert!(git(&["checkout", "-b", branch]), "checkout -b a échoué");
    Some(dir)
}

#[test]
fn cwd_et_branche_via_osc7() {
    let Some(repo) = init_temp_git_repo_on_branch("cb", "w3-branch") else {
        eprintln!("git absent : test cwd_et_branche_via_osc7 ignoré");
        return;
    };
    let pipe = format!(r"\\.\pipe\wimux-test-{}-w3cwd", std::process::id());
    start_daemon(&pipe); // shell par défaut = powershell.exe

    create_detached(&pipe, "W3");

    // Chemin natif (pour comparaison) et URL (slashes avant, hôte vide).
    let repo_native = repo.to_string_lossy().into_owned(); // C:\...\repo
    let repo_url = repo_native.replace('\\', "/"); // C:/...:/repo

    // Une seule ligne PowerShell : se placer dans le dépôt PUIS émettre un OSC 7
    // FIXE (indépendant du hook de prompt) pointant sur ce dépôt.
    let cmd = format!(
        "Set-Location -LiteralPath '{repo_native}'; [Console]::Out.Write(\"$([char]27)]7;file:///{repo_url}$([char]7)\")\r"
    );
    send_keys(&pipe, "W3", cmd.as_bytes());

    // Comparaison robuste à la forme courte (8.3) vs longue du chemin : sur
    // certains profils Windows, `std::env::temp_dir()` renvoie un chemin
    // court (ex. `FABRIC~1.AND`) alors que PowerShell, via l'OSC 7 émis
    // réellement au spawn, rapporte la forme longue du même dossier. On
    // canonicalise les deux côtés (même dossier => même racine `\\?\...`)
    // pour ne pas dépendre de la forme retournée.
    fn canon_or_raw(p: &str) -> String {
        std::fs::canonicalize(p)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
    }
    let want_native = repo_native.clone();
    let want_canon = canon_or_raw(&want_native);
    let ok = poll_list_until(&pipe, 25, |list| {
        list.iter().find(|s| s.name == "W3").is_some_and(|s| {
            s.cwd.as_deref().is_some_and(|c| {
                c.eq_ignore_ascii_case(&want_native)
                    || canon_or_raw(c).eq_ignore_ascii_case(&want_canon)
            }) && s.branch.as_deref() == Some("w3-branch")
        })
    });
    assert!(
        ok,
        "W3 devrait exposer cwd={repo_native} et branch=w3-branch : {:?}",
        fetch_list(&pipe)
    );

    let c = Arc::new(connect_retry(&pipe));
    handshake(&c);
    {
        let mut w: &PipeConn = &c;
        let _ = send(&mut w, &ClientMessage::Kill { name: "W3".into() });
    }
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&repo);
}

// --- B1 : non-régression du chemin serveur « retour arrière » d'un volet
//     navigateur (ne reproduit pas le défaut de concurrence, voir plus bas) --

/// Cherche la feuille `pane_id` dans l'arbre et renvoie son URL si c'est un
/// volet navigateur (`None` si l'id est absent ou si c'est un terminal).
fn find_web_url(tree: &LayoutNode, pane_id: u64) -> Option<String> {
    match tree {
        LayoutNode::Leaf { pane_id: id, kind } => {
            if *id != pane_id {
                return None;
            }
            match kind {
                PaneKind::Web { url } => Some(url.clone()),
                PaneKind::Terminal => None,
            }
        }
        LayoutNode::Split { a, b, .. } => {
            find_web_url(a, pane_id).or_else(|| find_web_url(b, pane_id))
        }
    }
}

/// Test de non-régression du chemin serveur historique `WebBack` : rejoue,
/// **directement via le pipe** (sans passer par le pont Tauri), la séquence
/// ouvrir un volet navigateur / naviguer / revenir en arrière.
///
/// AVERTISSEMENT (revue de suivi, Fix 1) : ce test avait été écrit à l'origine
/// pour reproduire un bug de « retour arrière » côté GUI ; son nom laissait
/// entendre qu'il en était la reproduction. Ce n'est pas le cas — il n'utilise
/// qu'une seule connexion, un seul écrivain, aucune écriture concurrente n'est
/// possible ici. Le diagnostic a montré que ce chemin serveur est sain (voir
/// `.superpowers/sdd/bug-retour-report.md`) et que le vrai défaut était une
/// absence de sérialisation des écritures côté pont Tauri
/// (`wimux-gui/src-tauri/src/lib.rs`, corrigé par `send_persistent`). Ce test
/// reste utile comme non-régression du chemin `WebBack`/historique, mais ne
/// couvre PAS le défaut de concurrence — voir le test de concurrence dans
/// `wimux-gui/src-tauri/src/lib.rs` pour celui-ci.
///
/// Le test consigne (`eprintln!`) chaque message poussé par le serveur après
/// `WebBack`, pour qu'on puisse voir EXACTEMENT ce qui arrive sur le pipe côté
/// serveur, indépendamment de tout défaut frontend.
#[test]
fn webback_via_pipe_ramene_l_url_precedente() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-webback", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "WB");

    // Consommer le layout initial (feuille terminal) avant d'ouvrir le volet web.
    let _ = wait_layout(&grx, 6);

    // 1) OpenWebPane { url: "http://a/" } -> PaneSpawned { pane_id }.
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::OpenWebPane {
                session: String::new(),
                from_pane: None,
                dir: SplitDir::LeftRight,
                url: "http://a/".into(),
            },
        )
        .unwrap();
    }
    let pane_id = {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneSpawned { pane_id }) => found = Some(pane_id),
                Ok(other) => eprintln!("[OpenWebPane] message ignoré : {other:?}"),
                Err(_) => {}
            }
        }
        found.expect("pas de PaneSpawned après OpenWebPane")
    };

    // 2) WebNavigate { pane, url: "http://b/" } -> un WindowLayout doit suivre,
    //    reflétant la feuille web sur http://b/ (ceci fonctionne d'après le
    //    rapport : "la page change bien -> la navigation fonctionne").
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::WebNavigate {
                session: String::new(),
                pane: pane_id,
                url: "http://b/".into(),
            },
        )
        .unwrap();
    }
    let after_navigate = {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    eprintln!("[WebNavigate] WindowLayout reçu : {tree:?}");
                    if let Some(url) = find_web_url(&tree, pane_id) {
                        found = Some(url);
                    }
                }
                Ok(other) => eprintln!("[WebNavigate] message reçu : {other:?}"),
                Err(_) => {}
            }
        }
        found
    };
    assert_eq!(
        after_navigate.as_deref(),
        Some("http://b/"),
        "après WebNavigate, le WindowLayout devrait montrer http://b/"
    );

    // 3) WebBack { pane } -> le point vérifié par ce test de non-régression :
    //    le serveur doit répondre par un WindowLayout dont la feuille est
    //    revenue sur http://a/. On consigne TOUT ce qui arrive après l'envoi,
    //    dans l'ordre, pour un diagnostic immédiat en cas d'échec.
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::WebBack {
                session: String::new(),
                pane: pane_id,
            },
        )
        .unwrap();
    }
    let mut trace: Vec<String> = Vec::new();
    let after_back = {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, active }) => {
                    let url = find_web_url(&tree, pane_id);
                    trace.push(format!(
                        "WindowLayout {{ active: {active}, url_du_volet: {url:?}, tree: {tree:?} }}"
                    ));
                    if let Some(u) = url {
                        found = Some(u);
                    }
                }
                Ok(other) => trace.push(format!("{other:?}")),
                Err(_) => {}
            }
        }
        found
    };
    eprintln!("--- Messages reçus après WebBack (dans l'ordre) ---");
    for line in &trace {
        eprintln!("  {line}");
    }
    if trace.is_empty() {
        eprintln!("  (AUCUN message reçu après WebBack dans le délai imparti)");
    }
    assert_eq!(
        after_back.as_deref(),
        Some("http://a/"),
        "après WebBack, le WindowLayout devrait montrer http://a/ (retour). Trace complète : {trace:#?}"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "WB".into() });
    std::thread::sleep(Duration::from_millis(200));
}
