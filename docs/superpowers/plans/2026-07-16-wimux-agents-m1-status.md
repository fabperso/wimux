# wimux multi-agents M1 — Statut d'agent : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Doter le serveur d'une notion de **session agent** et du **calcul de son statut** (Travaille / Au repos / Attention / Terminé / Erreur), exposé sur `SessionInfo` via le sondage `List` existant — couche serveur pure, sans création exposée ni frontend (M2).

**Architecture:** Un drapeau `agent: AtomicBool` sur `Session` (posé par le setter interne `mark_agent`, exercé uniquement par les tests M1) déclenche deux comportements. (1) `Session::agent_status(idle_threshold)` calcule le statut par priorité : volet racine sorti → `Done`/`Error` (code de sortie), sinon cloche G4 → `Attention`, sinon sortie récente (`Notifier::last_output_elapsed` < seuil) → `Working`, sinon `Idle`. (2) `Session::reap` est court-circuité pour un agent : sa fenêtre morte reste visible (`is_alive` vrai) jusqu'à `kill` manuel. Le `Notifier` (partagé par les volets, cf. G4) gagne un horodatage `last_output_at: Mutex<Instant>` mis à jour dans `bump()`. `Server::list` renseigne `agent`/`agent_status`, le seuil venant de `Config::agent_idle_seconds` (défaut 4).

**Tech Stack:** Rust (workspace, edition 2024), `wimux-vt`, ConPTY (`portable-pty`), Named Pipe + postcard, `std::time::{Instant, Duration}` (autorisés côté serveur).

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : suites TUI + G1/G2/G3/G4 vertes (`cargo test --workspace -- --test-threads=1`).
- Types partagés : `agent_idle_seconds` défaut **4** ; `AgentStatus { Working, Idle, Attention, Done, Error }`.
- Outil shell : **Bash tool** (git bash) ; certains tests lib lancent de vrais process (ConPTY/cmd.exe), `--test-threads=1`, patience.
- `cargo fmt` a tendance à reformater `crates/wimux-server/tests/gui_mode.rs` (hors périmètre) — le rétablir (`git checkout -- crates/wimux-server/tests/gui_mode.rs`) avant commit si la tâche ne le modifie pas.
- Chaque commit se termine par le trailer : `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, via `git commit -m "$(printf '...')"`.
- **Piège de spawn ConPTY (important pour les tests) :** `portable-pty` construit la ligne de commande en passant le *programme entier* comme `lpApplicationName` de `CreateProcessW`. Un jeton de programme contenant des espaces (ex. `"cmd /c exit 0"`) n'est donc **pas** résolu et le spawn échoue. Pour faire sortir un volet racine avec un code choisi, on lance `"cmd.exe"` (jeton unique, déjà éprouvé par les tests G4) puis on lui envoie `exit 0` / `exit 3` (`send_input`), ce qui termine le processus avec le code voulu — déterministe.
- Piège du daemon persistant : après tout changement de protocole, rebuild + redémarrage du serveur détaché (non requis pour ce plan, purement serveur/tests, mais à garder en tête).

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : nouvel enum `AgentStatus` ; `SessionInfo` gagne `agent: bool` + `agent_status: Option<AgentStatus>` ; test roundtrip + mise à jour du test existant.
- `crates/wimux-server/src/config.rs` — **modifier** : `Config.agent_idle_seconds: u64` (défaut 4) + directive `set agent-idle-seconds` ; 3 tests.
- `crates/wimux-server/src/pane.rs` — **modifier** : `Notifier.last_output_at: Mutex<Instant>` + `last_output_elapsed` ; `Pane::exit_code` ; 3 tests.
- `crates/wimux-server/src/session.rs` — **modifier** (cœur) : drapeau `agent` + `mark_agent`/`is_agent` ; `agent_status` ; court-circuit de `reap` ; tests unitaires du calcul + non-reap.
- `crates/wimux-server/src/daemon.rs` — **modifier** : `Server::list` renseigne `agent`/`agent_status` (Task 1 pose des placeholders `false`/`None`, Task 5 la vraie logique).

Le `ls` du CLI (`crates/wimux-cli/src/main.rs`) lit seulement `s.name`/`s.windows`/`s.attached` (vérifié) et ne CONSTRUIT pas de `SessionInfo` : aucune modification requise. Aucun frontend en M1.

---

## Task 1: Protocole — `AgentStatus` + `SessionInfo` étendu

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (le SEUL constructeur non-test de `SessionInfo`)
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces (utilisés par Tasks 4/5) :
  - `pub enum AgentStatus { Working, Idle, Attention, Done, Error }` (Serialize/Deserialize/Clone/Copy/Debug/PartialEq).
  - `pub struct SessionInfo { pub name: String, pub windows: u32, pub attached: bool, pub activity: bool, pub bell: bool, pub agent: bool, pub agent_status: Option<AgentStatus> }`.

- [ ] **Step 1: Écrire le test roundtrip agent (échoue)**

Dans le module `tests` de `crates/wimux-protocol/src/lib.rs`, ajouter :

```rust
    #[test]
    fn aller_retour_session_info_agent() {
        let info = SessionInfo {
            name: "bot".into(),
            windows: 1,
            attached: false,
            activity: false,
            bell: false,
            agent: true,
            agent_status: Some(AgentStatus::Working),
        };
        let msg = ServerMessage::Sessions(vec![info]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::Sessions(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "bot");
                assert!(v[0].agent);
                assert_eq!(v[0].agent_status, Some(AgentStatus::Working));
            }
            _ => panic!("mauvais variant"),
        }
    }
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-protocol`
Expected: FAIL — la compilation échoue (`cannot find type AgentStatus` + `missing fields agent, agent_status in initializer of SessionInfo`).

- [ ] **Step 3: Ajouter l'enum `AgentStatus`**

Dans `crates/wimux-protocol/src/lib.rs`, juste avant la définition de `SessionInfo` (avant `/// Résumé d'une session...`), ajouter :

```rust
/// Statut calculé d'une session agent (M1). Sérialisé sur [`SessionInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// Le volet racine produit de la sortie récemment.
    Working,
    /// Vivant mais silencieux au-delà du seuil d'inactivité.
    Idle,
    /// Une cloche (BEL) est en attente d'être vue.
    Attention,
    /// Le volet racine a quitté avec le code 0.
    Done,
    /// Le volet racine a quitté avec un code non nul.
    Error,
}
```

- [ ] **Step 4: Étendre `SessionInfo`**

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
    /// Est-ce une session agent ? (M1)
    pub agent: bool,
    /// Statut de l'agent ; `None` si `agent == false` (M1).
    pub agent_status: Option<AgentStatus>,
}
```

- [ ] **Step 5: Mettre à jour le test roundtrip existant**

Dans le même module `tests`, le test `aller_retour_session_info_activite` construit un `SessionInfo` sans les nouveaux champs (erreur de compilation). Remplacer sa construction :

```rust
        let info = SessionInfo {
            name: "dev".into(),
            windows: 2,
            attached: true,
            activity: true,
            bell: false,
        };
```

par :

```rust
        let info = SessionInfo {
            name: "dev".into(),
            windows: 2,
            attached: true,
            activity: true,
            bell: false,
            agent: false,
            agent_status: None,
        };
```

(Ne pas toucher aux assertions de ce test ; elles restent valides.)

- [ ] **Step 6: Mettre à jour le SEUL constructeur non-test (placeholder)**

Dans `crates/wimux-server/src/daemon.rs`, dans `Server::list`, remplacer le bloc `SessionInfo { ... }` :

```rust
                SessionInfo {
                    name,
                    windows: s.window_count() as u32,
                    attached: s.attached_count() > 0,
                    activity,
                    bell,
                }
```

par (la vraie logique arrive en Task 5) :

```rust
                SessionInfo {
                    name,
                    windows: s.window_count() as u32,
                    attached: s.attached_count() > 0,
                    activity,
                    bell,
                    agent: false,       // calculé en Task 5
                    agent_status: None, // calculé en Task 5
                }
```

- [ ] **Step 7: Lancer le test + build serveur (attendu PASS/OK)**

Run: `cargo test -p wimux-protocol`
Expected: PASS (dont `aller_retour_session_info_agent` et `aller_retour_session_info_activite`).

Run: `cargo build -p wimux-server`
Expected: OK.

- [ ] **Step 8: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a modifié `crates/wimux-server/tests/gui_mode.rs` (hors périmètre) : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

- [ ] **Step 9: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(protocol): AgentStatus + SessionInfo agent/agent_status (M1)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: Config — `agent-idle-seconds`

**Files:**
- Modify: `crates/wimux-server/src/config.rs`
- Test: `crates/wimux-server/src/config.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces (utilisé par Task 5) : `Config.agent_idle_seconds: u64` (défaut 4) ; directive `set agent-idle-seconds <n>`.

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-server/src/config.rs`, ajouter :

```rust
    #[test]
    fn agent_idle_seconds_defaut_est_4() {
        assert_eq!(Config::default().agent_idle_seconds, 4);
    }

    #[test]
    fn set_agent_idle_seconds_modifie_le_seuil() {
        let mut c = Config::default();
        c.apply("set agent-idle-seconds 10\n");
        assert_eq!(c.agent_idle_seconds, 10);
    }

    #[test]
    fn set_agent_idle_seconds_invalide_ignore() {
        let mut c = Config::default();
        c.apply("set agent-idle-seconds abc\n");
        assert_eq!(c.agent_idle_seconds, 4);
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --lib config`
Expected: FAIL — la compilation échoue (`no field agent_idle_seconds on type Config`).

- [ ] **Step 3: Ajouter le champ à `Config`**

Dans `crates/wimux-server/src/config.rs`, remplacer la définition de `Config` par :

```rust
/// Configuration résolue.
#[derive(Debug, Clone)]
pub struct Config {
    /// Octet de la touche de préfixe (Ctrl-b = 0x02 par défaut).
    pub prefix: u8,
    pub default_shell: String,
    /// Support de la souris (molette -> scrollback, clic -> sélection de volet).
    pub mouse: bool,
    /// Table des raccourcis de préfixe (octet -> action).
    pub bindings: HashMap<u8, Action>,
    /// Seuil (secondes) séparant *Travaille* de *Au repos* pour un agent (M1).
    pub agent_idle_seconds: u64,
}
```

- [ ] **Step 4: Initialiser le champ dans `Default`**

Dans `impl Default for Config`, remplacer le littéral final :

```rust
        Config {
            prefix: 0x02,
            default_shell: std::env::var("WIMUX_SHELL")
                .unwrap_or_else(|_| "powershell.exe".to_string()),
            mouse: true,
            bindings,
        }
```

par :

```rust
        Config {
            prefix: 0x02,
            default_shell: std::env::var("WIMUX_SHELL")
                .unwrap_or_else(|_| "powershell.exe".to_string()),
            mouse: true,
            bindings,
            agent_idle_seconds: 4,
        }
```

- [ ] **Step 5: Ajouter la directive dans `apply`**

Dans `Config::apply`, dans le `match tokens.as_slice()`, ajouter un bras juste avant `_ => {}` :

```rust
                ["set", "mouse", value] => self.mouse = matches!(*value, "on" | "true" | "1"),
                ["set", "agent-idle-seconds", n] => {
                    if let Ok(v) = n.parse::<u64>() {
                        self.agent_idle_seconds = v;
                    }
                }
                ["bind", key, rest @ ..] => {
```

(Ne montrer que l'insertion : le bras `["set","mouse",...]` et le bras `["bind",...]` existent déjà ; on intercale le nouveau bras entre eux.)

- [ ] **Step 6: Lancer les tests (attendu PASS)**

Run: `cargo test -p wimux-server --lib config`
Expected: PASS (dont les 3 nouveaux tests).

- [ ] **Step 7: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/config.rs
git commit -m "$(printf 'feat(config): agent-idle-seconds (defaut 4) (M1)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: `pane.rs` — horodatage de sortie + `exit_code`

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Test: `crates/wimux-server/src/pane.rs` (module `tests`)

**Interfaces:**
- Consumes: rien (le champ `PaneState.exit_code` existe déjà, posé par `reader_loop`).
- Produces (utilisés par Task 4) :
  - `Notifier::last_output_elapsed(&self) -> Duration` — durée depuis le dernier `bump`.
  - `Pane::exit_code(&self) -> Option<u32>` — code de sortie du processus, `None` s'il tourne encore.

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-server/src/pane.rs`, ajouter :

```rust
    #[test]
    fn notifier_horodatage_apres_bump() {
        let n = Notifier::new();
        n.bump();
        assert!(
            n.last_output_elapsed() < Duration::from_secs(1),
            "juste après un bump, l'écoulement doit être petit"
        );
    }

    #[test]
    fn notifier_neuf_a_un_horodatage_defini() {
        let n = Notifier::new();
        // Un Notifier neuf initialise last_output_at à Instant::now() : l'écoulement
        // est défini et petit tout de suite après la création.
        assert!(n.last_output_elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn exit_code_none_pour_volet_vivant() {
        let n = Notifier::new();
        let p = Pane::spawn(20, 5, "cmd.exe", n).unwrap();
        assert_eq!(p.exit_code(), None, "un volet vivant n'a pas de code de sortie");
        p.kill();
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --lib pane -- --test-threads=1`
Expected: FAIL — `no method named last_output_elapsed found for struct Notifier` (et `exit_code` sur `Pane`).

- [ ] **Step 3: Importer `Instant`**

Dans `crates/wimux-server/src/pane.rs`, remplacer :

```rust
use std::time::Duration;
```

par :

```rust
use std::time::{Duration, Instant};
```

- [ ] **Step 4: Ajouter le champ `last_output_at` au `struct Notifier`**

Remplacer la définition de `Notifier` par :

```rust
/// Signal de changement d'affichage partagé par tous les volets d'une session.
pub struct Notifier {
    generation: Mutex<u64>,
    cond: Condvar,
    /// Cloche (BEL) en attente pour cette session (G4).
    bell: AtomicBool,
    /// Horodatage de la dernière sortie (dernier `bump`), pour le calcul du
    /// statut d'agent (M1).
    last_output_at: Mutex<Instant>,
}
```

et, dans `Notifier::new`, initialiser le champ :

```rust
    pub fn new() -> Arc<Notifier> {
        Arc::new(Notifier {
            generation: Mutex::new(0),
            cond: Condvar::new(),
            bell: AtomicBool::new(false),
            last_output_at: Mutex::new(Instant::now()),
        })
    }
```

- [ ] **Step 5: Mettre à jour l'horodatage dans `bump`**

Remplacer la méthode `bump` par :

```rust
    /// Signale un changement (nouvelle sortie, changement de layout...).
    pub fn bump(&self) {
        let mut g = self.generation.lock().unwrap();
        *g += 1;
        *self.last_output_at.lock().unwrap() = Instant::now();
        self.cond.notify_all();
    }
```

- [ ] **Step 6: Ajouter `last_output_elapsed`**

Dans `impl Notifier`, juste après la méthode `generation`, ajouter :

```rust
    /// Durée écoulée depuis la dernière sortie (dernier `bump`). Sert au calcul
    /// du statut d'agent (M1).
    pub fn last_output_elapsed(&self) -> Duration {
        self.last_output_at.lock().unwrap().elapsed()
    }
```

- [ ] **Step 7: Ajouter `Pane::exit_code`**

Dans `impl Pane`, juste après la méthode `is_alive`, ajouter :

```rust
    /// Code de sortie du processus du volet, ou `None` s'il tourne encore (M1).
    pub fn exit_code(&self) -> Option<u32> {
        self.state.lock().unwrap().exit_code
    }
```

- [ ] **Step 8: Lancer les tests lib (attendu PASS)**

Run: `cargo test -p wimux-server --lib pane -- --test-threads=1`
Expected: PASS (dont `notifier_horodatage_apres_bump`, `notifier_neuf_a_un_horodatage_defini`, `exit_code_none_pour_volet_vivant`).

- [ ] **Step 9: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/pane.rs
git commit -m "$(printf 'feat(pane): horodatage last_output_at + Pane::exit_code (M1)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: `session.rs` — drapeau agent, `agent_status`, non-reap

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Test: `crates/wimux-server/src/session.rs` (module `tests`)

**Interfaces:**
- Consumes :
  - Task 1 : `wimux_protocol::AgentStatus`.
  - Task 3 : `Notifier::last_output_elapsed`, `Pane::exit_code`.
  - Existant : `Session::active_pane` (privé), `Session::has_bell`, `Session::send_input`, `Notifier::{bump, signal_bell}` (via `notifier()`), `Session::reap` (privé), `Session::is_alive`.
- Produces (utilisés par Task 5) :
  - `Session::mark_agent(&self)` — pose le drapeau agent.
  - `Session::is_agent(&self) -> bool`.
  - `Session::agent_status(&self, idle_threshold: Duration) -> Option<AgentStatus>`.

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-server/src/session.rs`, ajouter (après `suivi_activite_et_cloche`). Note : ces tests lancent de vrais `cmd.exe` (ConPTY) et pilotent leur sortie via `send_input` — voir le piège de spawn dans les Global Constraints.

```rust
    /// Sonde `agent_status` jusqu'à obtenir `want`, dans la limite du délai.
    fn poll_status(s: &Session, want: AgentStatus, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if s.agent_status(Duration::from_secs(4)) == Some(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn agent_status_none_si_pas_agent() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        assert_eq!(s.agent_status(Duration::from_secs(4)), None);
        s.kill();
    }

    #[test]
    fn agent_sortie_code_zero_donne_done() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.mark_agent();
        std::thread::sleep(Duration::from_millis(800));
        // Cloche posée AVANT la sortie : prouve que la SORTIE prime sur la cloche.
        s.notifier().signal_bell();
        s.send_input(b"exit 0\r\n");
        assert!(
            poll_status(&s, AgentStatus::Done, 20),
            "un agent dont le volet racine sort avec 0 doit être Done, obtenu {:?}",
            s.agent_status(Duration::from_secs(4))
        );
        s.kill();
    }

    #[test]
    fn agent_sortie_code_non_nul_donne_error() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.mark_agent();
        std::thread::sleep(Duration::from_millis(800));
        s.send_input(b"exit 3\r\n");
        assert!(
            poll_status(&s, AgentStatus::Error, 20),
            "un agent dont le volet racine sort avec 3 doit être Error, obtenu {:?}",
            s.agent_status(Duration::from_secs(4))
        );
        s.kill();
    }

    #[test]
    fn agent_vivant_avec_cloche_donne_attention() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.mark_agent();
        std::thread::sleep(Duration::from_millis(800));
        s.notifier().signal_bell();
        // Seuil long : sans la cloche ce serait Working ; la cloche prime.
        assert_eq!(
            s.agent_status(Duration::from_secs(60)),
            Some(AgentStatus::Attention)
        );
        s.kill();
    }

    #[test]
    fn agent_vivant_sortie_recente_donne_working() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.mark_agent();
        std::thread::sleep(Duration::from_millis(800));
        s.mark_seen(); // efface une éventuelle cloche de démarrage
        s.notifier().bump(); // horodatage de sortie « maintenant »
        assert_eq!(
            s.agent_status(Duration::from_secs(60)),
            Some(AgentStatus::Working)
        );
        s.kill();
    }

    #[test]
    fn agent_vivant_silencieux_donne_idle() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.mark_agent();
        std::thread::sleep(Duration::from_millis(800));
        s.mark_seen(); // efface une éventuelle cloche de démarrage
        s.notifier().bump();
        std::thread::sleep(Duration::from_millis(20));
        // Seuil 1 ms : la dernière sortie (≥ 20 ms) est « ancienne » → Idle.
        assert_eq!(
            s.agent_status(Duration::from_millis(1)),
            Some(AgentStatus::Idle)
        );
        s.kill();
    }

    #[test]
    fn agent_non_reape_apres_sortie() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.mark_agent();
        std::thread::sleep(Duration::from_millis(800));
        s.send_input(b"exit 0\r\n");
        assert!(
            poll_status(&s, AgentStatus::Done, 20),
            "l'agent aurait dû se terminer (Done)"
        );
        // Non-reap : reap() court-circuite et conserve la fenêtre morte.
        assert!(s.reap(), "reap d'un agent renvoie true sans rien retirer");
        assert!(
            s.is_alive(),
            "une session agent survit à la mort de son processus racine"
        );
        s.kill();
    }
```

- [ ] **Step 2: Lancer un test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib session::tests::agent_status_none_si_pas_agent`
Expected: FAIL — la compilation échoue (`no method named mark_agent found for struct Session`, `AgentStatus` introuvable).

- [ ] **Step 3: Mettre à jour les imports**

Dans `crates/wimux-server/src/session.rs`, remplacer :

```rust
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use wimux_protocol::{Frame, LayoutNode};
```

par :

```rust
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use wimux_protocol::{AgentStatus, Frame, LayoutNode};
```

- [ ] **Step 4: Ajouter le champ `agent` au `struct Session`**

Dans `pub struct Session`, ajouter le champ après `paste_buffer` :

```rust
    /// Génération du Notifier vue par la GUI la dernière fois (G4).
    last_seen_gen: AtomicU64,
    paste_buffer: Mutex<String>,
    /// Drapeau « session agent » (M1) : déclenche le calcul de statut et le
    /// non-reap. Posé par `mark_agent` (aucun chemin client en M1 ; c'est M2).
    agent: AtomicBool,
```

Dans `Session::new`, initialiser le champ après `paste_buffer` :

```rust
            attached: AtomicUsize::new(0),
            last_seen_gen: AtomicU64::new(0),
            paste_buffer: Mutex::new(String::new()),
            agent: AtomicBool::new(false),
```

- [ ] **Step 5: Ajouter `mark_agent`, `is_agent`, `agent_status`**

Dans `impl Session`, juste après la méthode `has_bell`, ajouter :

```rust
    /// M1 : marque cette session comme une session agent (setter interne,
    /// exercé par les tests ; la création exposée arrive en M2).
    pub fn mark_agent(&self) {
        self.agent.store(true, Ordering::Relaxed);
    }

    /// M1 : cette session est-elle une session agent ?
    pub fn is_agent(&self) -> bool {
        self.agent.load(Ordering::Relaxed)
    }

    /// M1 : statut calculé de l'agent, ou `None` si ce n'est pas un agent.
    ///
    /// Priorité : (1) volet racine sorti → `Done`(code 0)/`Error`(≠0) ;
    /// (2) cloche → `Attention` ; (3) sortie récente (< `idle_threshold`) →
    /// `Working` ; (4) sinon `Idle`.
    pub fn agent_status(&self, idle_threshold: Duration) -> Option<AgentStatus> {
        if !self.is_agent() {
            return None;
        }
        if let Some(pane) = self.active_pane()
            && let Some(code) = pane.exit_code()
        {
            return Some(if code == 0 {
                AgentStatus::Done
            } else {
                AgentStatus::Error
            });
        }
        if self.has_bell() {
            return Some(AgentStatus::Attention);
        }
        if self.notifier.last_output_elapsed() < idle_threshold {
            return Some(AgentStatus::Working);
        }
        Some(AgentStatus::Idle)
    }
```

- [ ] **Step 6: Court-circuiter `reap` pour un agent**

Dans `impl Session`, remplacer le début de `fn reap` :

```rust
    /// Retire les volets/fenêtres morts. Renvoie `true` s'il reste de la vie.
    fn reap(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
```

par :

```rust
    /// Retire les volets/fenêtres morts. Renvoie `true` s'il reste de la vie.
    ///
    /// M1 : pour une **session agent**, court-circuite sans rien retirer — la
    /// fenêtre morte est conservée (statut `Done`/`Error` visible) jusqu'à un
    /// `kill` manuel.
    fn reap(&self) -> bool {
        if self.is_agent() {
            return true;
        }
        let mut inner = self.inner.lock().unwrap();
```

(Le reste du corps de `reap` — la boucle `while` et le retour `!inner.windows.is_empty()` — est inchangé.)

- [ ] **Step 7: Lancer les tests lib session (attendu PASS)**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (dont les 7 nouveaux tests agent + `suivi_activite_et_cloche` + `window_layout_feuille_unique`). Tests lents : de vrais process ConPTY, patience.

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(session): drapeau agent, agent_status, non-reap (M1)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: `daemon.rs` — `Server::list` renseigne agent/agent_status

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: aucun nouveau test (voir note ci-dessous) ; vérification par non-régression `--workspace`.

**Interfaces:**
- Consumes (Task 4) : `Session::is_agent`, `Session::agent_status`. Config (Task 2) : `Config.agent_idle_seconds`.
- Produces : `SessionInfo.agent`/`.agent_status` reflètent l'état réel.

**Note (pas de test d'intégration ici) :** M1 n'expose AUCUN chemin de création de session agent (c'est M2). Un test d'intégration `gui_mode.rs` ne verrait donc que `agent: false` / `agent_status: None` partout — sans valeur. La couverture réelle du calcul est dans les tests lib de Task 4 (construction directe de `Session` + `mark_agent`). Cette tâche se limite au câblage et à la non-régression.

- [ ] **Step 1: Remplacer les placeholders dans `Server::list`**

Dans `crates/wimux-server/src/daemon.rs`, dans `Server::list`, remplacer le bloc `SessionInfo { ... }` posé en Task 1 :

```rust
                SessionInfo {
                    name,
                    windows: s.window_count() as u32,
                    attached: s.attached_count() > 0,
                    activity,
                    bell,
                    agent: false,       // calculé en Task 5
                    agent_status: None, // calculé en Task 5
                }
```

par :

```rust
                SessionInfo {
                    name,
                    windows: s.window_count() as u32,
                    attached: s.attached_count() > 0,
                    activity,
                    bell,
                    agent: s.is_agent(),
                    agent_status: s.agent_status(std::time::Duration::from_secs(
                        self.config.agent_idle_seconds,
                    )),
                }
```

(On qualifie `std::time::Duration` sur place pour éviter d'ajouter un import ; `self.config` est accessible dans la fermeture de `.map`, en emprunt partagé aux côtés du verrou `sessions`.)

- [ ] **Step 2: Build (attendu OK)**

Run: `cargo build -p wimux-server`
Expected: OK — le câblage compile avec les méthodes de Task 4 et le champ de config de Task 2.

- [ ] **Step 3: Non-régression lib session (attendu PASS)**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (les tests agent de Task 4 restent verts).

- [ ] **Step 4: Non-régression complète (attendu PASS)**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS (TUI + G1/G2/G3/G4 + protocole + config + pane + session). Tests lents ConPTY — patience.

- [ ] **Step 5: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
Expected: OK.

Si `cargo fmt` a touché `crates/wimux-server/tests/gui_mode.rs` (hors périmètre de cette tâche) : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(daemon): Server::list renseigne agent/agent_status (M1)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Self-Review

**Spec coverage :**
- Enum `AgentStatus { Working, Idle, Attention, Done, Error }` (Serialize/Deserialize/Clone/Copy/Debug/PartialEq) + `SessionInfo.agent`/`.agent_status` → Task 1 (avec test roundtrip agent + mise à jour du test existant + placeholder dans l'unique constructeur `daemon.rs`).
- `Config.agent_idle_seconds: u64` défaut 4 + directive `set agent-idle-seconds` (parse `u64`, ignore si invalide) → Task 2.
- `Notifier.last_output_at: Mutex<Instant>` (init `Instant::now`, MAJ dans `bump`) + `last_output_elapsed` ; `Pane::exit_code` → Task 3.
- Drapeau `agent` + `mark_agent`/`is_agent` ; `agent_status(idle_threshold)` avec l'ordre de priorité sortie > cloche > travaille > repos ; court-circuit de `reap` pour agent → Task 4, couvert par 7 tests lib (None, Done, Error, Attention, Working, Idle, non-reap).
- `Server::list` renseigne `agent = s.is_agent()` et `agent_status = s.agent_status(Duration::from_secs(config.agent_idle_seconds))` → Task 5, non-régression `--workspace`.
- `ls` du CLI inchangé (lit `name`/`windows`/`attached`, ne construit pas `SessionInfo` — vérifié). Aucun frontend (M1).

**Type consistency :** `AgentStatus` (Task 1) est consommé à l'identique par `agent_status` (Task 4) et par les tests (Task 1/4). `mark_agent`/`is_agent`/`agent_status(idle_threshold: Duration)` définis en Task 4 sont appelés en Task 5 exactement comme `s.is_agent()` / `s.agent_status(std::time::Duration::from_secs(self.config.agent_idle_seconds))`. `Notifier::last_output_elapsed() -> Duration` et `Pane::exit_code() -> Option<u32>` (Task 3) sont consommés par `agent_status` (Task 4). `Config.agent_idle_seconds: u64` (Task 2) est consommé par `Server::list` (Task 5). Le comparateur `last_output_elapsed() < idle_threshold` oppose bien deux `Duration`.

**Placeholder scan :** aucun « TODO/à compléter ». Les mentions `agent: false // calculé en Task 5` sont un vrai code intermédiaire (placeholder assumé de valeur, remplacé en Task 5), pas un placeholder de plan.

**Points signalés pour relecture (choix non spécifiés) :**
1. **Commandes de sortie des tests (Task 4)** — DÉVIATION de la lettre de la spec, imposée par `portable-pty`. La spec dit `Session::new(..., "cmd /c exit 0")`, mais `portable-pty` passe le programme entier comme `lpApplicationName` de `CreateProcessW` : un jeton avec espaces n'est pas résolu et le spawn échoue (vérifié dans `cmdbuilder.rs`/`psuedocon.rs` v0.9.0). On lance donc `"cmd.exe"` (jeton unique éprouvé par G4) puis on envoie `exit 0` / `exit 3` via `send_input`, ce qui termine le processus avec le code voulu — déterministe et sémantiquement identique. À valider.
2. **Ordre de priorité prouvé (Task 4)** — le test `Done` pose la cloche (`signal_bell`) AVANT la sortie pour démontrer que la sortie prime sur la cloche (exigence de la spec « sortie prime sur cloche »).
3. **Tests temporels (Task 4)** — `Working`/`Idle` reposent sur des délais réels (`sleep(20ms)` + seuil `Duration::from_millis(1)`), et un `mark_seen()` préalable efface une éventuelle cloche de démarrage de `cmd.exe`. Robustesse assumée (la spec reconnaît des tests lents/temporels) ; l'`Idle` reste le plus sensible au bruit de fond.
4. **Pas de test d'intégration en Task 5** — assumé et justifié par la spec (aucune création agent exposée en M1) : la couverture est en Task 4 (tests lib). Task 5 se contente du câblage + non-régression `--workspace`.
5. **`std::time::Duration` qualifié sur place dans `daemon.rs` (Task 5)** — évite un `use` supplémentaire et un éventuel reformatage ; choix de style, non imposé.

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-16-wimux-agents-m1-status.md`. Two execution options:**

**1. Subagent-Driven (recommended)** - Un subagent frais par tâche, revue entre les tâches, itération rapide.

**2. Inline Execution** - Exécution des tâches dans cette session via executing-plans, par lots avec checkpoints.

**Which approach?**
