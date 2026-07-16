# wimux multi-agents M2 — Création + lanceur : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rendre M1 démontrable : **lancer une session agent** depuis la GUI (modèle configuré + prompt + cwd), le serveur créant une session dont le volet racine est l'agent (marquée `agent`, M1), livrant le prompt (argument `{prompt}` ou stdin), et la GUI affichant un **glyphe de statut** dans le rail. Inclut le correctif M1 de `Pane::kill` (fermeture du `MasterPty`).

**Architecture:** Les **modèles d'agents** vivent dans la config serveur (`AgentTemplate { name, program, args }`, directive `agent-template`). La **substitution `{prompt}` se fait côté serveur** : si un arg contient `{prompt}` il est remplacé (prompt = un seul argument, pas de re-split) ; sinon le prompt + `\r` est envoyé sur stdin du volet racine après le spawn. Le spawn de volet est généralisé (`Pane::spawn_command(cols, rows, program, args, cwd, notifier)`) ; `Pane::spawn` (shell) délègue avec args vide / cwd `None`. `Session::new_agent(...)` construit le volet racine ainsi puis `mark_agent`. Le pont Tauri expose `agent`/`agent_status` sur `SessionDto` et deux commandes (`list_agent_templates`, `create_agent`). Le frontend ajoute un dialogue modal « + agent » et remplace, pour les sessions agent, la pastille G4 par un glyphe (⚙/○/❗/✓/✗).

**Tech Stack:** Rust (workspace, edition 2024), `wimux-vt`, ConPTY (`portable-pty` 0.9), Named Pipe + postcard, Tauri (`wimux-gui/src-tauri`), TypeScript + xterm.js (`wimux-gui/src`).

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : suites TUI + G1/G2/G3/G4 + M1 vertes (`cargo test --workspace -- --test-threads=1`) ; `npm run build` OK.
- `cargo fmt` peut reformater `crates/wimux-server/tests/gui_mode.rs` hors périmètre — le rétablir (`git checkout -- crates/wimux-server/tests/gui_mode.rs`) avant commit si la tâche ne le modifie pas.
- Outil shell : **Bash tool** (git bash) ; tests lents (ConPTY/PowerShell), `--test-threads=1`, patience.
- Chaque commit finit par le trailer : `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, via `git commit -m "$(printf '...')"`.
- **Piège `portable-pty` 0.9 :** `CommandBuilder::{arg, args, cwd}` prennent `&mut self` et renvoient `()` (pas de chaînage type-builder) — construire la commande mutablement. Le **programme est un seul jeton** (`lpApplicationName`) : un jeton avec espaces échoue au spawn ; les args sont séparés. Pour un volet racine déterministe dans les tests, lancer `"cmd.exe"` (jeton unique, éprouvé par G4/M1) et le piloter via `send_input` (`exit 0`) ou via des args (`/c echo ...`).
- **Piège du daemon persistant :** après tout changement de protocole, rebuild + redémarrage du serveur détaché (requis seulement pour la vérification manuelle M2 du frontend, pas pour les tests de ce plan).
- **Ordre des variants postcard :** postcard sérialise le discriminant d'enum par sa position — **toujours ajouter les nouveaux variants À LA FIN** de `ClientMessage`/`ServerMessage` pour ne pas décaler les index existants.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : `struct AgentTemplate` ; `ClientMessage::ListAgentTemplates` + `ClientMessage::CreateAgentSession {...}` (fin d'enum) ; `ServerMessage::AgentTemplates(Vec<AgentTemplate>)` (fin d'enum) ; 3 tests roundtrip.
- `crates/wimux-server/src/daemon.rs` — **modifier** : stubs no-op des 2 nouveaux `ClientMessage` (Task 1), puis `Server::{agent_templates, create_agent_session}` + câblage réel + point d'entrée de test `run_on_with_config` (Task 5).
- `crates/wimux-server/src/config.rs` — **modifier** : `Config.agent_templates: Vec<AgentTemplate>` (défaut vide) + directive `agent-template` ; 4 tests.
- `crates/wimux-server/src/pane.rs` — **modifier** : `PaneState.master: Option<...>` + `kill` ferme le master (correctif M1) ; `Pane::spawn_command` (spawn paramétré) + `Pane::spawn` délègue ; 1 test lib du déblocage.
- `crates/wimux-server/src/session.rs` — **modifier** : `Session::new_agent(...)` ; 2 tests lib.
- `crates/wimux-server/tests/common/mod.rs` — **modifier** : helper `start_daemon_with_config` (config injectée).
- `crates/wimux-server/tests/gui_mode.rs` — **modifier** : 4 tests d'intégration agents (Task 5).
- `wimux-gui/src-tauri/src/lib.rs` — **modifier** : `SessionDto` + `agent`/`agent_status` ; `AgentTemplateDto` ; commandes `list_agent_templates` / `create_agent` ; `invoke_handler!`.
- `wimux-gui/index.html` — **modifier** : bouton `#new-agent` + dialogue modal `#agent-modal`.
- `wimux-gui/src/main.ts` — **modifier** : type étendu, logique du modal, glyphe de statut dans `renderRail`.
- `wimux-gui/src/styles.css` — **modifier** : styles glyphe + modal.
- `wimux-gui/README.md` — **modifier** : section « Vérification manuelle M2 » (ajout en fin, comme G3/G4).

Le CLI (`crates/wimux-cli/src/main.rs`) n'a que des `if let` / bras `_ =>` sur `ServerMessage` (vérifié) : les nouveaux variants ne cassent aucune exhaustivité, aucune modification requise.

---

## Task 1: Protocole — `AgentTemplate` + messages agents

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (stubs d'exhaustivité)
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces (utilisés par Tasks 2/5/6) :
  - `pub struct AgentTemplate { pub name: String, pub program: String, pub args: Vec<String> }` (Serialize/Deserialize/Clone/Debug/PartialEq).
  - `ClientMessage::ListAgentTemplates` (unit).
  - `ClientMessage::CreateAgentSession { name: Option<String>, template: String, prompt: String, cwd: Option<String> }`.
  - `ServerMessage::AgentTemplates(Vec<AgentTemplate>)`.

- [ ] **Step 1: Écrire les tests roundtrip (échouent)**

Dans le module `tests` de `crates/wimux-protocol/src/lib.rs`, ajouter (à la fin du module, avant l'accolade fermante `}`) :

```rust
    #[test]
    fn aller_retour_agent_template() {
        let tpl = AgentTemplate {
            name: "claude".into(),
            program: "claude".into(),
            args: vec!["-p".into(), "{prompt}".into()],
        };
        let msg = ServerMessage::AgentTemplates(vec![tpl.clone()]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::AgentTemplates(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], tpl);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_list_agent_templates() {
        let msg = ClientMessage::ListAgentTemplates;
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        assert!(matches!(
            recv::<_, ClientMessage>(&mut cur).unwrap(),
            ClientMessage::ListAgentTemplates
        ));
    }

    #[test]
    fn aller_retour_create_agent_session() {
        let msg = ClientMessage::CreateAgentSession {
            name: Some("bot".into()),
            template: "claude".into(),
            prompt: "corrige le bug".into(),
            cwd: Some("C:\\proj".into()),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CreateAgentSession {
                name,
                template,
                prompt,
                cwd,
            } => {
                assert_eq!(name.as_deref(), Some("bot"));
                assert_eq!(template, "claude");
                assert_eq!(prompt, "corrige le bug");
                assert_eq!(cwd.as_deref(), Some("C:\\proj"));
            }
            _ => panic!("mauvais variant"),
        }
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-protocol`
Expected: FAIL — compilation (`cannot find type AgentTemplate`, `no variant ListAgentTemplates`, etc.).

- [ ] **Step 3: Ajouter le type `AgentTemplate`**

Dans `crates/wimux-protocol/src/lib.rs`, juste avant `/// Statut calculé d'une session agent (M1).` (la définition de `AgentStatus`), insérer :

```rust
/// Modèle d'agent configuré côté serveur (M2). Le frontend n'en lit que le nom ;
/// le serveur possède `program`/`args` et effectue la substitution `{prompt}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}
```

- [ ] **Step 4: Ajouter les 2 variants `ClientMessage` (fin d'enum)**

Dans `enum ClientMessage`, remplacer la fin :

```rust
    /// Vérifier que le serveur répond.
    Ping,
}
```

par :

```rust
    /// Vérifier que le serveur répond.
    Ping,
    /// Lister les modèles d'agents configurés (mode GUI, pour le lanceur).
    ListAgentTemplates,
    /// Crée une session agent depuis un modèle. Le serveur substitue `{prompt}`
    /// dans les args s'il est présent, sinon envoie le prompt + Entrée sur le
    /// stdin du volet racine après le spawn. Nom auto `<template>-<n>` si `None`.
    CreateAgentSession {
        name: Option<String>,
        template: String,
        prompt: String,
        cwd: Option<String>,
    },
}
```

- [ ] **Step 5: Ajouter le variant `ServerMessage` (fin d'enum)**

Dans `enum ServerMessage`, remplacer la fin :

```rust
    Pong,
    /// Acquittement générique.
    Ok,
}
```

par :

```rust
    Pong,
    /// Acquittement générique.
    Ok,
    /// Liste des modèles d'agents (réponse à `ListAgentTemplates`).
    AgentTemplates(Vec<AgentTemplate>),
}
```

- [ ] **Step 6: Ajouter les stubs no-op dans `daemon.rs` (exhaustivité)**

Dans `crates/wimux-server/src/daemon.rs`, dans `handle_client`, le `match msg` se termine par `ClientMessage::Hello(_) => {}`. Insérer juste AVANT cette ligne :

```rust
            ClientMessage::ListAgentTemplates => {} // câblé en Task 5
            ClientMessage::CreateAgentSession { .. } => {} // câblé en Task 5
            ClientMessage::Hello(_) => {}
```

(On n'ajoute AUCUN import ici : les stubs ne référencent pas `AgentTemplate`.)

- [ ] **Step 7: Lancer les tests + build serveur (attendu PASS/OK)**

Run: `cargo test -p wimux-protocol`
Expected: PASS (dont les 3 nouveaux tests).

Run: `cargo build -p wimux-server`
Expected: OK (le `match` est exhaustif grâce aux stubs).

- [ ] **Step 8: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a modifié `crates/wimux-server/tests/gui_mode.rs` (hors périmètre) : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

- [ ] **Step 9: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(protocol): AgentTemplate + messages agents (M2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: Config — directive `agent-template`

**Files:**
- Modify: `crates/wimux-server/src/config.rs`
- Test: `crates/wimux-server/src/config.rs` (module `tests`)

**Interfaces:**
- Consumes (Task 1) : `wimux_protocol::AgentTemplate`.
- Produces (utilisé par Task 5) : `Config.agent_templates: Vec<AgentTemplate>` (défaut vide) ; directive `agent-template <name> <program> [args...]`.

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-server/src/config.rs`, ajouter (à la fin du module, avant l'accolade fermante) :

```rust
    #[test]
    fn agent_templates_defaut_vide() {
        assert!(Config::default().agent_templates.is_empty());
    }

    #[test]
    fn agent_template_avec_placeholder() {
        let mut c = Config::default();
        c.apply("agent-template claude claude -p {prompt}\n");
        assert_eq!(c.agent_templates.len(), 1);
        let t = &c.agent_templates[0];
        assert_eq!(t.name, "claude");
        assert_eq!(t.program, "claude");
        assert_eq!(t.args, vec!["-p".to_string(), "{prompt}".to_string()]);
    }

    #[test]
    fn agent_template_sans_arg() {
        let mut c = Config::default();
        c.apply("agent-template shell cmd.exe\n");
        assert_eq!(c.agent_templates.len(), 1);
        let t = &c.agent_templates[0];
        assert_eq!(t.name, "shell");
        assert_eq!(t.program, "cmd.exe");
        assert!(t.args.is_empty());
    }

    #[test]
    fn plusieurs_agent_templates() {
        let mut c = Config::default();
        c.apply("agent-template a cmd.exe /c echo {prompt}\nagent-template b pwsh.exe\n");
        assert_eq!(c.agent_templates.len(), 2);
        assert_eq!(c.agent_templates[0].name, "a");
        assert_eq!(
            c.agent_templates[0].args,
            vec!["/c".to_string(), "echo".to_string(), "{prompt}".to_string()]
        );
        assert_eq!(c.agent_templates[1].name, "b");
        assert!(c.agent_templates[1].args.is_empty());
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --lib config`
Expected: FAIL — `no field agent_templates on type Config`.

- [ ] **Step 3: Importer `AgentTemplate`**

Dans `crates/wimux-server/src/config.rs`, sous les `use` existants (après `use std::collections::HashMap;`), ajouter :

```rust
use std::collections::HashMap;

use wimux_protocol::AgentTemplate;
```

- [ ] **Step 4: Ajouter le champ à `Config`**

Dans `pub struct Config`, ajouter le champ après `agent_idle_seconds` :

```rust
    /// Seuil (secondes) séparant *Travaille* de *Au repos* pour un agent (M1).
    pub agent_idle_seconds: u64,
    /// Modèles d'agents configurés (M2), directive `agent-template`.
    pub agent_templates: Vec<AgentTemplate>,
}
```

- [ ] **Step 5: Initialiser le champ dans `Default`**

Dans `impl Default for Config`, remplacer le littéral final :

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

par :

```rust
        Config {
            prefix: 0x02,
            default_shell: std::env::var("WIMUX_SHELL")
                .unwrap_or_else(|_| "powershell.exe".to_string()),
            mouse: true,
            bindings,
            agent_idle_seconds: 4,
            agent_templates: Vec::new(),
        }
```

- [ ] **Step 6: Ajouter la directive dans `apply`**

Dans `Config::apply`, dans le `match tokens.as_slice()`, insérer un bras juste AVANT le bras `["bind", key, rest @ ..]` :

```rust
                ["agent-template", name, program, args @ ..] => {
                    self.agent_templates.push(AgentTemplate {
                        name: name.to_string(),
                        program: program.to_string(),
                        args: args.iter().map(|s| s.to_string()).collect(),
                    });
                }
                ["bind", key, rest @ ..] => {
```

(Le bras `["bind", ...]` existe déjà ; on intercale le nouveau bras juste au-dessus. Le motif `["agent-template", name, program, args @ ..]` exige ≥ 3 jetons et se distingue des autres par son premier jeton.)

- [ ] **Step 7: Lancer les tests (attendu PASS)**

Run: `cargo test -p wimux-server --lib config`
Expected: PASS (dont les 4 nouveaux tests).

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/config.rs
git commit -m "$(printf 'feat(config): directive agent-template (M2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: `Pane::kill` ferme le master (correctif M1)

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Test: `crates/wimux-server/src/pane.rs` (module `tests`)

**Interfaces:**
- Consumes (existant) : `Pane::{spawn, send_input, exit_code, kill}`, `Notifier::new`.
- Produces : `PaneState.master: Option<Box<dyn MasterPty + Send>>` ; `Pane::kill` droppe le master après `child.kill()`. Comportement inchangé de `resize`/`spawn` côté appelants (signatures identiques).

> **RISQUE À VÉRIFIER (BLOCAGE possible) :** ce correctif suppose que dropper le `MasterPty` ferme la ConPTY et provoque l'EOF du `reader_loop` parqué. Si, sous ConPTY, le lecteur ne se débloque PAS (le compteur `Arc::strong_count` ne retombe pas à 1 dans le délai), c'est un **blocage réel** : l'implémenteur doit le **signaler (status BLOCKED)** — ne PAS masquer le test (ni allonger indéfiniment le délai, ni supprimer l'assertion). Le test ci-dessous est la preuve honnête du déblocage.

- [ ] **Step 1: Écrire le test lib (échoue)**

Dans le module `tests` de `crates/wimux-server/src/pane.rs`, ajouter (à la fin du module, avant l'accolade fermante) :

```rust
    #[test]
    fn kill_ferme_le_master_et_libere_le_lecteur() {
        let n = Notifier::new();
        let pane = Pane::spawn(20, 5, "cmd.exe", n).unwrap();
        // Faire sortir le shell proprement (code 0).
        pane.send_input(b"exit 0\r\n");
        // Attendre la détection de la sortie (wait_for_exit renseigne exit_code).
        let deadline = Instant::now() + Duration::from_secs(10);
        while pane.exit_code().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(pane.exit_code().is_some(), "le shell aurait dû sortir (exit 0)");
        // À ce stade, le reader_loop reste parqué sur read() (ConPTY n'EOF pas sur
        // sortie propre) et retient un Arc<Pane> -> strong_count >= 2.
        // kill() ferme le master -> EOF -> le thread lecteur se termine.
        pane.kill();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&pane) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            Arc::strong_count(&pane),
            1,
            "après kill (master fermé), le thread lecteur doit relâcher son Arc<Pane>"
        );
    }
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib pane::tests::kill_ferme_le_master_et_libere_le_lecteur -- --test-threads=1`
Expected: FAIL — sans le correctif, le `reader_loop` reste parqué et `Arc::strong_count` reste ≥ 2 : l'assertion échoue après le délai borné.

- [ ] **Step 3: Rendre `PaneState.master` optionnel**

Dans `crates/wimux-server/src/pane.rs`, dans `struct PaneState`, remplacer :

```rust
    master: Box<dyn MasterPty + Send>,
```

par :

```rust
    master: Option<Box<dyn MasterPty + Send>>,
```

- [ ] **Step 4: Adapter le spawn**

Dans `Pane::spawn`, dans le littéral `PaneState { ... }`, remplacer :

```rust
                writer,
                master: pair.master,
                child: Some(child),
```

par :

```rust
                writer,
                master: Some(pair.master),
                child: Some(child),
```

- [ ] **Step 5: Adapter `resize`**

Dans `Pane::resize`, remplacer :

```rust
        let _ = st.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        st.terminal.resize(cols, rows);
```

par :

```rust
        if let Some(m) = st.master.as_ref() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        st.terminal.resize(cols, rows);
```

- [ ] **Step 6: Fermer le master dans `kill`**

Remplacer la méthode `kill` :

```rust
    pub fn kill(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(child) = st.child.as_mut() {
            let _ = child.kill();
        }
    }
```

par :

```rust
    pub fn kill(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(child) = st.child.as_mut() {
            let _ = child.kill();
        }
        // Fermer la ConPTY (drop du master) provoque l'EOF du reader_loop parqué
        // sur read() : ConPTY n'EOF pas sur sortie propre, mais fermer le master
        // débloque le lecteur, qui se termine et relâche son Arc<Pane> (correctif
        // M1 : sinon tuer un agent terminé fuit un thread + un handle).
        st.master.take();
    }
```

- [ ] **Step 7: Lancer le test lib (attendu PASS)**

Run: `cargo test -p wimux-server --lib pane -- --test-threads=1`
Expected: PASS (dont `kill_ferme_le_master_et_libere_le_lecteur` et les tests pane existants). Tests lents ConPTY — patience.

> Si le test échoue encore (strong_count reste ≥ 2 après kill) : **STOP, statut BLOCKED** — dropper le master ne débloque pas le lecteur sous cette version de ConPTY/portable-pty. Signaler au lieu de contourner.

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/pane.rs
git commit -m "$(printf 'fix(pane): kill ferme le master pour debloquer le reader_loop (M1)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: `pane.rs`/`session.rs` — spawn paramétré + `Session::new_agent`

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Modify: `crates/wimux-server/src/session.rs`
- Test: `crates/wimux-server/src/session.rs` (module `tests`)

**Interfaces:**
- Consumes : Task 1 (`AgentStatus` déjà importé dans `session.rs`), Task 3 (`Pane` avec master optionnel), existant (`Notifier`, `Window::new`, `Session::{mark_agent, is_agent, agent_status, is_alive, send_input, kill}`, `content_rows`, `poll_status` du module de tests).
- Produces (utilisés par Task 5) :
  - `Pane::spawn_command(cols: u16, rows: u16, program: &str, args: &[String], cwd: Option<&str>, notifier: Arc<Notifier>) -> Result<Arc<Pane>>`.
  - `Session::new_agent(name: String, cols: u16, rows: u16, program: &str, args: &[String], cwd: Option<&str>) -> Result<Arc<Session>>` (volet racine paramétré + `mark_agent`).

- [ ] **Step 1: Écrire les tests lib session (échouent)**

Dans le module `tests` de `crates/wimux-server/src/session.rs`, ajouter (à la fin du module, avant l'accolade fermante ; le helper `poll_status` y existe déjà, on le réutilise) :

```rust
    #[test]
    fn new_agent_est_marquee_agent_et_se_termine() {
        let s = Session::new_agent(
            "a".into(),
            40,
            12,
            "cmd.exe",
            &["/c".into(), "echo".into(), "hi".into()],
            None,
        )
        .unwrap();
        assert!(s.is_agent(), "une session créée via new_agent doit être agent");
        // Le volet racine (cmd /c echo hi) se termine ; l'agent n'est pas reapé.
        assert!(
            poll_status(&s, AgentStatus::Done, 20),
            "l'agent one-shot aurait dû se terminer (Done), obtenu {:?}",
            s.agent_status(Duration::from_secs(4))
        );
        assert!(
            s.is_alive(),
            "une session agent survit à la sortie de son volet racine"
        );
        s.kill();
    }

    #[test]
    fn new_agent_avec_cwd_demarre() {
        // Un cwd valide (dossier temp du système) : le spawn réussit et vit.
        let dir = std::env::temp_dir();
        let dir = dir.to_str().expect("dossier temp en UTF-8");
        let s = Session::new_agent("b".into(), 40, 12, "cmd.exe", &[], Some(dir)).unwrap();
        assert!(s.is_agent());
        assert!(
            s.is_alive(),
            "le volet racine cmd.exe dans un cwd valide doit démarrer"
        );
        // Piloté via stdin : exit 0 -> Done.
        s.send_input(b"exit 0\r\n");
        assert!(
            poll_status(&s, AgentStatus::Done, 20),
            "cmd.exe après exit 0 doit être Done, obtenu {:?}",
            s.agent_status(Duration::from_secs(4))
        );
        s.kill();
    }
```

- [ ] **Step 2: Lancer un test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib session::tests::new_agent_est_marquee_agent_et_se_termine -- --test-threads=1`
Expected: FAIL — compilation (`no function ... new_agent found for struct Session`).

- [ ] **Step 3: Ajouter `Pane::spawn_command` et déléguer `Pane::spawn`**

Dans `crates/wimux-server/src/pane.rs`, remplacer la méthode `spawn` entière :

```rust
    /// Crée un volet : ouvre une pseudo-console, lance le shell, démarre le
    /// thread lecteur.
    pub fn spawn(cols: u16, rows: u16, shell: &str, notifier: Arc<Notifier>) -> Result<Arc<Pane>> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("ouverture de la pseudo-console")?;

        let child = pair
            .slave
            .spawn_command(CommandBuilder::new(shell))
            .context("lancement du shell")?;
        let reader = pair
            .master
            .try_clone_reader()
            .context("clonage du lecteur PTY")?;
        let writer = pair
            .master
            .take_writer()
            .context("prise de l'écrivain PTY")?;
        drop(pair.slave);

        let pane = Arc::new(Pane {
            id: NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed),
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
            }),
            notifier,
        });

        let reader_pane = Arc::clone(&pane);
        std::thread::spawn(move || reader_loop(reader_pane, reader));

        let waiter_pane = Arc::clone(&pane);
        std::thread::spawn(move || wait_for_exit(waiter_pane));
        Ok(pane)
    }
```

par :

```rust
    /// Crée un volet exécutant `shell` (jeton unique, sans args). Cas particulier
    /// de [`Pane::spawn_command`].
    pub fn spawn(cols: u16, rows: u16, shell: &str, notifier: Arc<Notifier>) -> Result<Arc<Pane>> {
        Pane::spawn_command(cols, rows, shell, &[], None, notifier)
    }

    /// Crée un volet : ouvre une pseudo-console, lance `program` avec `args` dans
    /// `cwd` (défaut = cwd du processus), démarre le thread lecteur. Le
    /// **programme est un seul jeton** (piège `portable-pty`) ; les args sont
    /// séparés. Sert de volet racine aux sessions agent (M2).
    pub fn spawn_command(
        cols: u16,
        rows: u16,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        notifier: Arc<Notifier>,
    ) -> Result<Arc<Pane>> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("ouverture de la pseudo-console")?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("lancement du programme")?;
        let reader = pair
            .master
            .try_clone_reader()
            .context("clonage du lecteur PTY")?;
        let writer = pair
            .master
            .take_writer()
            .context("prise de l'écrivain PTY")?;
        drop(pair.slave);

        let pane = Arc::new(Pane {
            id: NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed),
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
            }),
            notifier,
        });

        let reader_pane = Arc::clone(&pane);
        std::thread::spawn(move || reader_loop(reader_pane, reader));

        let waiter_pane = Arc::clone(&pane);
        std::thread::spawn(move || wait_for_exit(waiter_pane));
        Ok(pane)
    }
```

- [ ] **Step 4: Ajouter `Session::new_agent`**

Dans `crates/wimux-server/src/session.rs`, dans `impl Session`, juste après la méthode `pub fn new(...)` (avant `pub fn name(&self)`), ajouter :

```rust
    /// Crée une session agent (M2) : le volet racine exécute `program` + `args`
    /// dans `cwd`, dans une unique fenêtre, puis la session est marquée agent
    /// (non-reap + calcul de statut, cf. M1).
    pub fn new_agent(
        name: String,
        cols: u16,
        rows: u16,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
    ) -> Result<Arc<Session>> {
        let notifier = Notifier::new();
        let pane = Pane::spawn_command(
            cols,
            content_rows(rows),
            program,
            args,
            cwd,
            Arc::clone(&notifier),
        )?;
        let window = Window::new("win".to_string(), pane);

        let session = Arc::new(Session {
            name: Mutex::new(name),
            notifier,
            shell: program.to_string(),
            inner: Mutex::new(Inner {
                windows: vec![window],
                active_window: 0,
                cols,
                rows,
                command_line: None,
            }),
            attached: AtomicUsize::new(0),
            last_seen_gen: AtomicU64::new(0),
            paste_buffer: Mutex::new(String::new()),
            agent: AtomicBool::new(false),
        });
        session.reflow();
        session.mark_agent();
        Ok(session)
    }
```

- [ ] **Step 5: Lancer les tests lib session (attendu PASS)**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (dont `new_agent_est_marquee_agent_et_se_termine`, `new_agent_avec_cwd_demarre`, plus les tests M1 existants). Tests lents ConPTY — patience.

- [ ] **Step 6: Vérifier la non-régression pane (spawn délégué)**

Run: `cargo test -p wimux-server --lib pane -- --test-threads=1`
Expected: PASS (le `Pane::spawn` délégué se comporte comme avant).

- [ ] **Step 7: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/pane.rs crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(session): spawn_command parametre + Session::new_agent (M2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: `daemon.rs` — templates + création agent + câblage + tests d'intégration

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Modify: `crates/wimux-server/tests/common/mod.rs`
- Test: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces:**
- Consumes : Task 1 (`AgentTemplate`, `ClientMessage::{ListAgentTemplates, CreateAgentSession}`, `ServerMessage::AgentTemplates`), Task 2 (`Config.agent_templates`), Task 4 (`Session::new_agent`), existant (`Session::{name, send_input}`, `Config`).
- Produces :
  - `Server::agent_templates(&self) -> Vec<AgentTemplate>`.
  - `Server::create_agent_session(&self, name: Option<String>, template: &str, prompt: &str, cwd: Option<&str>) -> Result<Arc<Session>, String>`.
  - `daemon::run_on_with_config(pipe_name: &str, config: Config) -> Result<()>` (point d'entrée de test).
  - Helper de test `common::start_daemon_with_config(pipe: &str, conf: &str)`.

**Mécanisme de config de test (décision) :** `Config::load` ne lit QUE `%USERPROFILE%\.wimux.conf` puis `%APPDATA%\wimux\wimux.conf` (aucun override de chemin par variable d'env dans `config_path`). Écrire dans ces chemins réels clobberait la config de l'utilisateur, et `std::env::set_var` est `unsafe`/global (course entre tests). La voie **simple et fiable** retenue est donc un **point d'entrée de test avec config injectée** : `run_on_with_config(pipe, config)`. Le helper `start_daemon_with_config` construit une `Config::default()`, lui applique une chaîne de config (ce qui exerce aussi le vrai parseur `apply`), et démarre le démon dessus.

- [ ] **Step 1: Écrire les tests d'intégration (échouent)**

Dans `crates/wimux-server/tests/gui_mode.rs`, ajouter à la fin du fichier (les helpers `connect_retry`, `handshake`, `poll_list_until`, `fetch_list` existent déjà dans ce fichier / `common`) :

```rust
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
            s.name == "bot"
                && s.agent
                && s.agent_status == Some(wimux_protocol::AgentStatus::Done)
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
            s.name == "sh"
                && s.agent
                && s.agent_status == Some(wimux_protocol::AgentStatus::Done)
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
```

- [ ] **Step 2: Ajouter le helper de config injectée**

Dans `crates/wimux-server/tests/common/mod.rs`, à la fin du fichier, ajouter :

```rust
/// Démarre un démon de test avec une config construite depuis `conf` (contenu
/// façon `wimux.conf`, appliqué via `Config::apply`). Évite de toucher au
/// fichier utilisateur ou à des variables d'environnement globales.
pub fn start_daemon_with_config(pipe: &str, conf: &str) {
    let p = pipe.to_string();
    let mut config = wimux_server::config::Config::default();
    config.apply(conf);
    std::thread::spawn(move || {
        let _ = daemon::run_on_with_config(&p, config);
    });
    std::thread::sleep(Duration::from_millis(150));
}
```

- [ ] **Step 3: Lancer les tests d'intégration (attendu FAIL)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: FAIL — compilation (`no function run_on_with_config`, `no method agent_templates`, `create_agent_session` absente, câblage stub).

- [ ] **Step 4: Importer `AgentTemplate` dans `daemon.rs`**

Dans `crates/wimux-server/src/daemon.rs`, remplacer :

```rust
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, SessionInfo, recv, send,
};
```

par :

```rust
use wimux_protocol::{
    AgentTemplate, ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, SessionInfo,
    recv, send,
};
```

- [ ] **Step 5: Ajouter le constructeur injectable + le point d'entrée de test**

Dans `crates/wimux-server/src/daemon.rs`, remplacer la méthode `new` de `impl Server` :

```rust
    fn new() -> Arc<Server> {
        Arc::new(Server {
            sessions: Mutex::new(HashMap::new()),
            config: Config::load(),
            gui_viewed: Mutex::new(None),
        })
    }
```

par :

```rust
    fn new() -> Arc<Server> {
        Self::with_config(Config::load())
    }

    /// Construit un serveur avec une config donnée (utile aux tests qui doivent
    /// injecter des modèles d'agents sans toucher au fichier utilisateur).
    fn with_config(config: Config) -> Arc<Server> {
        Arc::new(Server {
            sessions: Mutex::new(HashMap::new()),
            config,
            gui_viewed: Mutex::new(None),
        })
    }
```

Puis remplacer la fonction `run_on` :

```rust
/// Lance le démon sur un pipe nommé donné (utile pour les tests isolés).
pub fn run_on(pipe_name: &str) -> Result<()> {
    let server = Server::new();
    let listener = PipeListener::bind(pipe_name);

    loop {
        match listener.accept() {
            Ok(conn) => {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(server, conn) {
                        eprintln!("wimux-server : client terminé sur erreur : {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("wimux-server : échec d'acceptation : {e}");
            }
        }
    }
}
```

par :

```rust
/// Lance le démon sur un pipe nommé donné (utile pour les tests isolés).
pub fn run_on(pipe_name: &str) -> Result<()> {
    serve(Server::new(), pipe_name)
}

/// Lance le démon avec une configuration injectée (tests : modèles d'agents).
pub fn run_on_with_config(pipe_name: &str, config: Config) -> Result<()> {
    serve(Server::with_config(config), pipe_name)
}

/// Boucle d'acceptation partagée par `run_on` et `run_on_with_config`.
fn serve(server: Arc<Server>, pipe_name: &str) -> Result<()> {
    let listener = PipeListener::bind(pipe_name);

    loop {
        match listener.accept() {
            Ok(conn) => {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(server, conn) {
                        eprintln!("wimux-server : client terminé sur erreur : {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("wimux-server : échec d'acceptation : {e}");
            }
        }
    }
}
```

- [ ] **Step 6: Ajouter `agent_templates` et `create_agent_session` à `Server`**

Dans `impl Server`, juste après la méthode `create_session`, ajouter :

```rust
    /// Modèles d'agents configurés (M2).
    fn agent_templates(&self) -> Vec<AgentTemplate> {
        self.config.agent_templates.clone()
    }

    /// Crée une session agent depuis un modèle. Substitue `{prompt}` dans les
    /// args s'il est présent ; sinon signale un envoi stdin. Nom auto
    /// `<template>-<n>` si `name` absent. Insère la session et, en cas de
    /// livraison stdin, envoie `prompt` + `\r` au volet racine après le spawn.
    fn create_agent_session(
        &self,
        name: Option<String>,
        template: &str,
        prompt: &str,
        cwd: Option<&str>,
    ) -> Result<Arc<Session>, String> {
        // Résoudre le modèle par son nom.
        let tpl = self
            .config
            .agent_templates
            .iter()
            .find(|t| t.name == template)
            .cloned()
            .ok_or_else(|| format!("modèle d'agent inconnu : {template}"))?;

        // Substituer {prompt} dans les args ; noter s'il reste à livrer sur stdin.
        let mut has_placeholder = false;
        let args: Vec<String> = tpl
            .args
            .iter()
            .map(|a| {
                if a.contains("{prompt}") {
                    has_placeholder = true;
                    a.replace("{prompt}", prompt)
                } else {
                    a.clone()
                }
            })
            .collect();
        let stdin_prompt = !has_placeholder;

        self.reap();
        let mut sessions = self.sessions.lock().unwrap();

        // Nom : fourni (doit être libre) ou auto `<template>-<n>`.
        let name = match name {
            Some(n) => {
                if sessions.contains_key(&n) {
                    return Err(format!("la session « {n} » existe déjà"));
                }
                n
            }
            None => {
                let mut i = 0;
                loop {
                    let candidate = format!("{template}-{i}");
                    if !sessions.contains_key(&candidate) {
                        break candidate;
                    }
                    i += 1;
                }
            }
        };

        let session = Session::new_agent(name.clone(), 80, 24, &tpl.program, &args, cwd)
            .map_err(|e| format!("création de la session agent : {e}"))?;
        sessions.insert(name, Arc::clone(&session));
        drop(sessions);

        if stdin_prompt && !prompt.is_empty() {
            let mut line = prompt.as_bytes().to_vec();
            line.push(b'\r');
            session.send_input(&line);
        }
        Ok(session)
    }
```

- [ ] **Step 7: Câbler les 2 messages (remplacer les stubs de Task 1)**

Dans `handle_client`, remplacer les deux stubs posés en Task 1 :

```rust
            ClientMessage::ListAgentTemplates => {} // câblé en Task 5
            ClientMessage::CreateAgentSession { .. } => {} // câblé en Task 5
            ClientMessage::Hello(_) => {}
```

par :

```rust
            ClientMessage::ListAgentTemplates => {
                let mut wr: &PipeConn = &conn;
                send(
                    &mut wr,
                    &ServerMessage::AgentTemplates(server.agent_templates()),
                )?;
            }
            ClientMessage::CreateAgentSession {
                name,
                template,
                prompt,
                cwd,
            } => {
                let reply =
                    match server.create_agent_session(name, &template, &prompt, cwd.as_deref()) {
                        Ok(s) => ServerMessage::SessionCreated { name: s.name() },
                        Err(e) => ServerMessage::Error(e),
                    };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Hello(_) => {}
```

- [ ] **Step 8: Lancer les tests d'intégration (attendu PASS)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS (dont les 4 nouveaux tests agents + les tests G3/G4 existants). Tests lents ConPTY — patience.

- [ ] **Step 9: Non-régression complète**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS (TUI + G1/G2/G3/G4 + M1 + protocole + config + pane + session + gui_mode).

- [ ] **Step 10: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
Expected: OK.

(Cette tâche modifie `tests/gui_mode.rs` et `tests/common/mod.rs` : les reformatages de `cargo fmt` y sont légitimes, NE PAS les `git checkout`.)

```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs crates/wimux-server/tests/common/mod.rs
git commit -m "$(printf 'feat(daemon): templates + creation de session agent + cablage (M2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 6: Pont Tauri — `SessionDto` étendu + commandes agents

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`
- Test: build (`cargo build`), pas de test unitaire (couche de pontage jetable).

**Interfaces:**
- Consumes : Task 1 (`ClientMessage::{ListAgentTemplates, CreateAgentSession}`, `ServerMessage::AgentTemplates`, `AgentStatus`), Task 5 (`SessionInfo.agent`/`.agent_status` réels via `List`).
- Produces (utilisés par Task 7) :
  - `SessionDto { name, attached, activity, bell, agent: bool, agent_status: Option<String> }`.
  - Commande `list_agent_templates() -> Result<Vec<AgentTemplateDto>, String>` avec `AgentTemplateDto { name: String }`.
  - Commande `create_agent(template: String, prompt: String, cwd: Option<String>, name: Option<String>) -> Result<String, String>`.

- [ ] **Step 1: Étendre `SessionDto` + helper de libellé**

Dans `wimux-gui/src-tauri/src/lib.rs`, remplacer :

```rust
#[derive(serde::Serialize)]
struct SessionDto {
    name: String,
    attached: bool,
    activity: bool,
    bell: bool,
}
```

par :

```rust
#[derive(serde::Serialize)]
struct SessionDto {
    name: String,
    attached: bool,
    activity: bool,
    bell: bool,
    agent: bool,
    agent_status: Option<String>,
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
```

- [ ] **Step 2: Renseigner `agent`/`agent_status` dans `list_sessions`**

Remplacer le corps de `list_sessions` :

```rust
            ServerMessage::Sessions(v) => Ok(v
                .into_iter()
                .map(|s| SessionDto {
                    name: s.name,
                    attached: s.attached,
                    activity: s.activity,
                    bell: s.bell,
                })
                .collect()),
```

par :

```rust
            ServerMessage::Sessions(v) => Ok(v
                .into_iter()
                .map(|s| SessionDto {
                    name: s.name,
                    attached: s.attached,
                    activity: s.activity,
                    bell: s.bell,
                    agent: s.agent,
                    agent_status: s.agent_status.map(agent_status_label),
                })
                .collect()),
```

- [ ] **Step 3: Ajouter `AgentTemplateDto` + les 2 commandes**

Dans `wimux-gui/src-tauri/src/lib.rs`, juste après la commande `create_session` (avant `kill_session`), ajouter :

```rust
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
```

- [ ] **Step 4: Enregistrer les commandes dans `invoke_handler!`**

Dans `run`, remplacer :

```rust
            list_sessions,
            create_session,
            kill_session,
            rename_session
        ])
```

par :

```rust
            list_sessions,
            create_session,
            kill_session,
            rename_session,
            list_agent_templates,
            create_agent
        ])
```

- [ ] **Step 5: Build (attendu OK)**

Run: `cd wimux-gui/src-tauri && cargo build`
Expected: OK.

- [ ] **Step 6: clippy + commit**

Run: `cd wimux-gui/src-tauri && RUSTFLAGS="-D warnings" cargo clippy --all-targets`
Expected: OK.

```bash
git add wimux-gui/src-tauri/src/lib.rs
git commit -m "$(printf 'feat(gui-bridge): SessionDto agent/agent_status + commandes agents (M2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 7: Frontend — lanceur modal + glyphe de statut

**Files:**
- Modify: `wimux-gui/index.html`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`
- Test: build (`npm run build`) + vérification manuelle (README).

**Interfaces:**
- Consumes (Task 6) : commandes `list_agent_templates` (→ `AgentTemplateDto[]`), `create_agent` (→ nom de session) ; `SessionDto` avec `agent: boolean` + `agent_status: string | null` ; commandes existantes `attach_session`, `create_session`, `kill_session`.
- Produces : dialogue modal « + agent » ; glyphe de statut dans le rail pour les sessions agent.

- [ ] **Step 1: Ajouter le bouton et le dialogue dans `index.html`**

Dans `wimux-gui/index.html`, remplacer :

```html
      <aside id="rail">
        <div id="sessions"></div>
        <button id="new-session" title="Nouvelle session">+</button>
      </aside>
      <div id="terminal"></div>
    </div>
```

par :

```html
      <aside id="rail">
        <div id="sessions"></div>
        <div id="rail-actions">
          <button id="new-session" title="Nouvelle session">+</button>
          <button id="new-agent" title="Lancer un agent">+ agent</button>
        </div>
      </aside>
      <div id="terminal"></div>
    </div>

    <div id="agent-modal" class="modal-overlay hidden">
      <div class="modal">
        <h2>Lancer un agent</h2>
        <label>Modèle
          <select id="agent-template"></select>
        </label>
        <label>Tâche / prompt
          <textarea id="agent-prompt" rows="4"></textarea>
        </label>
        <label>Répertoire (optionnel)
          <input id="agent-cwd" type="text" placeholder="cwd du daemon par défaut" />
        </label>
        <label>Nom (optionnel)
          <input id="agent-name" type="text" placeholder="auto : <modèle>-<n>" />
        </label>
        <div id="agent-error" class="agent-error"></div>
        <div class="modal-buttons">
          <button id="agent-cancel">Annuler</button>
          <button id="agent-launch">Lancer</button>
        </div>
      </div>
    </div>
```

- [ ] **Step 2: Étendre le type `SessionDto` et ajouter `AgentTemplateDto`**

Dans `wimux-gui/src/main.ts`, remplacer :

```ts
type SessionDto = { name: string; attached: boolean; activity: boolean; bell: boolean };
```

par :

```ts
type SessionDto = {
  name: string;
  attached: boolean;
  activity: boolean;
  bell: boolean;
  agent: boolean;
  agent_status: string | null;
};

type AgentTemplateDto = { name: string };
```

- [ ] **Step 3: Ajouter les helpers de glyphe et brancher le glyphe dans `renderRail`**

Dans `wimux-gui/src/main.ts`, juste avant `function renderRail(sessions: SessionDto[]) {`, ajouter :

```ts
function agentStatusGlyph(status: string | null): string {
  switch (status) {
    case "Working": return "⚙";
    case "Idle": return "○";
    case "Attention": return "❗";
    case "Done": return "✓";
    case "Error": return "✗";
    default: return "○";
  }
}

function agentStatusClass(status: string | null): string {
  switch (status) {
    case "Working": return "working";
    case "Idle": return "idle";
    case "Attention": return "attention";
    case "Done": return "done";
    case "Error": return "error";
    default: return "idle";
  }
}
```

Puis, dans `renderRail`, remplacer le bloc :

```ts
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
```

par :

```ts
    const isActive = s.name === activeSession;
    if (s.agent) {
      // Session agent : glyphe de statut (remplace les pastilles G4).
      const glyph = document.createElement("span");
      glyph.className = "agent-glyph " + agentStatusClass(s.agent_status);
      glyph.textContent = agentStatusGlyph(s.agent_status);
      glyph.title = s.agent_status ?? "agent";
      el.append(name, glyph, close);
    } else if (!isActive && (s.bell || s.activity)) {
      // Cloche prioritaire sur l'activité ; rien pour la session active.
      const dot = document.createElement("span");
      dot.className = "dot " + (s.bell ? "bell" : "activity");
      dot.textContent = s.bell ? "🔔" : "";
      el.append(name, dot, close);
    } else {
      el.append(name, close);
    }
```

- [ ] **Step 4: Ajouter la logique du dialogue modal**

Dans `wimux-gui/src/main.ts`, juste avant la ligne `document.getElementById("new-session")!.onclick = async () => {`, ajouter :

```ts
const agentModal = document.getElementById("agent-modal")!;
const agentTemplateSel = document.getElementById("agent-template") as HTMLSelectElement;
const agentPrompt = document.getElementById("agent-prompt") as HTMLTextAreaElement;
const agentCwd = document.getElementById("agent-cwd") as HTMLInputElement;
const agentName = document.getElementById("agent-name") as HTMLInputElement;
const agentError = document.getElementById("agent-error")!;

async function openAgentModal() {
  agentError.textContent = "";
  agentPrompt.value = "";
  agentCwd.value = "";
  agentName.value = "";
  agentTemplateSel.innerHTML = "";
  try {
    const templates = await invoke<AgentTemplateDto[]>("list_agent_templates");
    for (const t of templates) {
      const opt = document.createElement("option");
      opt.value = t.name;
      opt.textContent = t.name;
      agentTemplateSel.append(opt);
    }
  } catch (e) {
    agentError.textContent = "Impossible de charger les modèles : " + e;
  }
  agentModal.classList.remove("hidden");
}

function closeAgentModal() {
  agentModal.classList.add("hidden");
}

document.getElementById("new-agent")!.onclick = openAgentModal;
document.getElementById("agent-cancel")!.onclick = closeAgentModal;
document.getElementById("agent-launch")!.onclick = async () => {
  const template = agentTemplateSel.value;
  if (!template) {
    agentError.textContent = "Choisissez un modèle.";
    return;
  }
  const prompt = agentPrompt.value;
  const cwd = agentCwd.value.trim();
  const name = agentName.value.trim();
  try {
    const created = await invoke<string>("create_agent", {
      template,
      prompt,
      cwd: cwd || null,
      name: name || null,
    });
    closeAgentModal();
    await refresh();
    await switchTo(created);
  } catch (e) {
    agentError.textContent = "Échec : " + e;
  }
};
```

- [ ] **Step 5: Ajouter les styles (glyphe + boutons + modal)**

Dans `wimux-gui/src/styles.css`, à la fin du fichier, ajouter :

```css
/* M2 : actions du rail + glyphe d'agent + dialogue modal */
#rail-actions { display: flex; }
#rail-actions button { flex: 1; }
#new-agent { border: none; background: #2d2d2d; color: #ccc; padding: 8px; cursor: pointer; font-size: 13px; }
#new-agent:hover { background: #37373d; }

.session .agent-glyph { flex: 0 0 auto; line-height: 1; font-size: 13px; }
.session .agent-glyph.working { color: #0a84ff; }
.session .agent-glyph.idle { color: #888; }
.session .agent-glyph.attention { color: #ff9f0a; }
.session .agent-glyph.done { color: #30d158; }
.session .agent-glyph.error { color: #ff453a; }

.modal-overlay { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.5); display: flex; align-items: center; justify-content: center; z-index: 100; }
.modal-overlay.hidden { display: none; }
.modal { background: #252526; border: 1px solid #333; border-radius: 6px; padding: 16px; width: 360px; max-width: 90vw; display: flex; flex-direction: column; gap: 10px; color: #d4d4d4; }
.modal h2 { margin: 0 0 4px; font-size: 15px; }
.modal label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: #aaa; }
.modal select, .modal input, .modal textarea { background: #1e1e1e; color: #fff; border: 1px solid #3c3c3c; border-radius: 3px; padding: 6px; font: inherit; }
.modal textarea { resize: vertical; }
.modal .agent-error { color: #ff453a; font-size: 12px; min-height: 14px; }
.modal-buttons { display: flex; justify-content: flex-end; gap: 8px; }
.modal-buttons button { border: none; border-radius: 3px; padding: 6px 14px; cursor: pointer; }
#agent-cancel { background: #3a3a3a; color: #ddd; }
#agent-launch { background: #0a84ff; color: #fff; }
```

- [ ] **Step 6: Build frontend (attendu OK)**

Run: `cd wimux-gui && npm run build`
Expected: OK (compilation TypeScript + bundle Vite sans erreur).

- [ ] **Step 7: Ajouter la section « Vérification manuelle M2 » au README**

Dans `wimux-gui/README.md` (le même README que les vérifs manuelles G3/G4), **ajouter à la fin du fichier** :

```markdown
## Vérification manuelle M2 (agents)

Déclarer au moins un modèle dans `%USERPROFILE%\.wimux.conf` :

```text
agent-template echo   cmd.exe /c echo {prompt}
agent-template shell  cmd.exe
```

Rebuild + **redémarrer le démon détaché** (piège du serveur persistant), puis
lancer la GUI (`npm run tauri dev` dans `wimux-gui/`) et vérifier :

1. Cliquer **+ agent** : le dialogue s'ouvre, le menu liste `echo` et `shell`.
2. Modèle `echo`, prompt `bonjour`, cwd/nom vides → **Lancer**. La GUI bascule
   sur `echo-0` ; le glyphe passe ⚙ (*Working*, bleu) puis ✓ (*Done*, vert).
3. Modèle `shell` (interactif), prompt `echo salut` → **Lancer** : le prompt est
   injecté sur stdin (+ Entrée) ; la session vit puis (après `exit`) affiche ✓.
4. Un modèle absent / un cwd invalide affiche l'erreur dans le dialogue, sans
   créer de session.
5. Fermer un agent terminé via le `×` du rail.
```

- [ ] **Step 8: Commit**

```bash
git add wimux-gui/index.html wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "$(printf 'feat(gui): lanceur agent modal + glyphe de statut dans le rail (M2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Self-Review

**Spec coverage :**
- Config `agent-template <nom> <programme> [args...]` + `Config.agent_templates: Vec<AgentTemplate>` → Task 2 (4 tests : défaut vide, avec `{prompt}`, sans arg, plusieurs).
- `AgentTemplate { name, program, args }` sérialisable + `ListAgentTemplates`/`CreateAgentSession`/`AgentTemplates` → Task 1 (3 tests roundtrip, ajouts en fin d'enum, stubs d'exhaustivité).
- Correctif M1 `Pane::kill` ferme le `MasterPty` (master optionnel, `resize`/spawn adaptés) → Task 3, test lib du déblocage via `Arc::strong_count` + consigne BLOCKED.
- Création de volet paramétrée (`Pane::spawn_command`, `Pane::spawn` délègue) + `Session::new_agent` (+ `mark_agent`) → Task 4 (2 tests : marquée agent/one-shot, cwd valide).
- Serveur : résolution du modèle, substitution `{prompt}` (arg unique, pas de re-split), livraison stdin sinon, nommage auto `<template>-<n>`, insertion, câblage des 2 messages → Task 5 ; point d'entrée `run_on_with_config` + helper `start_daemon_with_config` pour la config de test ; 4 tests d'intégration (list, one-shot→Done, stdin→Done, modèle inconnu→Error) + non-régression `--workspace`.
- Pont Tauri : `SessionDto` + `agent`/`agent_status` (via `agent_status_label`), `AgentTemplateDto`, `list_agent_templates`, `create_agent`, `invoke_handler!` → Task 6.
- Frontend : bouton « + agent », dialogue modal (menu peuplé, prompt, cwd, nom, Lancer/Annuler), `create_agent` puis `switchTo`, glyphe de statut (⚙/○/❗/✓/✗) pour agents / pastilles G4 pour non-agents, CSS, README « Vérification manuelle M2 » → Task 7 (build `npm run build`).

**Placeholder scan :** aucun « TODO/à compléter ». Les stubs `=> {}` de Task 1 sont du code intermédiaire assumé, remplacés intégralement en Task 5, Step 7.

**Type consistency :**
- `AgentTemplate { name, program, args }` (Task 1) est consommé identiquement par `Config` (Task 2), `Server::{agent_templates, create_agent_session}` (Task 5) et le pont (Task 6, mappé vers `AgentTemplateDto { name }`).
- `Pane::spawn_command(cols, rows, program: &str, args: &[String], cwd: Option<&str>, notifier)` (Task 4) est appelé par `Pane::spawn` (mêmes args) et par `Session::new_agent` (Task 4), lui-même appelé par `Server::create_agent_session` avec `(name.clone(), 80, 24, &tpl.program, &args, cwd)` (Task 5) — signatures cohérentes.
- `Server::create_agent_session(name: Option<String>, template: &str, prompt: &str, cwd: Option<&str>)` (Task 5) est appelé depuis le bras `CreateAgentSession` avec `(name, &template, &prompt, cwd.as_deref())` — types alignés.
- `ClientMessage::CreateAgentSession { name, template, prompt, cwd }` / `ServerMessage::AgentTemplates(Vec<AgentTemplate>)` (Task 1) sont produits/consommés à l'identique par le pont (Task 6) et le frontend (Task 7).
- `SessionDto` (Task 6) : champs `agent: bool` + `agent_status: Option<String>` ↔ `SessionDto` TS `{ agent: boolean; agent_status: string | null }` (Task 7) ; libellés `agent_status_label` (`"Working"`.."Error"") ↔ `switch` de `agentStatusGlyph`/`agentStatusClass`.
- Helpers de test réutilisés sans redéfinition : `poll_status` (session, Task 4), `connect_retry`/`handshake`/`poll_list_until`/`fetch_list`/`start_daemon_with_config` (gui_mode/common, Task 5).

**Points signalés (choix non spécifiés) :**
1. **Mécanisme de config de test (Task 5)** — retenu : point d'entrée `run_on_with_config(pipe, Config)` + helper `start_daemon_with_config(pipe, conf)` qui applique la chaîne via `Config::apply`. Justifié par lecture de `config_path`/`Config::load` : aucun override de chemin par env, et écrire dans les chemins réels clobberait la config utilisateur ; `std::env::set_var` est `unsafe`/global (course inter-tests). C'est l'option la plus simple ET fiable, et elle exerce en prime le vrai parseur.
2. **Déblocage du lecteur sous ConPTY (Task 3)** — hypothèse : dropper le `MasterPty` ferme la ConPTY et EOF le `reader_loop`. Le test le prouve via `Arc::strong_count` (2 → 1). Incertitude assumée : si le compteur ne retombe pas à 1, c'est un **BLOCAGE réel** à signaler (status BLOCKED), pas à masquer — consigne explicite dans la tâche.
3. **`shell` de la session agent (Task 4)** — `Session::new_agent` renseigne `self.shell = program` : un futur split/`new_window` d'une session agent relancerait le programme de l'agent. Acceptable en M2 (un agent = un volet racine, cf. hors-périmètre) ; à revisiter si le split d'agents devient un cas d'usage.
4. **Glyphe affiché aussi pour l'agent actif (Task 7)** — contrairement aux pastilles G4 (masquées pour la session active), le glyphe de statut reste visible même sur l'agent actif (l'info Working/Done est utile en permanence). Choix de lisibilité, non imposé par la spec.
5. **cols/rows de création agent = 80×24 (Task 5)** — aligné sur `CreateSession` (mode GUI, la taille réelle vient de l'attache). Non imposé.

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-16-wimux-agents-m2-launcher.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — Un subagent frais par tâche, revue entre les tâches, itération rapide.

**2. Inline Execution** — Exécution des tâches dans cette session via executing-plans, par lots avec checkpoints.

**Which approach?**
