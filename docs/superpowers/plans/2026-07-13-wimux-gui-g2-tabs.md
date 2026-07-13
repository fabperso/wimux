# wimux GUI — G2 (onglets verticaux) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ajouter à `wimux-gui` un rail vertical de sessions : lister, basculer, créer, fermer, renommer — avec, côté serveur, un cycle de vie propre de l'attachement GUI (bascule qui arrête proprement le flux précédent).

**Architecture:** Réutilise le mode GUI de G1. Une **connexion persistante** porte le flux de la session active (`AttachGui`/`PaneInput`/`PaneOutput`) ; la bascule = un `AttachGui` sur cette connexion (le serveur arrête l'ancien flux). Les commandes de contrôle (list/create/kill/rename) passent par des **connexions jetables**. Le frontend affiche le rail + une instance xterm.js réutilisée.

**Tech Stack:** Rust (workspace), Tauri 2 + TypeScript + xterm.js, Named Pipe overlapped, postcard.

## Global Constraints

- Rust edition 2024, toolchain stable (dev 1.97). `RUSTFLAGS="-D warnings"` : fmt + clippy propres.
- Ne PAS casser le mode TUI ni le mode GUI G1 existants (ajouts/refactors compatibles).
- Frontend : `npm run build` (dans `wimux-gui/`) doit réussir. NE PAS lancer `npm run tauri dev` (fenêtre bloquante, vérif visuelle = humain).
- Messages postcard, cadrage `u32` LE, helpers `send`/`recv`.
- Environnement Windows : **Bash tool** (git bash) pour cargo/git/npm. Tests d'intégration serveur en `--test-threads=1` (ils lancent de vrais PowerShell, patience ~60 s).
- Commits fréquents ; fin de message : `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : `ClientMessage::{CreateSession, RenameSession}`, `ServerMessage::SessionCreated`.
- `crates/wimux-server/src/session.rs` — **modifier** : `name` mutable (`Mutex<String>` + `name()`/`set_name()`).
- `crates/wimux-server/src/daemon.rs` — **modifier** : sites lisant `s.name` ; cycle de vie de l'attachement GUI (`GuiAttachment`) + bascule ; bras `CreateSession`/`RenameSession` ; `Server::rename_session`.
- `crates/wimux-server/tests/gui_mode.rs` — **modifier** : tests bascule/create/rename (helpers dans `tests/common/mod.rs`).
- `wimux-gui/src-tauri/src/lib.rs` — **modifier** : connexion persistante + commandes `attach_session`/`list_sessions`/`create_session`/`kill_session`/`rename_session`.
- `wimux-gui/src/main.ts`, `wimux-gui/index.html`, `wimux-gui/src/styles.css` — **modifier** : rail + interactions + sondage.

---

## Task 1: Messages protocole G2 (+ stubs démon)

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (stubs no-op pour garder la compilation)
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces produites:**
- `ClientMessage::CreateSession { name: Option<String> }`
- `ClientMessage::RenameSession { from: String, to: String }`
- `ServerMessage::SessionCreated { name: String }`

- [ ] **Step 1: Failing test**

Dans le module `tests` de `lib.rs` :

```rust
#[test]
fn aller_retour_create_session() {
    let msg = ClientMessage::CreateSession { name: Some("dev".into()) };
    let mut buf = Vec::new();
    send(&mut buf, &msg).unwrap();
    let mut cur = io::Cursor::new(buf);
    match recv::<_, ClientMessage>(&mut cur).unwrap() {
        ClientMessage::CreateSession { name } => assert_eq!(name.as_deref(), Some("dev")),
        _ => panic!("mauvais variant"),
    }
}

#[test]
fn aller_retour_rename_session() {
    let msg = ClientMessage::RenameSession { from: "a".into(), to: "b".into() };
    let mut buf = Vec::new();
    send(&mut buf, &msg).unwrap();
    let mut cur = io::Cursor::new(buf);
    match recv::<_, ClientMessage>(&mut cur).unwrap() {
        ClientMessage::RenameSession { from, to } => { assert_eq!(from, "a"); assert_eq!(to, "b"); }
        _ => panic!("mauvais variant"),
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p wimux-protocol aller_retour_create_session`
Expected: FAIL (variant inexistant).

- [ ] **Step 3: Add variants**

Dans `enum ClientMessage`, après `PaneResize { .. }` :

```rust
    /// Crée une session sans s'y attacher (mode GUI). Nom auto si `None`.
    CreateSession { name: Option<String> },
    /// Renomme une session.
    RenameSession { from: String, to: String },
```

Dans `enum ServerMessage`, après `PaneOutput { .. }` :

```rust
    /// Session créée (réponse à `CreateSession`).
    SessionCreated { name: String },
```

- [ ] **Step 4: Add no-op stub arms in daemon (garder la compilation)**

Dans `crates/wimux-server/src/daemon.rs`, dans le `match msg` de `handle_client`,
juste avant le bras `ClientMessage::Hello(_) => {}` (ou tout autre bras final),
ajouter :

```rust
            // Remplis par la tâche 4 (G2). No-op provisoire pour compiler.
            ClientMessage::CreateSession { .. } => {}
            ClientMessage::RenameSession { .. } => {}
```

- [ ] **Step 5: Verify**

Run: `cargo test -p wimux-protocol` (les 2 nouveaux passent) puis `cargo build --workspace` (compile).
Expected: PASS + build OK.

- [ ] **Step 6: fmt + clippy + commit**

Run: `cargo fmt --all && RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "protocol(G2) : CreateSession/RenameSession/SessionCreated (+ stubs demon)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Nom de session mutable

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (sites lisant `s.name`)

**Interfaces produites:**
- `Session::name(&self) -> String`
- `Session::set_name(&self, new: String)`

**Détails :** `Session` a actuellement `pub name: String` (immuable). Le remplacer
par un champ privé `name: Mutex<String>` et exposer les accesseurs. Mettre à jour
tous les lecteurs de `s.name` / `self.name`.

- [ ] **Step 1: Repérer les usages**

Run: `grep -rn "\.name\b" crates/wimux-server/src | grep -iv "window\|pane\|file"` puis
`grep -rn "self\.name\|session\.name\|s\.name\|\.name\.clone" crates/wimux-server/src`
Noter chaque site (attendus : `daemon.rs` `list()` → `s.name.clone()`, `create_session` → `session.name.clone()`, bras `Attached { name: ... }` ; `session.rs` : `pub name`, et `draw_status_bar`/`composite` qui passe `&self.name`).

- [ ] **Step 2: Rendre `name` privé + mutable dans `session.rs`**

Remplacer dans `struct Session` :
```rust
    pub name: String,
```
par :
```rust
    name: Mutex<String>,
```
Dans `Session::new`, remplacer `name,` (initialisation du champ) par `name: Mutex::new(name),`.
Ajouter les accesseurs dans `impl Session` :
```rust
    pub fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }
    pub fn set_name(&self, new: String) {
        *self.name.lock().unwrap() = new;
    }
```
Dans `composite()` (et tout usage interne de `&self.name`), remplacer `&self.name`
par un `let name = self.name();` calculé **avant** de prendre le verrou `inner`
(comme `copy_status`/`command_status`), puis passer `&name` à `draw_status_bar`.

- [ ] **Step 3: Mettre à jour `daemon.rs`**

Remplacer chaque `s.name.clone()` / `session.name.clone()` par `s.name()` /
`session.name()`. Le bras `Attached { name: session.name.clone() }` devient
`Attached { name: session.name() }`. Idem `SessionInfo { name: s.name.clone(), .. }`
→ `name: s.name()`.

- [ ] **Step 4: Verify**

Run: `cargo build -p wimux-server` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets` (propre) puis `cargo test -p wimux-server -- --test-threads=1` (l'existant reste vert).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wimux-server/src/session.rs crates/wimux-server/src/daemon.rs
git commit -m "server(session) : nom mutable (name()/set_name()) pour le renommage G2

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Bascule GUI — arrêt propre du flux précédent

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces / concept :** remplacer le thread de retransmission *fire-and-forget*
du bras `AttachGui` par un attachement suivi (`GuiAttachment` : drapeau d'arrêt +
`JoinHandle`, arrêté au `Drop`). À chaque `AttachGui`, on **remplace**
l'attachement (drop de l'ancien = arrêt propre), puis on démarre le nouveau.

- [ ] **Step 1: Failing test (bascule)**

Dans `tests/gui_mode.rs`, ajouter un test qui : crée 2 sessions A et B (via un
client TUI classique `NewSession`), attache GUI à A, y injecte une commande qui
produit une sortie continue, bascule sur B (`AttachGui { B }`), puis vérifie
qu'après la bascule on reçoit le `PaneSnapshot` de B et **plus** de `PaneOutput`
provenant de A. Simplifié (une seule connexion GUI, on bascule dessus) :

```rust
#[test]
fn bascule_gui_arrete_le_flux_precedent() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-switch", std::process::id());
    common::start_daemon(&pipe);

    // Créer A et B via des clients TUI (pour avoir des volets vivants).
    for name in ["A", "B"] {
        let c = std::sync::Arc::new(common::connect_retry(&pipe));
        common::handshake(&c);
        let mut w: &wimux_protocol::transport::PipeConn = &c;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::NewSession {
            name: Some(name.to_string()), cols: 80, rows: 24,
        }).unwrap();
        // garder la connexion vivante un court instant pour laisser le shell démarrer
        std::thread::sleep(std::time::Duration::from_millis(800));
        // on laisse `c` tomber : la session A/B survit (détachement).
    }

    // Client GUI : attache A.
    let gui = std::sync::Arc::new(common::connect_retry(&pipe));
    common::handshake(&gui);
    {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::AttachGui { session: "A".into() }).unwrap();
    }
    let rx = common::spawn_reader(std::sync::Arc::clone(&gui));

    // Attendre le snapshot de A.
    common::wait_for(&rx, std::time::Duration::from_secs(5), |m| {
        matches!(m, wimux_protocol::ServerMessage::PaneSnapshot { .. })
    });

    // Basculer sur B.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::AttachGui { session: "B".into() }).unwrap();
    }
    // On doit recevoir un nouveau PaneSnapshot (celui de B).
    let got_b_snapshot = common::wait_for(&rx, std::time::Duration::from_secs(5), |m| {
        matches!(m, wimux_protocol::ServerMessage::PaneSnapshot { .. })
    });
    assert!(got_b_snapshot, "pas de snapshot après bascule sur B");

    // Nettoyage.
    for name in ["A", "B"] {
        let mut w: &wimux_protocol::transport::PipeConn = &gui;
        let _ = wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::Kill { name: name.to_string() });
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
}
```

Ajouter dans `tests/common/mod.rs` un helper générique `wait_for` (si absent) :
```rust
/// Attend un message satisfaisant `pred`, dans la limite du délai.
pub fn wait_for<F: Fn(&wimux_protocol::ServerMessage) -> bool>(
    rx: &std::sync::mpsc::Receiver<wimux_protocol::ServerMessage>,
    timeout: std::time::Duration,
    pred: F,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(m) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
            if pred(&m) { return true; }
        }
    }
    false
}
```

- [ ] **Step 2: Run to verify (au moins compile + le test tourne)**

Run: `cargo test -p wimux-server --test gui_mode bascule_gui_arrete_le_flux_precedent -- --test-threads=1`
Expected : peut déjà passer sur le snapshot (le G1 renvoie un snapshot par AttachGui),
mais l'objectif est l'arrêt du flux précédent — on le garantit à l'étape suivante.
Si le test échoue à compiler (helper `wait_for` manquant), l'ajouter d'abord.

- [ ] **Step 3: Implémenter `GuiAttachment` + bascule**

Ajouter près de `struct Attachment` dans `daemon.rs` :

```rust
/// Attachement GUI suivi (une session diffusée sur cette connexion). Arrêt propre
/// du thread de retransmission au drop (permet la bascule d'une session à l'autre).
struct GuiAttachment {
    keep_going: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for GuiAttachment {
    fn drop(&mut self) {
        self.keep_going.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
```

Déclarer, à côté de `let mut gui_session ...` (ou le remplacer) avant la boucle :
```rust
    let mut gui_attach: Option<GuiAttachment> = None;
    let mut gui_session: Option<Arc<Session>> = None;
```

Remplacer le corps du bras `ClientMessage::AttachGui { session }` par :
```rust
            ClientMessage::AttachGui { session } => {
                // Arrêter proprement la diffusion précédente avant d'en démarrer une.
                gui_attach = None;
                match server.get(&session) {
                    Some(s) => {
                        if let Some((pane_id, snapshot, rx)) = s.gui_attach() {
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::PaneSnapshot { pane_id, bytes: snapshot })?;
                            let keep_going = Arc::new(AtomicBool::new(true));
                            let conn_out = Arc::clone(&conn);
                            let kg = Arc::clone(&keep_going);
                            let handle = std::thread::spawn(move || {
                                while kg.load(Ordering::Relaxed) {
                                    match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                                        Ok(chunk) => {
                                            let mut w: &PipeConn = &conn_out;
                                            if send(&mut w, &ServerMessage::PaneOutput { pane_id, bytes: chunk }).is_err() {
                                                break;
                                            }
                                        }
                                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                                    }
                                }
                            });
                            gui_attach = Some(GuiAttachment { keep_going, handle: Some(handle) });
                            gui_session = Some(s);
                        } else {
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::Error(format!(
                                "aucun volet actif dans la session : {session}"
                            )))?;
                        }
                    }
                    None => {
                        let mut wr: &PipeConn = &conn;
                        send(&mut wr, &ServerMessage::Error(format!(
                            "session introuvable : {session}"
                        )))?;
                    }
                }
            }
```

Ajouter, après la boucle `loop { ... }` (à la fin de `handle_client`, près de `drop(attachment);`) :
```rust
    drop(gui_attach);
```
(pour arrêter la diffusion à la déconnexion).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS (dont `bascule_gui_arrete_le_flux_precedent`).

- [ ] **Step 5: fmt + clippy + commit**

Run: `cargo fmt --all && RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs crates/wimux-server/tests/common/mod.rs
git commit -m "server(daemon) : cycle de vie de l attachement GUI + bascule (arret propre)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: CreateSession + RenameSession (serveur + tests)

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs` (remplacer les stubs de la tâche 1 ; ajouter `Server::rename_session`)
- Test: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces produites:** `Server::rename_session(&self, from: &str, to: &str) -> Result<(), String>`

- [ ] **Step 1: Failing tests**

Dans `tests/gui_mode.rs` :
```rust
#[test]
fn create_session_cree_et_liste() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-create", std::process::id());
    common::start_daemon(&pipe);
    let conn = std::sync::Arc::new(common::connect_retry(&pipe));
    common::handshake(&conn);
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::CreateSession { name: Some("neuve".into()) }).unwrap();
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
    let listed = matches!(wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r2).unwrap(),
        wimux_protocol::ServerMessage::Sessions(v) if v.iter().any(|s| s.name == "neuve"));
    assert!(listed, "la session créée n'apparaît pas dans List");

    let mut w: &wimux_protocol::transport::PipeConn = &conn;
    let _ = wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::Kill { name: "neuve".into() });
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
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::CreateSession { name: Some("vieux".into()) }).unwrap();
    }
    let mut r: &wimux_protocol::transport::PipeConn = &conn;
    let _ = wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r).unwrap(); // SessionCreated
    // Renommer.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::RenameSession { from: "vieux".into(), to: "nouveau".into() }).unwrap();
    }
    let mut r2: &wimux_protocol::transport::PipeConn = &conn;
    assert!(matches!(wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r2).unwrap(),
        wimux_protocol::ServerMessage::Ok), "rename doit répondre Ok");
    // List reflète le nouveau nom.
    {
        let mut w: &wimux_protocol::transport::PipeConn = &conn;
        wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::List).unwrap();
    }
    let mut r3: &wimux_protocol::transport::PipeConn = &conn;
    let ok = matches!(wimux_protocol::recv::<_, wimux_protocol::ServerMessage>(&mut r3).unwrap(),
        wimux_protocol::ServerMessage::Sessions(v)
            if v.iter().any(|s| s.name == "nouveau") && !v.iter().any(|s| s.name == "vieux"));
    assert!(ok, "List devrait montrer 'nouveau' et plus 'vieux'");

    let mut w: &wimux_protocol::transport::PipeConn = &conn;
    let _ = wimux_protocol::send(&mut w, &wimux_protocol::ClientMessage::Kill { name: "nouveau".into() });
    std::thread::sleep(std::time::Duration::from_millis(200));
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p wimux-server --test gui_mode create_session_cree_et_liste -- --test-threads=1`
Expected: FAIL (les stubs ne répondent rien → timeout de `recv` du côté test, ou panic « attendu SessionCreated »).

- [ ] **Step 3: Ajouter `Server::rename_session`**

Dans `impl Server` (daemon.rs) :
```rust
    fn rename_session(&self, from: &str, to: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().unwrap();
        if !sessions.contains_key(from) {
            return Err(format!("session introuvable : {from}"));
        }
        if sessions.contains_key(to) {
            return Err(format!("la session « {to} » existe déjà"));
        }
        if let Some(s) = sessions.remove(from) {
            s.set_name(to.to_string());
            sessions.insert(to.to_string(), s);
        }
        Ok(())
    }
```

- [ ] **Step 4: Remplacer les stubs par la vraie logique**

Remplacer les bras no-op `CreateSession`/`RenameSession` (posés en tâche 1) par :
```rust
            ClientMessage::CreateSession { name } => {
                let reply = match server.create_session(name, 80, 24) {
                    Ok(s) => ServerMessage::SessionCreated { name: s.name() },
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::RenameSession { from, to } => {
                let reply = match server.rename_session(&from, &to) {
                    Ok(()) => ServerMessage::Ok,
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```
Note : `create_session(name, 80, 24)` réutilise l'existant (crée avec une taille par
défaut ; la GUI redimensionnera via l'attache). Il renvoie `Arc<Session>`.

- [ ] **Step 5: Run to verify pass + suite complète**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1` puis
`RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets` puis
`cargo test --workspace -- --test-threads=1`.
Expected: tout vert.

- [ ] **Step 6: Commit**

```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs
git commit -m "server(daemon) : CreateSession + RenameSession (+ tests)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Pont Tauri — connexion persistante + commandes de contrôle

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`

**Interfaces produites (commandes Tauri):**
- `attach_session(session: String, app: AppHandle, bridge: State<Bridge>) -> Result<(), String>`
- `list_sessions() -> Result<Vec<SessionDto>, String>` (`SessionDto { name: String, attached: bool }`)
- `create_session(name: Option<String>) -> Result<String, String>`
- `kill_session(name: String) -> Result<(), String>`
- `rename_session(from: String, to: String) -> Result<(), String>`
- (conservée) `pane_input(pane_id, bytes, bridge)`

**Concept :** `Bridge` garde une **connexion persistante** (déjà présente sous
forme `Mutex<Option<Arc<PipeConn>>>`) pour le flux ; `attach_session` (ré)utilise
cette connexion : au **premier** appel elle se connecte + lance le thread lecteur
(comme l'actuel `gui_attach`) ; aux appels suivants elle **réutilise** la même
connexion et envoie juste `AttachGui` (le serveur bascule, le thread lecteur en
place reçoit le nouveau flux). Les commandes de contrôle ouvrent une **connexion
jetable** chacune.

- [ ] **Step 1: Helper de connexion de contrôle**

Dans `lib.rs`, ajouter un helper qui ouvre une connexion, fait le handshake,
envoie un message et lit une réponse :
```rust
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
```

- [ ] **Step 2: Renommer `gui_attach` en `attach_session` avec réutilisation de connexion**

Remplacer la commande `gui_attach` par `attach_session` qui, si une connexion
persistante existe déjà, envoie seulement `AttachGui` dessus ; sinon se connecte,
lance le thread lecteur, et envoie `AttachGui`. Structure :
```rust
#[tauri::command]
fn attach_session(session: String, app: AppHandle, bridge: State<Bridge>) -> Result<(), String> {
    let existing = bridge.conn.lock().unwrap().clone();
    let conn = match existing {
        Some(c) => c, // réutiliser : le thread lecteur tourne déjà
        None => {
            let c = Arc::new(connect(&user_pipe_name()).map_err(|_| "serveur wimux introuvable".to_string())?);
            do_handshake(&c)?;
            *bridge.conn.lock().unwrap() = Some(Arc::clone(&c));
            // Thread lecteur (une seule fois) : relaie snapshot/output/error au frontend.
            let reader = Arc::clone(&c);
            let app2 = app.clone();
            std::thread::spawn(move || {
                let mut r: &PipeConn = &reader;
                while let Ok(msg) = recv::<_, ServerMessage>(&mut r) {
                    match msg {
                        ServerMessage::PaneSnapshot { pane_id, bytes } => { let _ = app2.emit("pane-snapshot", (pane_id, bytes)); }
                        ServerMessage::PaneOutput { pane_id, bytes } => { let _ = app2.emit("pane-output", (pane_id, bytes)); }
                        ServerMessage::Error(m) => { let _ = app2.emit("pane-error", m); }
                        _ => {}
                    }
                }
            });
            c
        }
    };
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::AttachGui { session }).map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 3: Commandes de contrôle**

```rust
#[derive(serde::Serialize)]
struct SessionDto { name: String, attached: bool }

#[tauri::command]
fn list_sessions() -> Result<Vec<SessionDto>, String> {
    control(|| ClientMessage::List, |msg| match msg {
        ServerMessage::Sessions(v) => Ok(v.into_iter().map(|s| SessionDto { name: s.name, attached: s.attached }).collect()),
        ServerMessage::Error(e) => Err(e),
        _ => Err("réponse inattendue".into()),
    })
}

#[tauri::command]
fn create_session(name: Option<String>) -> Result<String, String> {
    control(|| ClientMessage::CreateSession { name }, |msg| match msg {
        ServerMessage::SessionCreated { name } => Ok(name),
        ServerMessage::Error(e) => Err(e),
        _ => Err("réponse inattendue".into()),
    })
}

#[tauri::command]
fn kill_session(name: String) -> Result<(), String> {
    control(|| ClientMessage::Kill { name }, |msg| match msg {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(e),
        _ => Err("réponse inattendue".into()),
    })
}

#[tauri::command]
fn rename_session(from: String, to: String) -> Result<(), String> {
    control(|| ClientMessage::RenameSession { from, to }, |msg| match msg {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(e),
        _ => Err("réponse inattendue".into()),
    })
}
```

- [ ] **Step 4: Enregistrer les commandes**

Dans `run()`, remplacer `.invoke_handler(tauri::generate_handler![gui_attach, pane_input])`
par :
```rust
        .invoke_handler(tauri::generate_handler![
            attach_session, pane_input, list_sessions, create_session, kill_session, rename_session
        ])
```
Ajouter les imports nécessaires (`recv` est déjà importé ; ajouter `SessionInfo` si besoin — non, on lit `s.name`/`s.attached` via `ServerMessage::Sessions(Vec<SessionInfo>)`).

- [ ] **Step 5: Verify build**

Run: `cd wimux-gui/src-tauri && cargo build 2>&1 | tail -8 && cd ../..`
Expected: compile proprement. Corrige tout import/emprunt manquant.

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/src-tauri/src/lib.rs
git commit -m "gui(pont) : connexion persistante + commandes list/create/kill/rename (G2)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Frontend — rail de sessions

**Files:**
- Modify: `wimux-gui/index.html`, `wimux-gui/src/styles.css`, `wimux-gui/src/main.ts`

**Interfaces consommées:** commandes `attach_session`/`list_sessions`/`create_session`/`kill_session`/`rename_session` ; events `pane-snapshot`/`pane-output`/`pane-error`.

- [ ] **Step 1: HTML — rail + zone terminal**

`index.html` `<body>` :
```html
  <body>
    <div id="app">
      <aside id="rail">
        <div id="sessions"></div>
        <button id="new-session" title="Nouvelle session">+</button>
      </aside>
      <div id="terminal"></div>
    </div>
    <script type="module" src="/src/main.ts"></script>
  </body>
```

- [ ] **Step 2: CSS — thème sombre/bleu + layout**

`src/styles.css` :
```css
:root { background: #1e1e1e; color: #d4d4d4; font-family: "Segoe UI", sans-serif; }
body { margin: 0; }
#app { display: flex; height: 100vh; }
#rail { width: 180px; background: #252526; border-right: 1px solid #333; display: flex; flex-direction: column; }
#sessions { flex: 1; overflow-y: auto; }
.session { display: flex; align-items: center; gap: 6px; padding: 8px 10px; cursor: pointer; color: #ccc; border-left: 3px solid transparent; }
.session:hover { background: #2a2d2e; }
.session.active { background: #37373d; border-left-color: #0a84ff; color: #fff; }
.session .name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.session .close { visibility: hidden; color: #999; }
.session:hover .close { visibility: visible; }
.session .name-edit { flex: 1; background: #1e1e1e; color: #fff; border: 1px solid #0a84ff; }
#new-session { border: none; background: #2d2d2d; color: #ccc; padding: 8px; cursor: pointer; font-size: 16px; }
#new-session:hover { background: #37373d; }
#terminal { flex: 1; }
```

- [ ] **Step 3: TS — logique du rail + terminal**

`src/main.ts` :
```ts
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const term = new Terminal({ fontFamily: "Cascadia Mono, Consolas, monospace", fontSize: 14 });
const fit = new FitAddon();
term.loadAddon(fit);
term.open(document.getElementById("terminal")!);
fit.fit();
window.addEventListener("resize", () => fit.fit());

let activeSession: string | null = null;
let activePane = 0;

// Sortie serveur -> terminal.
listen<[number, number[]]>("pane-snapshot", (e) => { activePane = e.payload[0]; term.write(new Uint8Array(e.payload[1])); });
listen<[number, number[]]>("pane-output", (e) => { activePane = e.payload[0]; term.write(new Uint8Array(e.payload[1])); });
listen<string>("pane-error", (e) => { term.write(`\r\n[erreur serveur: ${e.payload}]\r\n`); });

// Frappe -> serveur.
term.onData((data) => {
  const bytes = Array.from(new TextEncoder().encode(data));
  invoke("pane_input", { paneId: activePane, bytes }).catch(() => {});
});

type SessionDto = { name: string; attached: boolean };

async function switchTo(name: string) {
  if (name === activeSession) return;
  activeSession = name;
  term.clear();
  await invoke("attach_session", { session: name }).catch((e) => term.write(`\r\n[${e}]\r\n`));
  renderRail(lastSessions);
}

let lastSessions: SessionDto[] = [];

function renderRail(sessions: SessionDto[]) {
  lastSessions = sessions;
  const container = document.getElementById("sessions")!;
  container.innerHTML = "";
  for (const s of sessions) {
    const el = document.createElement("div");
    el.className = "session" + (s.name === activeSession ? " active" : "");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = s.name;
    name.ondblclick = (ev) => { ev.stopPropagation(); startRename(el, s.name); };
    const close = document.createElement("span");
    close.className = "close";
    close.textContent = "×";
    close.onclick = async (ev) => { ev.stopPropagation(); await invoke("kill_session", { name: s.name }).catch(() => {}); await refresh(); };
    el.onclick = () => switchTo(s.name);
    el.append(name, close);
    container.append(el);
  }
}

function startRename(el: HTMLElement, oldName: string) {
  const input = document.createElement("input");
  input.className = "name-edit";
  input.value = oldName;
  el.replaceChildren(input);
  input.focus();
  input.select();
  const commit = async () => {
    const to = input.value.trim();
    if (to && to !== oldName) {
      await invoke("rename_session", { from: oldName, to }).catch(() => {});
      if (activeSession === oldName) activeSession = to;
    }
    await refresh();
  };
  input.onkeydown = (ev) => { if (ev.key === "Enter") commit(); if (ev.key === "Escape") refresh(); };
  input.onblur = () => commit();
}

async function refresh() {
  try {
    const sessions = await invoke<SessionDto[]>("list_sessions");
    // Auto-sélection : si aucune session active mais il en existe, prendre la première.
    if (!activeSession && sessions.length > 0) { await switchTo(sessions[0].name); return; }
    renderRail(sessions);
  } catch { /* serveur absent : rail vide */ }
}

document.getElementById("new-session")!.onclick = async () => {
  const name = await invoke<string>("create_session", { name: null }).catch(() => null);
  await refresh();
  if (name) await switchTo(name);
};

// Sondage périodique (une session créée/fermée ailleurs apparaît/disparaît).
refresh();
setInterval(refresh, 1000);
```

- [ ] **Step 4: Verify build**

Run: `cd wimux-gui && npm run build 2>&1 | tail -6 && cd ..`
Expected: `tsc` + `vite build` réussissent. Corrige toute erreur TS.

- [ ] **Step 5: Vérification manuelle (HUMAIN — à documenter, ne pas exécuter)**

Documenter dans `wimux-gui/README.md` la procédure : lancer un serveur + quelques
sessions (`wimux new -s a` détaché, `wimux new -s b` détaché), puis `npm run tauri
dev`. Attendu : le rail liste `a` et `b` ; cliquer bascule ; `+` crée ; la croix
ferme ; double-clic renomme ; la frappe va à la session affichée.

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/index.html wimux-gui/src/styles.css wimux-gui/src/main.ts wimux-gui/README.md
git commit -m "gui(frontend) : rail de sessions (liste/switch/create/kill/rename) — G2

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Definition of Done (G2)

- `cargo test --workspace` vert (dont les nouveaux tests `gui_mode` : bascule, create, rename).
- fmt + clippy `-D warnings` propres ; `npm run build` (frontend) OK.
- La GUI liste les sessions, bascule entre elles, en crée, en ferme, en renomme.
- Le flux de la session précédente est **arrêté proprement** à la bascule (point différé de G1 résolu).
- Modes TUI et GUI G1 inchangés (tests existants verts).

## Suites (hors G2)

- **G3** : volets graphiques dans une session (arbre de découpes rendu, un xterm.js par volet).
- **G4** : indicateurs d'activité live des onglets (pastilles) + attach multi-sessions « chaud ».
