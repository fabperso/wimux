//! Moteur navigateur pilotable par CDP (B2.1) : un Chromium externe visible,
//! possédé par le daemon via un thread tokio dédié, piloté par `chromiumoxide`.
//!
//! Le daemon reste synchrone : il parle au thread moteur par canaux (le pont
//! `BrowserEngine::exec` bloque jusqu'à la réponse). La logique métier (découverte
//! du binaire, garde d'URL, rendu de l'arbre d'accessibilité) est en fonctions
//! PURES, découplées des types `chromiumoxide` pour rester testables sans navigateur.

use std::path::PathBuf;

/// Renvoie le premier chemin candidat qui existe sur le disque, ou `None`.
pub fn find_browser_binary(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

/// Chemins d'installation standard, Chrome d'abord puis Edge (toujours présent
/// sur Windows 11). L'ordre encode la préférence.
pub fn default_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    // Chrome
    v.push(PathBuf::from(&pf).join(r"Google\Chrome\Application\chrome.exe"));
    v.push(PathBuf::from(&pf86).join(r"Google\Chrome\Application\chrome.exe"));
    if !local.is_empty() {
        v.push(PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe"));
    }
    // Edge (repli)
    v.push(PathBuf::from(&pf86).join(r"Microsoft\Edge\Application\msedge.exe"));
    v.push(PathBuf::from(&pf).join(r"Microsoft\Edge\Application\msedge.exe"));
    v
}

/// N'autorise que les schémas `http`/`https` (casse insensible). Refuse `file:`,
/// `javascript:`, `data:`, `about:`, et toute chaîne sans schéma.
pub fn is_allowed_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Nœud d'accessibilité simplifié (B2.1), découplé des types `chromiumoxide`.
#[derive(Debug, Clone, PartialEq)]
pub struct AxSnapshotNode {
    pub node_id: String,
    pub role: String,
    pub name: Option<String>,
    pub states: Vec<String>,
    pub child_ids: Vec<String>,
}

impl AxSnapshotNode {
    /// Un nœud est décoratif (élagable) s'il n'apporte rien : rôle ignoré/none,
    /// pas de nom — quel que soit son nombre d'enfants. Un nœud générique
    /// (ex. wrapper `<div>`) avec de vrais enfants reste décoratif : ses
    /// enfants sont promus d'un cran (voir `render_node`), sa propre ligne
    /// n'est pas imprimée.
    fn est_decoratif(&self) -> bool {
        (self.role == "none" || self.role == "ignored" || self.role.is_empty())
            && self.name.is_none()
    }
}

/// Rend l'arbre d'accessibilité en texte indenté : `rôle "nom" [états]`, un nœud
/// par ligne, profondeur = indentation de 2 espaces. Les nœuds décoratifs sont
/// élagués. La racine est le premier nœud (CDP renvoie la racine en tête).
pub fn render_ax_tree(nodes: &[AxSnapshotNode]) -> String {
    use std::collections::{HashMap, HashSet};
    if nodes.is_empty() {
        return String::new();
    }
    let index: HashMap<&str, &AxSnapshotNode> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let mut out = String::new();
    // Garde anti-cycle : les données viennent de la PAGE (via `map_ax_node`,
    // Task 5), donc non fiables. Un `child_ids` cyclique (A -> B -> A) sans
    // cette garde ferait déborder la pile et planterait le daemon entier.
    let mut visites: HashSet<&str> = HashSet::new();
    render_node(&nodes[0], &index, 0, &mut out, &mut visites);
    out.trim_end().to_string()
}

fn render_node<'a>(
    node: &'a AxSnapshotNode,
    index: &std::collections::HashMap<&'a str, &'a AxSnapshotNode>,
    depth: usize,
    out: &mut String,
    visites: &mut std::collections::HashSet<&'a str>,
) {
    // Marqué visité avant de descendre : chaque nœud est traité au plus une
    // fois, ce qui garantit la terminaison même en présence d'un cycle.
    if !visites.insert(node.node_id.as_str()) {
        return;
    }
    if !node.est_decoratif() {
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push_str(&node.role);
        if let Some(name) = &node.name {
            out.push_str(&format!(" \"{name}\""));
        }
        if !node.states.is_empty() {
            out.push_str(&format!(" [{}]", node.states.join(", ")));
        }
        out.push('\n');
    }
    // Un nœud décoratif ne consomme pas de profondeur : ses enfants remontent
    // (promotion), qu'il ait ou non déjà des enfants propres.
    let child_depth = if node.est_decoratif() {
        depth
    } else {
        depth + 1
    };
    for cid in &node.child_ids {
        if let Some(child) = index.get(cid.as_str()) {
            // `render_node` re-vérifie et ignore silencieusement si déjà visité.
            render_node(child, index, child_depth, out, visites);
        }
    }
}

use std::sync::Mutex;

use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

/// Commande adressée au thread moteur.
pub enum BrowserCommand {
    Launch,
    Close,
    Status,
    Navigate(String),
    Url,
    Snapshot,
    Screenshot,
}

/// Réponse du thread moteur.
#[derive(Debug)]
pub enum BrowserReply {
    Ok,
    Status { running: bool, url: Option<String> },
    Text(String),
    Shot(String),
}

/// Un travail : commande + canal de réponse (oneshot).
struct Job {
    cmd: BrowserCommand,
    reply: tokio::sync::oneshot::Sender<Result<BrowserReply, String>>,
}

/// Pont synchrone → thread moteur asynchrone. Démarre le thread au premier appel.
pub struct BrowserEngine {
    tx: Mutex<Option<tokio::sync::mpsc::Sender<Job>>>,
}

impl Default for BrowserEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserEngine {
    pub fn new() -> BrowserEngine {
        BrowserEngine {
            tx: Mutex::new(None),
        }
    }

    /// Exécute une commande sur le navigateur ; BLOQUE jusqu'à la réponse.
    /// Démarre le thread moteur (et donc pas le navigateur — lancement paresseux
    /// à la commande `Launch`/`Navigate`) au premier appel.
    pub fn exec(&self, cmd: BrowserCommand) -> Result<BrowserReply, String> {
        let sender = self.ensure_worker();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        sender
            .blocking_send(Job { cmd, reply: rtx })
            .map_err(|_| "moteur navigateur arrêté".to_string())?;
        rrx.blocking_recv()
            .map_err(|_| "pas de réponse du moteur navigateur".to_string())?
    }

    fn ensure_worker(&self) -> tokio::sync::mpsc::Sender<Job> {
        let mut g = self.tx.lock().unwrap();
        if let Some(tx) = g.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = tokio::sync::mpsc::channel::<Job>(32);
        std::thread::Builder::new()
            .name("wimux-browser".into())
            .spawn(move || worker(rx))
            .expect("thread moteur navigateur");
        *g = Some(tx.clone());
        tx
    }
}

/// Session active : le navigateur, sa page unique, et la tâche qui pompe le Handler.
struct Session {
    browser: Browser,
    page: chromiumoxide::Page,
    _handler: tokio::task::JoinHandle<()>,
}

/// Corps du thread moteur : un runtime tokio qui traite les commandes en série.
fn worker(mut rx: tokio::sync::mpsc::Receiver<Job>) {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Impossible de démarrer le runtime : répondre l'erreur à chaque job.
            while let Ok(job) = rx.try_recv() {
                let _ = job.reply.send(Err(format!("runtime tokio : {e}")));
            }
            return;
        }
    };
    rt.block_on(async move {
        let mut sess: Option<Session> = None;
        while let Some(job) = rx.recv().await {
            let res = dispatch(&mut sess, job.cmd).await;
            let _ = job.reply.send(res);
        }
        // Canal fermé (BrowserEngine droppé) : `sess` est droppé -> Chrome fermé.
    });
}

/// Lance le navigateur (découverte Chrome→Edge) et ouvre une page vierge.
async fn launch_session() -> Result<Session, String> {
    let bin = find_browser_binary(&default_candidates())
        .ok_or_else(|| "aucun navigateur Chrome/Edge trouvé sur cette machine".to_string())?;
    // Profil (`user-data-dir`) DÉDIÉ et JETABLE, unique par navigateur lancé :
    // - Isolation des tests : sans ça, tous les `BrowserEngine` partagent le
    //   profil par défaut de chromiumoxide (`%TEMP%\chromiumoxide-runner`), et
    //   deux navigateurs simultanés (tests en parallèle, threads par défaut de
    //   `cargo test`) se marchent dessus sur le fichier verrou du profil
    //   (« Lock file can not be created ! »).
    // - Sécurité (fil rouge B2) : le navigateur d'automatisation ne doit PAS
    //   hériter du profil réel de l'utilisateur (cookies, sessions connectées,
    //   historique). Un profil neuf à chaque lancement est le défaut sûr —
    //   un partage délibéré de profil serait une décision explicite, pas
    //   celle-ci. Le dossier peut rester sur disque après fermeture
    //   (best-effort, pas de nettoyage fragile).
    let profile_dir = browser_profile_dir()?;
    let config = BrowserConfig::builder()
        .with_head()
        .chrome_executable(bin)
        .user_data_dir(&profile_dir)
        .build()
        .map_err(|e| format!("config navigateur : {e}"))?;
    let (browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("lancement du navigateur : {e}"))?;
    let handler_task = tokio::spawn(async move {
        while let Some(ev) = handler.next().await {
            if ev.is_err() {
                break;
            }
        }
    });
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| format!("ouverture de page : {e}"))?;
    Ok(Session {
        browser,
        page,
        _handler: handler_task,
    })
}

/// Traite une commande. `sess` est l'état mutable de la session (None = non lancé).
async fn dispatch(sess: &mut Option<Session>, cmd: BrowserCommand) -> Result<BrowserReply, String> {
    match cmd {
        BrowserCommand::Launch => {
            if sess.is_none() {
                *sess = Some(launch_session().await?);
            }
            Ok(BrowserReply::Ok)
        }
        BrowserCommand::Close => {
            // Drop de la session : ferme Chrome (le Browser droppé tue le process).
            if let Some(s) = sess.take() {
                // Best-effort : fermeture propre du navigateur. `close()` ne fait
                // qu'envoyer la commande CDP `Browser.close` : le process OS peut
                // encore tourner un instant après. `wait()` attend sa sortie
                // effective avant de répondre, pour ne pas laisser un process
                // Chrome zombie derrière un `Close` qui semble terminé. (Chaque
                // session a désormais son propre `user-data-dir` — voir
                // `browser_profile_dir` — donc la collision de verrou entre
                // navigateurs n'est plus possible ; le dossier de profil reste
                // sur disque après fermeture, best-effort, sans nettoyage.)
                let mut b = s.browser;
                let _ = b.close().await;
                let _ = b.wait().await;
            }
            Ok(BrowserReply::Ok)
        }
        BrowserCommand::Status => {
            let url = match sess.as_ref() {
                Some(s) => s.page.url().await.ok().flatten(),
                None => None,
            };
            Ok(BrowserReply::Status {
                running: sess.is_some(),
                url,
            })
        }
        BrowserCommand::Navigate(url) => {
            if !is_allowed_url(&url) {
                return Err("URL refusée : http(s) seulement".into());
            }
            // Lancement paresseux.
            if sess.is_none() {
                *sess = Some(launch_session().await?);
            }
            let page = &sess.as_ref().unwrap().page;
            page.goto(url)
                .await
                .map_err(|e| format!("navigation : {e}"))?;
            page.wait_for_navigation()
                .await
                .map_err(|e| format!("attente de chargement : {e}"))?;
            let finale = page.url().await.ok().flatten().unwrap_or_default();
            Ok(BrowserReply::Text(finale))
        }
        BrowserCommand::Url => {
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            let u = s.page.url().await.ok().flatten().unwrap_or_default();
            Ok(BrowserReply::Text(u))
        }
        BrowserCommand::Snapshot => {
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            use chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeParams;
            let resp = s
                .page
                .execute(GetFullAxTreeParams::default())
                .await
                .map_err(|e| format!("arbre d'accessibilité : {e}"))?;
            // Mappe les AxNode CDP vers notre type découplé, puis rend.
            let nodes: Vec<AxSnapshotNode> = resp.result.nodes.iter().map(map_ax_node).collect();
            Ok(BrowserReply::Text(render_ax_tree(&nodes)))
        }
        BrowserCommand::Screenshot => {
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
            use chromiumoxide::page::ScreenshotParams;
            let png = s
                .page
                .screenshot(
                    ScreenshotParams::builder()
                        .format(CaptureScreenshotFormat::Png)
                        .build(),
                )
                .await
                .map_err(|e| format!("capture : {e}"))?;
            let path = screenshot_path()?;
            std::fs::write(&path, &png).map_err(|e| format!("écriture PNG : {e}"))?;
            Ok(BrowserReply::Shot(path))
        }
    }
}

/// Convertit un nœud d'accessibilité CDP en notre `AxSnapshotNode` découplé.
/// Isole les particularités `chromiumoxide` 0.9.1 : rôle/nom sont des `AxValue`
/// dont le `.value` est un `serde_json::Value` (chaîne JSON) — on le convertit
/// en `String` Rust via `Value::as_str` (pas un simple `to_string`, qui
/// garderait les guillemets JSON).
fn map_ax_node(n: &chromiumoxide::cdp::browser_protocol::accessibility::AxNode) -> AxSnapshotNode {
    let role = n
        .role
        .as_ref()
        .and_then(|v| v.value.as_ref())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let name = n
        .name
        .as_ref()
        .and_then(|v| v.value.as_ref())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // États pertinents (focusable, disabled, checked…) : les propriétés dont la
    // valeur booléenne calculée vaut `true`. `AxPropertyName` expose `AsRef<str>`
    // (via le nom CDP, ex. "focusable"), converti en minuscules pour l'affichage.
    let states: Vec<String> = n
        .properties
        .iter()
        .flatten()
        .filter_map(|p| {
            let est_vrai = p
                .value
                .value
                .as_ref()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            est_vrai.then(|| p.name.as_ref().to_ascii_lowercase())
        })
        .collect();
    AxSnapshotNode {
        node_id: n.node_id.inner().clone(),
        role,
        name,
        states,
        child_ids: n
            .child_ids
            .iter()
            .flatten()
            .map(|c| c.inner().clone())
            .collect(),
    }
}

/// Dossier de profil navigateur DÉDIÉ, sous
/// `%LOCALAPPDATA%\wimux\browser-profile\<pid>-<compteur>` — un par navigateur
/// lancé (même style que `screenshot_path` ci-dessous). Numérotation monotone
/// par process (pas d'horloge : déterministe, et unique même si deux
/// `BrowserEngine` du même process se lancent en parallèle, ex. tests).
fn browser_profile_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let base =
        std::env::var_os("LOCALAPPDATA").ok_or_else(|| "%LOCALAPPDATA% introuvable".to_string())?;
    let i = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = PathBuf::from(base)
        .join("wimux")
        .join("browser-profile")
        .join(format!("{pid}-{i}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("création dossier profil navigateur : {e}"))?;
    Ok(dir)
}

/// Chemin de capture sous `%LOCALAPPDATA%\wimux\screenshots\shot-<pid>-<compteur>.png`.
/// Numérotation monotone par process (pas d'horloge : évite une dépendance temps
/// et reste déterministe pour les tests).
fn screenshot_path() -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let base =
        std::env::var_os("LOCALAPPDATA").ok_or_else(|| "%LOCALAPPDATA% introuvable".to_string())?;
    let dir = std::path::PathBuf::from(base)
        .join("wimux")
        .join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("création dossier captures : {e}"))?;
    let i = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(dir
        .join(format!("shot-{pid}-{i}.png"))
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Un binaire navigateur est-il disponible ? (garde de test — conditionne les
    /// tests d'intégration, comme les tests git de M3/M4.)
    fn navigateur_dispo() -> bool {
        find_browser_binary(&default_candidates()).is_some()
    }

    /// Sert `html` sur `127.0.0.1:<port libre>` le temps du test ; renvoie l'URL.
    fn servir_page_locale(html: &'static str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            // Sert jusqu'à 8 connexions, puis s'arrête. Une seule connexion ne
            // suffit PAS : Chrome ouvre souvent plus d'une connexion vers la
            // même origine pour une seule navigation (ex. `favicon.ico` en
            // plus de la page). Avec un serveur à connexion unique, la
            // première connexion acceptée ferme le `TcpListener` (fin du
            // thread) avant que la seconde n'arrive, qui se voit alors
            // refusée (`ERR_CONNECTION_REFUSED`) — flaky de façon intermittente
            // selon laquelle des deux connexions gagne la course. Accepter
            // plusieurs connexions absorbe ces requêtes additionnelles sans
            // affecter le test (chaque connexion reçoit la même page).
            for _ in 0..8 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    html.len(),
                    html
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/"), handle)
    }

    #[test]
    fn navigate_refuse_les_schemas_non_http() {
        if !navigateur_dispo() {
            eprintln!("aucun navigateur : test navigate_refuse ignoré");
            return;
        }
        let engine = BrowserEngine::new();
        let err = engine
            .exec(BrowserCommand::Navigate("file:///C:/x".into()))
            .unwrap_err();
        assert!(err.contains("http"), "message de refus attendu : {err}");
        let _ = engine.exec(BrowserCommand::Close);
    }

    #[test]
    fn navigate_puis_url_reflete_la_page() {
        if !navigateur_dispo() {
            eprintln!("aucun navigateur : test navigate_puis_url ignoré");
            return;
        }
        let (url, _srv) = servir_page_locale("<!doctype html><title>T</title><h1>Bonjour</h1>");
        let engine = BrowserEngine::new();
        match engine.exec(BrowserCommand::Navigate(url.clone())).unwrap() {
            BrowserReply::Text(finale) => assert!(finale.starts_with("http://127.0.0.1:")),
            _ => panic!("Text attendu"),
        }
        match engine.exec(BrowserCommand::Url).unwrap() {
            BrowserReply::Text(u) => assert!(u.starts_with("http://127.0.0.1:"), "url : {u}"),
            _ => panic!("Text attendu"),
        }
        let _ = engine.exec(BrowserCommand::Close);
    }

    #[test]
    fn launch_status_close_cycle() {
        if !navigateur_dispo() {
            eprintln!("aucun Chrome/Edge : test launch_status_close ignoré");
            return;
        }
        let engine = BrowserEngine::new();
        // Avant lancement : pas en cours.
        match engine.exec(BrowserCommand::Status).unwrap() {
            BrowserReply::Status { running, .. } => assert!(!running),
            _ => panic!("Status attendu"),
        }
        // Lancement (paresseux) puis état.
        assert!(matches!(
            engine.exec(BrowserCommand::Launch).unwrap(),
            BrowserReply::Ok
        ));
        match engine.exec(BrowserCommand::Status).unwrap() {
            BrowserReply::Status { running, .. } => assert!(running),
            _ => panic!("Status attendu"),
        }
        // Une lecture sans page chargée ne panique pas (url vide/None acceptée).
        // Fermeture.
        assert!(matches!(
            engine.exec(BrowserCommand::Close).unwrap(),
            BrowserReply::Ok
        ));
        match engine.exec(BrowserCommand::Status).unwrap() {
            BrowserReply::Status { running, .. } => assert!(!running),
            _ => panic!("Status attendu"),
        }
    }

    #[test]
    fn find_binary_renvoie_le_premier_existant() {
        // Le binaire de test courant existe forcément ; un chemin bidon non.
        let moi = std::env::current_exe().unwrap();
        let bidon = PathBuf::from("Z:/inexistant/xyz.exe");
        assert_eq!(
            find_browser_binary(&[bidon.clone(), moi.clone()]),
            Some(moi)
        );
        assert_eq!(find_browser_binary(&[bidon]), None);
    }

    #[test]
    fn url_autorisee_http_https_seulement() {
        assert!(is_allowed_url("http://localhost:8899/"));
        assert!(is_allowed_url("https://example.com/x"));
        assert!(is_allowed_url("HTTPS://Example.com")); // casse insensible sur le schéma
        assert!(!is_allowed_url("file:///C:/x"));
        assert!(!is_allowed_url("javascript:alert(1)"));
        assert!(!is_allowed_url("data:text/html,x"));
        assert!(!is_allowed_url("about:blank"));
        assert!(!is_allowed_url("localhost:8899")); // sans schéma = refusé
    }

    #[test]
    fn render_ax_tree_indente_role_nom_etats_et_elague() {
        let nodes = vec![
            AxSnapshotNode {
                node_id: "1".into(),
                role: "RootWebArea".into(),
                name: Some("Page de test".into()),
                states: vec![],
                child_ids: vec!["2".into(), "3".into()],
            },
            AxSnapshotNode {
                node_id: "2".into(),
                role: "button".into(),
                name: Some("Continuer".into()),
                states: vec!["focusable".into()],
                child_ids: vec![],
            },
            // Nœud décoratif : role "none", sans nom, sans enfant -> élagué.
            AxSnapshotNode {
                node_id: "3".into(),
                role: "none".into(),
                name: None,
                states: vec![],
                child_ids: vec![],
            },
        ];
        let out = render_ax_tree(&nodes);
        assert!(
            out.contains("RootWebArea \"Page de test\""),
            "racine : {out}"
        );
        assert!(
            out.contains("  button \"Continuer\" [focusable]"),
            "bouton indenté : {out}"
        );
        assert!(
            !out.contains("none"),
            "le nœud décoratif doit être élagué : {out}"
        );
    }

    #[test]
    fn render_ax_tree_vide_donne_chaine_vide() {
        assert_eq!(render_ax_tree(&[]), "");
    }

    #[test]
    fn render_ax_tree_promeut_les_enfants_dun_noeud_decoratif_avec_enfants() {
        let nodes = vec![
            AxSnapshotNode {
                node_id: "1".into(),
                role: "RootWebArea".into(),
                name: Some("Page".into()),
                states: vec![],
                child_ids: vec!["2".into()],
            },
            // Nœud générique (wrapper), sans nom, mais AVEC de vrais enfants :
            // doit rester décoratif et promouvoir ses enfants d'un cran.
            AxSnapshotNode {
                node_id: "2".into(),
                role: "none".into(),
                name: None,
                states: vec![],
                child_ids: vec!["3".into(), "4".into()],
            },
            AxSnapshotNode {
                node_id: "3".into(),
                role: "button".into(),
                name: Some("A".into()),
                states: vec![],
                child_ids: vec![],
            },
            AxSnapshotNode {
                node_id: "4".into(),
                role: "link".into(),
                name: Some("B".into()),
                states: vec![],
                child_ids: vec![],
            },
        ];
        let out = render_ax_tree(&nodes);
        assert!(!out.contains("none"), "le wrapper élagué : {out}");
        // Le wrapper "none" n'a pas consommé de profondeur : button/link sont
        // au même niveau d'indentation que si "none" n'avait jamais existé,
        // c'est-à-dire un seul cran (2 espaces) sous la racine — pas deux.
        assert!(
            out.contains("\n  button \"A\"\n") || out.ends_with("\n  button \"A\""),
            "button indenté d'un seul cran : {out:?}"
        );
        assert!(
            out.contains("\n  link \"B\"\n") || out.ends_with("\n  link \"B\""),
            "link indenté d'un seul cran : {out:?}"
        );
        assert!(
            !out.contains("    button") && !out.contains("    link"),
            "pas de double indentation : {out:?}"
        );
    }

    #[test]
    fn snapshot_et_screenshot_sur_page_locale() {
        if !navigateur_dispo() {
            eprintln!("aucun navigateur : test snapshot/screenshot ignoré");
            return;
        }
        let (url, _srv) =
            servir_page_locale("<!doctype html><title>T</title><button>Continuer</button>");
        let engine = BrowserEngine::new();
        engine.exec(BrowserCommand::Navigate(url)).unwrap();

        match engine.exec(BrowserCommand::Snapshot).unwrap() {
            BrowserReply::Text(tree) => {
                assert!(
                    tree.contains("button"),
                    "le bouton doit apparaître : {tree}"
                );
                assert!(
                    tree.contains("Continuer"),
                    "son nom doit apparaître : {tree}"
                );
            }
            _ => panic!("Text attendu"),
        }

        match engine.exec(BrowserCommand::Screenshot).unwrap() {
            BrowserReply::Shot(path) => {
                let p = std::path::Path::new(&path);
                assert!(p.exists(), "le PNG doit exister : {path}");
                assert!(
                    std::fs::metadata(p).unwrap().len() > 0,
                    "le PNG ne doit pas être vide"
                );
                let _ = std::fs::remove_file(p);
            }
            _ => panic!("Shot attendu"),
        }
        let _ = engine.exec(BrowserCommand::Close);
    }

    #[test]
    fn render_ax_tree_cycle_termine_sans_deborder_la_pile() {
        // A et B se pointent mutuellement : sans garde anti-cycle, récursion
        // infinie -> débordement de pile -> crash du binaire de test.
        let nodes = vec![
            AxSnapshotNode {
                node_id: "a".into(),
                role: "generic".into(),
                name: Some("A".into()),
                states: vec![],
                child_ids: vec!["b".into()],
            },
            AxSnapshotNode {
                node_id: "b".into(),
                role: "generic".into(),
                name: Some("B".into()),
                states: vec![],
                child_ids: vec!["a".into()],
            },
        ];
        let out = render_ax_tree(&nodes);
        // La fonction doit retourner (pas de panic/débordement) et chaque
        // nœud n'apparaît qu'une seule fois.
        assert_eq!(out.matches("generic \"A\"").count(), 1, "sortie : {out}");
        assert_eq!(out.matches("generic \"B\"").count(), 1, "sortie : {out}");
    }
}
