# wimux GUI — G1 (tuyauterie) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Une fenêtre graphique Tauri affiche **une session / un volet** d'un serveur wimux persistant dans un `xterm.js`, avec la frappe clavier qui fonctionne — prouvant toute la chaîne GUI ↔ serveur.

**Architecture:** Le serveur wimux existant reste le moteur. On lui ajoute un **mode GUI** dans le protocole : le client s'abonne au flux brut du volet actif (`PaneOutput`) et reçoit un instantané initial (`PaneSnapshot`), et envoie les frappes (`PaneInput`). Une app Tauri (backend Rust réutilisant `wimux-protocol`, frontend TypeScript + xterm.js) fait le pont.

**Tech Stack:** Rust (workspace existant), Tauri 2, TypeScript + Vite, xterm.js, Named Pipe (transport existant), postcard.

## Global Constraints

- Rust edition 2024, toolchain stable ≥ 1.85 (dev sur 1.97). Cible `x86_64-pc-windows-msvc`.
- `RUSTFLAGS="-D warnings"` : fmt + clippy propres obligatoires.
- Ne PAS modifier le comportement du mode TUI existant : le mode GUI est **ajouté**.
- Messages protocole sérialisés avec `postcard`, cadrage longueur `u32` LE (helpers `send`/`recv` existants).
- Le serveur reste la source de vérité (sessions, volets, VT, persistance).
- Commits fréquents, un par tâche minimum. Fin de message de commit :
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : nouveaux variants `ClientMessage::{AttachGui, PaneInput, PaneResize}` et `ServerMessage::{PaneSnapshot, PaneOutput}`.
- `crates/wimux-server/src/pane.rs` — **modifier** : abonnement au flux brut (`subscribe`) + diffusion dans `reader_loop` + `snapshot_bytes`.
- `crates/wimux-server/src/session.rs` — **modifier** : `gui_attach()` (id du volet actif + snapshot + récepteur de flux) et `gui_input()`.
- `crates/wimux-server/src/daemon.rs` — **modifier** : traiter `AttachGui`/`PaneInput`, thread de retransmission `PaneOutput`.
- `crates/wimux-server/tests/gui_mode.rs` — **créer** : test d'intégration du mode GUI.
- `wimux-gui/` — **créer** : app Tauri (backend `src-tauri/`, frontend `src/`).

---

## Task 1: Messages protocole du mode GUI

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces:**
- Produces:
  - `ClientMessage::AttachGui { session: String }`
  - `ClientMessage::PaneInput { pane_id: u64, bytes: Vec<u8> }`
  - `ClientMessage::PaneResize { pane_id: u64, cols: u16, rows: u16 }`
  - `ServerMessage::PaneSnapshot { pane_id: u64, bytes: Vec<u8> }`
  - `ServerMessage::PaneOutput { pane_id: u64, bytes: Vec<u8> }`

- [ ] **Step 1: Write the failing test**

Dans le module `tests` de `crates/wimux-protocol/src/lib.rs`, ajouter :

```rust
#[test]
fn aller_retour_attach_gui() {
    let msg = ClientMessage::AttachGui { session: "dev".into() };
    let mut buf = Vec::new();
    send(&mut buf, &msg).unwrap();
    let mut cur = io::Cursor::new(buf);
    match recv::<_, ClientMessage>(&mut cur).unwrap() {
        ClientMessage::AttachGui { session } => assert_eq!(session, "dev"),
        _ => panic!("mauvais variant"),
    }
}

#[test]
fn aller_retour_pane_output() {
    let msg = ServerMessage::PaneOutput { pane_id: 7, bytes: b"hello".to_vec() };
    let mut buf = Vec::new();
    send(&mut buf, &msg).unwrap();
    let mut cur = io::Cursor::new(buf);
    match recv::<_, ServerMessage>(&mut cur).unwrap() {
        ServerMessage::PaneOutput { pane_id, bytes } => {
            assert_eq!(pane_id, 7);
            assert_eq!(bytes, b"hello");
        }
        _ => panic!("mauvais variant"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wimux-protocol aller_retour_attach_gui`
Expected: FAIL (compilation : variant `AttachGui` inexistant).

- [ ] **Step 3: Add the message variants**

Dans `enum ClientMessage`, après `Input(Vec<u8>)` :

```rust
    /// S'attacher en mode GUI (flux bruts par volet).
    AttachGui { session: String },
    /// Frappe(s) vers un volet précis (mode GUI).
    PaneInput { pane_id: u64, bytes: Vec<u8> },
    /// Un volet a changé de taille dans la GUI.
    PaneResize { pane_id: u64, cols: u16, rows: u16 },
```

Dans `enum ServerMessage`, après `Detached` :

```rust
    /// Contenu initial d'un volet (mode GUI), pour restaurer l'affichage.
    PaneSnapshot { pane_id: u64, bytes: Vec<u8> },
    /// Flux brut d'un volet (mode GUI).
    PaneOutput { pane_id: u64, bytes: Vec<u8> },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p wimux-protocol`
Expected: PASS (tous les tests, dont les deux nouveaux).

- [ ] **Step 5: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs
git commit -m "protocol : messages du mode GUI (AttachGui/PaneInput/PaneOutput/PaneSnapshot)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Abonnement au flux brut du volet + snapshot

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Test: `crates/wimux-server/src/pane.rs` (module `tests`, pour `snapshot_bytes`)

**Interfaces:**
- Consumes: `Terminal` (grille VT), `PaneState`.
- Produces (méthodes publiques de `Pane`) :
  - `fn subscribe(&self) -> std::sync::mpsc::Receiver<Vec<u8>>`
  - `fn snapshot_bytes(&self) -> Vec<u8>`

**Détails d'implémentation :**

Ajouter un champ à `PaneState` :

```rust
    subscribers: Vec<std::sync::mpsc::Sender<Vec<u8>>>,
```

L'initialiser à `Vec::new()` dans `Pane::spawn` (avec les autres champs de `PaneState`).

- [ ] **Step 1: Write the failing test (snapshot from grid)**

Ce test passe par `Terminal` (pas de ConPTY). Ajouter dans le module `tests` de `pane.rs`. D'abord exposer une fonction interne testable : implémenter `snapshot_bytes` en s'appuyant sur une fonction libre `grid_to_bytes(term: &Terminal) -> Vec<u8>` (testable directement).

```rust
#[test]
fn snapshot_reproduit_le_texte_visible() {
    let mut term = wimux_vt::Terminal::new(20, 3);
    term.advance(b"abc\r\ndef");
    let bytes = grid_to_bytes(&term);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("abc"));
    assert!(text.contains("def"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wimux-server snapshot_reproduit_le_texte_visible`
Expected: FAIL (`grid_to_bytes` inexistante).

- [ ] **Step 3: Implement `grid_to_bytes`, `snapshot_bytes`, and `subscribe`**

Fonction libre en bas de `pane.rs` :

```rust
/// Reconstruit une séquence d'octets rejouable (écran visible) depuis la grille.
/// Version G1 : texte brut, une ligne par rangée, séparé par CRLF, précédé d'un
/// effacement d'écran. Les couleurs suivront (G2+).
fn grid_to_bytes(term: &Terminal) -> Vec<u8> {
    let grid = term.grid();
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[2J\x1b[H"); // effacer + curseur en haut à gauche
    for row in 0..grid.rows() {
        let line: String = grid
            .row(row)
            .iter()
            .filter(|c| c.width != 0)
            .map(|c| c.ch)
            .collect();
        out.extend_from_slice(line.trim_end().as_bytes());
        if row + 1 < grid.rows() {
            out.extend_from_slice(b"\r\n");
        }
    }
    out
}
```

Méthodes de `Pane` (près de `snapshot`) :

```rust
    /// Reconstruit le contenu visible du volet en octets (pour PaneSnapshot).
    pub fn snapshot_bytes(&self) -> Vec<u8> {
        let st = self.state.lock().unwrap();
        grid_to_bytes(&st.terminal)
    }

    /// S'abonne au flux brut de sortie du volet. Chaque fragment lu depuis
    /// ConPTY sera envoyé sur le récepteur retourné.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.state.lock().unwrap().subscribers.push(tx);
        rx
    }
```

Dans `reader_loop`, à l'endroit où l'on tient déjà `st` après lecture d'un chunk
`&buf[..n]`, **après** avoir alimenté l'émulateur, diffuser aux abonnés (en
retirant ceux qui sont morts) :

```rust
                // Diffuser le flux brut aux clients GUI abonnés.
                st.subscribers.retain(|tx| tx.send(buf[..n].to_vec()).is_ok());
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wimux-server snapshot_reproduit_le_texte_visible`
Expected: PASS.

- [ ] **Step 5: fmt + clippy**

Run: `cargo fmt --all && RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: aucun avertissement.

- [ ] **Step 6: Commit**

```bash
git add crates/wimux-server/src/pane.rs
git commit -m "server(pane) : abonnement au flux brut + snapshot_bytes (mode GUI)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Accès GUI au niveau session

**Files:**
- Modify: `crates/wimux-server/src/session.rs`

**Interfaces:**
- Consumes: `Pane::{subscribe, snapshot_bytes, send_input}`, `Pane::id`.
- Produces (méthodes publiques de `Session`) :
  - `fn gui_attach(&self) -> Option<(u64, Vec<u8>, std::sync::mpsc::Receiver<Vec<u8>>)>`
    (renvoie `(pane_id actif, snapshot, récepteur de flux)`)
  - `fn gui_input(&self, pane_id: u64, bytes: &[u8])`

**Détails :** pour G1, on cible le **volet actif** de la fenêtre active. `gui_input`
route vers ce volet (on ignore `pane_id` pour G1 mais on garde la signature pour G2+).

- [ ] **Step 1: Implement the methods**

Ajouter dans `impl Session` (près de `active_pane`) :

```rust
    /// Prépare un attachement GUI : renvoie le volet actif, son instantané et un
    /// abonnement à son flux brut.
    pub fn gui_attach(&self) -> Option<(u64, Vec<u8>, std::sync::mpsc::Receiver<Vec<u8>>)> {
        let pane = self.active_pane()?;
        Some((pane.id, pane.snapshot_bytes(), pane.subscribe()))
    }

    /// Frappe GUI vers le volet actif (G1 : `pane_id` ignoré).
    pub fn gui_input(&self, _pane_id: u64, bytes: &[u8]) {
        if let Some(pane) = self.active_pane() {
            pane.send_input(bytes);
        }
    }
```

Note : `active_pane` est actuellement privée (`fn active_pane`). La garder privée ;
`gui_attach`/`gui_input` sont les points d'entrée publics.

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build -p wimux-server`
Expected: succès.

- [ ] **Step 3: fmt + clippy**

Run: `cargo fmt --all && RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: aucun avertissement.

- [ ] **Step 4: Commit**

```bash
git add crates/wimux-server/src/session.rs
git commit -m "server(session) : gui_attach/gui_input (mode GUI)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Le démon sert le mode GUI (+ test d'intégration)

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Create: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces:**
- Consumes: `Session::{gui_attach, gui_input}`, messages du protocole (Task 1).

**Détails :** dans `handle_client`, ajouter le traitement de `AttachGui` et
`PaneInput`. À l'`AttachGui` : récupérer `(pane_id, snapshot, rx)`, envoyer
`PaneSnapshot`, puis lancer un thread qui lit `rx` et envoie des `PaneOutput` au
client tant que la connexion vit. Réutiliser le `conn: Arc<PipeConn>`.

- [ ] **Step 1: Write the failing integration test**

Créer `crates/wimux-server/tests/gui_mode.rs`. Réutiliser le style des helpers de
`tests/detach_reattach.rs` (copie locale des helpers `start_daemon`, `connect_retry`,
`handshake`, `spawn_reader`) :

```rust
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use wimux_protocol::transport::{PipeConn, connect};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, recv, send,
};
use wimux_server::daemon;

fn start_daemon(pipe: &str) {
    let p = pipe.to_string();
    std::thread::spawn(move || {
        let _ = daemon::run_on(&p);
    });
    std::thread::sleep(Duration::from_millis(150));
}

fn connect_retry(pipe: &str) -> PipeConn {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match connect(pipe) {
            Ok(c) => return c,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("connexion impossible : {e}"),
        }
    }
}

fn handshake(conn: &PipeConn) {
    let mut w: &PipeConn = conn;
    send(&mut w, &ClientMessage::Hello(Hello {
        client_version: PROTOCOL_VERSION,
        client_build: "test".into(),
    })).unwrap();
    let mut r: &PipeConn = conn;
    assert!(matches!(recv::<_, ServerMessage>(&mut r).unwrap(),
        ServerMessage::Hello(HelloReply::Ok { .. })));
}

fn spawn_reader(conn: Arc<PipeConn>) -> Receiver<ServerMessage> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut r: &PipeConn = &conn;
        while let Ok(m) = recv::<_, ServerMessage>(&mut r) {
            if tx.send(m).is_err() { break; }
        }
    });
    rx
}

#[test]
fn attach_gui_recoit_snapshot_puis_flux() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-gui", std::process::id());
    start_daemon(&pipe);

    // Créer une session en mode TUI classique (pour avoir un volet vivant).
    let owner = Arc::new(connect_retry(&pipe));
    handshake(&owner);
    {
        let mut w: &PipeConn = &owner;
        send(&mut w, &ClientMessage::NewSession {
            name: Some("g".into()), cols: 80, rows: 24,
        }).unwrap();
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
        send(&mut w, &ClientMessage::AttachGui { session: "g".into() }).unwrap();
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
        send(&mut w, &ClientMessage::PaneInput {
            pane_id,
            bytes: b"Write-Output ('GUI' + 'OK')\r".to_vec(),
        }).unwrap();
    }

    let mut acc = String::new();
    let found = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneOutput { bytes, .. }) => {
                    acc.push_str(&String::from_utf8_lossy(&bytes));
                    if acc.contains("GUIOK") { break true; }
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => break false,
            }
        }
    };
    assert!(found, "PaneOutput ne contient pas la sortie.\nRecu :\n{acc}");

    let mut w: &PipeConn = &owner;
    send(&mut w, &ClientMessage::Kill { name: "g".into() }).unwrap();
    std::thread::sleep(Duration::from_millis(200));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wimux-server --test gui_mode`
Expected: FAIL (le serveur ne gère pas `AttachGui` : pas de `PaneSnapshot`).

- [ ] **Step 3: Handle AttachGui / PaneInput in `handle_client`**

Dans le `match msg` de `handle_client`, il existe déjà des bras **no-op
provisoires** pour `AttachGui`, `PaneInput` et `PaneResize` (posés lors de la
tâche 1 pour que le workspace compile). **Remplacer** les no-op `AttachGui` et
`PaneInput` par la logique ci-dessous (laisser `PaneResize` en no-op) :

```rust
            ClientMessage::AttachGui { session } => {
                if let Some(s) = server.get(&session) {
                    if let Some((pane_id, snapshot, rx)) = s.gui_attach() {
                        let mut wr: &PipeConn = &conn;
                        send(&mut wr, &ServerMessage::PaneSnapshot {
                            pane_id,
                            bytes: snapshot,
                        })?;
                        // Thread de retransmission du flux brut.
                        let conn_out = Arc::clone(&conn);
                        std::thread::spawn(move || {
                            for chunk in rx {
                                let mut w: &PipeConn = &conn_out;
                                if send(&mut w, &ServerMessage::PaneOutput {
                                    pane_id,
                                    bytes: chunk,
                                }).is_err() {
                                    break;
                                }
                            }
                        });
                        gui_session = Some(s);
                    }
                } else {
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::Error(format!(
                        "session introuvable : {session}"
                    )))?;
                }
            }
            ClientMessage::PaneInput { pane_id, bytes } => {
                if let Some(s) = &gui_session {
                    s.gui_input(pane_id, &bytes);
                }
            }
            ClientMessage::PaneResize { .. } => {} // G1 : ignoré (voir G3)
```

Et déclarer, avant la boucle `loop {` (près de `let mut attachment`) :

```rust
    let mut gui_session: Option<Arc<Session>> = None;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + suite complète**

Run: `cargo fmt --all && RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets && cargo test --workspace -- --test-threads=1`
Expected: tout vert (l'existant + `gui_mode`).

- [ ] **Step 6: Commit**

```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs
git commit -m "server(daemon) : mode GUI (AttachGui/PaneInput -> PaneSnapshot/PaneOutput)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Scaffolder l'application Tauri `wimux-gui`

**Files:**
- Create: `wimux-gui/` (arborescence Tauri 2)

**Prérequis :** Node.js LTS et npm installés ; Rust déjà présent ; WebView2 (fourni avec Windows 11).

- [ ] **Step 1: Créer le projet Tauri**

Depuis la racine du dépôt :

```bash
npm create tauri-app@latest wimux-gui -- --template vanilla-ts --manager npm --yes
```

Cela crée `wimux-gui/` avec `src/` (frontend TS + Vite) et `src-tauri/` (backend Rust).

- [ ] **Step 2: Ajouter xterm.js au frontend**

```bash
cd wimux-gui
npm install @xterm/xterm @xterm/addon-fit
cd ..
```

- [ ] **Step 3: Vérifier que l'app se lance (fenêtre vide)**

```bash
cd wimux-gui
npm run tauri dev
```
Expected: une fenêtre native s'ouvre avec la page par défaut. Fermer la fenêtre.

- [ ] **Step 4: Exclure les artefacts du dépôt**

Ajouter à `.gitignore` (racine) :

```
# App GUI (Tauri)
wimux-gui/node_modules/
wimux-gui/dist/
wimux-gui/src-tauri/target/
```

- [ ] **Step 5: Commit**

```bash
git add wimux-gui .gitignore
git commit -m "gui : scaffold de l'application Tauri (vanilla-ts + xterm.js)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Pont backend Tauri ↔ serveur wimux

**Files:**
- Modify: `wimux-gui/src-tauri/Cargo.toml` (dépendance vers `wimux-protocol`)
- Modify: `wimux-gui/src-tauri/src/lib.rs` (commandes + events Tauri)

**Interfaces:**
- Consumes: `wimux_protocol::transport::{connect, user_pipe_name, PipeConn}`,
  `wimux_protocol::{ClientMessage, ServerMessage, Hello, HelloReply, PROTOCOL_VERSION, send, recv}`.
- Produces (côté frontend) :
  - commande Tauri `gui_attach(session: String) -> Result<(), String>`
  - commande Tauri `pane_input(pane_id: u64, bytes: Vec<u8>) -> Result<(), String>`
  - event Tauri `pane-output` (payload `{ pane_id: u64, bytes: number[] }`)
  - event Tauri `pane-snapshot` (payload `{ pane_id: u64, bytes: number[] }`)

- [ ] **Step 1: Déclarer la dépendance au protocole**

Dans `wimux-gui/src-tauri/Cargo.toml`, section `[dependencies]`, ajouter (chemin relatif depuis `src-tauri`) :

```toml
wimux-protocol = { path = "../../crates/wimux-protocol" }
```

- [ ] **Step 2: Implémenter le pont**

Remplacer le contenu de `wimux-gui/src-tauri/src/lib.rs` par :

```rust
use std::sync::Arc;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use wimux_protocol::transport::{PipeConn, connect, user_pipe_name};
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, recv, send,
};

/// Connexion partagée au serveur wimux (écrivain).
#[derive(Default)]
struct Bridge {
    conn: Mutex<Option<Arc<PipeConn>>>,
}

fn do_handshake(conn: &PipeConn) -> Result<(), String> {
    let mut w: &PipeConn = conn;
    send(&mut w, &ClientMessage::Hello(Hello {
        client_version: PROTOCOL_VERSION,
        client_build: env!("CARGO_PKG_VERSION").to_string(),
    })).map_err(|e| e.to_string())?;
    let mut r: &PipeConn = conn;
    match recv::<_, ServerMessage>(&mut r).map_err(|e| e.to_string())? {
        ServerMessage::Hello(HelloReply::Ok { .. }) => Ok(()),
        _ => Err("handshake refusé".into()),
    }
}

#[tauri::command]
fn gui_attach(session: String, app: AppHandle, bridge: State<Bridge>) -> Result<(), String> {
    let conn = Arc::new(connect(&user_pipe_name()).map_err(|_| "serveur wimux introuvable".to_string())?);
    do_handshake(&conn)?;
    {
        let mut w: &PipeConn = &conn;
        send(&mut w, &ClientMessage::AttachGui { session }).map_err(|e| e.to_string())?;
    }
    *bridge.conn.lock().unwrap() = Some(Arc::clone(&conn));

    // Thread lecteur : relaye les messages serveur vers le frontend.
    let reader = Arc::clone(&conn);
    std::thread::spawn(move || {
        let mut r: &PipeConn = &reader;
        while let Ok(msg) = recv::<_, ServerMessage>(&mut r) {
            match msg {
                ServerMessage::PaneSnapshot { pane_id, bytes } => {
                    let _ = app.emit("pane-snapshot", (pane_id, bytes));
                }
                ServerMessage::PaneOutput { pane_id, bytes } => {
                    let _ = app.emit("pane-output", (pane_id, bytes));
                }
                _ => {}
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn pane_input(pane_id: u64, bytes: Vec<u8>, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::PaneInput { pane_id, bytes }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Bridge::default())
        .invoke_handler(tauri::generate_handler![gui_attach, pane_input])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de wimux-gui");
}
```

Note : selon la version du template, l'appel de `run()` est dans `src-tauri/src/main.rs`
(`wimux_gui_lib::run()`). Ne pas y toucher.

- [ ] **Step 3: Vérifier la compilation du backend**

```bash
cd wimux-gui/src-tauri
cargo build
cd ../..
```
Expected: succès (le pont compile, dépend de `wimux-protocol`).

- [ ] **Step 4: Commit**

```bash
git add wimux-gui/src-tauri/Cargo.toml wimux-gui/src-tauri/src/lib.rs
git commit -m "gui(backend) : pont Tauri vers le serveur wimux (mode GUI)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Frontend xterm.js + vérification bout-en-bout

**Files:**
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/index.html`
- Modify: `wimux-gui/src/styles.css` (thème sombre/bleu minimal)

**Interfaces:**
- Consumes: événements Tauri `pane-output` / `pane-snapshot`, commandes `gui_attach` / `pane_input`.

- [ ] **Step 1: HTML — conteneur du terminal**

Remplacer le `<body>` de `wimux-gui/index.html` par :

```html
  <body>
    <div id="terminal"></div>
    <script type="module" src="/src/main.ts"></script>
  </body>
```

- [ ] **Step 2: Style sombre/bleu minimal**

Mettre dans `wimux-gui/src/styles.css` :

```css
:root { background: #1e1e1e; }
body { margin: 0; }
#terminal { width: 100vw; height: 100vh; }
```

- [ ] **Step 3: Frontend — brancher xterm.js**

Remplacer `wimux-gui/src/main.ts` par :

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

// Volet actif (G1 : un seul).
let paneId = 0;

// Sortie serveur -> terminal.
listen<[number, number[]]>("pane-snapshot", (e) => {
  paneId = e.payload[0];
  term.write(new Uint8Array(e.payload[1]));
});
listen<[number, number[]]>("pane-output", (e) => {
  paneId = e.payload[0];
  term.write(new Uint8Array(e.payload[1]));
});

// Frappe -> serveur.
term.onData((data) => {
  const bytes = Array.from(new TextEncoder().encode(data));
  invoke("pane_input", { paneId, bytes });
});

// S'attacher a la session "dev" au demarrage (G1 : nom fixe).
invoke("gui_attach", { session: "dev" }).catch((err) => term.write(`\r\n[erreur: ${err}]\r\n`));
```

Note : Tauri convertit `pane_id` (Rust) ↔ `paneId` (JS) automatiquement (camelCase).

- [ ] **Step 4: Vérification manuelle bout-en-bout**

1. Construire le workspace Rust et lancer un serveur + une session `dev` :
```bash
cargo build --release
target/release/wimux.exe new -s dev
```
   (Laisser cette fenêtre TUI ouverte, ou se détacher avec `Ctrl-b d` — la session `dev` reste vivante.)
2. Lancer la GUI :
```bash
cd wimux-gui && npm run tauri dev
```
3. **Attendu :** la fenêtre GUI affiche le contenu de la session `dev` (snapshot), et taper des commandes dans la fenêtre GUI les exécute (l'output s'affiche). Tester `Get-Date` puis Entrée.
4. Vérifier la **persistance** : fermer la fenêtre GUI, la relancer → le contenu réapparaît (snapshot), la session a survécu.

- [ ] **Step 5: Documenter la vérification**

Créer `wimux-gui/README.md` :

```markdown
# wimux-gui (fondation G1)

Interface graphique Tauri pour wimux. G1 : affiche une session (`dev`) dans un
xterm.js, frappe fonctionnelle, persistance via le serveur wimux.

## Développement
- Prérequis : un serveur wimux avec une session `dev` (`wimux new -s dev`).
- `npm install` puis `npm run tauri dev`.
```

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/src/main.ts wimux-gui/index.html wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "gui(frontend) : xterm.js branché au pont + doc (jalon G1 atteint)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Definition of Done (G1)

- `cargo test --workspace` vert, dont `tests/gui_mode.rs`.
- fmt + clippy `-D warnings` propres sur le workspace.
- La fenêtre GUI affiche une session wimux vivante, la frappe fonctionne, et la
  session **survit** à la fermeture/réouverture de la fenêtre.
- Le mode TUI existant est inchangé (tests existants toujours verts).

## Suites (plans ultérieurs, hors G1)

- **G2** : onglets verticaux (rail des sessions, `Structure` + deltas, création/fermeture).
- **G3** : volets graphiques (arbre de découpes, un xterm.js par volet, `PaneResize`).
- **G4** : indicateurs d'activité (`PaneActivity`).
