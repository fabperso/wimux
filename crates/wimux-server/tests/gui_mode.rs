//! Test d'intégration du mode GUI : un client GUI s'attache à une session
//! existante, reçoit un `PaneSnapshot` puis un flux de `PaneOutput`, et peut
//! injecter des frappes via `PaneInput`.

mod common;

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use common::*;
use wimux_protocol::transport::PipeConn;
use wimux_protocol::{ClientMessage, LayoutNode, ServerMessage, SplitDir, send};

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

    // Attendre le snapshot de A et capturer son pane_id.
    let pane_id_a = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(wimux_protocol::ServerMessage::PaneSnapshot { pane_id, .. }) => break pane_id,
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de PaneSnapshot pour A"),
            }
        }
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
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(wimux_protocol::ServerMessage::PaneSnapshot { pane_id, .. }) => break pane_id,
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de snapshot après bascule sur B"),
            }
        }
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
    loop {
        match orx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::Frame(_)) => break,
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de frame (shell non démarré)"),
        }
    }
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
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowLayout { tree, active }) => return (tree, active),
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de WindowLayout"),
        }
    }
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
                LayoutNode::Leaf { pane_id } => {
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
        LayoutNode::Leaf { pane_id } => pane_id,
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
                    if let LayoutNode::Leaf { pane_id } = *a {
                        ids.push(pane_id);
                    }
                    if let LayoutNode::Leaf { pane_id } = *b {
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
        LayoutNode::Leaf { pane_id } => pane_id,
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
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    if let LayoutNode::Split { node_id, .. } = tree {
                        break node_id;
                    }
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de WindowLayout à un split"),
            }
        }
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
        LayoutNode::Leaf { pane_id } => pane_id,
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
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneSnapshot { pane_id, .. }) if pane_id != leaf => {
                    break pane_id;
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de snapshot du nouveau volet"),
            }
        }
    };

    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::ClosePane { pane_id: new_id }).unwrap();
    }
    // Attendre un WindowLayout redevenu une feuille == leaf.
    let back_to_leaf = {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    if let LayoutNode::Leaf { pane_id } = tree {
                        break pane_id == leaf;
                    }
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => break false,
            }
        }
    };
    assert!(
        back_to_leaf,
        "après fermeture, l'arbre n'est pas redevenu la feuille restante"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "C".into() });
    std::thread::sleep(Duration::from_millis(200));
}
