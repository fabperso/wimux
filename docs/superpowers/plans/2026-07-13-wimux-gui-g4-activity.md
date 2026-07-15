# wimux GUI G4 — Indicateurs d'activité : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Afficher dans le rail de `wimux-gui` une pastille par session inactive — « activité » (sortie non vue) ou « cloche » (BEL explicite) — via un suivi léger côté serveur exposé par le sondage `List` existant.

**Architecture:** Suivi léger, sans diffusion de fond. L'activité d'une session = `Notifier.generation() > last_seen_gen` (la génération du `Notifier` partagé par les volets n'avance que sur de vraies sorties pour une session inactive que personne ne manipule). La cloche = un drapeau `AtomicBool` sur le `Notifier`, posé par le `reader_loop` d'un volet quand l'émulateur `wimux-vt` a vu un BEL. L'état « vue » est global au `Server` (`gui_viewed: Mutex<Option<String>>`), posé à l'`AttachGui` et effacé à la fin de la connexion GUI qui l'a posé. `Server::list` calcule `activity`/`bell` par session (session vue → indicateurs effacés + baseline rafraîchi).

**Tech Stack:** Rust (workspace, edition 2024), `vte`/`wimux-vt`, ConPTY (`portable-pty`), Named Pipe + postcard, Tauri 2 + TypeScript + xterm.js.

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : suites TUI + G1 + G2 + G3 vertes (`cargo test --workspace -- --test-threads=1`).
- Tests serveur = unitaires (vt, pane `Notifier`, session) + intégration dans `crates/wimux-server/tests/gui_mode.rs`. Frontend = build `npm run build` + vérif manuelle documentée dans `wimux-gui/README.md` (pas de test auto front).
- Outil shell : **Bash tool** (git bash) ; tests d'intégration lents (ConPTY), `--test-threads=1`, patience.
- `cargo fmt` a tendance à reformater `tests/gui_mode.rs` hors périmètre — le rétablir (`git checkout -- crates/wimux-server/tests/gui_mode.rs`) avant commit si on ne le modifie pas dans la tâche.
- Chaque commit se termine par le trailer : `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, via `git commit -m "$(printf '...')"`.
- Détection de cloche : un BEL isolé (`0x07`) arrive par `Perform::execute(0x07)` ; un BEL terminateur d'OSC (`ESC ] … BEL`) est consommé par le parser `vte` et routé vers `osc_dispatch` (JAMAIS `execute`) — le bras `0x07` de `execute` est donc naturellement immunisé contre les faux positifs OSC. `vte` n'a **pas** de méthode `Perform::bell`.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : `SessionInfo` gagne `activity`/`bell` ; test roundtrip.
- `crates/wimux-vt/src/emulator.rs` — **modifier** : `Screen.bell_pending`, bras `0x07` dans `execute`, `Terminal::take_bell` ; 3 tests unitaires.
- `crates/wimux-server/src/pane.rs` — **modifier** : `Notifier.bell: AtomicBool` + `signal_bell`/`bell`/`clear_bell` ; câblage `reader_loop` ; test unitaire `Notifier`.
- `crates/wimux-server/src/session.rs` — **modifier** : `Session.last_seen_gen: AtomicU64` + `mark_seen`/`has_activity`/`has_bell` ; test unitaire.
- `crates/wimux-server/src/daemon.rs` — **modifier** : `Server.gui_viewed` + `set_gui_viewed` ; `Server::list` calcule les indicateurs ; `AttachGui` pose l'état vue ; fin de `handle_client` l'efface. (Task 1 pose des placeholders dans `list`.)
- `crates/wimux-server/tests/gui_mode.rs` — **modifier** : tests d'intégration activité + cloche.
- `wimux-gui/src-tauri/src/lib.rs` — **modifier** : `SessionDto` gagne `activity`/`bell`.
- `wimux-gui/src/main.ts`, `wimux-gui/src/styles.css`, `wimux-gui/README.md` — **modifier** : pastilles du rail, effacement optimiste, styles, vérif manuelle.

---

## Task 1: Protocole — étendre `SessionInfo`

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (le SEUL constructeur de `SessionInfo`)
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces (utilisés par Tasks 5/6) :
  - `pub struct SessionInfo { pub name: String, pub windows: u32, pub attached: bool, pub activity: bool, pub bell: bool }` (Serialize/Deserialize/Clone/Debug).

- [ ] **Step 1: Écrire le test roundtrip (échoue)**

Dans le module `tests` de `crates/wimux-protocol/src/lib.rs`, ajouter :

```rust
    #[test]
    fn aller_retour_session_info_activite() {
        let info = SessionInfo {
            name: "dev".into(),
            windows: 2,
            attached: true,
            activity: true,
            bell: false,
        };
        let msg = ServerMessage::Sessions(vec![info]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::Sessions(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "dev");
                assert_eq!(v[0].windows, 2);
                assert!(v[0].attached);
                assert!(v[0].activity);
                assert!(!v[0].bell);
            }
            _ => panic!("mauvais variant"),
        }
    }
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-protocol`
Expected: FAIL — `missing fields activity, bell in initializer of SessionInfo`.

- [ ] **Step 3: Ajouter les champs à `SessionInfo`**

Dans `crates/wimux-protocol/src/lib.rs`, remplacer la définition de `SessionInfo` par :

```rust
/// Résumé d'une session, tel qu'affiché par `wimux list-sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    /// Sortie non vue depuis la dernière vue GUI (G4).
    pub activity: bool,
    /// BEL explicite reçu depuis la dernière vue GUI (G4).
    pub bell: bool,
}
```

- [ ] **Step 4: Mettre à jour le SEUL constructeur (placeholder)**

Dans `crates/wimux-server/src/daemon.rs`, dans `Server::list`, remplacer le `.map(...)` :

```rust
            .map(|s| SessionInfo {
                name: s.name(),
                windows: s.window_count() as u32,
                attached: s.attached_count() > 0,
            })
```

par (la vraie logique arrive en Task 5) :

```rust
            .map(|s| SessionInfo {
                name: s.name(),
                windows: s.window_count() as u32,
                attached: s.attached_count() > 0,
                activity: false, // calculé en Task 5
                bell: false,     // calculé en Task 5
            })
```

(Le `ls` du CLI, dans `crates/wimux-cli/src/main.rs`, lit `s.name`/`s.windows`/`s.attached` et ne CONSTRUIT pas de `SessionInfo` : aucune modification requise.)

- [ ] **Step 5: Lancer le test + build serveur (attendu PASS/OK)**

Run: `cargo test -p wimux-protocol`
Expected: PASS (dont `aller_retour_session_info_activite`).

Run: `cargo build -p wimux-server`
Expected: OK.

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a modifié `crates/wimux-server/tests/gui_mode.rs` (hors périmètre) : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

- [ ] **Step 7: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(protocol): SessionInfo gagne activity/bell (G4)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: `wimux-vt` — détection de cloche

**Files:**
- Modify: `crates/wimux-vt/src/emulator.rs`
- Test: `crates/wimux-vt/src/emulator.rs` (module `tests`)

**Interfaces:**
- Consumes: `vte::Perform` (déjà implémenté par `Screen`).
- Produces (utilisé par Task 3) : `Terminal::take_bell(&mut self) -> bool` (miroir de `take_responses` : renvoie et remet à faux le drapeau).

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-vt/src/emulator.rs`, ajouter :

```rust
    #[test]
    fn bel_nu_leve_la_cloche() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"\x07");
        assert!(t.take_bell(), "un BEL nu doit lever la cloche");
        assert!(!t.take_bell(), "la cloche est consommée au premier take");
    }

    #[test]
    fn bel_terminateur_osc_ne_leve_pas_la_cloche() {
        let mut t = Terminal::new(10, 2);
        // ESC ] 0 ; titre BEL : le BEL termine l'OSC, il ne passe pas par execute.
        t.advance(b"\x1b]0;titre\x07");
        assert!(!t.take_bell(), "un BEL terminateur d'OSC ne doit PAS lever la cloche");
    }

    #[test]
    fn sans_bel_pas_de_cloche() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"abc");
        assert!(!t.take_bell());
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-vt`
Expected: FAIL — `no method named take_bell found for struct Terminal`.

- [ ] **Step 3: Ajouter le drapeau au `struct Screen`**

Dans `crates/wimux-vt/src/emulator.rs`, dans `struct Screen`, ajouter le champ (par exemple après `responses: Vec<u8>,`) :

```rust
    responses: Vec<u8>,
    /// Un BEL (`0x07`) a été reçu comme contrôle C0 depuis le dernier `take_bell`.
    bell_pending: bool,
```

Dans `Screen::new`, initialiser le champ (après `responses: Vec::new(),`) :

```rust
            responses: Vec::new(),
            bell_pending: false,
```

- [ ] **Step 4: Détecter le BEL dans `execute`**

Dans `impl Perform for Screen`, méthode `execute`, ajouter le bras `0x07` avant le `_ => {}` final du `match byte` :

```rust
            0x09 => {
                let next = ((self.cx / 8) + 1) * 8;
                self.cx = next.min(self.cols - 1);
                self.wrap_next = false;
            }
            0x07 => self.bell_pending = true,
            _ => {}
```

- [ ] **Step 5: Ajouter `Terminal::take_bell`**

Dans `impl Terminal`, juste après la méthode `take_responses`, ajouter :

```rust
    /// Récupère (et remet à faux) le drapeau « cloche » : `true` si au moins un
    /// BEL (`0x07`) a été reçu comme contrôle depuis le dernier appel. Miroir de
    /// `take_responses`.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.screen.bell_pending)
    }
```

- [ ] **Step 6: Lancer les tests (attendu PASS)**

Run: `cargo test -p wimux-vt`
Expected: PASS (dont `bel_nu_leve_la_cloche`, `bel_terminateur_osc_ne_leve_pas_la_cloche`, `sans_bel_pas_de_cloche`).

- [ ] **Step 7: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-vt --all-targets`
Expected: OK.

```bash
git add crates/wimux-vt/src/emulator.rs
git commit -m "$(printf 'feat(vt): detection de cloche (BEL) via take_bell\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: `pane.rs` — cloche sur le `Notifier` + câblage `reader_loop`

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Test: `crates/wimux-server/src/pane.rs` (module `tests`)

**Interfaces:**
- Consumes (Task 2) : `Terminal::take_bell`.
- Produces (utilisés par Task 4) :
  - `Notifier::signal_bell(&self)`
  - `Notifier::bell(&self) -> bool`
  - `Notifier::clear_bell(&self)`

- [ ] **Step 1: Écrire le test unitaire (échoue)**

Dans le module `tests` de `crates/wimux-server/src/pane.rs`, ajouter :

```rust
    #[test]
    fn notifier_cloche() {
        let n = Notifier::new();
        assert!(!n.bell(), "cloche neuve à faux");
        n.signal_bell();
        assert!(n.bell());
        n.clear_bell();
        assert!(!n.bell());
    }
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib pane::tests::notifier_cloche`
Expected: FAIL — `no method named signal_bell found for struct Notifier`.

- [ ] **Step 3: Ajouter le drapeau `bell` au `struct Notifier`**

Dans `crates/wimux-server/src/pane.rs`, remplacer la définition de `Notifier` par :

```rust
/// Signal de changement d'affichage partagé par tous les volets d'une session.
pub struct Notifier {
    generation: Mutex<u64>,
    cond: Condvar,
    /// Cloche (BEL) en attente pour cette session (G4).
    bell: AtomicBool,
}
```

et, dans `Notifier::new`, ajouter le champ :

```rust
    pub fn new() -> Arc<Notifier> {
        Arc::new(Notifier {
            generation: Mutex::new(0),
            cond: Condvar::new(),
            bell: AtomicBool::new(false),
        })
    }
```

(`AtomicBool` et `Ordering` sont déjà importés en tête de `pane.rs`.)

- [ ] **Step 4: Ajouter les méthodes cloche**

Dans `impl Notifier`, après la méthode `notify`, ajouter :

```rust
    /// Pose le drapeau cloche (appelé par le `reader_loop` d'un volet).
    pub fn signal_bell(&self) {
        self.bell.store(true, Ordering::Relaxed);
    }

    /// Lit le drapeau cloche.
    pub fn bell(&self) -> bool {
        self.bell.load(Ordering::Relaxed)
    }

    /// Efface le drapeau cloche (quand la session est vue).
    pub fn clear_bell(&self) {
        self.bell.store(false, Ordering::Relaxed);
    }
```

- [ ] **Step 5: Remonter la cloche dans `reader_loop`**

Dans `fn reader_loop`, remplacer le bras `Ok(n) => { ... }` par :

```rust
            Ok(n) => {
                let rang = {
                    let mut st = pane.state.lock().unwrap();
                    st.terminal.advance(&buf[..n]);
                    let responses = st.terminal.take_responses();
                    if !responses.is_empty() {
                        let _ = st.writer.write_all(&responses);
                        let _ = st.writer.flush();
                    }
                    // Diffuser le flux brut aux clients GUI abonnés.
                    st.subscribers
                        .retain(|tx| tx.send((pane.id, buf[..n].to_vec())).is_ok());
                    // Cloche détectée par l'émulateur pendant l'advance (sous le verrou).
                    st.terminal.take_bell()
                };
                if rang {
                    // Hors du verrou du volet : le drapeau vit sur le Notifier partagé.
                    pane.notifier.signal_bell();
                }
                pane.notifier.bump();
            }
```

- [ ] **Step 6: Lancer les tests lib (attendu PASS)**

Run: `cargo test -p wimux-server --lib pane -- --test-threads=1`
Expected: PASS (dont `notifier_cloche`).

- [ ] **Step 7: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/pane.rs
git commit -m "$(printf 'feat(pane): cloche sur le Notifier + remontee depuis reader_loop\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: `session.rs` — suivi vu / activité / cloche

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Test: `crates/wimux-server/src/session.rs` (module `tests`)

**Interfaces:**
- Consumes (Task 3) : `Notifier::{generation, bump, signal_bell, bell, clear_bell}`.
- Produces (utilisés par Task 5) :
  - `Session::mark_seen(&self)` — pose `last_seen_gen = notifier.generation()` et `notifier.clear_bell()`.
  - `Session::has_activity(&self) -> bool` — `notifier.generation() > last_seen_gen`.
  - `Session::has_bell(&self) -> bool` — `notifier.bell()`.

- [ ] **Step 1: Écrire le test unitaire (échoue)**

Dans le module `tests` de `crates/wimux-server/src/session.rs`, ajouter (à côté de `window_layout_feuille_unique`) :

```rust
    #[test]
    fn suivi_activite_et_cloche() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        // Laisser le shell démarrer puis se taire (cmd.exe à l'invite est inactif),
        // pour que la génération se stabilise avant les assertions.
        std::thread::sleep(std::time::Duration::from_millis(1000));

        // Activité : un bump au-delà du baseline vu.
        s.mark_seen();
        s.notifier().bump();
        assert!(
            s.has_activity(),
            "un bump après mark_seen doit marquer l'activité"
        );
        s.mark_seen();
        assert!(
            !s.has_activity(),
            "après mark_seen, plus d'activité en attente"
        );

        // Cloche : drapeau sur le Notifier, effacé par mark_seen.
        s.notifier().signal_bell();
        assert!(s.has_bell());
        s.mark_seen();
        assert!(!s.has_bell(), "mark_seen efface la cloche");

        s.kill();
    }
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib session::tests::suivi_activite_et_cloche`
Expected: FAIL — `no method named mark_seen found for struct Session`.

- [ ] **Step 3: Importer `AtomicU64`**

Dans `crates/wimux-server/src/session.rs`, remplacer :

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
```

par :

```rust
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
```

- [ ] **Step 4: Ajouter le champ `last_seen_gen` au `struct Session`**

Dans `pub struct Session`, ajouter le champ (après `attached: AtomicUsize,`) :

```rust
    attached: AtomicUsize,
    /// Génération du Notifier vue par la GUI la dernière fois (G4).
    last_seen_gen: AtomicU64,
    paste_buffer: Mutex<String>,
```

Dans `Session::new`, initialiser le champ (après `attached: AtomicUsize::new(0),`) :

```rust
            attached: AtomicUsize::new(0),
            last_seen_gen: AtomicU64::new(0),
            paste_buffer: Mutex::new(String::new()),
```

- [ ] **Step 5: Ajouter les méthodes de suivi**

Dans `impl Session`, juste après la méthode `notifier`, ajouter :

```rust
    /// G4 : marque la session comme « vue » — rafraîchit le baseline d'activité
    /// et efface la cloche. Ce qu'on regarde n'est jamais « non vu ».
    pub fn mark_seen(&self) {
        self.last_seen_gen
            .store(self.notifier.generation(), Ordering::Relaxed);
        self.notifier.clear_bell();
    }

    /// G4 : la session a-t-elle produit de la sortie depuis la dernière vue ?
    pub fn has_activity(&self) -> bool {
        self.notifier.generation() > self.last_seen_gen.load(Ordering::Relaxed)
    }

    /// G4 : une cloche (BEL) est-elle en attente sur cette session ?
    pub fn has_bell(&self) -> bool {
        self.notifier.bell()
    }
```

- [ ] **Step 6: Lancer le test (attendu PASS)**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (dont `suivi_activite_et_cloche`).

- [ ] **Step 7: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(session): suivi vu/activite/cloche (mark_seen, has_activity, has_bell)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: `daemon.rs` — état vue global + calcul dans `list` + intégration

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces:**
- Consumes (Task 4) : `Session::{mark_seen, has_activity, has_bell}`.
- Produces : `SessionInfo.activity`/`.bell` reflètent l'état réel ; `Server::set_gui_viewed(&self, v: Option<String>)`.

- [ ] **Step 1: Écrire les tests d'intégration (échouent)**

Dans `crates/wimux-server/tests/gui_mode.rs`, remplacer la ligne d'import :

```rust
use wimux_protocol::{ClientMessage, LayoutNode, ServerMessage, SplitDir, send};
```

par :

```rust
use wimux_protocol::{ClientMessage, LayoutNode, ServerMessage, SessionInfo, SplitDir, recv, send};
```

Puis ajouter, en fin de fichier, les helpers et les 3 tests :

```rust
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
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::PaneSnapshot { .. }) => break,
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de PaneSnapshot pour {name}"),
        }
    }
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
    loop {
        match grx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::PaneSnapshot { .. }) => break,
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de snapshot après bascule sur B"),
        }
    }

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
```

- [ ] **Step 2: Lancer un test (attendu FAIL)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1 activite_marquee_pour_session_inactive`
Expected: FAIL — B ne devient jamais active (placeholders `activity: false` de Task 1).

- [ ] **Step 3: Ajouter `gui_viewed` au `struct Server`**

Dans `crates/wimux-server/src/daemon.rs`, remplacer la définition de `Server` par :

```rust
pub struct Server {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    config: Config,
    /// Session actuellement affichée par la connexion GUI persistante (G4).
    gui_viewed: Mutex<Option<String>>,
}
```

et, dans `Server::new`, initialiser le champ :

```rust
    fn new() -> Arc<Server> {
        Arc::new(Server {
            sessions: Mutex::new(HashMap::new()),
            config: Config::load(),
            gui_viewed: Mutex::new(None),
        })
    }
```

- [ ] **Step 4: Ajouter `set_gui_viewed` et réécrire `list`**

Dans `impl Server`, après `fn get`, ajouter :

```rust
    fn set_gui_viewed(&self, v: Option<String>) {
        *self.gui_viewed.lock().unwrap() = v;
    }
```

Remplacer entièrement `fn list` par :

```rust
    fn list(&self) -> Vec<SessionInfo> {
        self.reap();
        let viewed = self.gui_viewed.lock().unwrap().clone();
        let sessions = self.sessions.lock().unwrap();
        let mut infos: Vec<SessionInfo> = sessions
            .values()
            .map(|s| {
                let name = s.name();
                // La session vue n'a jamais d'indicateur : on efface et on
                // rafraîchit son baseline à chaque sondage (paresseux).
                let (activity, bell) = if Some(&name) == viewed.as_ref() {
                    s.mark_seen();
                    (false, false)
                } else {
                    (s.has_activity(), s.has_bell())
                };
                SessionInfo {
                    name,
                    windows: s.window_count() as u32,
                    attached: s.attached_count() > 0,
                    activity,
                    bell,
                }
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    }
```

- [ ] **Step 5: Suivre l'état « vue » dans `handle_client`**

Dans `fn handle_client`, ajouter un drapeau local après `let mut gui_session: Option<Arc<Session>> = None;` :

```rust
    let mut gui_session: Option<Arc<Session>> = None;
    // A-t-on posé l'état « vue » global ? (Seule cette connexion l'efface au drop.)
    let mut viewed_set = false;
    let mut prefix = PrefixState::default();
```

Dans le bras `ClientMessage::AttachGui { session }`, juste après `Some(s) => {`, insérer le marquage de la session vue :

```rust
                match server.get(&session) {
                    Some(s) => {
                        // G4 : cette session devient « vue » ; on efface ses indicateurs.
                        server.set_gui_viewed(Some(session.clone()));
                        s.mark_seen();
                        viewed_set = true;
                        if let Some((tree, active, snaps, rx, tx)) = s.gui_attach_window() {
```

Enfin, à la toute fin de `handle_client`, remplacer :

```rust
    drop(attachment);
    drop(gui_attach);
    Ok(())
```

par :

```rust
    drop(attachment);
    drop(gui_attach);
    // G4 : n'effacer l'état « vue » que si CETTE connexion l'a posé (les
    // connexions `List` jetables ne doivent pas l'effacer).
    if viewed_set {
        server.set_gui_viewed(None);
    }
    Ok(())
```

- [ ] **Step 6: Lancer les tests d'intégration G4 (attendu PASS)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1 activite_marquee_pour_session_inactive bascule_efface_activite cloche_marquee_pour_session_inactive`
Expected: PASS (tests lents ConPTY — patience).

- [ ] **Step 7: Non-régression complète + fmt + clippy**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS (dont les tests G1/G2/G3 existants).

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS.

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
Expected: OK.

- [ ] **Step 8: Commit**

```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs
git commit -m "$(printf 'feat(daemon): etat vue global + calcul activity/bell dans list (G4)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 6: Pont Tauri — `SessionDto` étendu

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes (Task 1) : `SessionInfo.activity`/`.bell`.
- Produces (utilisé par Task 7) : `SessionDto { name, attached, activity, bell }` sérialisé pour le frontend.

- [ ] **Step 1: Étendre `SessionDto`**

Dans `wimux-gui/src-tauri/src/lib.rs`, remplacer la définition de `SessionDto` par :

```rust
#[derive(serde::Serialize)]
struct SessionDto {
    name: String,
    attached: bool,
    activity: bool,
    bell: bool,
}
```

- [ ] **Step 2: Mapper les nouveaux champs dans `list_sessions`**

Dans `fn list_sessions`, remplacer le `.map(...)` par :

```rust
                .map(|s| SessionDto {
                    name: s.name,
                    attached: s.attached,
                    activity: s.activity,
                    bell: s.bell,
                })
```

- [ ] **Step 3: Build (attendu OK)**

Run: `cd wimux-gui/src-tauri && cargo build`
Expected: OK.

- [ ] **Step 4: Commit**

```bash
git add wimux-gui/src-tauri/src/lib.rs
git commit -m "$(printf 'feat(gui-bridge): SessionDto expose activity/bell (G4)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 7: Frontend — pastilles du rail

**Files:**
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`
- (Pas de test auto : cycle = code → `npm run build` → vérif manuelle README.)

**Interfaces:**
- Consumes (Task 6) : commande `list_sessions` renvoyant `SessionDto[]` avec `activity`/`bell`.
- Produces : pastille par session inactive dans `renderRail` ; effacement optimiste dans `switchTo`.

- [ ] **Step 1: Étendre le type `SessionDto`**

Dans `wimux-gui/src/main.ts`, remplacer :

```ts
type SessionDto = { name: string; attached: boolean };
```

par :

```ts
type SessionDto = { name: string; attached: boolean; activity: boolean; bell: boolean };
```

- [ ] **Step 2: Effacement optimiste dans `switchTo`**

Remplacer la fonction `switchTo` par :

```ts
async function switchTo(name: string) {
  if (name === activeSession) return;
  activeSession = name;
  // Effacement optimiste : la session qu'on regarde n'a plus d'indicateur, sans
  // attendre le prochain sondage.
  for (const s of lastSessions) {
    if (s.name === name) {
      s.activity = false;
      s.bell = false;
    }
  }
  paneManager.reset();
  await invoke("attach_session", { session: name }).catch((e) =>
    console.error("attach:", e),
  );
  renderRail(lastSessions);
}
```

- [ ] **Step 3: Ajouter la pastille dans `renderRail`**

Dans `renderRail`, remplacer la fin de la boucle `for (const s of sessions)` — c.-à-d. la ligne `el.append(name, close);` — par un ajout conditionnel de pastille :

```ts
    close.onclick = async (ev) => { ev.stopPropagation(); await invoke("kill_session", { name: s.name }).catch(() => {}); await refresh(); };
    el.onclick = () => {
      if (clickTimer !== null) return; // 2e clic d'un double-clic : ignore, laisse ondblclick gerer
      clickTimer = window.setTimeout(() => { clickTimer = null; switchTo(s.name); }, 200);
    };
    const isActive = s.name === activeSession;
    if (!isActive && (s.bell || s.activity)) {
      // Cloche prioritaire sur l'activité ; rien pour la session active.
      const dot = document.createElement("span");
      dot.className = "dot " + (s.bell ? "bell" : "activity");
      dot.textContent = s.bell ? "🔔" : "";
      el.append(name, dot, close);
    } else {
      el.append(name, close);
    }
    container.append(el);
```

(La ligne `container.append(el);` existante est conservée ; ne pas la dupliquer.)

- [ ] **Step 4: Styles des pastilles**

Dans `wimux-gui/src/styles.css`, ajouter après la règle `.session .close { ... }` :

```css
.session .dot { flex: 0 0 auto; line-height: 1; }
.session .dot.activity { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: #0a84ff; }
.session .dot.bell { font-size: 12px; }
```

- [ ] **Step 5: Build (attendu OK)**

Run: `cd wimux-gui && npm run build`
Expected: OK (tsc sans erreur, vite build produit `dist/`).

- [ ] **Step 6: Documenter la vérif manuelle G4 dans le README**

Dans `wimux-gui/README.md`, ajouter une section :

```markdown
## Vérification manuelle G4 (indicateurs d'activité)

Prérequis : deux sessions (ex. `dev` et `build`), la GUI attachée à `dev`.

1. Dans la session **inactive** `build` (via un TUI attaché ailleurs, ou
   `wimux send-keys -t build ...`), produire de la sortie, p. ex. `ls` ou
   `Write-Output test`.
   - **Attendu :** dans le rail, une **pastille bleue discrète** apparaît à droite
     du nom `build` (activité non vue), en ~1 s (sondage `List`).
2. Dans `build`, provoquer un **BEL** en sortie, p. ex. `[Console]::Write([char]7)`.
   - **Attendu :** la pastille de `build` devient une **cloche 🔔** (prioritaire
     sur l'activité).
3. Cliquer sur `build` dans le rail pour la regarder.
   - **Attendu :** sa pastille **disparaît immédiatement** (effacement optimiste),
     et le sondage suivant la maintient éteinte tant que `build` est affichée.
4. La session **active** n'affiche jamais de pastille.
```

- [ ] **Step 7: Commit**

```bash
git add wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "$(printf 'feat(gui): pastilles activite/cloche dans le rail + effacement optimiste\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Self-Review

**Spec coverage :**
- Extension `SessionInfo` (`activity`/`bell`) → Task 1. Détection de cloche `wimux-vt` (via `execute(0x07)`, immunisé OSC) + `take_bell` → Task 2. Drapeau cloche sur le `Notifier` + câblage `reader_loop` → Task 3. Suivi vu/activité/cloche sur `Session` (`mark_seen`/`has_activity`/`has_bell`) → Task 4. État vue global `gui_viewed` + calcul dans `Server::list` + intégration `AttachGui`/fin de connexion → Task 5. Pont Tauri `SessionDto` → Task 6. Pastilles du rail + effacement optimiste + CSS + README → Task 7. Tests : vt unitaires (Task 2), Notifier unitaire (Task 3), session unitaire (Task 4), intégration activité/cloche `gui_mode.rs` (Task 5), build front + vérif manuelle (Task 7). Le `ls` du CLI est inchangé (lit `name`/`windows`/`attached`) — confirmé en Task 1.

**Type consistency :** `mark_seen`/`has_activity`/`has_bell` définis en Task 4 sont consommés **à l'identique** en Task 5 (`s.mark_seen()`, `s.has_activity()`, `s.has_bell()`). `Notifier::{signal_bell, bell, clear_bell}` (Task 3) consommés par `Session` (Task 4) et par `reader_loop` (Task 3). `Terminal::take_bell` (Task 2) consommé par `reader_loop` (Task 3). `SessionInfo.activity`/`.bell` (Task 1) consommés par `Server::list` (Tasks 1 placeholder / 5 réel) et par `SessionDto` (Task 6), puis par le type TS `SessionDto` (Task 7).

**Points signalés pour relecture (choix non spécifiés) :**
1. **Test unitaire `session.rs` (Task 4)** : la session neuve lance un vrai shell ConPTY qui produit de la sortie asynchrone. Le test insère un `sleep(1000 ms)` pour laisser `cmd.exe` se taire, puis fait `mark_seen()` **avant** le `bump()` (baseline stable) au lieu de l'ordre littéral de la spec ; sémantiquement identique et robuste au bruit de fond. À valider.
2. **Test cloche en intégration (Task 5)** : réalisé sur une session **jamais attachée par la GUI** (`gui_viewed = None`) pour que la cloche persiste jusqu'au sondage, plutôt que de dépendre d'un timing d'attache. Commande PowerShell `[Console]::Write([char]7)` + `\r` (dépendance shell documentée dans le test et le README).
3. **Test activité (Task 5)** : B (jamais vue) est intrinsèquement `activity=true` dès sa sortie de démarrage ; le `send_keys` de sortie explicite conserve le récit de la spec et garde le test robuste. Le sondage attend `B.activity && !A.activity` (A restant marquée vue à chaque `List`).
4. **Pastille activité** : point bleu (`#0a84ff`, cohérent avec l'accent existant du rail) ; cloche = emoji `🔔`. Choix visuel non imposé par la spec.
5. **`fetch_list` de test** ouvre une connexion de contrôle **jetable** par sondage (miroir exact de la commande Tauri `list_sessions`), ce qui n'altère jamais l'état vue (`viewed_set` reste faux pour ces connexions).
