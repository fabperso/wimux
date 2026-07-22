# B2.1 — Moteur navigateur pilotable (lecture seule) — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Donner au daemon un Chromium externe pilotable par CDP (fenêtre visible), et une CLI `wimux browser` en lecture seule (navigate/url/snapshot/screenshot) — la fondation de B2.

**Architecture:** Un thread dédié fait tourner un runtime tokio qui possède le navigateur (`chromiumoxide`) ; le daemon, synchrone, lui parle par canaux (`tokio::sync::mpsc` + `oneshot`, via `blocking_send`/`blocking_recv`). La logique métier (découverte du binaire, garde d'URL, rendu de l'arbre d'accessibilité) est en fonctions pures testables, découplées des types `chromiumoxide`.

**Tech Stack:** Rust (`wimux-protocol` postcard/serde, `wimux-server`, `wimux-cli`), `chromiumoxide` (CDP), `tokio`, `futures`.

## Global Constraints

- **Compat postcard** : nouvelles variantes d'enum / champs de struct **EN FIN**.
- **Daemon persistant** : après tout changement de `wimux-protocol`/`wimux-server`, **rebuild release + redémarrer le daemon détaché**.
- **Le daemon reste synchrone** : seul le thread moteur connaît tokio. La frontière est le couple `BrowserEngine::exec` (bloquant) ↔ canaux.
- **Aucune exécution de JS de page** en B2.1 : que des lectures CDP natives (AX tree, screenshot, url, title). Pas de `Runtime.evaluate`.
- **Garde d'URL** : `navigate` n'accepte que `http`/`https` (refus `file:`/`javascript:`/`data:`), casse insensible sur le schéma.
- **Découverte du binaire** : Chrome si présent, sinon `msedge.exe`, sinon `Err` clair.
- **Cycle de vie** : un seul navigateur par daemon, lancement paresseux, `close` explicite, meurt avec le daemon ; ne survit pas à un redémarrage.
- **Snapshot = arbre d'accessibilité** ; pas de refs réutilisables.
- **Tests navigateur conditionnels** : ignorés proprement si aucun binaire trouvé (motif M3/M4) ; navigation vers une **page locale** servie par le test, jamais d'accès réseau externe.
- **Qualité** : `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` verts ; et dans `wimux-gui/src-tauri` (hors workspace) : clippy `--all-targets -D warnings` + `fmt --check` ; `npm run build`.
- **Langue** : commentaires et messages en français.

---

## File Structure

**Créés :**
- `crates/wimux-server/src/browser.rs` — TOUT le moteur B2.1 : les fonctions pures (découverte du binaire, garde d'URL, rendu AX), le `BrowserEngine` (pont sync↔async), le thread worker tokio pilotant `chromiumoxide`, et le mapping `chromiumoxide::AxNode → AxSnapshotNode`.

**Modifiés :**
- `crates/wimux-server/Cargo.toml` — deps `chromiumoxide`, `tokio`, `futures`.
- `crates/wimux-server/src/lib.rs` — `pub mod browser;`.
- `crates/wimux-protocol/src/lib.rs` — 7 `ClientMessage` + 3 `ServerMessage` (en fin).
- `crates/wimux-server/src/daemon.rs` — `Server` possède un `BrowserEngine` ; handlers.
- `crates/wimux-cli/src/main.rs` — namespace `wimux browser` étendu (launch/close/status/navigate/url/snapshot/screenshot) + aide.

**Interfaces clés (verrouillées ici) :**

```rust
// wimux-protocol — ClientMessage (en fin) :
//   BrowserLaunch | BrowserClose | BrowserStatus
//   BrowserNavigate { url: String } | BrowserUrl | BrowserSnapshot | BrowserScreenshot
// ServerMessage (en fin) :
//   BrowserState { running: bool, url: Option<String> }
//   BrowserText(String)         // url / navigate (url finale) / snapshot
//   BrowserShot { path: String }

// crates/wimux-server/src/browser.rs
pub fn find_browser_binary(candidates: &[std::path::PathBuf]) -> Option<std::path::PathBuf>;
pub fn default_candidates() -> Vec<std::path::PathBuf>;   // Chrome puis Edge, chemins Windows
pub fn is_allowed_url(url: &str) -> bool;                 // http/https seulement

/// Nœud d'accessibilité simplifié, DÉCOUPLÉ des types chromiumoxide (testable seul).
pub struct AxSnapshotNode {
    pub node_id: String,
    pub role: String,
    pub name: Option<String>,
    pub states: Vec<String>,
    pub child_ids: Vec<String>,
}
pub fn render_ax_tree(nodes: &[AxSnapshotNode]) -> String;

pub struct BrowserEngine { /* Mutex<Option<Sender<Job>>> */ }
impl BrowserEngine {
    pub fn new() -> BrowserEngine;
    pub fn exec(&self, cmd: BrowserCommand) -> Result<BrowserReply, String>; // bloquant
}
pub enum BrowserCommand { Launch, Close, Status, Navigate(String), Url, Snapshot, Screenshot }
pub enum BrowserReply { Ok, Status { running: bool, url: Option<String> }, Text(String), Shot(String) }
```

---

## Phase B2.1.1 — Fonctions pures & dépendances

### Task 1 : deps + découverte binaire + garde d'URL + rendu AX

**Files:**
- Modify: `crates/wimux-server/Cargo.toml`, `crates/wimux-server/src/lib.rs`
- Create: `crates/wimux-server/src/browser.rs`
- Test: `crates/wimux-server/src/browser.rs`

**Interfaces:**
- Produces: `find_browser_binary`, `default_candidates`, `is_allowed_url`, `AxSnapshotNode`, `render_ax_tree`.

- [ ] **Step 1 : Ajouter les dépendances**

Run:
```bash
cargo add chromiumoxide -p wimux-server --features tokio-runtime
cargo add tokio -p wimux-server --features rt-multi-thread,macros,sync,time
cargo add futures -p wimux-server
```
Puis `cargo build -p wimux-server`. **Si la compilation tire `async-std`** (feature par défaut de chromiumoxide), désactiver les défauts :
`cargo add chromiumoxide -p wimux-server --no-default-features --features tokio-runtime`
et rebuild. Noter la version retenue dans le rapport.

- [ ] **Step 2 : Écrire les tests des fonctions pures (échouent)**

Créer `crates/wimux-server/src/browser.rs` avec **seulement** le module de tests :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        assert!(out.contains("RootWebArea \"Page de test\""), "racine : {out}");
        assert!(out.contains("  button \"Continuer\" [focusable]"), "bouton indenté : {out}");
        assert!(!out.contains("none"), "le nœud décoratif doit être élagué : {out}");
    }

    #[test]
    fn render_ax_tree_vide_donne_chaine_vide() {
        assert_eq!(render_ax_tree(&[]), "");
    }
}
```

- [ ] **Step 3 : Vérifier l'échec**

Run: `cargo test -p wimux-server browser::tests`
Expected: FAIL — les fonctions n'existent pas.

- [ ] **Step 4 : Déclarer le module**

Dans `crates/wimux-server/src/lib.rs`, ajouter en ordre alphabétique (après `pub mod batch;`) :

```rust
pub mod browser;
```

- [ ] **Step 5 : Écrire les fonctions pures**

En tête de `browser.rs`, **avant** le module de tests :

```rust
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
    let pf86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_else(|_| r"C:\Program Files (x86)".into());
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
    /// pas de nom, pas d'enfant.
    fn est_decoratif(&self) -> bool {
        (self.role == "none" || self.role == "ignored" || self.role.is_empty())
            && self.name.is_none()
            && self.child_ids.is_empty()
    }
}

/// Rend l'arbre d'accessibilité en texte indenté : `rôle "nom" [états]`, un nœud
/// par ligne, profondeur = indentation de 2 espaces. Les nœuds décoratifs sont
/// élagués. La racine est le premier nœud (CDP renvoie la racine en tête).
pub fn render_ax_tree(nodes: &[AxSnapshotNode]) -> String {
    use std::collections::HashMap;
    if nodes.is_empty() {
        return String::new();
    }
    let index: HashMap<&str, &AxSnapshotNode> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let mut out = String::new();
    render_node(&nodes[0], &index, 0, &mut out);
    out.trim_end().to_string()
}

fn render_node(
    node: &AxSnapshotNode,
    index: &std::collections::HashMap<&str, &AxSnapshotNode>,
    depth: usize,
    out: &mut String,
) {
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
    // Un nœud décoratif ne consomme pas de profondeur : ses enfants remontent.
    let child_depth = if node.est_decoratif() { depth } else { depth + 1 };
    for cid in &node.child_ids {
        if let Some(child) = index.get(cid.as_str()) {
            render_node(child, index, child_depth, out);
        }
    }
}
```

- [ ] **Step 6 : Lancer les tests**

Run: `cargo test -p wimux-server browser::tests`
Expected: PASS (4 tests).

- [ ] **Step 7 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/Cargo.toml Cargo.lock crates/wimux-server/src/lib.rs crates/wimux-server/src/browser.rs
git commit -m "feat(browser): deps CDP + fonctions pures (decouverte binaire, garde URL, rendu AX)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B2.1.2 — Protocole

### Task 2 : messages navigateur + daemon compilable

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (bras intérimaire)
- Test: `crates/wimux-protocol/src/lib.rs`

**Interfaces:**
- Produces: `BrowserLaunch`/`BrowserClose`/`BrowserStatus`/`BrowserNavigate`/`BrowserUrl`/`BrowserSnapshot`/`BrowserScreenshot` ; `BrowserState`/`BrowserText`/`BrowserShot`.

- [ ] **Step 1 : Écrire le test de round-trip (échoue)**

Dans le module de tests de `crates/wimux-protocol/src/lib.rs` :

```rust
#[test]
fn aller_retour_messages_navigateur() {
    let msg = ClientMessage::BrowserNavigate {
        url: "http://localhost:8899/".into(),
    };
    let bytes = postcard::to_allocvec(&msg).unwrap();
    assert!(matches!(
        postcard::from_bytes::<ClientMessage>(&bytes).unwrap(),
        ClientMessage::BrowserNavigate { url } if url == "http://localhost:8899/"
    ));

    let reply = ServerMessage::BrowserState {
        running: true,
        url: Some("http://localhost:8899/".into()),
    };
    let bytes = postcard::to_allocvec(&reply).unwrap();
    match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
        ServerMessage::BrowserState { running, url } => {
            assert!(running);
            assert_eq!(url.as_deref(), Some("http://localhost:8899/"));
        }
        _ => panic!("variante inattendue"),
    }

    let shot = ServerMessage::BrowserShot {
        path: "C:\\x\\1.png".into(),
    };
    let bytes = postcard::to_allocvec(&shot).unwrap();
    assert!(matches!(
        postcard::from_bytes::<ServerMessage>(&bytes).unwrap(),
        ServerMessage::BrowserShot { path } if path == "C:\\x\\1.png"
    ));
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-protocol aller_retour_messages_navigateur`
Expected: FAIL — variantes absentes.

- [ ] **Step 3 : Ajouter les variantes `ClientMessage` EN FIN**

Avant l'accolade fermante de `enum ClientMessage` :

```rust
    /// B2.1 : lance le navigateur pilotable (no-op s'il tourne déjà).
    BrowserLaunch,
    /// B2.1 : ferme le navigateur pilotable.
    BrowserClose,
    /// B2.1 : état du navigateur (lancé ? URL courante ?).
    BrowserStatus,
    /// B2.1 : navigue (lance au besoin) ; refuse les schémas non http(s).
    BrowserNavigate { url: String },
    /// B2.1 : URL courante (erreur si non lancé).
    BrowserUrl,
    /// B2.1 : arbre d'accessibilité de la page (erreur si non lancé).
    BrowserSnapshot,
    /// B2.1 : capture PNG écrite sur disque, renvoie le chemin (erreur si non lancé).
    BrowserScreenshot,
```

- [ ] **Step 4 : Ajouter les variantes `ServerMessage` EN FIN**

```rust
    /// B2.1 : réponse à `BrowserStatus`.
    BrowserState { running: bool, url: Option<String> },
    /// B2.1 : réponse texte (url / navigate / snapshot).
    BrowserText(String),
    /// B2.1 : réponse à `BrowserScreenshot` — chemin du PNG.
    BrowserShot { path: String },
```

- [ ] **Step 5 : Garder `wimux-server` compilable (bras intérimaire)**

L'ajout casse le `match msg` exhaustif de `daemon.rs`. Ajouter un bras intérimaire
**juste avant** `ClientMessage::Input(bytes) =>` (il sera remplacé en Task 6) :

```rust
            // B2.1 (intérim) : handlers réels en Task 6.
            ClientMessage::BrowserLaunch
            | ClientMessage::BrowserClose
            | ClientMessage::BrowserStatus
            | ClientMessage::BrowserNavigate { .. }
            | ClientMessage::BrowserUrl
            | ClientMessage::BrowserSnapshot
            | ClientMessage::BrowserScreenshot => {
                let mut wr: &PipeConn = &conn;
                send(
                    &mut wr,
                    &ServerMessage::Error("navigateur : pas encore implémenté (Task 6)".into()),
                )?;
            }
```

- [ ] **Step 6 : Lancer + qualité + commit**

Run: `cargo test -p wimux-protocol && cargo build -p wimux-server && cargo fmt -p wimux-protocol -p wimux-server && cargo clippy -p wimux-protocol --all-targets -- -D warnings`
Expected: vert.

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "feat(browser): protocole B2.1 — messages launch/close/status/navigate/url/snapshot/screenshot

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B2.1.3 — Le moteur (pont sync↔async)

### Task 3 : `BrowserEngine` + launch/close/status

**Files:**
- Modify: `crates/wimux-server/src/browser.rs`
- Test: `crates/wimux-server/src/browser.rs`

**Interfaces:**
- Produces: `BrowserEngine::new/exec`, `BrowserCommand`, `BrowserReply`.
- Consumes: `find_browser_binary`, `default_candidates` (Task 1) ; `chromiumoxide`.

- [ ] **Step 1 : Écrire le test d'intégration conditionnel (échoue)**

Dans le module de tests de `browser.rs` :

```rust
    /// Un binaire navigateur est-il disponible ? (garde de test — conditionne les
    /// tests d'intégration, comme les tests git de M3/M4.)
    fn navigateur_dispo() -> bool {
        find_browser_binary(&default_candidates()).is_some()
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
        assert!(matches!(engine.exec(BrowserCommand::Launch).unwrap(), BrowserReply::Ok));
        match engine.exec(BrowserCommand::Status).unwrap() {
            BrowserReply::Status { running, .. } => assert!(running),
            _ => panic!("Status attendu"),
        }
        // Une lecture sans page chargée ne panique pas (url vide/None acceptée).
        // Fermeture.
        assert!(matches!(engine.exec(BrowserCommand::Close).unwrap(), BrowserReply::Ok));
        match engine.exec(BrowserCommand::Status).unwrap() {
            BrowserReply::Status { running, .. } => assert!(!running),
            _ => panic!("Status attendu"),
        }
    }
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server launch_status_close_cycle`
Expected: FAIL — `BrowserEngine`/`BrowserCommand` absents.

- [ ] **Step 3 : Écrire le moteur**

Ajouter dans `browser.rs` (avant le module de tests). **Confirmer les imports
`chromiumoxide` au build** (les chemins exacts peuvent varier selon la version
retenue en Task 1) :

```rust
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
        BrowserEngine { tx: Mutex::new(None) }
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
    let config = BrowserConfig::builder()
        .with_head()
        .chrome_executable(bin)
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
                // Best-effort : fermeture propre du navigateur.
                let mut b = s.browser;
                let _ = b.close().await;
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
        // Navigate/Url/Snapshot/Screenshot : Tasks 4 et 5.
        _ => Err("commande non implémentée".into()),
    }
}
```

- [ ] **Step 4 : Lancer le test**

Run: `cargo test -p wimux-server launch_status_close_cycle -- --nocapture`
Expected: PASS si Chrome/Edge présent (une fenêtre s'ouvre puis se ferme) ; sinon
« ignoré ». **Ne pas** lancer en parallèle d'autres tests navigateur (`--test-threads=1`
recommandé pour les tests d'intégration navigateur).

- [ ] **Step 5 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/src/browser.rs
git commit -m "feat(browser): BrowserEngine — pont sync/async tokio + launch/close/status

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B2.1.4 — Navigation & lecture

### Task 4 : `navigate` + `url`

**Files:**
- Modify: `crates/wimux-server/src/browser.rs`
- Test: `crates/wimux-server/src/browser.rs`

**Interfaces:**
- Consumes: `is_allowed_url` (Task 1), `Session`, `dispatch`.

- [ ] **Step 1 : Écrire le test (échoue)**

Ajouter dans le module de tests. Il sert une page **locale** sur un port éphémère
(aucun accès réseau externe) :

```rust
    /// Sert `html` sur `127.0.0.1:<port libre>` le temps du test ; renvoie l'URL.
    fn servir_page_locale(html: &'static str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            // Sert la même page à la première connexion, puis s'arrête.
            if let Ok((mut stream, _)) = listener.accept() {
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
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server navigate_ -- --test-threads=1`
Expected: FAIL — `Navigate`/`Url` renvoient encore « non implémentée ».

- [ ] **Step 3 : Implémenter dans `dispatch`**

Remplacer le bras `_ => Err("commande non implémentée".into())` par :

```rust
        BrowserCommand::Navigate(url) => {
            if !is_allowed_url(&url) {
                return Err("URL refusée : http(s) seulement".into());
            }
            // Lancement paresseux.
            if sess.is_none() {
                *sess = Some(launch_session().await?);
            }
            let page = &sess.as_ref().unwrap().page;
            page.goto(url).await.map_err(|e| format!("navigation : {e}"))?;
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
        // Snapshot/Screenshot : Task 5.
        _ => Err("commande non implémentée".into()),
```

- [ ] **Step 4 : Lancer + qualité + commit**

Run: `cargo test -p wimux-server navigate_ -- --test-threads=1`
Expected: PASS (ou ignoré si pas de navigateur).

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/src/browser.rs
git commit -m "feat(browser): navigate (garde URL + attente de chargement) + url

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 5 : `snapshot` + `screenshot`

**Files:**
- Modify: `crates/wimux-server/src/browser.rs`
- Test: `crates/wimux-server/src/browser.rs`

**Interfaces:**
- Consumes: `render_ax_tree`, `AxSnapshotNode` (Task 1) ; CDP `GetFullAxTreeParams` ; `ScreenshotParams`.

- [ ] **Step 1 : Écrire le test (échoue)**

```rust
    #[test]
    fn snapshot_et_screenshot_sur_page_locale() {
        if !navigateur_dispo() {
            eprintln!("aucun navigateur : test snapshot/screenshot ignoré");
            return;
        }
        let (url, _srv) = servir_page_locale(
            "<!doctype html><title>T</title><button>Continuer</button>",
        );
        let engine = BrowserEngine::new();
        engine.exec(BrowserCommand::Navigate(url)).unwrap();

        match engine.exec(BrowserCommand::Snapshot).unwrap() {
            BrowserReply::Text(tree) => {
                assert!(tree.contains("button"), "le bouton doit apparaître : {tree}");
                assert!(tree.contains("Continuer"), "son nom doit apparaître : {tree}");
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
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server snapshot_et_screenshot -- --test-threads=1`
Expected: FAIL — encore « non implémentée ».

- [ ] **Step 3 : Implémenter dans `dispatch`**

Remplacer le bras `_ => Err(...)` par les deux handlers. **Confirmer au build** les
chemins CDP exacts (module `accessibility`, champs de `AxNode`) et `ScreenshotParams` :

```rust
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
            // Mapper les AxNode CDP vers notre type découplé, puis rendre.
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
        // Les autres bras (Launch/Close/Status/Navigate/Url) sont au-dessus.
```

Et ajouter les deux helpers (le mapping isole les particularités `chromiumoxide` ;
le nom de la valeur de propriété AX — `value.value` — est à confirmer au build) :

```rust
/// Convertit un nœud d'accessibilité CDP en notre `AxSnapshotNode` découplé.
fn map_ax_node(n: &chromiumoxide::cdp::browser_protocol::accessibility::AxNode) -> AxSnapshotNode {
    let role = n
        .role
        .as_ref()
        .and_then(|v| v.value.as_ref())
        .map(|v| v.to_string().trim_matches('"').to_string())
        .unwrap_or_default();
    let name = n
        .name
        .as_ref()
        .and_then(|v| v.value.as_ref())
        .map(|v| v.to_string().trim_matches('"').to_string());
    // États pertinents (focusable, disabled, checked…) : extraits des propriétés.
    let states: Vec<String> = n
        .properties
        .iter()
        .flatten()
        .filter_map(|p| {
            let name = format!("{:?}", p.name);
            let val = p.value.value.as_ref().map(|v| v.to_string());
            match val.as_deref() {
                Some("true") => Some(name.to_ascii_lowercase()),
                _ => None,
            }
        })
        .collect();
    AxSnapshotNode {
        node_id: n.node_id.inner().to_string(),
        role,
        name,
        states,
        child_ids: n
            .child_ids
            .iter()
            .flatten()
            .map(|c| c.inner().to_string())
            .collect(),
    }
}

/// Chemin de capture sous `%LOCALAPPDATA%\wimux\screenshots\<compteur>.png`.
/// Numérotation monotone par process (pas d'horloge : évite une dépendance temps
/// et reste déterministe pour les tests).
fn screenshot_path() -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "%LOCALAPPDATA% introuvable".to_string())?;
    let dir = std::path::PathBuf::from(base).join("wimux").join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("création dossier captures : {e}"))?;
    let i = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(dir
        .join(format!("shot-{pid}-{i}.png"))
        .to_string_lossy()
        .into_owned())
}
```

> **Note d'implémentation** : les accès `n.role.value`, `n.properties`, `AxValue`,
> `NodeId::inner()` dépendent de la forme générée par la version de `chromiumoxide`
> retenue. Si un champ diffère, ouvrir la doc générée (`cargo doc -p chromiumoxide
> --open`) et ajuster le **mapping uniquement** — `render_ax_tree` (testé, découplé)
> ne bouge pas.

- [ ] **Step 4 : Lancer + qualité + commit**

Run: `cargo test -p wimux-server snapshot_et_screenshot -- --test-threads=1`
Expected: PASS (ou ignoré).

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/src/browser.rs
git commit -m "feat(browser): snapshot (arbre d'accessibilite) + screenshot (PNG -> fichier)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B2.1.5 — Daemon & CLI

### Task 6 : `Server` possède le moteur + handlers

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/src/daemon.rs`

**Interfaces:**
- Consumes: `BrowserEngine`, `BrowserCommand`, `BrowserReply`.

- [ ] **Step 1 : Écrire le test (échoue)**

Dans le module de tests de `daemon.rs` (pur, sans navigateur : vérifie que le statut
répond « non lancé » sans effet de bord) :

```rust
#[test]
fn browser_status_sans_lancement() {
    let server = Server::new();
    match server.browser.exec(crate::browser::BrowserCommand::Status).unwrap() {
        crate::browser::BrowserReply::Status { running, .. } => assert!(!running),
        _ => panic!("Status attendu"),
    }
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server browser_status_sans_lancement`
Expected: FAIL — `Server` n'a pas de champ `browser`.

- [ ] **Step 3 : Ajouter le champ `browser` à `Server`**

Dans `struct Server`, ajouter :

```rust
    /// Moteur navigateur pilotable (B2.1), unique au daemon, lancé paresseusement.
    browser: crate::browser::BrowserEngine,
```

Et dans `Server::with_config` (le constructeur), initialiser :

```rust
            browser: crate::browser::BrowserEngine::new(),
```

- [ ] **Step 4 : Remplacer le bras intérimaire par les handlers**

Supprimer le bras intérimaire B2.1 (celui de Task 2) et mettre à la place. Chaque
handler traduit `BrowserReply` → `ServerMessage`. Un helper local factorise :

```rust
            ClientMessage::BrowserLaunch => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Launch));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserClose => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Close));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserStatus => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Status));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserNavigate { url } => {
                let reply = browser_reply(
                    server.browser.exec(crate::browser::BrowserCommand::Navigate(url)),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserUrl => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Url));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserSnapshot => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Snapshot));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserScreenshot => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Screenshot));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

Ajouter le helper libre (à côté des autres fonctions libres de `daemon.rs`) :

```rust
/// Traduit une réponse du moteur navigateur (B2.1) en message serveur.
fn browser_reply(res: Result<crate::browser::BrowserReply, String>) -> ServerMessage {
    use crate::browser::BrowserReply;
    match res {
        Ok(BrowserReply::Ok) => ServerMessage::Ok,
        Ok(BrowserReply::Status { running, url }) => ServerMessage::BrowserState { running, url },
        Ok(BrowserReply::Text(t)) => ServerMessage::BrowserText(t),
        Ok(BrowserReply::Shot(path)) => ServerMessage::BrowserShot { path },
        Err(e) => ServerMessage::Error(e),
    }
}
```

- [ ] **Step 5 : Lancer + qualité + rebuild + redémarrage daemon**

Run: `cargo test -p wimux-server && cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings && cargo build --release`
Expected: vert. Puis `./target/release/wimux.exe kill-server`.

- [ ] **Step 6 : Commit**

```bash
git add crates/wimux-server/src/daemon.rs
git commit -m "feat(browser): Server possede le BrowserEngine + handlers B2.1

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 7 : CLI `wimux browser` (verbes B2.1)

**Files:**
- Modify: `crates/wimux-cli/src/main.rs`
- Test: `crates/wimux-cli/src/main.rs`

**Interfaces:**
- Consumes: `connected()`, `send`/`recv`, les messages B2.1.

- [ ] **Step 1 : Écrire le test de parsing (échoue)**

Le namespace `wimux browser` existe déjà (B1 : `open`). On ajoute un parseur pour le
drapeau `--url` de `navigate`. Ajouter dans les tests :

```rust
#[cfg(test)]
mod browser_engine_tests {
    use super::browser::parse_url_flag;

    #[test]
    fn parse_url_flag_lit_l_url() {
        assert_eq!(
            parse_url_flag(&["--url".into(), "http://x/".into()]).unwrap(),
            "http://x/"
        );
        assert!(parse_url_flag(&[]).is_err());
    }
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-cli parse_url_flag_lit_l_url`
Expected: FAIL — `parse_url_flag` absent.

- [ ] **Step 3 : Ajouter le parseur et les commandes**

Dans le module `browser` existant de `main.rs`, ajouter :

```rust
    /// Lit `--url <url>` (pour `navigate`).
    pub fn parse_url_flag(args: &[String]) -> io::Result<String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--url" {
                return args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| io::Error::other("--url attend une valeur"));
            }
            i += 1;
        }
        Err(io::Error::other("usage : wimux browser navigate --url <url>"))
    }
```

Étendre le dispatch `cmd_browser` (qui gère déjà `open`) :

```rust
fn cmd_browser(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("open") => browser_open(&args[1..]),          // B1 : volet iframe
        Some("launch") => browser_simple(ClientMessage::BrowserLaunch),
        Some("close") => browser_simple(ClientMessage::BrowserClose),
        Some("status") => browser_status(),
        Some("navigate") => browser_navigate(&args[1..]),
        Some("url") => browser_text(ClientMessage::BrowserUrl),
        Some("snapshot") => browser_text(ClientMessage::BrowserSnapshot),
        Some("screenshot") => browser_screenshot(),
        _ => Err(io::Error::other(
            "usage : wimux browser <open|launch|close|status|navigate|url|snapshot|screenshot> …",
        )),
    }
}

/// Envoie un message et attend `Ok`/`Error`.
fn browser_simple(msg: ClientMessage) -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &msg)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn browser_status() -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::BrowserStatus)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BrowserState { running, url } => {
            let u = url
                .map(|u| format!("\"{}\"", agent::json_escape(&u)))
                .unwrap_or_else(|| "null".into());
            println!("{{\"running\":{running},\"url\":{u}}}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn browser_navigate(args: &[String]) -> io::Result<()> {
    let url = browser::parse_url_flag(args)?;
    browser_text(ClientMessage::BrowserNavigate { url })
}

/// Envoie un message et imprime la réponse texte (url / navigate / snapshot).
fn browser_text(msg: ClientMessage) -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &msg)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BrowserText(t) => {
            println!("{t}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn browser_screenshot() -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::BrowserScreenshot)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BrowserShot { path } => {
            println!("{{\"path\":\"{}\"}}", agent::json_escape(&path));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}
```

- [ ] **Step 4 : Aide**

Dans `print_help`, remplacer/compléter la ligne `browser` :

```
             browser <sous-cmd>  Navigateur : open (volet) | launch/close/status/navigate/url/snapshot/screenshot (pilotable)\n    \
```

- [ ] **Step 5 : Lancer, qualité, test manuel**

Run: `cargo test -p wimux-cli && cargo clippy -p wimux-cli --all-targets -- -D warnings && cargo fmt -p wimux-cli`
Expected: vert.

Test manuel (rebuild release + redémarrer daemon) :
```bash
wimux browser launch
wimux browser navigate --url http://localhost:8899/
wimux browser url
wimux browser snapshot
wimux browser screenshot
wimux browser status
wimux browser close
```
Expected : une fenêtre Chrome/Edge s'ouvre, va sur la page, `snapshot` montre l'arbre
d'accessibilité, `screenshot` renvoie un chemin de PNG lisible, `close` ferme la fenêtre.

- [ ] **Step 6 : Commit**

```bash
git add crates/wimux-cli/src/main.rs
git commit -m "feat(browser): CLI wimux browser launch/close/status/navigate/url/snapshot/screenshot

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Revue finale (toute la branche)

- [ ] **Step 1 : Suite complète + qualité**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cd wimux-gui/src-tauri && cargo clippy --all-targets -- -D warnings && cargo fmt -- --check
cd wimux-gui && npm run build
```
Expected: tout vert. (Les tests navigateur s'ignorent proprement sans binaire.)

- [ ] **Step 2 : Rebuild release + redémarrage daemon + démo**

```bash
cargo build --release
./target/release/wimux.exe kill-server
```
Puis la démo bout-en-bout de Task 7, sur une page locale servie (ex. `python -m http.server`).

- [ ] **Step 3 : Mémoire projet** — consigner B2.1 fait dans `wimux-etat-avancement`.

---

## Notes de conception (rappels)

- **Le daemon reste synchrone** : `BrowserEngine::exec` bloque via `blocking_send`/
  `blocking_recv` ; seul le thread `wimux-browser` connaît tokio. `blocking_*` ne
  paniquent que si appelés DANS un contexte async — les handlers du daemon sont
  synchrones, donc c'est sûr.
- **`render_ax_tree` est découplé de `chromiumoxide`** : la seule couche à ajuster si
  l'API générée diffère est `map_ax_node`. La logique testée ne bouge pas.
- **Aucun JS de page en B2.1** : `snapshot` passe par `Accessibility.getFullAxTree`
  (CDP natif), pas par `Runtime.evaluate`. `eval` arrive en B2.3, sous garde.
- **Tests navigateur conditionnels et sériés** (`--test-threads=1` recommandé) :
  ouvrir plusieurs fenêtres en parallèle fragiliserait les assertions.
- **Deux « browser » dans la CLI** : `wimux browser open` = volet iframe de B1 (dans
  la disposition) ; les autres verbes = le navigateur pilotable CDP (fenêtre séparée).
  Le skill de B2.4 clarifiera l'usage.
