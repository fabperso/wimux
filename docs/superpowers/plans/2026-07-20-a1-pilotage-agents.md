# A1 — Pilotage d'agents par Claude (CLI `wimux agent` + skill) — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permettre à un Claude exécuté dans un volet wimux de créer des volets « agents » (une commande par tâche), de lire leur sortie (journal + capture) et de les piloter, via une CLI `wimux agent` typée + un skill.

**Architecture:** Nouveaux messages protocole typés (additifs, en fin d'enum) ; le serveur injecte un contexte d'env dans chaque volet et journalise les volets agents ; une CLI `wimux agent` parle au daemon en JSON ; un skill enseigne la boucle à Claude ; la GUI reflète les volets créés en CLI via un compteur de révision de layout + réattachement.

**Tech Stack:** Rust (workspace : `wimux-protocol` postcard/serde, `wimux-server` portable-pty/ConPTY, `wimux-cli`), TypeScript/Vite + Tauri v2 (`wimux-gui`).

## Global Constraints

- **Compat protocole postcard** : toute nouvelle variante d'enum ou nouveau champ de struct s'ajoute **EN FIN** (indexation par position). Ne jamais insérer au milieu.
- **Daemon persistant** : après tout changement de `wimux-protocol` ou `wimux-server`, **rebuild release + redémarrer le daemon détaché** (`wimux kill-server` puis relance), sinon échec silencieux.
- **Qualité** : `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` verts ; `npm run build` OK dans `wimux-gui`.
- **Plateforme** : Windows (ConPTY, Named Pipe `\\.\pipe\wimux-<user>`, `%LOCALAPPDATA%`).
- **Emplacement des journaux** : `%LOCALAPPDATA%\wimux\logs\<session>\<pane_id>.log` (nom de session assaini).
- **Nommage** : namespace CLI `wimux agent <verbe>` ; env `WIMUX_SESSION`, `WIMUX_PANE`, `WIMUX_PIPE`.
- **Langue** : commentaires/messages en français, cohérents avec le code existant.

---

## File Structure

**Créés :**
- `skills/wimux/SKILL.md` — instructions du skill pour Claude.
- `skills/wimux/references/commands.md` — référence détaillée des commandes.

**Modifiés :**
- `crates/wimux-protocol/src/lib.rs` — `PaneInfo` + variantes `ClientMessage`/`ServerMessage` + champ `layout_rev` sur `SessionInfo` (tous en fin).
- `crates/wimux-server/src/pane.rs` — `PaneSpawnCtx`, injection d'env, journalisation (`log`/`log_path` sur `PaneState`, tee dans `reader_loop`), `Pane::spawn`/`spawn_command` signatures.
- `crates/wimux-server/src/session.rs` — threading du nom de session au spawn, `spawn_pane`/`capture_pane`/`pane_infos`/`send_keys_pane`/`kill_pane`, compteur `layout_rev`.
- `crates/wimux-server/src/daemon.rs` — handlers des nouveaux messages ; `layout_rev` dans `list()`.
- `crates/wimux-cli/src/main.rs` — namespace `wimux agent`, parsing, JSON, lecture/tail + dé-ANSI du journal.
- `wimux-gui/src-tauri/src/lib.rs` — `SessionDto` += `layout_rev`.
- `wimux-gui/src/main.ts` — détection de changement de `layout_rev` → réattachement.
- `README.md` (ou `wimux-gui/README` selon l'emplacement) — section d'installation du skill.

**Interfaces clés (verrouillées ici) :**

```rust
// wimux-protocol
pub struct PaneInfo {
    pub pane_id: u64,
    pub cwd: Option<String>,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub log_path: Option<String>,
}
// SessionInfo gagne (en fin) : pub layout_rev: u64,
// ClientMessage gagne (en fin) :
//   SpawnPane { session: String, from_pane: Option<u64>, dir: SplitDir, cwd: Option<String>, program: String, args: Vec<String> }
//   CapturePane { session: String, pane: u64 }
//   ListPanes { session: String }
//   SendKeysPane { session: String, pane: u64, keys: Vec<u8> }
//   KillPane { session: String, pane: u64 }
// ServerMessage gagne (en fin) :
//   PaneSpawned { pane_id: u64 }
//   PaneCapture(String)
//   PaneList(Vec<PaneInfo>)

// wimux-server / pane.rs
pub struct PaneSpawnCtx { pub session: String, pub log: bool }
impl PaneSpawnCtx { pub fn shell(session: &str) -> Self; pub fn agent(session: &str) -> Self }
impl Pane {
    pub fn spawn(cols: u16, rows: u16, shell: &str, notifier: Arc<Notifier>, ctx: PaneSpawnCtx) -> Result<Arc<Pane>>;
    pub fn spawn_command(cols: u16, rows: u16, program: &str, args: &[String], cwd: Option<&str>, notifier: Arc<Notifier>, ctx: PaneSpawnCtx) -> Result<Arc<Pane>>;
    pub fn log_path(&self) -> Option<String>;
}

// wimux-server / session.rs
impl Session {
    pub fn spawn_pane(&self, from_pane: Option<u64>, dir: SplitDir, cwd: Option<&str>, program: &str, args: &[String]) -> Option<u64>;
    pub fn capture_pane(&self, pane_id: u64) -> Option<String>;
    pub fn pane_infos(&self) -> Vec<wimux_protocol::PaneInfo>;
    pub fn send_keys_pane(&self, pane_id: u64, bytes: &[u8]) -> bool;
    pub fn kill_pane(&self, pane_id: u64) -> bool;
    pub fn layout_rev(&self) -> u64;
}
```

---

## Phase A1.1 — Protocole

### Task 1 : Nouveaux messages + `PaneInfo` + `layout_rev`

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Test: `crates/wimux-protocol/src/lib.rs` (module `#[cfg(test)]` existant)

**Interfaces:**
- Produces: `PaneInfo`, les variantes `SpawnPane`/`CapturePane`/`ListPanes`/`SendKeysPane`/`KillPane` (ClientMessage), `PaneSpawned`/`PaneCapture`/`PaneList` (ServerMessage), `SessionInfo.layout_rev`.

- [ ] **Step 1 : Écrire le test de round-trip qui échoue**

Ajouter dans le module de tests de `crates/wimux-protocol/src/lib.rs` :

```rust
#[test]
fn aller_retour_spawn_pane_et_pane_list() {
    let msg = ClientMessage::SpawnPane {
        session: "s".into(),
        from_pane: Some(3),
        dir: SplitDir::LeftRight,
        cwd: Some("C:\\repo".into()),
        program: "claude".into(),
        args: vec!["-p".into(), "tache".into()],
    };
    let bytes = postcard::to_allocvec(&msg).unwrap();
    match postcard::from_bytes::<ClientMessage>(&bytes).unwrap() {
        ClientMessage::SpawnPane { program, args, from_pane, .. } => {
            assert_eq!(program, "claude");
            assert_eq!(args, vec!["-p".to_string(), "tache".to_string()]);
            assert_eq!(from_pane, Some(3));
        }
        _ => panic!("variante inattendue"),
    }

    let info = PaneInfo {
        pane_id: 7,
        cwd: Some("C:\\repo".into()),
        running: true,
        exit_code: None,
        log_path: Some("C:\\log\\7.log".into()),
    };
    let reply = ServerMessage::PaneList(vec![info.clone()]);
    let bytes = postcard::to_allocvec(&reply).unwrap();
    match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
        ServerMessage::PaneList(v) => assert_eq!(v[0], info),
        _ => panic!("variante inattendue"),
    }
}
```

- [ ] **Step 2 : Lancer le test pour vérifier l'échec de compilation**

Run: `cargo test -p wimux-protocol aller_retour_spawn_pane_et_pane_list`
Expected: FAIL — `SpawnPane` / `PaneInfo` / `PaneList` n'existent pas (erreur de compilation).

- [ ] **Step 3 : Ajouter `PaneInfo`**

Après la déclaration de `NotificationInfo` (vers la ligne 178 de `lib.rs`) :

```rust
/// Résumé d'un volet, pour l'orchestration agent (A1). Renvoyé par `ListPanes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Identifiant global du volet.
    pub pane_id: u64,
    /// cwd courant (dernier OSC 7 capté), `None` si inconnu.
    pub cwd: Option<String>,
    /// Le processus du volet est-il encore vivant ?
    pub running: bool,
    /// Code de sortie si terminé, `None` s'il tourne encore.
    pub exit_code: Option<i32>,
    /// Chemin du fichier journal si ce volet est journalisé (volet agent).
    pub log_path: Option<String>,
}
```

- [ ] **Step 4 : Ajouter `layout_rev` EN FIN de `SessionInfo`**

Dans `struct SessionInfo`, après le champ `pub pinned: bool,` :

```rust
    /// Compteur de révision de la topologie de volets (A1) : bumpé à chaque
    /// création/fermeture de volet. La GUI le compare pour se réattacher et
    /// refléter en direct les volets créés via la CLI.
    pub layout_rev: u64,
```

- [ ] **Step 5 : Ajouter les variantes `ClientMessage` EN FIN de l'enum**

Juste avant l'accolade fermante de `enum ClientMessage` :

```rust
    /// A1 : découpe la fenêtre active de `session` (à partir de `from_pane`, défaut
    /// volet actif) et lance `program`/`args` dans le nouveau volet (journalisé).
    SpawnPane {
        session: String,
        from_pane: Option<u64>,
        dir: SplitDir,
        cwd: Option<String>,
        program: String,
        args: Vec<String>,
    },
    /// A1 : capture le contenu visible du volet `pane` de `session`.
    CapturePane { session: String, pane: u64 },
    /// A1 : liste les volets de `session` (structuré).
    ListPanes { session: String },
    /// A1 : envoie des octets au volet `pane` de `session`.
    SendKeysPane { session: String, pane: u64, keys: Vec<u8> },
    /// A1 : ferme le volet `pane` de `session`.
    KillPane { session: String, pane: u64 },
```

- [ ] **Step 6 : Ajouter les variantes `ServerMessage` EN FIN de l'enum**

Juste avant l'accolade fermante de `enum ServerMessage` :

```rust
    /// A1 : réponse à `SpawnPane` — identifiant du volet créé.
    PaneSpawned { pane_id: u64 },
    /// A1 : réponse à `CapturePane` — contenu visible du volet.
    PaneCapture(String),
    /// A1 : réponse à `ListPanes`.
    PaneList(Vec<PaneInfo>),
```

- [ ] **Step 7 : Compléter les constructions de `SessionInfo` des tests existants**

L'ajout de `layout_rev` casse les constructions littérales de `SessionInfo` dans les tests. Chercher chaque `SessionInfo {` du module de tests et ajouter `layout_rev: 0,` (les 3 constructions, cf. lignes ~597, ~630, ~840). Exemple :

```rust
        SessionInfo {
            // ... champs existants ...
            color: None,
            pinned: false,
            layout_rev: 0,
        }
```

- [ ] **Step 8 : Lancer les tests du crate**

Run: `cargo test -p wimux-protocol`
Expected: PASS (le nouveau test + tous les existants).

- [ ] **Step 9 : fmt + clippy**

Run: `cargo fmt -p wimux-protocol && cargo clippy -p wimux-protocol -- -D warnings`
Expected: aucun warning.

- [ ] **Step 10 : Commit**

```bash
git add crates/wimux-protocol/src/lib.rs
git commit -m "feat(agent): protocole A1 — PaneInfo, SpawnPane/CapturePane/ListPanes/SendKeysPane/KillPane, layout_rev

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase A1.2 — Serveur

### Task 2 : `PaneSpawnCtx` + injection d'env de contexte

**Files:**
- Modify: `crates/wimux-server/src/pane.rs` (struct `PaneState`, `Pane::spawn`/`spawn_command`, imports)
- Modify: `crates/wimux-server/src/session.rs` (call sites), `crates/wimux-server/src/window.rs` (tests)
- Test: `crates/wimux-server/src/session.rs` (module de tests)

**Interfaces:**
- Produces: `PaneSpawnCtx { session, log }` + constructeurs ; `Pane::spawn`/`spawn_command` avec paramètre `ctx`.
- Consumes: rien de nouveau.

- [ ] **Step 1 : (infra — pas de test comportemental ici)**

L'injection d'env n'est **observable** qu'à travers un volet créé par `spawn_pane`
(Task 4). Cette task est donc de l'**infra** : on la valide par la **compilation**
et la **non-régression** des tests existants. Le test d'injection d'env
(`spawn_pane_injecte_wimux_session_dans_l_env`) vit en **Task 4**, où
`spawn_pane`/`capture_pane` existent. Les steps 3–9 ci-dessous n'ajoutent que
l'infra d'env (aucun nouveau test isolable).

- [ ] **Step 3 : Ajouter les imports et `PaneSpawnCtx` dans `pane.rs`**

En tête de `pane.rs`, ajouter aux imports :

```rust
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use wimux_protocol::transport::user_pipe_name;
```

Après `pub type PaneId = u64;` (vers la ligne 52) :

```rust
/// Contexte de spawn d'un volet (A1) : le nom de session (pour l'env de contexte)
/// et un drapeau de journalisation (posé pour les volets agents).
#[derive(Clone)]
pub struct PaneSpawnCtx {
    pub session: String,
    pub log: bool,
}

impl PaneSpawnCtx {
    /// Volet shell ordinaire : env de contexte, pas de journal.
    pub fn shell(session: &str) -> Self {
        Self { session: session.to_string(), log: false }
    }
    /// Volet agent (A1) : env de contexte + journalisation.
    pub fn agent(session: &str) -> Self {
        Self { session: session.to_string(), log: true }
    }
}

/// Ouvre (crée) le fichier journal d'un volet agent sous
/// `%LOCALAPPDATA%\wimux\logs\<session-assaini>\<pane_id>.log`. Best-effort :
/// renvoie `None` si `%LOCALAPPDATA%` est absent ou l'ouverture échoue.
fn open_pane_log(session: &str, pane_id: PaneId) -> Option<(File, String)> {
    let sanitized: String = session
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let base = std::env::var_os("LOCALAPPDATA")?;
    let dir = PathBuf::from(base).join("wimux").join("logs").join(sanitized);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{pane_id}.log"));
    let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    Some((file, path.to_string_lossy().into_owned()))
}
```

- [ ] **Step 4 : Ajouter `log`/`log_path` à `PaneState`**

Dans `struct PaneState`, après `sniffer: Osc7Sniffer,` :

```rust
    /// Fichier journal du volet (A1), `None` si non journalisé.
    log: Option<File>,
    /// Chemin du journal, exposé via `pane_infos` (A1).
    log_path: Option<String>,
```

- [ ] **Step 5 : Réécrire `Pane::spawn` et `Pane::spawn_command` avec `ctx`**

Remplacer la méthode `spawn` :

```rust
    /// Crée un volet exécutant `shell` (jeton unique, sans args). Cas particulier
    /// de [`Pane::spawn_command`].
    pub fn spawn(
        cols: u16,
        rows: u16,
        shell: &str,
        notifier: Arc<Notifier>,
        ctx: PaneSpawnCtx,
    ) -> Result<Arc<Pane>> {
        Pane::spawn_command(cols, rows, shell, &[], None, notifier, ctx)
    }
```

Dans `spawn_command`, remplacer la signature et le corps jusqu'à la construction du `Pane` :

```rust
    pub fn spawn_command(
        cols: u16,
        rows: u16,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        notifier: Arc<Notifier>,
        ctx: PaneSpawnCtx,
    ) -> Result<Arc<Pane>> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        // Allouer l'id AVANT la CommandBuilder pour pouvoir l'injecter en env.
        let id = NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("ouverture de la pseudo-console")?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // Contexte d'orchestration (A1) : un process lancé dans ce volet sait où il tourne.
        cmd.env("WIMUX_SESSION", &ctx.session);
        cmd.env("WIMUX_PANE", id.to_string());
        cmd.env("WIMUX_PIPE", user_pipe_name());

        let child = pair.slave.spawn_command(cmd).context("lancement du programme")?;
        let reader = pair.master.try_clone_reader().context("clonage du lecteur PTY")?;
        let writer = pair.master.take_writer().context("prise de l'écrivain PTY")?;
        drop(pair.slave);

        // Journalisation (A1) : uniquement pour les volets agents.
        let (log, log_path) = if ctx.log {
            match open_pane_log(&ctx.session, id) {
                Some((f, p)) => (Some(f), Some(p)),
                None => (None, None),
            }
        } else {
            (None, None)
        };

        let pane = Arc::new(Pane {
            id,
            state: Mutex::new(PaneState {
                terminal: Terminal::new(cols, rows),
                writer,
                master: Some(pair.master),
                child: Some(child),
                cols,
                rows,
                exit_code: None,
                copy: None,
                subscribers: Vec::new(),
                cwd: None,
                sniffer: Osc7Sniffer::default(),
                log,
                log_path,
            }),
            notifier,
        });

        let reader_pane = Arc::clone(&pane);
        std::thread::spawn(move || reader_loop(reader_pane, reader));
        let waiter_pane = Arc::clone(&pane);
        std::thread::spawn(move || wait_for_exit(waiter_pane));
        Ok(pane)
    }

    /// Chemin du fichier journal du volet (A1), `None` si non journalisé.
    pub fn log_path(&self) -> Option<String> {
        self.state.lock().unwrap().log_path.clone()
    }
```

- [ ] **Step 6 : Mettre à jour les call sites dans `session.rs`**

Remplacer `spawn_shell_pane_with` (free fn, bas du fichier) :

```rust
fn spawn_shell_pane_with(
    cols: u16,
    rows: u16,
    shell: &str,
    notifier: &Arc<Notifier>,
    session: &str,
) -> Result<Arc<Pane>> {
    let ctx = crate::pane::PaneSpawnCtx::shell(session);
    match osc7_prompt_injection(shell) {
        Some(args) => {
            Pane::spawn_command(cols, rows, shell, &args, None, Arc::clone(notifier), ctx)
        }
        None => Pane::spawn(cols, rows, shell, Arc::clone(notifier), ctx),
    }
}
```

Dans `Session::new`, remplacer l'appel :
`let pane = spawn_shell_pane_with(cols, content_rows(rows), shell, &notifier, &name)?;`

Dans `Session::new_agent`, remplacer l'appel `Pane::spawn_command(...)` :

```rust
        let pane = Pane::spawn_command(
            cols,
            content_rows(rows),
            program,
            args,
            cwd,
            Arc::clone(&notifier),
            crate::pane::PaneSpawnCtx::shell(&name),
        )?;
```

Dans `Session::spawn_shell_pane` (méthode) :

```rust
    fn spawn_shell_pane(&self, cols: u16, rows: u16) -> Result<Arc<Pane>> {
        let name = self.name();
        spawn_shell_pane_with(cols, rows, &self.shell, &self.notifier, &name)
    }
```

Dans `Session::split`, remplacer `Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier))` :

```rust
        let new_pane = Pane::spawn(
            1,
            1,
            &self.shell,
            Arc::clone(&self.notifier),
            crate::pane::PaneSpawnCtx::shell(&self.name()),
        );
```

Dans `Session::new_window`, remplacer `Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier))` par le même appel à 5 arguments (avec `PaneSpawnCtx::shell(&self.name())`).

- [ ] **Step 7 : Mettre à jour les call sites `Pane::spawn` des tests**

Dans `window.rs`, `dummy_pane` :

```rust
    fn dummy_pane() -> Arc<Pane> {
        Pane::spawn(
            10,
            5,
            "cmd.exe",
            crate::pane::Notifier::new(),
            crate::pane::PaneSpawnCtx::shell("test"),
        )
        .unwrap()
    }
```

Chercher tout autre `Pane::spawn(` / `Pane::spawn_command(` dans les tests (`rg "Pane::spawn" crates/wimux-server`) et ajouter l'argument `PaneSpawnCtx::shell("test")` (resp. le `ctx` adéquat).

- [ ] **Step 8 : Compiler + non-régression**

Run: `cargo build -p wimux-server && cargo test -p wimux-server`
Expected: compile ; **tous les tests existants** restent verts (aucune régression). Pas de nouveau test comportemental dans cette task (cf. Step 1).

- [ ] **Step 9 : Commit**

```bash
git add crates/wimux-server/src/pane.rs crates/wimux-server/src/session.rs crates/wimux-server/src/window.rs
git commit -m "feat(agent): PaneSpawnCtx + injection env WIMUX_SESSION/PANE/PIPE au spawn de volet

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 3 : Journalisation des volets agents (tee PTY → fichier)

**Files:**
- Modify: `crates/wimux-server/src/pane.rs` (`reader_loop`)
- Test: `crates/wimux-server/src/session.rs` (module de tests)

**Interfaces:**
- Consumes: `PaneState.log` (Task 2).

- [ ] **Step 1 : (infra — test comportemental en Task 4)**

Le tee n'est **observable** qu'à travers un volet agent créé par `spawn_pane`
(Task 4). Cette task ajoute uniquement le tee (infra) ; le test de journalisation
(`spawn_pane_journalise_la_sortie`) vit en **Task 4**. Validation ici :
compilation + non-régression.

- [ ] **Step 2 : Ajouter le tee dans `reader_loop`**

Dans `reader_loop` (`pane.rs`), à l'intérieur du bloc `Ok(n) =>`, juste après `st.terminal.advance(&buf[..n]);` :

```rust
                    // Journalisation (A1) : tee des octets bruts vers le fichier.
                    if let Some(f) = st.log.as_mut() {
                        let _ = f.write_all(&buf[..n]);
                    }
```

(`use std::io::Write` est déjà importé en tête de `pane.rs`.)

- [ ] **Step 3 : Compiler + non-régression**

Run: `cargo build -p wimux-server && cargo test -p wimux-server`
Expected: compile ; tests existants verts. Le test comportemental de journalisation est en Task 4.

- [ ] **Step 4 : Commit**

```bash
git add crates/wimux-server/src/pane.rs
git commit -m "feat(agent): journalisation par volet — tee du flux PTY vers le fichier journal

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 4 : Méthodes `Session` d'orchestration + `layout_rev`

**Files:**
- Modify: `crates/wimux-server/src/session.rs` (champ `layout_rev`, méthodes)
- Test: `crates/wimux-server/src/session.rs`

**Interfaces:**
- Produces: `Session::spawn_pane`, `capture_pane`, `pane_infos`, `send_keys_pane`, `kill_pane`, `layout_rev`.
- Consumes: `Window::pane`, `Window::pane_ids`, `Window::split_pane`, `Window::close_pane`, `PaneSpawnCtx::agent`, `PaneInfo`.

- [ ] **Step 1 : Écrire le test des méthodes (échoue)**

Dans le module de tests de `session.rs` (cette task introduit `spawn_pane`/
`capture_pane`/`pane_infos`, donc les tests d'env — Task 2 — et de journal —
Task 3 — deviennent observables et vivent ici) :

```rust
/// Sonde `capture_pane` jusqu'à ce qu'il contienne `needle`, dans la limite du délai.
fn poll_capture_contains(s: &Session, pane_id: u64, needle: &str, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if let Some(txt) = s.capture_pane(pane_id) {
            if txt.contains(needle) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn spawn_pane_injecte_wimux_session_dans_l_env() {
    // Vérifie l'injection d'env de Task 2 (observable seulement via spawn_pane).
    let s = Session::new("envtest".into(), 80, 24, "cmd.exe").unwrap();
    let id = s
        .spawn_pane(
            None,
            SplitDir::LeftRight,
            None,
            "cmd.exe",
            &["/c".into(), "echo".into(), "%WIMUX_SESSION%".into()],
        )
        .expect("spawn_pane doit renvoyer un id");
    assert!(
        poll_capture_contains(&s, id, "envtest", 20),
        "la sortie du volet doit contenir le nom de session injecté"
    );
    s.kill();
}

#[test]
fn spawn_pane_journalise_la_sortie() {
    // Vérifie le tee de journalisation de Task 3.
    let s = Session::new("logtest".into(), 80, 24, "cmd.exe").unwrap();
    let id = s
        .spawn_pane(
            None,
            SplitDir::LeftRight,
            None,
            "cmd.exe",
            &["/c".into(), "echo".into(), "HELLO_LOG".into()],
        )
        .expect("spawn_pane id");
    let info = s
        .pane_infos()
        .into_iter()
        .find(|p| p.pane_id == id)
        .expect("le volet doit être listé");
    let path = info.log_path.expect("un volet agent doit être journalisé");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if content.contains("HELLO_LOG") {
                found = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    s.kill();
    let _ = std::fs::remove_file(&path);
    assert!(found, "le journal doit contenir la sortie du volet");
}

#[test]
fn spawn_pane_ajoute_un_volet_et_bump_layout_rev() {
    let s = Session::new("orch".into(), 80, 24, "cmd.exe").unwrap();
    let rev0 = s.layout_rev();
    let before = s.pane_infos().len();
    let id = s
        .spawn_pane(None, SplitDir::LeftRight, None, "cmd.exe", &[])
        .expect("un id de volet");
    assert_eq!(s.pane_infos().len(), before + 1, "un volet en plus");
    assert!(s.pane_infos().iter().any(|p| p.pane_id == id));
    assert!(s.layout_rev() > rev0, "layout_rev doit être bumpé");
    // kill_pane retire le volet et bump encore.
    let rev1 = s.layout_rev();
    assert!(s.kill_pane(id), "kill_pane doit trouver le volet");
    assert!(s.layout_rev() > rev1);
    s.kill();
}

#[test]
fn send_keys_pane_cible_le_bon_volet() {
    let s = Session::new("sk".into(), 80, 24, "cmd.exe").unwrap();
    let id = s
        .spawn_pane(None, SplitDir::LeftRight, None, "cmd.exe", &[])
        .unwrap();
    // Écrire "echo PONG\r" dans le volet agent, pas ailleurs.
    assert!(s.send_keys_pane(id, b"echo PONG\r\n"));
    assert!(
        poll_capture_contains(&s, id, "PONG", 20),
        "la frappe doit atteindre le volet ciblé"
    );
    assert!(!s.send_keys_pane(999_999, b"x"), "id inconnu -> false");
    s.kill();
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server spawn_pane_ajoute_un_volet_et_bump_layout_rev`
Expected: FAIL — méthodes absentes.

- [ ] **Step 3 : Ajouter le champ `layout_rev`**

Dans `struct Session`, après `pinned: AtomicBool,` :

```rust
    /// Compteur de révision de topologie de volets (A1) : bumpé aux créations/
    /// fermetures de volet ; lu par la GUI pour se réattacher et refléter les
    /// volets créés en CLI.
    layout_rev: AtomicU64,
```

Dans les DEUX constructeurs (`Session::new` et `Session::new_agent`), ajouter au littéral `Session { ... }`, après `pinned: AtomicBool::new(false),` :

```rust
            layout_rev: AtomicU64::new(0),
```

- [ ] **Step 4 : Ajouter les accesseurs `layout_rev` + import `PaneInfo`**

En tête de `session.rs`, étendre l'import protocole :

```rust
use wimux_protocol::{AgentStatus, Frame, LayoutNode, PaneInfo, WindowInfo};
```

Ajouter dans `impl Session` (près des autres accesseurs) :

```rust
    /// Révision courante de la topologie de volets (A1).
    pub fn layout_rev(&self) -> u64 {
        self.layout_rev.load(Ordering::Relaxed)
    }

    /// Incrémente la révision de topologie (à chaque création/fermeture de volet).
    fn bump_layout_rev(&self) {
        self.layout_rev.fetch_add(1, Ordering::Relaxed);
    }
```

- [ ] **Step 5 : Ajouter les méthodes d'orchestration**

Dans `impl Session` (par ex. après `list_panes_text`) :

```rust
    /// A1 : découpe la fenêtre active à partir de `from_pane` (défaut : volet
    /// actif) et lance `program`/`args` (journalisé) dans le nouveau volet.
    /// Renvoie l'id du volet créé, ou `None` si le spawn échoue / plus de fenêtre.
    pub fn spawn_pane(
        &self,
        from_pane: Option<u64>,
        dir: SplitDir,
        cwd: Option<&str>,
        program: &str,
        args: &[String],
    ) -> Option<u64> {
        // Spawn HORS verrou (lance un processus).
        let ctx = crate::pane::PaneSpawnCtx::agent(&self.name());
        let new_pane =
            Pane::spawn_command(1, 1, program, args, cwd, Arc::clone(&self.notifier), ctx).ok()?;
        let new_id = new_pane.id;
        {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            let Some(win) = inner.windows.get_mut(aw) else {
                drop(inner);
                new_pane.kill();
                return None;
            };
            let target = from_pane
                .filter(|id| win.pane(*id).is_some())
                .unwrap_or_else(|| win.active_pane_id());
            win.split_pane(target, dir, Arc::clone(&new_pane));
            let area = content_area(inner.cols, inner.rows);
            inner.windows[aw].reflow(area);
        }
        self.bump_layout_rev();
        self.notifier.bump();
        Some(new_id)
    }

    /// A1 : contenu visible du volet `pane_id` (n'importe quelle fenêtre), texte.
    pub fn capture_pane(&self, pane_id: u64) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.windows.iter().find_map(|w| w.pane(pane_id)).map(|p| p.capture_text())
    }

    /// A1 : inventaire structuré des volets de toutes les fenêtres.
    pub fn pane_infos(&self) -> Vec<PaneInfo> {
        let inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        for win in &inner.windows {
            for id in win.pane_ids() {
                if let Some(p) = win.pane(id) {
                    out.push(PaneInfo {
                        pane_id: id,
                        cwd: p.cwd(),
                        running: p.is_alive(),
                        exit_code: p.exit_code().map(|c| c as i32),
                        log_path: p.log_path(),
                    });
                }
            }
        }
        out
    }

    /// A1 : envoie des octets au volet `pane_id`. `false` si l'id est introuvable.
    pub fn send_keys_pane(&self, pane_id: u64, bytes: &[u8]) -> bool {
        let pane = {
            let inner = self.inner.lock().unwrap();
            inner.windows.iter().find_map(|w| w.pane(pane_id))
        };
        match pane {
            Some(p) => {
                p.send_input(bytes);
                true
            }
            None => false,
        }
    }

    /// A1 : ferme le volet `pane_id` (dans quelque fenêtre que ce soit). Retire la
    /// fenêtre si elle devient vide. `false` si l'id est introuvable.
    pub fn kill_pane(&self, pane_id: u64) -> bool {
        let found = {
            let mut inner = self.inner.lock().unwrap();
            let mut hit = None;
            for (wi, win) in inner.windows.iter_mut().enumerate() {
                if win.pane(pane_id).is_some() {
                    let empty = win.close_pane(pane_id);
                    hit = Some((wi, empty));
                    break;
                }
            }
            if let Some((wi, empty)) = hit {
                if empty {
                    inner.windows.remove(wi);
                    if inner.active_window >= inner.windows.len() && !inner.windows.is_empty() {
                        inner.active_window = inner.windows.len() - 1;
                    }
                }
                true
            } else {
                false
            }
        };
        if found {
            self.reflow();
            self.bump_layout_rev();
            self.notifier.bump();
        }
        found
    }
```

- [ ] **Step 6 : Bumper `layout_rev` sur les autres chemins de création/fermeture**

Dans `Session::split`, `Session::new_window`, `Session::close_active_pane`, ajouter `self.bump_layout_rev();` juste avant le `self.notifier.bump();` final (chemins CLI/TUI ; les chemins `gui_*` ne bumpent PAS, ils poussent déjà `WindowLayout`). Exemple pour `close_active_pane` :

```rust
        self.reflow();
        self.bump_layout_rev();
        self.notifier.bump();
```

- [ ] **Step 7 : Lancer les tests serveur (dont ceux des Tasks 2/3)**

Run: `cargo test -p wimux-server spawn_pane_ ; cargo test -p wimux-server send_keys_pane_cible ; cargo test -p wimux-server spawn_pane_journalise ; cargo test -p wimux-server spawn_pane_injecte`
Expected: PASS pour tous.

- [ ] **Step 8 : Suite complète + fmt + clippy**

Run: `cargo test -p wimux-server && cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings`
Expected: vert, aucun warning.

- [ ] **Step 9 : Commit**

```bash
git add crates/wimux-server/src/session.rs
git commit -m "feat(agent): Session::spawn_pane/capture_pane/pane_infos/send_keys_pane/kill_pane + layout_rev

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 5 : Handlers daemon + `layout_rev` dans `list()`

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs` (handlers, `list()`)
- Test: `crates/wimux-server/src/daemon.rs` ou fichier d'intégration existant (`tests`/`gui_mode.rs`) selon convention

**Interfaces:**
- Consumes: `Session::spawn_pane`/`capture_pane`/`pane_infos`/`send_keys_pane`/`kill_pane`/`layout_rev` ; variantes protocole (Task 1).

- [ ] **Step 1 : Ajouter `layout_rev` dans `Server::list`**

Dans `daemon.rs`, dans la construction de `SessionInfo` (méthode `list`), après `pinned: s.pinned(),` :

```rust
                    layout_rev: s.layout_rev(),
```

- [ ] **Step 2 : Écrire un test de handler (échoue)**

Ajouter un test d'intégration serveur qui exerce le chemin complet via `Server`. Si un harnais `Server`-direct existe (ex. `Server::create_session`), tester les méthodes de session à travers le serveur ; sinon test unitaire minimal sur `list()` :

```rust
#[test]
fn list_expose_layout_rev() {
    let server = Server::new(crate::config::Config::default());
    let s = server.create_session(Some("lr".into()), 80, 24).unwrap();
    let before = server.list().iter().find(|i| i.name == "lr").unwrap().layout_rev;
    s.spawn_pane(None, crate::window::SplitDir::LeftRight, None, "cmd.exe", &[]);
    let after = server.list().iter().find(|i| i.name == "lr").unwrap().layout_rev;
    assert!(after > before, "layout_rev remonté par list() après spawn_pane");
    server.kill("lr");
}
```

(Adapter `Server::new`/`create_session` aux signatures réelles — cf. `daemon.rs`. Si `create_session` renvoie `Arc<Session>`, l'utiliser ; sinon récupérer via `server.get("lr")`.)

- [ ] **Step 3 : Vérifier l'échec**

Run: `cargo test -p wimux-server list_expose_layout_rev`
Expected: FAIL (avant d'ajouter `layout_rev` au `SessionInfo`, ou si `create_session` diffère — ajuster).

- [ ] **Step 4 : Ajouter les handlers dans `handle_client`**

Dans le `match msg { ... }` de `handle_client`, après le bras `ClientMessage::Command { .. }` :

```rust
            ClientMessage::SpawnPane {
                session,
                from_pane,
                dir,
                cwd,
                program,
                args,
            } => {
                let reply = match server.get(&session) {
                    Some(s) => match s.spawn_pane(
                        from_pane,
                        dir.into(),
                        cwd.as_deref(),
                        &program,
                        &args,
                    ) {
                        Some(pane_id) => ServerMessage::PaneSpawned { pane_id },
                        None => ServerMessage::Error("échec du spawn de volet".into()),
                    },
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::CapturePane { session, pane } => {
                let reply = match server.get(&session) {
                    Some(s) => match s.capture_pane(pane) {
                        Some(text) => ServerMessage::PaneCapture(text),
                        None => ServerMessage::Error(format!("volet introuvable : {pane}")),
                    },
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::ListPanes { session } => {
                let reply = match server.get(&session) {
                    Some(s) => ServerMessage::PaneList(s.pane_infos()),
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::SendKeysPane { session, pane, keys } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        if s.send_keys_pane(pane, &keys) {
                            ServerMessage::Ok
                        } else {
                            ServerMessage::Error(format!("volet introuvable : {pane}"))
                        }
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::KillPane { session, pane } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        if s.kill_pane(pane) {
                            ServerMessage::Ok
                        } else {
                            ServerMessage::Error(format!("volet introuvable : {pane}"))
                        }
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

(`dir.into()` : `wimux_protocol::SplitDir` → `crate::window::SplitDir` via le `From` existant, cf. `window.rs`.)

- [ ] **Step 5 : Lancer les tests + suite serveur**

Run: `cargo test -p wimux-server`
Expected: PASS.

- [ ] **Step 6 : fmt + clippy + rebuild release + redémarrage du daemon**

Run:
```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings
cargo build --release
```
Puis redémarrer le daemon détaché (piège) :
```bash
./target/release/wimux.exe kill-server
```
(le prochain appel CLI relancera un daemon à jour).

- [ ] **Step 7 : Commit**

```bash
git add crates/wimux-server/src/daemon.rs
git commit -m "feat(agent): handlers SpawnPane/CapturePane/ListPanes/SendKeysPane/KillPane + layout_rev dans list

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase A1.3 — CLI `wimux agent`

### Task 6 : Parsing des arguments `wimux agent` (pur, testable)

**Files:**
- Modify: `crates/wimux-cli/src/main.rs` (nouveau module `agent`)
- Test: `crates/wimux-cli/src/main.rs` (module de tests)

**Interfaces:**
- Produces: `agent::SpawnArgs`, `agent::parse_spawn`, `agent::parse_target_pane`, `agent::json_escape`, `agent::strip_ansi`.

- [ ] **Step 1 : Écrire les tests de parsing (échouent)**

Ajouter en bas de `main.rs` :

```rust
#[cfg(test)]
mod agent_tests {
    use super::agent::*;
    use wimux_protocol::SplitDir;

    #[test]
    fn parse_spawn_separe_le_programme_apres_double_tiret() {
        let a = parse_spawn(&[
            "--dir".into(), "v".into(),
            "-t".into(), "sess".into(),
            "--from-pane".into(), "4".into(),
            "--".into(), "claude".into(), "-p".into(), "fais X".into(),
        ])
        .unwrap();
        assert_eq!(a.session.as_deref(), Some("sess"));
        assert_eq!(a.from_pane, Some(4));
        assert!(matches!(a.dir, SplitDir::TopBottom));
        assert_eq!(a.program, "claude");
        assert_eq!(a.program_args, vec!["-p".to_string(), "fais X".to_string()]);
    }

    #[test]
    fn parse_spawn_defaut_dir_horizontal_sans_programme_est_erreur() {
        let a = parse_spawn(&["--".into(), "cmd.exe".into()]).unwrap();
        assert!(matches!(a.dir, SplitDir::LeftRight)); // défaut
        assert!(parse_spawn(&["--dir".into(), "h".into()]).is_err()); // pas de programme
    }

    #[test]
    fn json_escape_echappe_backslash_et_guillemets() {
        assert_eq!(json_escape("C:\\a\"b"), "C:\\\\a\\\"b");
    }

    #[test]
    fn strip_ansi_retire_csi_et_osc() {
        assert_eq!(strip_ansi("\x1b[31mrouge\x1b[0m"), "rouge");
        assert_eq!(strip_ansi("\x1b]9;notif\x07texte"), "texte");
    }
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-cli parse_spawn_separe`
Expected: FAIL — module `agent` absent.

- [ ] **Step 3 : Écrire le module `agent` (parsing pur)**

Ajouter dans `main.rs` (après les `use`, avant `fn main`) :

```rust
mod agent {
    use std::io;
    use wimux_protocol::SplitDir;

    /// Arguments analysés de `wimux agent spawn`.
    pub struct SpawnArgs {
        pub session: Option<String>,
        pub from_pane: Option<u64>,
        pub dir: SplitDir,
        pub cwd: Option<String>,
        pub program: String,
        pub program_args: Vec<String>,
    }

    /// Analyse `wimux agent spawn [--dir h|v] [--cwd DIR] [-t SESSION] [--from-pane ID] -- <prog...>`.
    pub fn parse_spawn(args: &[String]) -> io::Result<SpawnArgs> {
        let mut session = None;
        let mut from_pane = None;
        let mut dir = SplitDir::LeftRight; // défaut : côte à côte
        let mut cwd = None;
        let mut i = 0;
        let mut rest: Vec<String> = Vec::new();
        while i < args.len() {
            match args[i].as_str() {
                "--" => {
                    rest = args[i + 1..].to_vec();
                    break;
                }
                "-t" | "--target" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                "--from-pane" | "-p" => {
                    from_pane = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                "--cwd" => {
                    cwd = args.get(i + 1).cloned();
                    i += 2;
                }
                "--dir" => {
                    dir = match args.get(i + 1).map(String::as_str) {
                        Some("v") | Some("vertical") => SplitDir::TopBottom,
                        _ => SplitDir::LeftRight,
                    };
                    i += 2;
                }
                _ => i += 1,
            }
        }
        let program = rest
            .first()
            .cloned()
            .ok_or_else(|| io::Error::other("usage : wimux agent spawn [flags] -- <commande...>"))?;
        Ok(SpawnArgs {
            session,
            from_pane,
            dir,
            cwd,
            program,
            program_args: rest[1..].to_vec(),
        })
    }

    /// Extrait `(session, pane)` de flags `-t SESSION -p PANE` (capture/logs/send/kill).
    pub fn parse_target_pane(args: &[String]) -> (Option<String>, Option<u64>, Vec<String>) {
        let mut session = None;
        let mut pane = None;
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-t" | "--target" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                "-p" | "--pane" => {
                    pane = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                other => {
                    rest.push(other.to_string());
                    i += 1;
                }
            }
        }
        (session, pane, rest)
    }

    /// Échappe une chaîne pour l'insérer dans du JSON (backslash + guillemets + contrôles simples).
    pub fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out
    }

    /// Retire les séquences CSI (`ESC[..lettre`) et OSC (`ESC]..BEL|ST`) pour rendre
    /// un journal lisible. Suffisant pour un flux ligne à ligne.
    pub fn strip_ansi(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(s.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'[' => {
                        i += 2;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                        i += 1;
                    }
                    b']' => {
                        i += 2;
                        while i < bytes.len() && bytes[i] != 0x07 {
                            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                        i += 1;
                    }
                    _ => i += 2,
                }
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}
```

- [ ] **Step 4 : Lancer les tests de parsing**

Run: `cargo test -p wimux-cli agent_tests`
Expected: PASS.

- [ ] **Step 5 : Commit**

```bash
git add crates/wimux-cli/src/main.rs
git commit -m "feat(agent): parsing CLI 'wimux agent' (spawn/target) + json_escape + strip_ansi

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 7 : Verbes `wimux agent` (spawn/list/capture/send/kill/whoami)

**Files:**
- Modify: `crates/wimux-cli/src/main.rs` (dispatch + fonctions de commande)

**Interfaces:**
- Consumes: `agent::*` (Task 6) ; protocole (Task 1) ; helpers existants `connect`/`user_pipe_name`/`handshake`/`translate_keys`.

- [ ] **Step 1 : Router `agent` dans `main`**

Dans le `match cmd` de `main()`, ajouter un bras avant le `Some(other)` :

```rust
        Some("agent") => cmd_agent(&args[1..]),
```

- [ ] **Step 2 : Écrire `cmd_agent` et les sous-commandes**

Ajouter (près de `cmd_control`) :

```rust
/// Ouvre une connexion + handshake, ou erreur si aucun serveur.
fn connected() -> io::Result<PipeConn> {
    let conn = connect(&user_pipe_name()).map_err(|_| io::Error::other("aucun serveur en cours"))?;
    handshake(&conn)?;
    Ok(conn)
}

/// Session par défaut : `-t` explicite, sinon `$WIMUX_SESSION`.
fn default_session(explicit: Option<String>) -> io::Result<String> {
    explicit
        .or_else(|| std::env::var("WIMUX_SESSION").ok())
        .ok_or_else(|| io::Error::other("aucune session : passez -t <session> ou lancez depuis un volet wimux"))
}

/// Pane par défaut : `-p` explicite, sinon `$WIMUX_PANE`.
fn default_pane(explicit: Option<u64>) -> io::Result<u64> {
    explicit
        .or_else(|| std::env::var("WIMUX_PANE").ok().and_then(|s| s.parse().ok()))
        .ok_or_else(|| io::Error::other("aucun volet : passez -p <pane> ou lancez depuis un volet wimux"))
}

fn cmd_agent(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("spawn") => agent_spawn(&args[1..]),
        Some("list") => agent_list(&args[1..]),
        Some("capture") => agent_capture(&args[1..]),
        Some("logs") => agent_logs(&args[1..]),
        Some("send") => agent_send(&args[1..]),
        Some("kill") => agent_kill(&args[1..]),
        Some("whoami") => agent_whoami(),
        _ => Err(io::Error::other(
            "usage : wimux agent <spawn|list|logs|capture|send|kill|whoami> ...",
        )),
    }
}

fn agent_spawn(args: &[String]) -> io::Result<()> {
    let a = agent::parse_spawn(args)?;
    let session = default_session(a.session)?;
    let from_pane = a.from_pane.or_else(|| std::env::var("WIMUX_PANE").ok().and_then(|s| s.parse().ok()));
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::SpawnPane {
            session,
            from_pane,
            dir: a.dir,
            cwd: a.cwd,
            program: a.program,
            args: a.program_args,
        },
    )?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneSpawned { pane_id } => {
            println!("{{\"pane_id\":{pane_id}}}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_list(args: &[String]) -> io::Result<()> {
    let (session, _pane, _rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ListPanes { session })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneList(panes) => {
            // JSON array manuel (pas de dépendance serde_json).
            let items: Vec<String> = panes
                .iter()
                .map(|p| {
                    let cwd = p.cwd.as_deref().map(|c| format!("\"{}\"", agent::json_escape(c))).unwrap_or_else(|| "null".into());
                    let log = p.log_path.as_deref().map(|c| format!("\"{}\"", agent::json_escape(c))).unwrap_or_else(|| "null".into());
                    let ec = p.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "null".into());
                    format!(
                        "{{\"pane_id\":{},\"running\":{},\"exit_code\":{},\"cwd\":{},\"log_path\":{}}}",
                        p.pane_id, p.running, ec, cwd, log
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_capture(args: &[String]) -> io::Result<()> {
    let (session, pane, _rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::CapturePane { session, pane })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneCapture(text) => {
            println!("{text}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_send(args: &[String]) -> io::Result<()> {
    let (session, pane, rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    if rest.is_empty() {
        return Err(io::Error::other("aucune touche à envoyer"));
    }
    let keys = translate_keys(&rest);
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::SendKeysPane { session, pane, keys })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_kill(args: &[String]) -> io::Result<()> {
    let (session, pane, _rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::KillPane { session, pane })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn agent_whoami() -> io::Result<()> {
    let session = std::env::var("WIMUX_SESSION").unwrap_or_default();
    let pane = std::env::var("WIMUX_PANE").unwrap_or_default();
    let pipe = std::env::var("WIMUX_PIPE").unwrap_or_else(|_| user_pipe_name());
    println!(
        "{{\"session\":\"{}\",\"pane\":\"{}\",\"pipe\":\"{}\"}}",
        agent::json_escape(&session),
        agent::json_escape(&pane),
        agent::json_escape(&pipe)
    );
    Ok(())
}
```

- [ ] **Step 3 : Compiler**

Run: `cargo build -p wimux-cli`
Expected: compile (`agent_logs` défini en Task 8 — l'ajouter en stub temporaire renvoyant `Ok(())` si nécessaire pour compiler, puis remplacé en Task 8).

Stub temporaire (à mettre pour compiler cette task, remplacé en Task 8) :

```rust
fn agent_logs(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("wimux agent logs : implémenté en Task 8"))
}
```

- [ ] **Step 4 : Test manuel de bout en bout**

Rebuild + relancer un volet wimux, puis dans le volet :
```bash
wimux agent whoami
wimux agent spawn -- cmd.exe /c echo BONJOUR
wimux agent list
wimux agent capture -p <id>
```
Expected: `whoami` renvoie session/pane ; `spawn` renvoie `{"pane_id":N}` ; `list` montre le volet ; `capture` montre `BONJOUR`.

- [ ] **Step 5 : Commit**

```bash
git add crates/wimux-cli/src/main.rs
git commit -m "feat(agent): verbes CLI wimux agent spawn/list/capture/send/kill/whoami (JSON)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 8 : `wimux agent logs` (lecture/tail + dé-ANSI)

**Files:**
- Modify: `crates/wimux-cli/src/main.rs` (remplacer le stub `agent_logs`)

**Interfaces:**
- Consumes: `agent::strip_ansi`, `ListPanes` (pour résoudre `log_path`).

- [ ] **Step 1 : Écrire un test de dé-ANSI ligne à ligne (déjà couvert Task 6)**

Le comportement de dé-ANSI est testé en Task 6 (`strip_ansi_retire_csi_et_osc`). `agent_logs` est une glue I/O (fichier) validée manuellement — pas de test unitaire supplémentaire.

- [ ] **Step 2 : Implémenter `agent_logs`**

Remplacer le stub :

```rust
fn agent_logs(args: &[String]) -> io::Result<()> {
    let (session, pane, rest) = agent::parse_target_pane(args);
    let session = default_session(session)?;
    let pane = default_pane(pane)?;
    let mut tail: Option<usize> = None;
    let mut follow = false;
    let mut raw = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--tail" => {
                tail = rest.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--follow" | "-f" => {
                follow = true;
                i += 1;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    // Résoudre le chemin du journal via ListPanes.
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ListPanes { session: session.clone() })?;
    let mut r: &PipeConn = &conn;
    let path = match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneList(panes) => panes
            .into_iter()
            .find(|p| p.pane_id == pane)
            .and_then(|p| p.log_path)
            .ok_or_else(|| io::Error::other(format!("volet {pane} sans journal")))?,
        ServerMessage::Error(e) => return Err(io::Error::other(e)),
        _ => return Err(io::Error::other("réponse inattendue du serveur")),
    };

    let render = |content: &str| -> String {
        let text = if raw { content.to_string() } else { agent::strip_ansi(content) };
        match tail {
            Some(n) => {
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            }
            None => text,
        }
    };

    // Lecture initiale.
    let mut last_len = 0u64;
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    println!("{}", render(&content));
    last_len = content.as_bytes().len() as u64;

    if !follow {
        return Ok(());
    }
    // Suivi : relire les octets ajoutés au fichier (best-effort, dé-ANSI par bloc).
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let content = std::fs::read(&path).unwrap_or_default();
        if (content.len() as u64) > last_len {
            let slice = &content[last_len as usize..];
            let chunk = String::from_utf8_lossy(slice);
            let text = if raw { chunk.to_string() } else { agent::strip_ansi(&chunk) };
            print!("{text}");
            use std::io::Write as _;
            let _ = io::stdout().flush();
            last_len = content.len() as u64;
        }
    }
}
```

- [ ] **Step 3 : Compiler + test manuel**

Run: `cargo build -p wimux-cli`
Puis dans un volet wimux :
```bash
wimux agent spawn -- cmd.exe /c echo LIGNE_JOURNAL
wimux agent logs -p <id>
```
Expected: le journal affiche `LIGNE_JOURNAL` (dé-ANSI).

- [ ] **Step 4 : Suite CLI + fmt + clippy**

Run: `cargo test -p wimux-cli && cargo fmt -p wimux-cli && cargo clippy -p wimux-cli -- -D warnings`
Expected: vert.

- [ ] **Step 5 : Commit**

```bash
git add crates/wimux-cli/src/main.rs
git commit -m "feat(agent): wimux agent logs — lecture/tail du journal + dé-ANSI

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 9 : Aide `wimux --help` (mention `agent`)

**Files:**
- Modify: `crates/wimux-cli/src/main.rs` (`print_help`)

- [ ] **Step 1 : Ajouter la ligne d'aide**

Dans `print_help`, dans la section `COMMANDES :`, ajouter avant `kill-session` :

```
             agent <sous-cmd>    Orchestration d'agents (spawn/list/logs/capture/send/kill/whoami)\n    \
```

- [ ] **Step 2 : Compiler + commit**

```bash
cargo build -p wimux-cli
git add crates/wimux-cli/src/main.rs
git commit -m "docs(agent): mention 'wimux agent' dans l'aide CLI

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase A1.4 — Skill

### Task 10 : Skill `wimux` + README

**Files:**
- Create: `skills/wimux/SKILL.md`, `skills/wimux/references/commands.md`
- Modify: `README.md` (section installation du skill)

- [ ] **Step 1 : Écrire `skills/wimux/SKILL.md`**

```markdown
---
name: wimux-orchestration
description: Use when running inside a wimux pane and you need to spawn sub-agents in their own terminals and read their output. Provides `wimux agent` commands to create panes running a task, list them, read their logs, capture their screen, send input, and close them.
---

# Orchestrer des agents avec wimux

Tu tournes dans un **volet wimux**. Tu peux créer d'autres volets (« agents »),
chacun lançant une tâche dans son propre terminal, puis lire leur sortie.

## Contexte
- `wimux agent whoami` → `{"session","pane","pipe"}` : ta session et ton volet.
- Les variables d'env `WIMUX_SESSION` / `WIMUX_PANE` sont déjà posées ; les
  commandes `wimux agent` les prennent par défaut (`-t`/`-p` pour surcharger).

## Boucle type
1. **Lancer un agent pour une tâche** (mode print pour un journal = transcript propre) :
   `wimux agent spawn --dir v -- claude -p "<décris la tâche>"`
   → imprime `{"pane_id":N}`. Garde N.
2. **Surveiller** : `wimux agent list` → pour chaque volet `running`/`exit_code`.
3. **Lire la sortie** : `wimux agent logs -p N --tail 50` (ou `--follow`).
4. **Photographier** un agent en TUI plein écran : `wimux agent capture -p N`.
5. **Répondre à une invite** : `wimux agent send -p N "oui" Enter`.
6. **Fermer** : `wimux agent kill -p N`.

## Bonnes pratiques
- Préfère les sous-agents **non-interactifs / print** (`claude -p ...`) : leur
  journal est un transcript linéaire lisible. Pour un agent TUI qui se redessine,
  utilise `capture` plutôt que `logs`.
- Un agent est **terminé** quand `wimux agent list` montre `running:false` et un
  `exit_code`. Le journal ne grossit plus.
- `--dir v` empile (haut/bas), `--dir h` (défaut) place côte à côte.

## Référence
Voir `references/commands.md` pour tous les drapeaux.
```

- [ ] **Step 2 : Écrire `skills/wimux/references/commands.md`**

```markdown
# Référence `wimux agent`

Toutes les commandes prennent `-t <session>` (défaut `$WIMUX_SESSION`) et, quand
un volet est ciblé, `-p <pane>` (défaut `$WIMUX_PANE`).

## spawn
`wimux agent spawn [--dir h|v] [--cwd DIR] [-t SESSION] [--from-pane ID] -- <commande...>`
Découpe la fenêtre active (à partir de `--from-pane`, défaut volet courant) et
lance la commande dans le nouveau volet (journalisé). Sortie : `{"pane_id":N}`.

## list
`wimux agent list [-t SESSION]`
Sortie JSON : `[{"pane_id","running","exit_code","cwd","log_path"}, ...]`.

## logs
`wimux agent logs -p PANE [-t SESSION] [--tail N] [--follow] [--raw]`
Lit le journal du volet (dé-ANSI par défaut ; `--raw` pour les octets bruts ;
`--follow` pour suivre).

## capture
`wimux agent capture -p PANE [-t SESSION]`
Contenu visible (photo) du volet — utile pour un agent TUI.

## send
`wimux agent send -p PANE [-t SESSION] <touches...>`
Envoie des frappes. Jetons : `Enter`, `Tab`, `Space`, `Escape`, `C-<x>` ; le reste
littéral.

## kill
`wimux agent kill -p PANE [-t SESSION]`
Ferme le volet.

## whoami
`wimux agent whoami`
`{"session","pane","pipe"}` — le contexte courant.
```

- [ ] **Step 3 : Ajouter une section README d'installation du skill**

Dans `README.md`, ajouter une section :

```markdown
## Skill Claude (orchestration d'agents)

wimux fournit un skill dans `skills/wimux/` qui apprend à Claude à créer des
volets-agents et lire leur sortie via `wimux agent`. Pour l'activer, lie ou copie
`skills/wimux` dans le dossier des skills de ton client Claude (par ex.
`~/.claude/skills/wimux`), puis lance Claude **depuis un volet wimux**. Vérifie
avec `wimux agent whoami`.
```

- [ ] **Step 4 : Validation manuelle**

Depuis un vrai Claude dans un volet wimux : lancer un sous-agent, lire son journal,
le tuer. Vérifier que la boucle du SKILL.md fonctionne.

- [ ] **Step 5 : Commit**

```bash
git add skills/wimux README.md
git commit -m "docs(agent): skill wimux (SKILL.md + référence) + section README d'installation

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase A1.5 — GUI live (réflexion des volets-agents)

### Task 11 : `layout_rev` → `SessionDto` → réattachement frontend

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs` (`SessionDto`)
- Modify: `wimux-gui/src/main.ts` (type `SessionDto`, `refresh`, réattachement)

**Interfaces:**
- Consumes: `SessionInfo.layout_rev` (Task 1/5).

- [ ] **Step 1 : Ajouter `layout_rev` à `SessionDto` (Rust)**

Dans `wimux-gui/src-tauri/src/lib.rs`, dans `struct SessionDto`, après `pinned: bool,` :

```rust
    layout_rev: u64,
```

Et dans le `.map(|s| SessionDto { ... })` de `list_sessions`, après `pinned: s.pinned,` :

```rust
            layout_rev: s.layout_rev,
```

- [ ] **Step 2 : Ajouter `layout_rev` au type TS**

Dans `wimux-gui/src/main.ts`, dans `type SessionDto = { ... }`, après `pinned: boolean;` :

```typescript
  layout_rev: number;
```

- [ ] **Step 3 : Réattacher quand `layout_rev` change pour la session active**

Dans `main.ts`, déclarer près de `let lastSessions` :

```typescript
let lastLayoutRev = -1; // révision de layout vue pour la session active (A1.5)
```

Ajouter une fonction de réattachement (sans le garde `name === activeSession` de `switchTo`) :

```typescript
// Réattache la session active (réutilisé quand un volet a été créé/fermé hors GUI,
// ex. `wimux agent spawn` : le serveur ne pousse pas WindowLayout à cette
// connexion, on redemande donc un attachement complet). (A1.5)
async function reattachActive() {
  if (!activeSession) return;
  paneManager.reset();
  lastActiveWindow = -1;
  await invoke("attach_session", { session: activeSession }).catch((e) =>
    console.error("reattach:", e),
  );
}
```

Dans `refresh()`, après `renderRail(sessions);` et le bloc de nettoyage de session disparue, ajouter :

```typescript
    // A1.5 : un volet a-t-il été créé/fermé hors GUI ? (layout_rev de la session
    // active a changé) → réattachement pour refléter la nouvelle topologie.
    if (activeSession) {
      const active = sessions.find((s) => s.name === activeSession);
      if (active) {
        if (lastLayoutRev === -1) {
          lastLayoutRev = active.layout_rev;
        } else if (active.layout_rev !== lastLayoutRev) {
          lastLayoutRev = active.layout_rev;
          await reattachActive();
        }
      }
    }
```

Réinitialiser `lastLayoutRev = -1;` dans `switchTo` (au changement de session), juste après `activeSession = name;`, pour ne pas réattacher à tort au premier sondage de la nouvelle session.

- [ ] **Step 4 : Build frontend**

Run: `cd wimux-gui && npm run build`
Expected: build TypeScript OK.

- [ ] **Step 5 : Test manuel bout-en-bout**

Rebuild release + relancer daemon + GUI. Attaché à une session dans la GUI, depuis
un volet de CETTE session (terminal) :
```bash
wimux agent spawn -- cmd.exe /k echo AGENT_VISIBLE
```
Expected : dans la seconde qui suit, le nouveau volet apparaît **en direct** dans la
GUI (réattachement déclenché par le changement de `layout_rev`). `wimux agent kill -p <id>`
→ le volet disparaît de la GUI.

- [ ] **Step 6 : Commit**

```bash
git add wimux-gui/src-tauri/src/lib.rs wimux-gui/src/main.ts
git commit -m "feat(agent): GUI reflète en direct les volets créés en CLI (layout_rev + réattachement)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Revue finale (toute la branche)

- [ ] **Step 1 : Suite complète + qualité**

Run:
```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cd wimux-gui && npm run build
```
Expected: tout vert.

- [ ] **Step 2 : Rebuild release + redémarrage daemon + démo complète**

Rebuild, redémarrer le daemon, lancer la GUI, ouvrir une session, y lancer Claude
(ou un shell) et exécuter la boucle du skill (`spawn` → `list` → `logs` → `kill`),
en vérifiant l'apparition live dans la GUI.

- [ ] **Step 3 : Mettre à jour la mémoire de projet** (`wimux-etat-avancement.md`, `wimux-parite-cmux.md`) : A1 fait (CLI `wimux agent` + skill + GUI live).

---

## Notes de conception (rappels)

- **Capture = grille visible** (pas de scrollback) : l'historique passe par le
  **journal** (`logs`). Le paramètre `lines` de la spec initiale est abandonné (YAGNI).
- **Journal brut, dé-ANSI à la lecture** : le fichier stocke les octets PTY fidèles ;
  `wimux agent logs` dé-ANSI (sauf `--raw`).
- **`layout_rev`** n'est bumpé que sur les chemins **non-GUI** (`spawn_pane`,
  `kill_pane`, `split`, `new_window`, `close_active_pane`) : les actions GUI
  poussent déjà `WindowLayout`, inutile de les faire réattacher.
- **Env de contexte sur TOUS les volets** (shell comme agent) ; **journal sur les
  volets agents seulement** (`PaneSpawnCtx::agent`).
```
