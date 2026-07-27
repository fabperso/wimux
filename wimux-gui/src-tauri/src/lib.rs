use std::sync::Arc;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use wimux_protocol::transport::{connect, user_pipe_name, PipeConn};
use wimux_protocol::{
    recv, send, ClientMessage, Hello, HelloReply, ServerMessage, SplitDir, PROTOCOL_VERSION,
};

/// Écrivain de la connexion GUI persistante. `None` tant qu'aucun
/// `attach_session` n'a encore établi de connexion : écrire est alors un
/// no-op silencieux (les commandes de volet appelées trop tôt ne doivent pas
/// échouer bruyamment, elles n'ont simplement aucun effet).
///
/// Délègue sur `&PipeConn`, qui implémente `Write` sans exiger d'exclusivité
/// côté OS (I/O overlappée, voir `transport.rs`) : lecture et écriture
/// peuvent réellement se dérouler en parallèle sur le même handle. Le
/// problème n'est donc pas l'accès concurrent au handle lui-même, mais
/// l'entrelacement des trames applicatives que produiraient deux `send`
/// concurrents (chacun fait DEUX `write_all` non atomiques : longueur puis
/// corps). C'est ce que sérialise le `Mutex` qui enveloppe cet écrivain (voir
/// `send_serialise`) — le verrou porte directement sur l'écrivain, si bien
/// qu'aucun appelant ne peut écrire sans le prendre.
struct PersistentWriter(Option<Arc<PipeConn>>);

impl std::io::Write for PersistentWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &self.0 {
            // `&PipeConn` implémente `Write`, mais `.write()` prend `&mut
            // self` : il faut donc une variable mutable (pas une simple
            // expression `&**c`) pour pouvoir la reborrow mutablement, comme
            // partout ailleurs dans ce fichier (`do_handshake`, `control`...).
            Some(c) => {
                let mut w: &PipeConn = c;
                w.write(buf)
            }
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &self.0 {
            Some(c) => {
                let mut w: &PipeConn = c;
                w.flush()
            }
            None => Ok(()),
        }
    }
}

/// Connexion partagée au serveur wimux (écrivain).
struct Bridge {
    /// Sérialise TOUTES les écritures sur la connexion GUI persistante
    /// (pane_input, pane_resize, web_back, list_windows, attach_session...) —
    /// chaque commande Tauri peut s'exécuter sur son propre thread,
    /// indépendamment des autres. Sans ce verrou, deux `send` concurrents
    /// entrelaceraient leurs trames sur le même handle de pipe et
    /// désynchroniseraient le protocole côté serveur (celui-ci `break`rait
    /// alors sa boucle de lecture sur la trame illisible, coupant
    /// silencieusement tout flux futur — c'est exactement le défaut que
    /// `gui_write` évite côté serveur pour ses propres écritures ; il
    /// manquait le pendant côté client).
    ///
    /// Fix 3 : ce verrou ne doit JAMAIS être tenu pendant une I/O réseau
    /// (`connect`/`do_handshake`, ce dernier contenant un `recv` bloquant SANS
    /// échéance). Si le verrou était tenu là, un daemon qui accepte la
    /// connexion sans répondre au `Hello` bloquerait indéfiniment TOUTES les
    /// commandes Tauri (elles passent toutes par `send_persistent`, qui prend
    /// ce même verrou). C'est `init_lock`, ci-dessous, qui protège la phase
    /// connexion/handshake — `write_lock` n'est repris que pour la
    /// vérification et la publication, chacune brièvement.
    write_lock: Mutex<PersistentWriter>,

    /// Fix 3 : garde d'initialisation dédiée à `attach_session`, DISTINCTE de
    /// `write_lock`. Elle est tenue sans interruption sur TOUTE la phase
    /// « vérifier qu'aucune connexion n'existe → connecter → handshake →
    /// démarrer le thread lecteur → publier », ce qui est exactement ce qui
    /// empêche deux appels concurrents à froid de voir tous deux `None` et
    /// d'ouvrir chacun leur connexion (le TOCTOU que corrigeait déjà l'ancien
    /// verrou unique). La différence avec l'ancien schéma : la connexion et le
    /// handshake (I/O réseau potentiellement lente/bloquante) se font PENDANT
    /// que `init_lock` est tenu mais SANS tenir `write_lock` — celui-ci n'est
    /// repris que pour les deux instants brefs (lire l'état courant, puis
    /// écrire le résultat), toujours sous `init_lock`, donc sans réintroduire
    /// de fenêtre de course entre la vérification et la publication.
    init_lock: Mutex<()>,
}

impl Default for Bridge {
    fn default() -> Self {
        Bridge {
            write_lock: Mutex::new(PersistentWriter(None)),
            init_lock: Mutex::new(()),
        }
    }
}

/// Sérialise l'envoi d'un message sur `lock`, sous verrou. Générique sur
/// l'écrivain (et non lié à `PipeConn`) pour rester testable indépendamment
/// du pipe nommé réel : voir le module `tests` en bas de ce fichier, où un
/// test de concurrence prouve que retirer ce verrou entrelace les trames.
fn send_serialise<W: std::io::Write>(lock: &Mutex<W>, msg: &ClientMessage) -> Result<(), String> {
    let mut w = lock.lock().unwrap();
    send(&mut *w, msg).map_err(|e| e.to_string())
}

/// Envoie un message sur la connexion GUI persistante, sous `write_lock`.
fn send_persistent(bridge: &Bridge, msg: &ClientMessage) -> Result<(), String> {
    send_serialise(&bridge.write_lock, msg)
}

fn do_handshake(conn: &PipeConn) -> Result<(), String> {
    let mut w: &PipeConn = conn;
    send(
        &mut w,
        &ClientMessage::Hello(Hello {
            client_version: PROTOCOL_VERSION,
            client_build: env!("CARGO_PKG_VERSION").to_string(),
        }),
    )
    .map_err(|e| e.to_string())?;
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
    {
        // Fix 3 : `init_lock` (et NON `write_lock`) sérialise TOUTE la phase
        // vérification + connexion + handshake + démarrage du thread lecteur
        // + publication. Il reste tenu sans interruption d'un bout à l'autre
        // de ce bloc, ce qui rend impossible la course où deux appels
        // concurrents à froid verraient tous deux « pas de connexion » et
        // ouvriraient chacun la leur. `write_lock`, lui, n'est repris que
        // brièvement (lecture de l'état, puis écriture du résultat) : la
        // lente I/O réseau (`connect`/`do_handshake`, qui contient un `recv`
        // bloquant sans échéance) ne le tient donc JAMAIS — sinon un daemon
        // qui accepte la connexion sans répondre au `Hello` bloquerait
        // indéfiniment toutes les autres commandes Tauri (`pane_input`,
        // `split_pane`... qui passent toutes par `send_persistent`).
        let _init = bridge.init_lock.lock().unwrap();

        let deja_connecte = bridge.write_lock.lock().unwrap().0.is_some();
        if !deja_connecte {
            // AUCUN verrou tenu ici pendant l'I/O réseau (voir ci-dessus).
            let c = Arc::new(
                connect(&user_pipe_name()).map_err(|_| "serveur wimux introuvable".to_string())?,
            );
            do_handshake(&c)?;
            // Thread lecteur : démarré une seule fois, garanti par `init_lock`
            // tenu sans interruption depuis la vérification ci-dessus. Il
            // relaie snapshot/output/error au frontend.
            //
            // Fix 2 : à la sortie de la boucle (EOF — typiquement un
            // redémarrage du daemon, geste routinier sur ce projet), la
            // connexion morte reste sinon publiée pour toujours : plus aucun
            // `attach_session` ne reconnecterait (la garde ci-dessus verrait
            // « déjà connecté »), et la GUI resterait inerte jusqu'à sa
            // propre relance. On reprend donc le verrou pour republier
            // `None`, mais SEULEMENT si la connexion encore publiée est bien
            // CELLE-CI (comparaison de pointeur `Arc`) — un `attach_session`
            // plus récent a pu entre-temps publier une connexion plus
            // fraîche, qu'il ne faut surtout pas écraser.
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
                        ServerMessage::WindowList { windows, active } => {
                            let _ = app2.emit("window-list", (windows, active));
                        }
                        ServerMessage::Error(m) => {
                            let _ = app2.emit("pane-error", m);
                        }
                        _ => {}
                    }
                }
                // La boucle de lecture est sortie (EOF/erreur) : la connexion
                // est morte. Remettre `None` pour qu'un futur `attach_session`
                // reconnecte, mais seulement si personne n'a déjà publié une
                // connexion plus récente à sa place.
                if let Some(b) = app2.try_state::<Bridge>() {
                    let mut guard = b.write_lock.lock().unwrap();
                    if guard
                        .0
                        .as_ref()
                        .is_some_and(|cur| Arc::ptr_eq(cur, &reader))
                    {
                        guard.0 = None;
                    }
                }
                let _ = app2.emit("server-disconnected", ());
            });
            bridge.write_lock.lock().unwrap().0 = Some(c);
        }
    } // `init_lock` relâché ICI ; `send_persistent` prend `write_lock`, un
      // verrou DIFFÉRENT — pas de risque d'interblocage.
    send_persistent(&bridge, &ClientMessage::AttachGui { session })
}

#[derive(serde::Serialize)]
struct SessionDto {
    name: String,
    attached: bool,
    activity: bool,
    bell: bool,
    agent: bool,
    agent_status: Option<String>,
    group: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    color: Option<String>,
    pinned: bool,
    layout_rev: u64,
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
                    group: s.group,
                    cwd: s.cwd,
                    branch: s.branch,
                    color: s.color,
                    pinned: s.pinned,
                    layout_rev: s.layout_rev,
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
fn create_batch(
    template: String,
    prompt: String,
    base_repo: String,
    count: u32,
) -> Result<String, String> {
    control(
        || ClientMessage::CreateAgentBatch {
            template,
            prompt,
            base_repo,
            count,
        },
        |msg| match msg {
            ServerMessage::BatchCreated { group, .. } => Ok(group),
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
fn reorder_sessions(names: Vec<String>) -> Result<(), String> {
    control(
        || ClientMessage::ReorderSessions { names },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn set_session_color(name: String, color: Option<String>) -> Result<(), String> {
    control(
        || ClientMessage::SetSessionColor { name, color },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn set_session_pinned(name: String, pinned: bool) -> Result<(), String> {
    control(
        || ClientMessage::SetSessionPinned { name, pinned },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[derive(serde::Serialize)]
struct NotificationDto {
    session: String,
    title: Option<String>,
    body: String,
}

#[tauri::command]
fn take_notifications() -> Result<Vec<NotificationDto>, String> {
    control(
        || ClientMessage::TakeNotifications,
        |msg| match msg {
            ServerMessage::Notifications(v) => Ok(v
                .into_iter()
                .map(|n| NotificationDto {
                    session: n.session,
                    title: n.title,
                    body: n.body,
                })
                .collect()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn mark_session_read(name: String) -> Result<(), String> {
    control(
        || ClientMessage::MarkSessionRead { name },
        |msg| match msg {
            ServerMessage::Ok => Ok(()),
            ServerMessage::Error(e) => Err(e),
            _ => Err("réponse inattendue".into()),
        },
    )
}

#[tauri::command]
fn mark_session_unread(name: String) -> Result<(), String> {
    control(
        || ClientMessage::MarkSessionUnread { name },
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
    send_persistent(&bridge, &ClientMessage::PaneInput { pane_id, bytes })
}

#[tauri::command]
fn split_pane(pane_id: u64, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    send_persistent(&bridge, &ClientMessage::SplitPane { pane_id, dir })
}

#[tauri::command]
fn close_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::ClosePane { pane_id })
}

#[tauri::command]
fn focus_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::FocusPane { pane_id })
}

#[tauri::command]
fn set_split_ratio(node_id: u32, ratio: f32, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::SetSplitRatio { node_id, ratio })
}

#[tauri::command]
fn pane_resize(pane_id: u64, cols: u16, rows: u16, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::PaneResize {
            pane_id,
            cols,
            rows,
        },
    )
}

#[tauri::command]
fn open_web_pane(url: String, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    send_persistent(
        &bridge,
        &ClientMessage::OpenWebPane {
            session: String::new(),
            from_pane: None,
            dir,
            url,
        },
    )
}

#[tauri::command]
fn web_navigate(pane_id: u64, url: String, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::WebNavigate {
            session: String::new(),
            pane: pane_id,
            url,
        },
    )
}

#[tauri::command]
fn web_back(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::WebBack {
            session: String::new(),
            pane: pane_id,
        },
    )
}

#[tauri::command]
fn web_forward(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(
        &bridge,
        &ClientMessage::WebForward {
            session: String::new(),
            pane: pane_id,
        },
    )
}

#[tauri::command]
fn new_window(bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::NewWindow)
}

#[tauri::command]
fn list_windows(bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::ListWindows)
}

#[tauri::command]
fn select_window(index: u32, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::SelectWindow { index })
}

#[tauri::command]
fn close_window(index: u32, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::CloseWindow { index })
}

#[tauri::command]
fn rename_window(index: u32, name: String, bridge: State<Bridge>) -> Result<(), String> {
    send_persistent(&bridge, &ClientMessage::RenameWindow { index, name })
}

/// Démarre le daemon `wimux-server` embarqué (ressource de l'app) s'il n'est
/// pas déjà joignable sur le pipe nommé. Best-effort : en cas d'échec (ex.
/// lancement dev sans ressource empaquetée), on laisse la GUI se lancer — elle
/// affichera « serveur wimux introuvable » comme avant, sans planter.
///
/// Le daemon est lancé DÉTACHÉ et sans fenêtre console : il survit à la GUI
/// (comportement habituel de wimux — le serveur persiste entre attaches) et ne
/// fait pas clignoter de console au démarrage.
fn ensure_daemon(app: &AppHandle) {
    // Déjà lancé ? Le pipe répond → rien à faire.
    if connect(&user_pipe_name()).is_ok() {
        return;
    }
    // Binaire embarqué à côté de l'app (cf. `bundle.resources`).
    let exe = match app
        .path()
        .resolve("binaries/wimux-server.exe", tauri::path::BaseDirectory::Resource)
    {
        Ok(p) if p.exists() => p,
        _ => return, // pas de ressource (dev) : on abandonne silencieusement
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if cmd.spawn().is_err() {
        return;
    }
    // Attendre que le daemon ouvre son pipe (démarrage à froid, max ~5 s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if connect(&user_pipe_name()).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // Autonomie de l'installeur : si aucun daemon ne tourne, on lance
            // celui empaqueté avec l'app avant que le frontend ne tente son
            // premier `attach_session`.
            ensure_daemon(app.handle());
            Ok(())
        })
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
            reorder_sessions,
            set_session_color,
            set_session_pinned,
            take_notifications,
            mark_session_read,
            mark_session_unread,
            rename_session,
            list_agent_templates,
            create_agent,
            create_batch,
            new_window,
            list_windows,
            select_window,
            close_window,
            rename_window,
            open_web_pane,
            web_navigate,
            web_back,
            web_forward
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de wimux-gui");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::Duration;

    /// Puits d'octets partagé (Fix 6) : verrouillé PAR APPEL À `write()`, et
    /// non pas pour toute la durée d'un `send()` (qui fait DEUX `write_all`,
    /// longueur puis corps). Les octets de deux `send` concurrents restent
    /// donc parfaitement entrelaçables — c'est précisément ce que ce test
    /// doit exercer — mais chaque `extend_from_slice` individuel reste une
    /// opération atomique et sans data race : plus de comportement indéfini.
    ///
    /// AVANT ce correctif, ce puits était un `UnsafeCell<Vec<u8>>` planqué
    /// derrière un `unsafe impl Sync` : sans le verrou EXTERNE de
    /// `send_serialise`, deux `extend_from_slice` concurrents auraient été une
    /// data race complète (réallocation concurrente du `Vec` → use-after-free
    /// potentiel), pas un simple échec d'assertion propre. Un `Mutex` interne
    /// prouve la même chose (l'entrelacement des TRAMES applicatives) sans
    /// jamais franchir la ligne de l'UB, même si `send_serialise` était cassé.
    struct RaceySink(Mutex<Vec<u8>>);

    struct RaceyWriter(Arc<RaceySink>);

    impl std::io::Write for RaceyWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut v = self.0 .0.lock().unwrap();
            // Élargit la fenêtre de course entre les deux `write_all` que fait
            // `send` (longueur puis corps) : sans le verrou EXTERNE de
            // `send_serialise`, un autre thread a largement le temps de
            // s'intercaler entre ces deux appels à `write` (chacun protégé
            // individuellement ici, mais pas la paire).
            std::thread::sleep(Duration::from_micros(50));
            v.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `THREADS` threads envoient chacun `PER_THREAD` messages `PaneInput`
    /// distincts vers un écrivain partagé, via `send_serialise`. On relit
    /// ensuite le flux accumulé avec `recv` et on vérifie qu'on retrouve
    /// EXACTEMENT `THREADS * PER_THREAD` trames décodables et cohérentes.
    ///
    /// Sans le verrou dans `send_serialise` (retiré temporairement pour
    /// vérification manuelle — voir le rapport de revue), ce test échoue : le
    /// flux entrelacé produit un nombre de trames incorrect ou un échec de
    /// décodage postcard. C'est la preuve que ce test garde bien la
    /// régression visée par le correctif (contrairement à l'ancien test
    /// d'intégration mono-écrivain qui passait déjà avant le correctif).
    #[test]
    fn send_serialise_serialise_vraiment_les_ecritures_concurrentes() {
        const THREADS: u64 = 8;
        const PER_THREAD: u64 = 25;

        let sink = Arc::new(RaceySink(Mutex::new(Vec::new())));
        let lock = Arc::new(Mutex::new(RaceyWriter(Arc::clone(&sink))));

        let mut handles = Vec::new();
        for t in 0..THREADS {
            let lock = Arc::clone(&lock);
            handles.push(std::thread::spawn(move || {
                for i in 0..PER_THREAD {
                    let pane_id = t * PER_THREAD + i;
                    let msg = ClientMessage::PaneInput {
                        pane_id,
                        bytes: format!("t{t}-m{i}").into_bytes(),
                    };
                    send_serialise(&lock, &msg).expect("send_serialise a échoué");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Tous les threads écrivains ont fini (join ci-dessus) : plus aucun
        // accès concurrent n'est possible à ce stade, mais on relit quand même
        // via le `Mutex` (Fix 6) — plus besoin d'un commentaire SAFETY, il n'y
        // a plus d'`unsafe` dans ce test.
        let bytes = sink.0.lock().unwrap().clone();
        let mut cursor: &[u8] = &bytes;
        let mut seen = HashSet::new();
        while !cursor.is_empty() {
            let msg: ClientMessage =
                recv(&mut cursor).expect("trame illisible : les écritures se sont entrelacées");
            match msg {
                ClientMessage::PaneInput { pane_id, bytes } => {
                    let expected = {
                        let t = pane_id / PER_THREAD;
                        let i = pane_id % PER_THREAD;
                        format!("t{t}-m{i}")
                    };
                    assert_eq!(
                        String::from_utf8(bytes).unwrap(),
                        expected,
                        "contenu incohérent pour pane_id {pane_id}"
                    );
                    assert!(
                        seen.insert(pane_id),
                        "pane_id {pane_id} vu deux fois (trame dupliquée par l'entrelacement)"
                    );
                }
                other => panic!("message inattendu : {other:?}"),
            }
        }
        assert_eq!(
            seen.len(),
            (THREADS * PER_THREAD) as usize,
            "nombre de trames décodées incorrect : entrelacement suspecté"
        );
    }
}
