# wimux multi-agents M3 — Orchestration fan-out : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lancer un **lot** de N agents (même template) sur la même tâche, **chacun isolé dans un worktree git** d'un repo de base, orchestré **côté serveur** par un unique `CreateAgentBatch`. Chaque session du lot partage un `group` (id `batch<N>` via compteur `AtomicU64`), est nommée `<template>-<group>-<i>`, tourne dans une branche `wimux/<group>/<i>` et un worktree `<agent-worktree-root>/<group>-<i>`. Le rail GUI **regroupe** les membres sous un en-tête de lot affichant l'agrégat des statuts, avec un bouton « fermer le lot ». Le nettoyage (worktree + branche) a lieu au `kill` explicite de chaque membre. Réutilise M2 (`Session::new_agent` avec `cwd`, substitution `{prompt}`/stdin) et M1 (statut par agent, non-reap).

**Architecture:** Un **module serveur `worktree.rs`** encapsule les commandes git (`std::process::Command`, git dans le PATH) : `is_git_repo(base)`, `add(base, path, branch)` (crée le parent puis `git -C base worktree add path -b branch HEAD`), `remove(base, path, branch)` (best-effort `worktree remove --force` + `branch -D`), et le type `Worktree { base_repo, path, branch }`. La `Session` gagne `group: Option<String>` et `worktree: Option<Worktree>` (Mutex, setters/accès) ; `Session::kill` retire le worktree **après** avoir tué les volets, hors verrou `inner`. `Server::create_agent_batch(template, prompt, base_repo, count)` vérifie la base (git) puis le template, génère un `group` unique (`NEXT_BATCH_ID`), boucle `i` (worktree `add` + `Session::new_agent(cwd=worktree)` + `set_group`/`set_worktree`) **hors du verrou `sessions`** (git + spawn sont lents), puis verrouille `sessions` **une seule fois** pour insérer les N, et livre le prompt sur stdin si pas de `{prompt}` (comme M2). Échec partiel (git base KO, `add` KO, spawn KO) → rollback : `kill` des sessions déjà créées (chacune nettoie son worktree) + `worktree::remove` du worktree orphelin éventuel + `Err`, rien n'est inséré. `Server::list` renseigne `group = s.group()`. Le pont Tauri expose `SessionDto.group` et une commande `create_batch`. Le frontend ajoute un bouton « ⇉ lot » + un dialogue modal fan-out, et regroupe les membres d'un même `group` dans le rail (en-tête + agrégat + bouton fermer-le-lot qui boucle `kill_session`).

**Tech Stack:** Rust (workspace, edition 2024), `wimux-vt`, ConPTY (`portable-pty` 0.9), Named Pipe + postcard, git CLI (worktrees), Tauri (`wimux-gui/src-tauri`), TypeScript + xterm.js (`wimux-gui/src`).

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : suites TUI + G1/G2/G3/G4 + M1 + M2 vertes (`cargo test --workspace -- --test-threads=1`) ; `npm run build` OK.
- `cargo fmt` peut reformater `crates/wimux-server/tests/gui_mode.rs` hors périmètre — le rétablir (`git checkout -- crates/wimux-server/tests/gui_mode.rs`) avant commit si la tâche ne le modifie pas.
- Outil shell : **Bash tool** (git bash) ; tests lents (ConPTY + git worktree), `--test-threads=1`, patience.
- Chaque commit finit par le trailer : `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, via `git commit -m "$(printf '...')"`.
- **Piège `portable-pty` 0.9 :** le **programme est un seul jeton** ; les args sont séparés. Pour un volet racine déterministe dans les tests, utiliser le template `echo` (= `cmd.exe /c echo {prompt}`), éprouvé par M2.
- **Piège du daemon persistant :** après tout changement de protocole, rebuild + redémarrage du serveur détaché (requis seulement pour la vérification manuelle M3 du frontend, pas pour les tests de ce plan).
- **Ordre des variants postcard :** postcard sérialise le discriminant d'enum par sa position — **toujours ajouter les nouveaux variants À LA FIN** de `ClientMessage`/`ServerMessage` pour ne pas décaler les index existants.
- **Tests git :** nécessitent `git` dans le PATH (présent dans l'env : `git version 2.42.x`). Un repo git temporaire est créé PAR le test (`git init` + un commit `--allow-empty` avec `-c user.name`/`-c user.email` pour ne pas dépendre d'une config globale). Chaque test qui a besoin de git commence par une **garde** (`git --version`) et se termine tôt (`return` avec un `eprintln!` documenté) si git est absent. Nettoyage du dossier temp en fin de test.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : `SessionInfo` gagne `pub group: Option<String>` ; `ClientMessage::CreateAgentBatch { template, prompt, base_repo, count: u32 }` (fin d'enum) ; `ServerMessage::BatchCreated { group: String, sessions: Vec<String> }` (fin d'enum) ; 2 tests roundtrip + mise à jour des 2 tests `SessionInfo` existants.
- `crates/wimux-server/src/config.rs` — **modifier** : `Config.agent_worktree_root: PathBuf` (défaut `%LOCALAPPDATA%\wimux\worktrees`, repli `temp/wimux-worktrees`) + directive `set agent-worktree-root <path>` ; 2 tests.
- `crates/wimux-server/src/lib.rs` — **modifier** : `pub mod worktree;`.
- `crates/wimux-server/src/worktree.rs` — **créer** : `Worktree`, `is_git_repo`, `add`, `remove` + 2 tests d'intégration git.
- `crates/wimux-server/src/session.rs` — **modifier** : champs `group`/`worktree` (Mutex) + setters/accès ; `kill()` nettoie le worktree ; init `None` dans `new`/`new_agent` ; 1 test lib.
- `crates/wimux-server/src/daemon.rs` — **modifier** : stub no-op `CreateAgentBatch` (Task 1) puis `NEXT_BATCH_ID` + `Server::create_agent_batch` + câblage réel + `list` renseigne `group` (Task 5) ; `SessionInfo { .., group: None }` placeholder (Task 1).
- `crates/wimux-server/tests/gui_mode.rs` — **modifier** : helper `init_temp_git_repo` + 2 tests d'intégration fan-out (Task 5).
- `wimux-gui/src-tauri/src/lib.rs` — **modifier** : `SessionDto.group: Option<String>` ; commande `create_batch` ; `invoke_handler!`.
- `wimux-gui/index.html` — **modifier** : bouton `#new-batch` + dialogue modal `#batch-modal`.
- `wimux-gui/src/main.ts` — **modifier** : `type SessionDto` + `group` ; refactor `renderSession`/`renderBatchHeader` + regroupement ; logique du modal fan-out.
- `wimux-gui/src/styles.css` — **modifier** : styles en-tête de lot.
- `wimux-gui/README.md` — **modifier** : section « Vérification manuelle M3 » (ajout en fin, comme M2).

Le CLI (`crates/wimux-cli/src/main.rs`) ne construit **aucun** `SessionInfo` (seuls `daemon.rs` et 2 tests protocole le font — vérifié) et ne pattern-matche que par lecture de champs : ajouter `group` ne casse aucune exhaustivité, aucune modification requise.

---

## Task 1: Protocole — `SessionInfo.group` + messages de lot

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (placeholder `group: None` + stub d'exhaustivité)
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces (utilisés par Tasks 5/6) :
  - `SessionInfo` gagne `pub group: Option<String>` (dernier champ).
  - `ClientMessage::CreateAgentBatch { template: String, prompt: String, base_repo: String, count: u32 }`.
  - `ServerMessage::BatchCreated { group: String, sessions: Vec<String> }`.

- [ ] **Step 1: Écrire/mettre à jour les tests roundtrip (échouent)**

Dans le module `tests` de `crates/wimux-protocol/src/lib.rs` :

(a) Mettre à jour les **deux** tests `SessionInfo` existants pour ajouter le champ `group`. Dans `aller_retour_session_info_activite`, remplacer le littéral :

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
            group: None,
        };
```

Dans `aller_retour_session_info_agent`, remplacer le littéral :

```rust
        let info = SessionInfo {
            name: "bot".into(),
            windows: 1,
            attached: false,
            activity: false,
            bell: false,
            agent: true,
            agent_status: Some(AgentStatus::Working),
        };
```

par :

```rust
        let info = SessionInfo {
            name: "bot".into(),
            windows: 1,
            attached: false,
            activity: false,
            bell: false,
            agent: true,
            agent_status: Some(AgentStatus::Working),
            group: Some("batch0".into()),
        };
```

et, dans ce même test, après `assert_eq!(v[0].agent_status, Some(AgentStatus::Working));`, ajouter :

```rust
                assert_eq!(v[0].group.as_deref(), Some("batch0"));
```

(b) Ajouter, à la fin du module `tests` (avant l'accolade fermante `}`) :

```rust
    #[test]
    fn aller_retour_create_agent_batch() {
        let msg = ClientMessage::CreateAgentBatch {
            template: "echo".into(),
            prompt: "corrige le bug".into(),
            base_repo: "C:\\proj".into(),
            count: 3,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CreateAgentBatch {
                template,
                prompt,
                base_repo,
                count,
            } => {
                assert_eq!(template, "echo");
                assert_eq!(prompt, "corrige le bug");
                assert_eq!(base_repo, "C:\\proj");
                assert_eq!(count, 3);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_batch_created() {
        let msg = ServerMessage::BatchCreated {
            group: "batch0".into(),
            sessions: vec!["echo-batch0-0".into(), "echo-batch0-1".into()],
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::BatchCreated { group, sessions } => {
                assert_eq!(group, "batch0");
                assert_eq!(sessions, vec!["echo-batch0-0", "echo-batch0-1"]);
            }
            _ => panic!("mauvais variant"),
        }
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-protocol`
Expected: FAIL — compilation (`missing field group`, `no variant CreateAgentBatch`, `no variant BatchCreated`).

- [ ] **Step 3: Ajouter le champ `group` à `SessionInfo`**

Dans `crates/wimux-protocol/src/lib.rs`, dans `pub struct SessionInfo`, remplacer :

```rust
    /// Statut de l'agent ; `None` si `agent == false` (M1).
    pub agent_status: Option<AgentStatus>,
}
```

par :

```rust
    /// Statut de l'agent ; `None` si `agent == false` (M1).
    pub agent_status: Option<AgentStatus>,
    /// Identifiant de lot (M3) : les sessions d'un même fan-out le partagent
    /// (`batch<N>`). `None` pour une session hors lot.
    pub group: Option<String>,
}
```

- [ ] **Step 4: Ajouter le variant `ClientMessage` (fin d'enum)**

Dans `enum ClientMessage`, remplacer la fin (le variant `CreateAgentSession { .. }` est le dernier ; le `}` fermant l'enum le suit) :

```rust
    CreateAgentSession {
        name: Option<String>,
        template: String,
        prompt: String,
        cwd: Option<String>,
    },
}
```

par :

```rust
    CreateAgentSession {
        name: Option<String>,
        template: String,
        prompt: String,
        cwd: Option<String>,
    },
    /// Crée un **lot** (M3) : `count` sessions agent depuis un même modèle, chacune
    /// dans un worktree git de `base_repo`. Orchestration côté serveur (atomique).
    CreateAgentBatch {
        template: String,
        prompt: String,
        base_repo: String,
        count: u32,
    },
}
```

- [ ] **Step 5: Ajouter le variant `ServerMessage` (fin d'enum)**

Dans `enum ServerMessage`, remplacer la fin (le variant `AgentTemplates(...)` est le dernier) :

```rust
    /// Liste des modèles d'agents (réponse à `ListAgentTemplates`).
    AgentTemplates(Vec<AgentTemplate>),
}
```

par :

```rust
    /// Liste des modèles d'agents (réponse à `ListAgentTemplates`).
    AgentTemplates(Vec<AgentTemplate>),
    /// Lot créé (réponse à `CreateAgentBatch`) : identifiant de groupe + noms des
    /// sessions membres.
    BatchCreated {
        group: String,
        sessions: Vec<String>,
    },
}
```

- [ ] **Step 6: Adapter `daemon.rs` (placeholder `group` + stub d'exhaustivité)**

Dans `crates/wimux-server/src/daemon.rs`, dans `Server::list`, dans le littéral `SessionInfo { ... }`, remplacer :

```rust
                    agent: s.is_agent(),
                    agent_status: s.agent_status(std::time::Duration::from_secs(
                        self.config.agent_idle_seconds,
                    )),
                }
```

par :

```rust
                    agent: s.is_agent(),
                    agent_status: s.agent_status(std::time::Duration::from_secs(
                        self.config.agent_idle_seconds,
                    )),
                    group: None, // vraie valeur (`s.group()`) posée en Task 5
                }
```

Puis, dans `handle_client`, le `match msg` se termine par `ClientMessage::Hello(_) => {}`. Insérer juste AVANT cette ligne :

```rust
            ClientMessage::CreateAgentBatch { .. } => {} // câblé en Task 5
            ClientMessage::Hello(_) => {}
```

- [ ] **Step 7: Lancer les tests + build serveur (attendu PASS/OK)**

Run: `cargo test -p wimux-protocol`
Expected: PASS (dont les 2 nouveaux tests + les 2 `SessionInfo` mis à jour).

Run: `cargo build -p wimux-server`
Expected: OK (le `match` est exhaustif grâce au stub ; `SessionInfo` complet).

- [ ] **Step 8: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a modifié `crates/wimux-server/tests/gui_mode.rs` (hors périmètre) : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

- [ ] **Step 9: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(protocol): SessionInfo.group + messages de lot (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: Config — `agent-worktree-root`

**Files:**
- Modify: `crates/wimux-server/src/config.rs`
- Test: `crates/wimux-server/src/config.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces (utilisé par Task 5) : `Config.agent_worktree_root: PathBuf` (défaut `%LOCALAPPDATA%\wimux\worktrees`, repli `std::env::temp_dir().join("wimux-worktrees")`) ; directive `set agent-worktree-root <path>` (les jetons du chemin sont joints par un espace pour tolérer un chemin espacé).

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-server/src/config.rs`, ajouter (à la fin du module, avant l'accolade fermante) :

```rust
    #[test]
    fn agent_worktree_root_defaut_non_vide() {
        assert!(
            !Config::default()
                .agent_worktree_root
                .as_os_str()
                .is_empty(),
            "la racine de worktrees par défaut doit être non vide"
        );
    }

    #[test]
    fn set_agent_worktree_root_modifie_le_chemin() {
        let mut c = Config::default();
        c.apply("set agent-worktree-root C:\\x\\y\n");
        assert_eq!(
            c.agent_worktree_root,
            std::path::PathBuf::from("C:\\x\\y")
        );
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --lib config`
Expected: FAIL — `no field agent_worktree_root on type Config`.

- [ ] **Step 3: Importer `PathBuf`**

Dans `crates/wimux-server/src/config.rs`, sous les `use` existants (après `use std::collections::HashMap;`), ajouter :

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use wimux_protocol::AgentTemplate;
```

- [ ] **Step 4: Ajouter le champ à `Config`**

Dans `pub struct Config`, ajouter le champ après `agent_templates` :

```rust
    /// Modèles d'agents configurés (M2), directive `agent-template`.
    pub agent_templates: Vec<AgentTemplate>,
    /// Racine des worktrees de lots (M3), directive `set agent-worktree-root`.
    pub agent_worktree_root: PathBuf,
}
```

- [ ] **Step 5: Ajouter la fonction de défaut + l'initialiser dans `Default`**

Dans `crates/wimux-server/src/config.rs`, ajouter (après la fonction `config_path`, hors de tout `impl`) :

```rust
/// Racine par défaut des worktrees de lots : `%LOCALAPPDATA%\wimux\worktrees`,
/// avec repli sur le dossier temp du système si `LOCALAPPDATA` est absent.
fn default_worktree_root() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local).join("wimux").join("worktrees")
    } else {
        std::env::temp_dir().join("wimux-worktrees")
    }
}
```

Puis, dans `impl Default for Config`, remplacer le littéral final :

```rust
            agent_idle_seconds: 4,
            agent_templates: Vec::new(),
        }
```

par :

```rust
            agent_idle_seconds: 4,
            agent_templates: Vec::new(),
            agent_worktree_root: default_worktree_root(),
        }
```

- [ ] **Step 6: Ajouter la directive dans `apply`**

Dans `Config::apply`, dans le `match tokens.as_slice()`, insérer un bras juste AVANT le bras `["agent-template", name, program, args @ ..]` :

```rust
                ["set", "agent-worktree-root", rest @ ..] if !rest.is_empty() => {
                    self.agent_worktree_root = PathBuf::from(rest.join(" "));
                }
                ["agent-template", name, program, args @ ..] => {
```

(Le motif `["set", "agent-worktree-root", rest @ ..]` capture le chemin en fin de ligne et joint ses jetons par un espace — tolère un chemin espacé même si le défaut n'en a pas.)

- [ ] **Step 7: Lancer les tests (attendu PASS)**

Run: `cargo test -p wimux-server --lib config`
Expected: PASS (dont les 2 nouveaux tests).

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/config.rs
git commit -m "$(printf 'feat(config): directive agent-worktree-root (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: Module `worktree.rs` — git worktrees

**Files:**
- Modify: `crates/wimux-server/src/lib.rs` (`pub mod worktree;`)
- Create: `crates/wimux-server/src/worktree.rs`
- Test: `crates/wimux-server/src/worktree.rs` (module `tests`)

**Interfaces:**
- Consumes: rien (git CLI via `std::process::Command`).
- Produces (utilisés par Tasks 4/5) :
  - `pub struct Worktree { pub base_repo: PathBuf, pub path: PathBuf, pub branch: String }` (Clone, Debug).
  - `pub fn is_git_repo(base: &Path) -> bool`.
  - `pub fn add(base: &Path, path: &Path, branch: &str) -> Result<(), String>`.
  - `pub fn remove(base: &Path, path: &Path, branch: &str)` (best-effort, ne panique jamais).

- [ ] **Step 1: Déclarer le module**

Dans `crates/wimux-server/src/lib.rs`, ajouter `pub mod worktree;` en conservant l'ordre alphabétique existant (entre `window` — non, `window` est en fin ; place-le après `window`) :

```rust
pub mod commands;
pub mod config;
pub mod daemon;
pub mod pane;
pub mod pty;
pub mod session;
pub mod window;
pub mod worktree;
```

- [ ] **Step 2: Écrire le module avec ses tests (les tests échouent tant que le fichier n'existe pas)**

Créer `crates/wimux-server/src/worktree.rs` avec ce contenu **complet** :

```rust
//! Gestion des worktrees git pour l'orchestration fan-out (M3). Chaque agent d'un
//! lot tourne dans un worktree git isolé d'un repo de base, sur une branche
//! dédiée. Les commandes git passent par `std::process::Command` (git doit être
//! dans le PATH).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Worktree git porté par une session agent d'un lot (M3). Cloné pour être posé
/// sur la session (setter `set_worktree`) et rejoué au nettoyage (`kill`).
#[derive(Debug, Clone)]
pub struct Worktree {
    pub base_repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
}

/// `base` est-il un dépôt git ? (`git -C base rev-parse --is-inside-work-tree`
/// doit réussir ET écrire `true`). Renvoie `false` si git est absent ou si le
/// répertoire n'est pas un repo.
pub fn is_git_repo(base: &Path) -> bool {
    match Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
    {
        Ok(out) => out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// Crée un worktree : `git -C base worktree add path -b branch HEAD`. Crée le
/// dossier **parent** de `path` au préalable (git worktree add exige que le parent
/// existe, mais pas `path` lui-même). En cas d'échec, renvoie le `stderr` de git.
pub fn add(base: &Path, path: &Path, branch: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("création du dossier parent du worktree : {e}"))?;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(base)
        .arg("worktree")
        .arg("add")
        .arg(path)
        .arg("-b")
        .arg(branch)
        .arg("HEAD")
        .output()
        .map_err(|e| format!("git worktree add : {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Retire un worktree et sa branche (best-effort) : `git -C base worktree remove
/// --force path` puis `git -C base branch -D branch`. Ne panique jamais ; log les
/// erreurs sur stderr (un worktree orphelin après crash serveur reste toléré).
pub fn remove(base: &Path, path: &Path, branch: &str) {
    match Command::new("git")
        .arg("-C")
        .arg(base)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(path)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "wimux: échec `git worktree remove {}` : {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("wimux: `git worktree remove` indisponible : {e}"),
    }
    match Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["branch", "-D", branch])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "wimux: échec `git branch -D {branch}` : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("wimux: `git branch -D` indisponible : {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git est-il disponible ? (garde de test — l'env CI/dev l'a normalement.)
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Exécute `git <args>` dans `dir`, renvoie true si succès.
    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn add_puis_remove_worktree() {
        if !git_available() {
            eprintln!("git absent : test add_puis_remove_worktree ignoré");
            return;
        }
        // Repo de base temporaire (init + commit vide, config locale au commit).
        let base = std::env::temp_dir().join(format!("wimux-wt-base-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert!(git_in(&base, &["init"]), "git init a échoué");
        assert!(
            git_in(
                &base,
                &[
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "init",
                ],
            ),
            "commit initial a échoué"
        );
        assert!(is_git_repo(&base), "la base devrait être un repo git");

        let tree = std::env::temp_dir().join(format!("wimux-wt-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        add(&base, &tree, "wimux/test/0").expect("add worktree devrait réussir");
        assert!(tree.exists(), "le dossier worktree devrait exister après add");

        remove(&base, &tree, "wimux/test/0");
        assert!(
            !tree.exists(),
            "le dossier worktree devrait avoir disparu après remove"
        );

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&tree);
    }

    #[test]
    fn is_git_repo_faux_hors_repo() {
        let dir = std::env::temp_dir().join(format!("wimux-wt-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !is_git_repo(&dir),
            "un dossier vide hors repo ne doit pas être vu comme un repo git"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 3: Lancer les tests worktree (attendu PASS)**

Run: `cargo test -p wimux-server --lib worktree -- --test-threads=1`
Expected: PASS (`add_puis_remove_worktree`, `is_git_repo_faux_hors_repo`). Tests git réels — patience.

> Si `add_puis_remove_worktree` échoue avec un message git parlant (ex. branche déjà existante d'un run précédent avorté) : nettoyer manuellement (`git -C <base> worktree prune`, supprimer les dossiers temp `wimux-wt-*`) et relancer. Le test purge ses dossiers en début de run, mais une branche fantôme dans un `base` réutilisé serait à `prune`.

- [ ] **Step 4: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/lib.rs crates/wimux-server/src/worktree.rs
git commit -m "$(printf 'feat(worktree): module git worktree add/remove/is_git_repo (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: `session.rs` — champs group/worktree + nettoyage au kill

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Test: `crates/wimux-server/src/session.rs` (module `tests`)

**Interfaces:**
- Consumes: Task 3 (`crate::worktree::{Worktree, remove}`), existant (`Session::{new_agent, is_agent, group?, kill}`, `Mutex`).
- Produces (utilisés par Task 5) :
  - Champs privés `group: Mutex<Option<String>>`, `worktree: Mutex<Option<crate::worktree::Worktree>>` (init `None`).
  - `pub fn set_group(&self, group: String)`.
  - `pub fn set_worktree(&self, wt: crate::worktree::Worktree)`.
  - `pub fn group(&self) -> Option<String>`.
  - `pub fn worktree(&self) -> Option<crate::worktree::Worktree>`.
  - `Session::kill(&self)` retire le worktree (via `crate::worktree::remove`) **après** avoir tué les volets, **hors** verrou `inner`.

- [ ] **Step 1: Écrire le test lib (échoue)**

Dans le module `tests` de `crates/wimux-server/src/session.rs`, ajouter (à la fin du module, avant l'accolade fermante) :

```rust
    #[test]
    fn agent_sans_worktree_group_none_et_kill_ne_panique_pas() {
        let s = Session::new_agent(
            "a".into(),
            40,
            12,
            "cmd.exe",
            &["/c".into(), "echo".into(), "hi".into()],
            None,
        )
        .unwrap();
        // Aucun group ni worktree posé : group() est None.
        assert_eq!(s.group(), None);
        assert!(s.worktree().is_none());
        // set_group est visible via group().
        s.set_group("batch0".into());
        assert_eq!(s.group().as_deref(), Some("batch0"));
        // kill() sans worktree ne panique pas (rien à nettoyer).
        s.kill();
    }
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib session::tests::agent_sans_worktree_group_none_et_kill_ne_panique_pas -- --test-threads=1`
Expected: FAIL — compilation (`no method group` / `no method set_group` sur `Session`).

- [ ] **Step 3: Ajouter les champs à `struct Session`**

Dans `crates/wimux-server/src/session.rs`, dans `pub struct Session`, remplacer :

```rust
    /// Drapeau « session agent » (M1) : déclenche le calcul de statut et le
    /// non-reap. Posé par `mark_agent` (aucun chemin client en M1 ; c'est M2).
    agent: AtomicBool,
}
```

par :

```rust
    /// Drapeau « session agent » (M1) : déclenche le calcul de statut et le
    /// non-reap. Posé par `mark_agent` (aucun chemin client en M1 ; c'est M2).
    agent: AtomicBool,
    /// Identifiant de lot (M3), posé par `set_group` à la création du lot.
    group: Mutex<Option<String>>,
    /// Worktree git isolé de cette session de lot (M3), posé par `set_worktree`.
    /// Nettoyé (`worktree::remove`) au `kill`.
    worktree: Mutex<Option<crate::worktree::Worktree>>,
}
```

- [ ] **Step 4: Initialiser les champs dans `new` ET `new_agent`**

Dans `Session::new`, dans le littéral `Session { ... }`, remplacer :

```rust
            paste_buffer: Mutex::new(String::new()),
            agent: AtomicBool::new(false),
        });
        session.reflow();
        Ok(session)
    }
```

par :

```rust
            paste_buffer: Mutex::new(String::new()),
            agent: AtomicBool::new(false),
            group: Mutex::new(None),
            worktree: Mutex::new(None),
        });
        session.reflow();
        Ok(session)
    }
```

Dans `Session::new_agent`, dans le littéral `Session { ... }`, remplacer :

```rust
            paste_buffer: Mutex::new(String::new()),
            agent: AtomicBool::new(false),
        });
        session.reflow();
        session.mark_agent();
        Ok(session)
    }
```

par :

```rust
            paste_buffer: Mutex::new(String::new()),
            agent: AtomicBool::new(false),
            group: Mutex::new(None),
            worktree: Mutex::new(None),
        });
        session.reflow();
        session.mark_agent();
        Ok(session)
    }
```

- [ ] **Step 5: Ajouter les setters/accès (M3)**

Dans `impl Session`, juste après la méthode `is_agent` (avant `agent_status`), ajouter :

```rust
    /// M3 : rattache cette session à un lot (identifiant de groupe).
    pub fn set_group(&self, group: String) {
        *self.group.lock().unwrap() = Some(group);
    }

    /// M3 : identifiant de lot de cette session, ou `None` hors lot.
    pub fn group(&self) -> Option<String> {
        self.group.lock().unwrap().clone()
    }

    /// M3 : associe un worktree git à cette session (nettoyé au `kill`).
    pub fn set_worktree(&self, wt: crate::worktree::Worktree) {
        *self.worktree.lock().unwrap() = Some(wt);
    }

    /// M3 : worktree git de cette session, ou `None` s'il n'y en a pas.
    pub fn worktree(&self) -> Option<crate::worktree::Worktree> {
        self.worktree.lock().unwrap().clone()
    }
```

- [ ] **Step 6: Nettoyer le worktree dans `kill`**

Remplacer la méthode `kill` :

```rust
    pub fn kill(&self) {
        let inner = self.inner.lock().unwrap();
        for win in &inner.windows {
            win.kill_all();
        }
    }
```

par :

```rust
    pub fn kill(&self) {
        // Tuer les volets sous le verrou `inner`, puis le RELÂCHER avant le
        // nettoyage git (une commande externe lente ne doit pas tenir `inner`).
        {
            let inner = self.inner.lock().unwrap();
            for win in &inner.windows {
                win.kill_all();
            }
        }
        // M3 : retirer le worktree une fois les volets tués (git worktree remove
        // --force suffit même si le process racine vient de mourir).
        if let Some(wt) = self.worktree.lock().unwrap().take() {
            crate::worktree::remove(&wt.base_repo, &wt.path, &wt.branch);
        }
    }
```

- [ ] **Step 7: Lancer les tests lib session (attendu PASS)**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (dont `agent_sans_worktree_group_none_et_kill_ne_panique_pas` + tous les tests M1/M2 existants). Tests lents ConPTY — patience.

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

Si `cargo fmt` a touché `tests/gui_mode.rs` : `git checkout -- crates/wimux-server/tests/gui_mode.rs`.

```bash
git add crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(session): champs group/worktree + nettoyage worktree au kill (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: `daemon.rs` — `create_agent_batch` + câblage + intégration

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces:**
- Consumes : Task 1 (`ClientMessage::CreateAgentBatch`, `ServerMessage::BatchCreated`, `SessionInfo.group`), Task 2 (`Config.agent_worktree_root`), Task 3 (`crate::worktree::{Worktree, is_git_repo, add, remove}`), Task 4 (`Session::{set_group, set_worktree, group, kill}`), existant (`Session::{new_agent, name, send_input}`).
- Produces :
  - `static NEXT_BATCH_ID: AtomicU64` (compteur de lots, unique par démon).
  - `Server::create_agent_batch(&self, template: &str, prompt: &str, base_repo: &str, count: u32) -> Result<(String, Vec<String>), String>`.
  - Câblage `ClientMessage::CreateAgentBatch` → `ServerMessage::BatchCreated { group, sessions }` / `Error`.
  - `Server::list` renseigne `group: s.group()`.
  - Helper de test `init_temp_git_repo` (dans `gui_mode.rs`). `common::start_daemon_with_config` existe déjà (M2).

**Rappel de conception (verrous + atomicité) :** le travail lourd (`worktree::add` = process git, `Session::new_agent` = spawn ConPTY) se fait **hors** du verrou `sessions`, en accumulant les sessions créées dans un `Vec` local. Le verrou `sessions` n'est pris **qu'une fois**, à la fin, pour insérer les N membres d'un coup (aucune session partiellement visible). En cas d'échec au i-ème (git base KO en amont, `add` KO, spawn KO, chemin non-UTF-8), rollback : `s.kill()` sur chaque session déjà créée (chacune retire son propre worktree via Task 4) + `worktree::remove` du worktree **orphelin** éventuel (créé par `add` mais qu'aucune session ne porte encore, cas spawn KO), puis `Err` — **rien** n'est inséré dans `sessions`. Le `group` est unique (`NEXT_BATCH_ID.fetch_add`) donc les noms/branches ne collisionnent jamais entre lots.

- [ ] **Step 1: Écrire les tests d'intégration (échouent)**

Dans `crates/wimux-server/tests/gui_mode.rs`, ajouter à la fin du fichier (les helpers `connect_retry`, `handshake`, `poll_list_until`, `fetch_list`, `common::start_daemon_with_config` existent déjà) :

```rust
// --- M3 : orchestration fan-out (lots d'agents en worktrees) --------------

/// Crée un dépôt git temporaire (init + commit vide) et renvoie son chemin.
/// Renvoie `None` si git est absent (le test se termine alors proprement).
fn init_temp_git_repo(label: &str) -> Option<std::path::PathBuf> {
    let git_ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("wimux-m3-repo-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    assert!(git(&["init"]), "git init a échoué");
    assert!(
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]),
        "commit initial a échoué"
    );
    Some(dir)
}

#[test]
fn create_agent_batch_cree_worktrees_puis_les_nettoie() {
    let Some(repo) = init_temp_git_repo("ok") else {
        eprintln!("git absent : test create_agent_batch_cree_worktrees_puis_les_nettoie ignoré");
        return;
    };
    let pipe = format!(r"\\.\pipe\wimux-test-{}-batchok", std::process::id());
    let root = std::env::temp_dir().join(format!("wimux-m3-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Template déterministe `echo` (= cmd.exe /c echo {prompt}) + racine de worktrees.
    let conf = format!(
        "agent-template echo cmd.exe /c echo {{prompt}}\nset agent-worktree-root {}\n",
        root.display()
    );
    common::start_daemon_with_config(&pipe, &conf);

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentBatch {
                template: "echo".into(),
                prompt: "salut".into(),
                base_repo: repo.to_string_lossy().into_owned(),
                count: 2,
            },
        )
        .unwrap();
    }
    let (group, names) = {
        let mut r: &PipeConn = &conn;
        match recv::<_, ServerMessage>(&mut r).unwrap() {
            ServerMessage::BatchCreated { group, sessions } => (group, sessions),
            other => panic!("attendu BatchCreated, reçu {other:?}"),
        }
    };
    assert_eq!(names.len(), 2, "le lot devrait avoir 2 membres : {names:?}");

    // List : 2 sessions du même group, marquées agent.
    let g = group.clone();
    let listed = poll_list_until(&pipe, 20, |list| {
        list.iter()
            .filter(|s| s.group.as_deref() == Some(g.as_str()) && s.agent)
            .count()
            == 2
    });
    assert!(
        listed,
        "List devrait montrer 2 membres agent du group {group} : {:?}",
        fetch_list(&pipe)
    );

    // Les 2 dossiers de worktree existent.
    let wt0 = root.join(format!("{group}-0"));
    let wt1 = root.join(format!("{group}-1"));
    assert!(wt0.exists(), "worktree 0 absent : {}", wt0.display());
    assert!(wt1.exists(), "worktree 1 absent : {}", wt1.display());

    // Kill des 2 membres : chacun nettoie son worktree.
    for name in &names {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::Kill {
                name: name.clone(),
            },
        )
        .unwrap();
        let mut r: &PipeConn = &conn;
        let _ = recv::<_, ServerMessage>(&mut r); // Ok
    }

    // Les dossiers de worktree ont disparu (nettoyage).
    let gone = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !wt0.exists() && !wt1.exists() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    assert!(
        gone,
        "les worktrees devraient avoir été nettoyés après Kill : {} / {}",
        wt0.display(),
        wt1.display()
    );

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn create_agent_batch_base_non_git_renvoie_error() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-batchnogit", std::process::id());
    common::start_daemon_with_config(&pipe, "agent-template echo cmd.exe /c echo {prompt}\n");
    // Dossier existant mais qui n'est PAS un repo git.
    let notgit = std::env::temp_dir().join(format!("wimux-m3-notgit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&notgit);
    std::fs::create_dir_all(&notgit).unwrap();

    let conn = Arc::new(connect_retry(&pipe));
    handshake(&conn);
    {
        let mut w: &PipeConn = &conn;
        send(
            &mut w,
            &ClientMessage::CreateAgentBatch {
                template: "echo".into(),
                prompt: "x".into(),
                base_repo: notgit.to_string_lossy().into_owned(),
                count: 2,
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
        "une base non-git doit répondre Error"
    );

    let _ = std::fs::remove_dir_all(&notgit);
}
```

- [ ] **Step 2: Lancer les tests d'intégration (attendu FAIL)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: FAIL — compilation (`no variant/field ...`, `create_agent_batch` absente, câblage stub) ou assertion (`BatchCreated` non renvoyé).

- [ ] **Step 3: Importer `AtomicU64` et `Worktree` dans `daemon.rs`**

Dans `crates/wimux-server/src/daemon.rs`, remplacer :

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

par :

```rust
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
```

Et remplacer :

```rust
use crate::config::{Action, Config};
use crate::pane::CopyAction;
use crate::session::Session;
use crate::window::{Move, SplitDir};
```

par :

```rust
use crate::config::{Action, Config};
use crate::pane::CopyAction;
use crate::session::Session;
use crate::window::{Move, SplitDir};
use crate::worktree::{self, Worktree};
```

- [ ] **Step 4: Ajouter le compteur de lots (static)**

Dans `crates/wimux-server/src/daemon.rs`, juste après le bloc `use` (avant `pub struct Server`), ajouter :

```rust
/// Compteur de lots (M3), unique par démon : garantit des `group` (donc des
/// branches `wimux/<group>/<i>`) distincts entre fan-outs successifs.
static NEXT_BATCH_ID: AtomicU64 = AtomicU64::new(0);
```

- [ ] **Step 5: Renseigner `group` dans `Server::list`**

Dans `Server::list`, remplacer le placeholder posé en Task 1 :

```rust
                    group: None, // vraie valeur (`s.group()`) posée en Task 5
```

par :

```rust
                    group: s.group(),
```

- [ ] **Step 6: Ajouter `Server::create_agent_batch`**

Dans `impl Server`, juste après la méthode `create_agent_session`, ajouter :

```rust
    /// Crée un **lot** de `count` sessions agent (M3), chacune dans un worktree
    /// git de `base_repo`, sur une branche `wimux/<group>/<i>`. Le travail lourd
    /// (git + spawn) se fait hors verrou `sessions` ; l'insertion des N membres
    /// est atomique (un seul verrou). Rollback complet en cas d'échec partiel.
    fn create_agent_batch(
        &self,
        template: &str,
        prompt: &str,
        base_repo: &str,
        count: u32,
    ) -> Result<(String, Vec<String>), String> {
        // (1) La base doit être un dépôt git.
        let base = std::path::PathBuf::from(base_repo);
        if !worktree::is_git_repo(&base) {
            return Err(format!(
                "le répertoire de base n'est pas un dépôt git : {base_repo}"
            ));
        }

        // (2) Résoudre le modèle par son nom.
        let tpl = self
            .config
            .agent_templates
            .iter()
            .find(|t| t.name == template)
            .cloned()
            .ok_or_else(|| format!("modèle d'agent inconnu : {template}"))?;

        // Substituer {prompt} dans les args (une fois, commun aux N agents) ;
        // sinon livraison stdin après spawn (comme M2).
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

        // (3) Identifiant de lot unique.
        let group = format!("batch{}", NEXT_BATCH_ID.fetch_add(1, Ordering::Relaxed));
        let root = &self.config.agent_worktree_root;

        // (4) Boucle i : worktree + spawn, HORS verrou `sessions`. Rollback des
        // sessions déjà créées (chacune retire son worktree via kill).
        let rollback = |created: &[(String, Arc<Session>)]| {
            for (_, s) in created {
                s.kill();
            }
        };
        let mut created: Vec<(String, Arc<Session>)> = Vec::new();
        for i in 0..count {
            let name = format!("{template}-{group}-{i}");
            let branch = format!("wimux/{group}/{i}");
            let path = root.join(format!("{group}-{i}"));
            let Some(path_str) = path.to_str().map(|s| s.to_string()) else {
                rollback(&created);
                return Err(format!(
                    "chemin de worktree non-UTF-8 : {}",
                    path.display()
                ));
            };

            // Créer le worktree.
            if let Err(e) = worktree::add(&base, &path, &branch) {
                rollback(&created);
                return Err(format!("échec de création du worktree {i} : {e}"));
            }

            // Spawn l'agent dans le worktree.
            match Session::new_agent(name.clone(), 80, 24, &tpl.program, &args, Some(&path_str)) {
                Ok(session) => {
                    session.set_group(group.clone());
                    session.set_worktree(Worktree {
                        base_repo: base.clone(),
                        path: path.clone(),
                        branch: branch.clone(),
                    });
                    created.push((name, session));
                }
                Err(e) => {
                    // Worktree orphelin (aucune session ne le porte) : le retirer,
                    // puis rollback des sessions déjà créées.
                    worktree::remove(&base, &path, &branch);
                    rollback(&created);
                    return Err(format!("échec du lancement de l'agent {i} : {e}"));
                }
            }
        }

        // (5) Insertion atomique des N membres sous un unique verrou.
        {
            let mut sessions = self.sessions.lock().unwrap();
            for (name, session) in &created {
                sessions.insert(name.clone(), Arc::clone(session));
            }
        }

        // (6) Livraison stdin si pas de placeholder (comme M2).
        if stdin_prompt && !prompt.is_empty() {
            let mut line = prompt.as_bytes().to_vec();
            line.push(b'\r');
            for (_, session) in &created {
                session.send_input(&line);
            }
        }

        let names = created.into_iter().map(|(n, _)| n).collect();
        Ok((group, names))
    }
```

- [ ] **Step 7: Câbler le message (remplacer le stub de Task 1)**

Dans `handle_client`, remplacer le stub posé en Task 1 :

```rust
            ClientMessage::CreateAgentBatch { .. } => {} // câblé en Task 5
            ClientMessage::Hello(_) => {}
```

par :

```rust
            ClientMessage::CreateAgentBatch {
                template,
                prompt,
                base_repo,
                count,
            } => {
                let reply =
                    match server.create_agent_batch(&template, &prompt, &base_repo, count) {
                        Ok((group, sessions)) => ServerMessage::BatchCreated { group, sessions },
                        Err(e) => ServerMessage::Error(e),
                    };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Hello(_) => {}
```

- [ ] **Step 8: Lancer les tests d'intégration (attendu PASS)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS (dont les 2 nouveaux tests M3 + les tests G3/G4/M2 existants). Tests lents (ConPTY + git worktree) — patience.

- [ ] **Step 9: Non-régression complète**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS (TUI + G1/G2/G3/G4 + M1 + M2 + protocole + config + pane + session + worktree + gui_mode).

- [ ] **Step 10: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
Expected: OK.

(Cette tâche modifie `tests/gui_mode.rs` : les reformatages de `cargo fmt` y sont légitimes, NE PAS les `git checkout`.)

```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs
git commit -m "$(printf 'feat(daemon): create_agent_batch (fan-out worktrees) + cablage + list.group (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 6: Pont Tauri — `SessionDto.group` + commande `create_batch`

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`
- Test: build (`cargo build`), pas de test unitaire (couche de pontage jetable).

**Interfaces:**
- Consumes : Task 1 (`ClientMessage::CreateAgentBatch`, `ServerMessage::BatchCreated`, `SessionInfo.group`), Task 5 (`List` renseigne `group`).
- Produces (utilisés par Task 7) :
  - `SessionDto { name, attached, activity, bell, agent, agent_status, group: Option<String> }`.
  - Commande `create_batch(template: String, prompt: String, base_repo: String, count: u32) -> Result<String, String>` (renvoie le `group`).

- [ ] **Step 1: Étendre `SessionDto`**

Dans `wimux-gui/src-tauri/src/lib.rs`, remplacer :

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
    group: Option<String>,
}
```

- [ ] **Step 2: Mapper `group` dans `list_sessions`**

Dans la commande `list_sessions`, dans le `.map(|s| SessionDto { ... })`, remplacer :

```rust
                    agent: s.agent,
                    agent_status: s.agent_status.map(agent_status_label),
                })
```

par :

```rust
                    agent: s.agent,
                    agent_status: s.agent_status.map(agent_status_label),
                    group: s.group,
                })
```

- [ ] **Step 3: Ajouter la commande `create_batch`**

Dans `wimux-gui/src-tauri/src/lib.rs`, juste après la commande `create_agent`, ajouter :

```rust
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
```

- [ ] **Step 4: Enregistrer la commande**

Dans `invoke_handler!`, remplacer :

```rust
            list_agent_templates,
            create_agent
        ])
```

par :

```rust
            list_agent_templates,
            create_agent,
            create_batch
        ])
```

- [ ] **Step 5: Build + clippy**

Run: `cd wimux-gui/src-tauri && cargo build`
Expected: OK.

Run: `cd wimux-gui/src-tauri && RUSTFLAGS="-D warnings" cargo clippy --all-targets`
Expected: OK.

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/src-tauri/src/lib.rs
git commit -m "$(printf 'feat(gui-bridge): SessionDto.group + commande create_batch (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 7: Frontend — dialogue fan-out + regroupement du rail

**Files:**
- Modify: `wimux-gui/index.html`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`
- Test: `npm run build` (OK) + vérification manuelle (README).

**Interfaces:**
- Consumes : Task 6 (`SessionDto.group`, commande `create_batch`), existant (`list_agent_templates`, `kill_session`, `switchTo`, `refresh`, `renderRail`).
- Produces : `type SessionDto` + `group: string | null` ; `renderSession(s)` / `renderBatchHeader(group, members)` ; dialogue `#batch-modal` ; bouton `#new-batch`.

- [ ] **Step 1: Ajouter le bouton + le dialogue modal (HTML)**

Dans `wimux-gui/index.html`, dans `#rail-actions`, remplacer :

```html
        <div id="rail-actions">
          <button id="new-session" title="Nouvelle session">+</button>
          <button id="new-agent" title="Lancer un agent">+ agent</button>
        </div>
```

par :

```html
        <div id="rail-actions">
          <button id="new-session" title="Nouvelle session">+</button>
          <button id="new-agent" title="Lancer un agent">+ agent</button>
          <button id="new-batch" title="Lancer un lot d'agents">⇉ lot</button>
        </div>
```

Puis, juste APRÈS le bloc `<div id="agent-modal" ...> ... </div>` (avant `<script type="module" ...>`), ajouter le dialogue fan-out :

```html
    <div id="batch-modal" class="modal-overlay hidden">
      <div class="modal">
        <h2>Lancer un lot d'agents</h2>
        <label>Repo de base (git)
          <input id="batch-repo" type="text" placeholder="C:\chemin\vers\repo" />
        </label>
        <label>Modèle
          <select id="batch-template"></select>
        </label>
        <label>Tâche / prompt
          <textarea id="batch-prompt" rows="4"></textarea>
        </label>
        <label>Nombre d'agents
          <input id="batch-count" type="number" min="1" value="2" />
        </label>
        <div id="batch-error" class="agent-error"></div>
        <div class="modal-buttons">
          <button id="batch-cancel">Annuler</button>
          <button id="batch-launch">Lancer</button>
        </div>
      </div>
    </div>
```

- [ ] **Step 2: Étendre le type + refactor `renderRail` (TS)**

Dans `wimux-gui/src/main.ts`, remplacer le type :

```ts
type SessionDto = {
  name: string;
  attached: boolean;
  activity: boolean;
  bell: boolean;
  agent: boolean;
  agent_status: string | null;
};
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
  group: string | null;
};
```

Puis remplacer **entièrement** la fonction `renderRail` existante par les **trois** fonctions suivantes : le corps de l'ancienne boucle `for` devient `renderSession` (inchangé, mais renvoie l'élément au lieu de l'`append`), `renderBatchHeader` construit l'en-tête de lot, et le nouveau `renderRail` orchestre le regroupement par `group` :

```ts
function renderSession(s: SessionDto): HTMLElement {
  const el = document.createElement("div");
  el.className = "session" + (s.name === activeSession ? " active" : "");
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = s.name;
  let clickTimer: number | null = null;
  name.ondblclick = (ev) => {
    ev.stopPropagation();
    if (clickTimer !== null) { clearTimeout(clickTimer); clickTimer = null; }
    startRename(el, s.name);
  };
  const close = document.createElement("span");
  close.className = "close";
  close.textContent = "×";
  close.onclick = async (ev) => { ev.stopPropagation(); await invoke("kill_session", { name: s.name }).catch(() => {}); await refresh(); };
  el.onclick = () => {
    if (clickTimer !== null) return; // 2e clic d'un double-clic : ignore, laisse ondblclick gerer
    clickTimer = window.setTimeout(() => { clickTimer = null; switchTo(s.name); }, 200);
  };
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
  return el;
}

function renderBatchHeader(group: string, members: SessionDto[]): HTMLElement {
  const el = document.createElement("div");
  el.className = "batch-header";
  const title = document.createElement("span");
  title.className = "batch-name";
  title.textContent = group;
  // Agrégat des statuts : ⚙ Working, ✓ Done, ✗ Error.
  let working = 0, done = 0, error = 0;
  for (const m of members) {
    if (m.agent_status === "Working") working++;
    else if (m.agent_status === "Done") done++;
    else if (m.agent_status === "Error") error++;
  }
  const agg = document.createElement("span");
  agg.className = "batch-agg";
  agg.textContent = `⚙${working} ✓${done} ✗${error}`;
  const close = document.createElement("span");
  close.className = "batch-close";
  close.textContent = "×";
  close.title = "Fermer le lot";
  close.onclick = async (ev) => {
    ev.stopPropagation();
    for (const m of members) {
      await invoke("kill_session", { name: m.name }).catch(() => {});
    }
    await refresh();
  };
  el.append(title, agg, close);
  return el;
}

function renderRail(sessions: SessionDto[]) {
  lastSessions = sessions;
  const container = document.getElementById("sessions")!;
  container.innerHTML = "";
  // Regrouper par `group` en préservant l'ordre d'apparition ; les sessions
  // sans group sont rendues comme avant, après les lots.
  const groups = new Map<string, SessionDto[]>();
  const ungrouped: SessionDto[] = [];
  for (const s of sessions) {
    if (s.group) {
      let arr = groups.get(s.group);
      if (!arr) { arr = []; groups.set(s.group, arr); }
      arr.push(s);
    } else {
      ungrouped.push(s);
    }
  }
  for (const [group, members] of groups) {
    container.append(renderBatchHeader(group, members));
    for (const s of members) container.append(renderSession(s));
  }
  for (const s of ungrouped) container.append(renderSession(s));
}
```

- [ ] **Step 3: Ajouter la logique du dialogue fan-out (TS)**

Dans `wimux-gui/src/main.ts`, juste APRÈS le bloc du modal agent (après le handler `document.getElementById("agent-launch")!.onclick = async () => { ... };`, avant `document.getElementById("new-session")!.onclick`), ajouter :

```ts
const batchModal = document.getElementById("batch-modal")!;
const batchRepo = document.getElementById("batch-repo") as HTMLInputElement;
const batchTemplateSel = document.getElementById("batch-template") as HTMLSelectElement;
const batchPrompt = document.getElementById("batch-prompt") as HTMLTextAreaElement;
const batchCount = document.getElementById("batch-count") as HTMLInputElement;
const batchError = document.getElementById("batch-error")!;

async function openBatchModal() {
  batchError.textContent = "";
  batchRepo.value = "";
  batchPrompt.value = "";
  batchCount.value = "2";
  batchTemplateSel.innerHTML = "";
  try {
    const templates = await invoke<AgentTemplateDto[]>("list_agent_templates");
    for (const t of templates) {
      const opt = document.createElement("option");
      opt.value = t.name;
      opt.textContent = t.name;
      batchTemplateSel.append(opt);
    }
  } catch (e) {
    batchError.textContent = "Impossible de charger les modèles : " + e;
  }
  batchModal.classList.remove("hidden");
}

function closeBatchModal() {
  batchModal.classList.add("hidden");
}

document.getElementById("new-batch")!.onclick = openBatchModal;
document.getElementById("batch-cancel")!.onclick = closeBatchModal;
document.getElementById("batch-launch")!.onclick = async () => {
  const template = batchTemplateSel.value;
  if (!template) {
    batchError.textContent = "Choisissez un modèle.";
    return;
  }
  const baseRepo = batchRepo.value.trim();
  if (!baseRepo) {
    batchError.textContent = "Indiquez le repo de base.";
    return;
  }
  const count = parseInt(batchCount.value, 10);
  if (!Number.isFinite(count) || count < 1) {
    batchError.textContent = "Le nombre d'agents doit être ≥ 1.";
    return;
  }
  const prompt = batchPrompt.value;
  try {
    const group = await invoke<string>("create_batch", {
      template,
      prompt,
      baseRepo,
      count,
    });
    closeBatchModal();
    await refresh();
    console.log("lot créé:", group);
  } catch (e) {
    batchError.textContent = "Échec : " + e;
  }
};
```

- [ ] **Step 4: Ajouter les styles (CSS)**

Dans `wimux-gui/src/styles.css`, à la fin du fichier, ajouter :

```css
/* M3 : bouton lot + en-tête de lot dans le rail */
#new-batch { border: none; background: #2d2d2d; color: #ccc; padding: 8px; cursor: pointer; font-size: 13px; }
#new-batch:hover { background: #37373d; }

.batch-header { display: flex; align-items: center; gap: 6px; padding: 6px 10px; background: #202022; color: #9aa; border-left: 3px solid #6a5acd; font-size: 12px; }
.batch-header .batch-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
.batch-header .batch-agg { flex: 0 0 auto; color: #bbb; letter-spacing: 1px; }
.batch-header .batch-close { flex: 0 0 auto; color: #999; cursor: pointer; visibility: hidden; }
.batch-header:hover .batch-close { visibility: visible; }
.batch-header .batch-close:hover { color: #ff453a; }
```

- [ ] **Step 5: Build (attendu OK)**

Run: `cd wimux-gui && npm run build`
Expected: OK (compilation TypeScript sans erreur ; `renderRail`/`renderSession`/`renderBatchHeader` cohérents ; `SessionDto.group` référencé partout).

- [ ] **Step 6: Documenter la vérification manuelle (README)**

Dans `wimux-gui/README.md`, à la fin du fichier (après la section « Vérification manuelle M2 (agents) »), ajouter :

```markdown

## Vérification manuelle M3 (lots fan-out)

Prérequis : au moins un modèle configuré (cf. M2), un **repo git** local
(`git init` + un commit), et le serveur relancé après rebuild (piège du daemon
persistant). Racine des worktrees : `%LOCALAPPDATA%\wimux\worktrees` par défaut,
ou une directive `set agent-worktree-root <chemin>` dans `%USERPROFILE%\.wimux.conf`.

1. Cliquer **⇉ lot** : le dialogue s'ouvre (repo de base, modèle, prompt, nombre).
2. Renseigner le **repo de base** (chemin d'un dépôt git), modèle `echo`, prompt
   `bonjour`, nombre `2` → **Lancer**.
   - **Attendu :** le rail affiche un **en-tête de lot** (`batch0`) avec l'agrégat
     des statuts (`⚙2 ✓0 ✗0` puis `⚙0 ✓2 ✗0`), et **2 membres** en dessous
     (`echo-batch0-0`, `echo-batch0-1`), chacun avec son glyphe M2 (⚙ → ✓).
   - Sous `%LOCALAPPDATA%\wimux\worktrees`, deux dossiers `batch0-0` / `batch0-1`
     apparaissent (worktrees git ; `git -C <repo> worktree list` les montre).
3. Cliquer un membre : le terminal bascule sur cette session (agent dans son
   worktree).
4. Cliquer le **×** de l'en-tête de lot (visible au survol) : **fermer le lot**
   tue les 2 membres ; leurs dossiers de worktree et branches `wimux/batch0/*`
   disparaissent (`git -C <repo> worktree list` / `git branch` le confirment).
5. Un **repo non-git** (ou un chemin invalide) affiche l'erreur dans le dialogue,
   sans créer ni session ni worktree.
```

- [ ] **Step 7: Commit**

```bash
git add wimux-gui/index.html wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "$(printf 'feat(gui): dialogue fan-out + regroupement du rail par lot (M3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Vérification finale (récapitulatif)

- [ ] `cargo test --workspace -- --test-threads=1` — vert (TUI + G1/G2/G3/G4 + M1 + M2 + M3 : protocole, config, worktree, session, daemon/gui_mode).
- [ ] `cargo fmt --check` propre ; `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets` propre.
- [ ] `cd wimux-gui/src-tauri && RUSTFLAGS="-D warnings" cargo clippy --all-targets` propre.
- [ ] `cd wimux-gui && npm run build` OK.
- [ ] Vérification manuelle M3 (README) : lancer un lot sur un repo git, voir l'en-tête + agrégat évoluer, fermer le lot (worktrees + branches nettoyés).
- [ ] Piège daemon persistant : rebuild + redémarrer le serveur détaché avant la vérif manuelle du frontend.
