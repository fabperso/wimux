# wimux GUI W3 — Rail enrichi (cwd + branche git) : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Afficher, sous le nom de chaque session dans le rail GUI, son **répertoire courant vivant** (qui suit les `cd`) et sa **branche git**, à la manière de CMUX. Le cwd est capté par un **renifleur OSC 7 passif** dans le flux de sortie du volet actif ; la branche en est dérivée par lecture de `.git/HEAD` (sans lancer `git`).

**Architecture:** Quatre briques indépendantes.
1. **Renifleur OSC 7 à état** (`pane.rs`) : dans `reader_loop`, sous le verrou d'état du volet et APRÈS `st.terminal.advance(&buf[..n])`, on fait passer `&buf[..n]` dans une petite machine à états (`Osc7Sniffer`) qui reconnaît `ESC ] 7 ; <file://host/chemin> (BEL|ST)`, décode l'URI en chemin natif Windows et met à jour `PaneState.cwd`. L'état (octets partiels d'une séquence coupée entre deux lectures) vit dans `PaneState`. Le reste du pipeline (advance, réponses, diffusion aux abonnés, cloche) est **inchangé** — sniff purement passif, indépendant de `strip_ansi`.
2. **Branche git** (`git.rs`, nouveau module) : `git_branch(cwd: &Path) -> Option<String>` remonte les dossiers parents jusqu'à un `.git` répertoire, lit `HEAD` (`ref: refs/heads/<b>` → `<b>` ; sha 40 hex détaché → sha court ; `.git` fichier de worktree → `None`).
3. **Injection OSC 7 au spawn** (`session.rs`) : PowerShell/pwsh n'émet pas l'OSC 7 par défaut. `osc7_prompt_injection(shell) -> Option<Vec<String>>` (fonction pure) renvoie les args de spawn `-NoExit -Command "<hook de prompt>"` pour un shell PowerShell/pwsh, `None` sinon (repli sans régression : `cwd`/`branch` restent `None`, rail nom seul). Câblé au spawn du shell dans `Session::new`, `gui_new_window`, `gui_split`.
4. **Transport + rendu** : `SessionInfo` gagne `cwd`/`branch` **en fin de struct** (postcard : ajout en fin, aucun réordonnancement) ; `daemon.rs::list` les renseigne via `Session::active_pane_cwd()` (cwd du volet actif de la fenêtre active) + `git_branch`. Le frontend (`SessionDto` Rust + TS) ajoute une 2e ligne de rail (cwd abrégé + branche), masquée si `cwd == null`.

**Tech Stack:** Rust edition 2024 (workspace `wimux-protocol` / `wimux-server`), postcard (sérialisation par index de position/champ), Named Pipe Windows, ConPTY (`portable-pty`), Tauri 2 + TypeScript/Vite + xterm.js (`wimux-gui`). Shell par défaut des tests : `powershell.exe` (`Config::default().default_shell`).

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : `cargo test --workspace -- --test-threads=1` vert ; `npm run build` OK.
- **Postcard** : `cwd`/`branch` ajoutés **EN FIN** de `SessionInfo` (après `group`) ; aucun réordonnancement de champ.
- **Injection PowerShell (Task 5)** : l'émission OSC 7 réelle n'est **pas testable en unitaire** de façon déterministe → la valider MANUELLEMENT (README). Les tests automatiques couvrent le PARSER (renifleur + décodeur), `git_branch`, la fonction de décision d'injection, et la plomberie `SessionInfo` (en injectant une séquence OSC 7 FIXE dans le flux de sortie d'un volet, **sans dépendre du hook de prompt**).
- `cargo fmt` peut reformater `crates/wimux-server/tests/gui_mode.rs` hors périmètre — le rétablir (`git checkout -- crates/wimux-server/tests/gui_mode.rs`) avant commit si la tâche ne le modifie pas. **Task 4 le modifie légitimement : ne pas le rétablir.**
- Outil shell : **Bash tool** (git bash). Tests lents (ConPTY) : `--test-threads=1`, patience (timeouts généreux).
- Piège daemon détaché : rebuild + redémarrage du serveur après tout changement de protocole (manuel seulement ; sans objet pour les tests, qui lancent leur propre démon).
- Chaque commit finit par `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, via `git commit -m "$(printf '...')"`.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — `SessionInfo` += `cwd: Option<String>` / `branch: Option<String>` (fin de struct) ; compléter les 2 constructeurs de test ; nouveau test roundtrip `aller_retour_session_info_cwd_branche`. (Task 1)
- `crates/wimux-server/src/daemon.rs` — `list()` : ajout `cwd: None, branch: None` (stub Task 1) puis `cwd: s.active_pane_cwd()` / `branch = cwd.and_then(git_branch)` (Task 4). (Tasks 1, 4)
- `crates/wimux-server/src/pane.rs` — `PaneState.cwd` + `PaneState.sniffer` ; `struct Osc7Sniffer` + `enum SniffState` + `feed()` ; `decode_osc7_uri` / `percent_decode` / `uri_path_to_windows` ; hook dans `reader_loop` ; `Pane::cwd()` ; tests unitaires du renifleur/décodeur. (Task 2)
- `crates/wimux-server/src/git.rs` — **nouveau** : `git_branch` + `read_head_branch` + tests. (Task 3)
- `crates/wimux-server/src/lib.rs` — `pub mod git;`. (Task 3)
- `crates/wimux-server/src/session.rs` — `Session::active_pane_cwd()` ; `osc7_prompt_injection` (pure) + `OSC7_PS_SNIPPET` ; helpers `spawn_shell_pane_with` / `spawn_shell_pane` ; câblage au spawn dans `Session::new`, `gui_split`, `gui_new_window` ; tests unitaires de `osc7_prompt_injection`. (Tasks 4, 5)
- `crates/wimux-server/tests/gui_mode.rs` — test d'intégration `cwd_et_branche_via_osc7`. (Task 4)
- `wimux-gui/src-tauri/src/lib.rs` — `SessionDto` (Rust) += `cwd`/`branch` + mapping dans `list_sessions`. (Task 6)
- `wimux-gui/src/main.ts` — type `SessionDto` += `cwd`/`branch` ; 2e ligne de rail (`.session-main` / `.session-meta`) + `abbreviateCwd`. (Task 6)
- `wimux-gui/src/styles.css` — styles `.session-main` / `.session-meta` / `.meta-cwd` / `.meta-branch`. (Task 6)
- `wimux-gui/README.md` — section « Vérification manuelle W3 ». (Task 6)

---

## Task 1: Protocole — `SessionInfo.cwd` / `.branch`

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify (stub pour compiler) : `crates/wimux-server/src/daemon.rs`

**Interfaces:**
- Produces (modifié) :
  ```rust
  pub struct SessionInfo {
      pub name: String,
      pub windows: u32,
      pub activity: bool,
      pub bell: bool,
      pub agent: bool,
      pub agent_status: Option<AgentStatus>,
      pub group: Option<String>,
      pub attached: bool,          // (champs existants, ordre INCHANGÉ)
      /// cwd courant du volet actif (chemin natif affichable), `None` si inconnu.
      pub cwd: Option<String>,     // AJOUTÉ EN FIN
      /// branche git du cwd, `None` si hors repo / inconnu.
      pub branch: Option<String>,  // AJOUTÉ EN FIN
  }
  ```
  (Note : l'ordre réel des champs existants reste tel quel dans le fichier ; seuls `cwd` puis `branch` sont **appendus après `group`**.)

- [ ] **Step 1: Écrire le test roundtrip (échoue : champs absents)**

Ajouter dans `#[cfg(test)] mod tests` de `crates/wimux-protocol/src/lib.rs`, avant la `}` finale du module :

```rust
    #[test]
    fn aller_retour_session_info_cwd_branche() {
        let info = SessionInfo {
            name: "dev".into(),
            windows: 1,
            attached: true,
            activity: false,
            bell: false,
            agent: false,
            agent_status: None,
            group: None,
            cwd: Some("C:\\proj\\wimux".into()),
            branch: Some("main".into()),
        };
        let msg = ServerMessage::Sessions(vec![info]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::Sessions(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].cwd.as_deref(), Some("C:\\proj\\wimux"));
                assert_eq!(v[0].branch.as_deref(), Some("main"));
            }
            _ => panic!("mauvais variant"),
        }
    }
```

Vérifier l'échec de compilation (champs manquants) :

```bash
cargo test -p wimux-protocol 2>&1 | tail -20
```

FAIL attendu : `error[E0063]: missing fields cwd, branch in initializer of SessionInfo`.

- [ ] **Step 2: Ajouter les champs + compléter les 2 constructeurs de test existants**

Dans `struct SessionInfo`, après le champ `group: Option<String>,` (dernier champ), ajouter :

```rust
    /// cwd courant du volet actif (chemin natif affichable), `None` si inconnu (W3).
    pub cwd: Option<String>,
    /// Branche git du cwd, `None` si hors repo / inconnu (W3).
    pub branch: Option<String>,
```

Compléter les deux constructeurs de test existants (`aller_retour_session_info_activite` ~ligne 535 et `aller_retour_session_info_agent` ~ligne 564) en ajoutant, après leur champ `group: ...,` :

```rust
            cwd: None,
            branch: None,
```

- [ ] **Step 3: Stub daemon pour compiler (rempli en Task 4)**

Dans `crates/wimux-server/src/daemon.rs`, méthode `list()`, littéral `SessionInfo { ... }` (~ligne 82), après `group: s.group(),` ajouter :

```rust
                    cwd: None,
                    branch: None,
```

- [ ] **Step 4: Vert protocole + compilation serveur**

```bash
cargo test -p wimux-protocol -- --test-threads=1 2>&1 | tail -20
cargo build -p wimux-server 2>&1 | tail -20
cargo fmt
RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets 2>&1 | tail -20
```

PASS attendu : tous les tests protocole verts (dont `aller_retour_session_info_cwd_branche`), `wimux-server` compile, clippy propre.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "$(printf 'feat(protocol): SessionInfo.cwd + branch (W3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: Renifleur OSC 7 (`pane.rs`)

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`

**Interfaces:**
- Produces :
  - `Pane::cwd(&self) -> Option<String>` — cwd courant du volet (dernier OSC 7 capté), `None` si aucun.
  - `struct Osc7Sniffer` (privé) : `Osc7Sniffer::default()` ; `fn feed(&mut self, bytes: &[u8]) -> Option<String>` (renvoie le DERNIER cwd complété dans le chunk).
  - `fn decode_osc7_uri(payload: &[u8]) -> Option<String>` (privé, pur) : `file://<host>/<chemin>` → chemin natif Windows ; host vide ou `localhost` (insensible à la casse) accepté, sinon `None`.
- Consumes : `PaneState` (ajout de 2 champs).

- [ ] **Step 1: Écrire les tests du renifleur + décodeur (échouent : types absents)**

Ajouter dans `#[cfg(test)] mod tests` de `crates/wimux-server/src/pane.rs`, avant la `}` finale du module :

```rust
    #[test]
    fn sniffer_bel_extrait_le_cwd() {
        let mut s = Osc7Sniffer::default();
        let out = s.feed(b"before\x1b]7;file:///C:/a/b\x07after");
        assert_eq!(out.as_deref(), Some("C:\\a\\b"));
    }

    #[test]
    fn sniffer_st_extrait_le_cwd() {
        let mut s = Osc7Sniffer::default();
        let out = s.feed(b"\x1b]7;file:///C:/a/b\x1b\\");
        assert_eq!(out.as_deref(), Some("C:\\a\\b"));
    }

    #[test]
    fn sniffer_sequence_coupee_en_deux_lectures() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]7;file:///C:/a"), None);
        let out = s.feed(b"/b\x07");
        assert_eq!(out.as_deref(), Some("C:\\a\\b"));
    }

    #[test]
    fn sniffer_url_decode_espace() {
        let mut s = Osc7Sniffer::default();
        let out = s.feed(b"\x1b]7;file:///C:/a%20b\x07");
        assert_eq!(out.as_deref(), Some("C:\\a b"));
    }

    #[test]
    fn sniffer_localhost_accepte() {
        let mut s = Osc7Sniffer::default();
        let out = s.feed(b"\x1b]7;file://localhost/C:/x\x07");
        assert_eq!(out.as_deref(), Some("C:\\x"));
    }

    #[test]
    fn sniffer_host_distant_ignore() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]7;file://autremachine/C:/x\x07"), None);
    }

    #[test]
    fn sniffer_sans_osc7_pas_de_faux_positif() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"texte ordinaire\r\nPS C:\\> "), None);
    }

    #[test]
    fn sniffer_osc_autre_que_7_ignore() {
        let mut s = Osc7Sniffer::default();
        // OSC 0 (titre de fenêtre) : ignoré, pas de cwd.
        assert_eq!(s.feed(b"\x1b]0;mon titre\x07"), None);
    }

    #[test]
    fn decode_osc7_uri_hote_vide() {
        assert_eq!(
            decode_osc7_uri(b"file:///C:/foo/bar").as_deref(),
            Some("C:\\foo\\bar")
        );
    }
```

Vérifier l'échec :

```bash
cargo test -p wimux-server --lib pane 2>&1 | tail -20
```

FAIL attendu : `cannot find type Osc7Sniffer` / `cannot find function decode_osc7_uri`.

- [ ] **Step 2: Implémenter le renifleur + le décodeur**

Dans `crates/wimux-server/src/pane.rs`, après la définition de `struct PaneState { ... }` (avant `pub struct Pane`), ajouter :

```rust
/// Garde-fou : un payload OSC 7 plausible (une URI de chemin) ne dépasse pas
/// cette taille ; au-delà on abandonne la séquence (protection anti-emballement).
const OSC7_MAX: usize = 4096;

/// Renifleur OSC 7 **à état** : reconnaît `ESC ] 7 ; <payload> (BEL | ESC \)`
/// dans le flux brut d'un volet. À état car une séquence peut être coupée entre
/// deux lectures PTY (le `payload` partiel survit d'un `feed` au suivant).
#[derive(Default)]
struct Osc7Sniffer {
    state: SniffState,
    /// Chiffres du paramètre `Ps` (avant le premier `;`).
    ps: Vec<u8>,
    /// Octets du payload d'un OSC 7 (après `7;`), jusqu'au terminateur.
    payload: Vec<u8>,
}

#[derive(Default, PartialEq)]
enum SniffState {
    /// Hors séquence.
    #[default]
    Ground,
    /// Vu `ESC` (0x1b).
    Esc,
    /// Vu `ESC ]` : on lit `Ps` jusqu'au `;`.
    Ps,
    /// Dans le payload d'un OSC 7.
    Payload,
    /// Vu `ESC` dans le payload (attend `\` pour le terminateur ST).
    PayloadEsc,
    /// OSC non-7 : on jette jusqu'au terminateur.
    Skip,
    /// Vu `ESC` dans un OSC non-7.
    SkipEsc,
}

impl Osc7Sniffer {
    /// Fait avancer la machine sur `bytes`. Renvoie le DERNIER cwd complété dans
    /// ce chunk (le plus récent l'emporte), ou `None` si aucun.
    fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        let mut last: Option<String> = None;
        for &b in bytes {
            match self.state {
                SniffState::Ground => {
                    if b == 0x1b {
                        self.state = SniffState::Esc;
                    }
                }
                SniffState::Esc => {
                    if b == 0x5d {
                        // ESC ]
                        self.ps.clear();
                        self.state = SniffState::Ps;
                    } else if b == 0x1b {
                        self.state = SniffState::Esc;
                    } else {
                        self.state = SniffState::Ground;
                    }
                }
                SniffState::Ps => match b {
                    b';' => {
                        if self.ps.len() == 1 && self.ps[0] == b'7' {
                            self.payload.clear();
                            self.state = SniffState::Payload;
                        } else {
                            self.state = SniffState::Skip;
                        }
                    }
                    0x30..=0x39 => {
                        self.ps.push(b);
                        if self.ps.len() > 4 {
                            self.state = SniffState::Skip;
                        }
                    }
                    0x07 => self.state = SniffState::Ground, // BEL prématuré
                    0x1b => self.state = SniffState::SkipEsc,
                    _ => self.state = SniffState::Skip,
                },
                SniffState::Payload => match b {
                    0x07 => {
                        if let Some(p) = decode_osc7_uri(&self.payload) {
                            last = Some(p);
                        }
                        self.state = SniffState::Ground;
                    }
                    0x1b => self.state = SniffState::PayloadEsc,
                    _ => {
                        self.payload.push(b);
                        if self.payload.len() > OSC7_MAX {
                            self.state = SniffState::Ground;
                        }
                    }
                },
                SniffState::PayloadEsc => {
                    if b == b'\\' {
                        if let Some(p) = decode_osc7_uri(&self.payload) {
                            last = Some(p);
                        }
                    }
                    // ESC suivi de `\` (ST) : séquence terminée. Autre chose :
                    // séquence abandonnée. Dans les deux cas on repart à Ground.
                    self.state = SniffState::Ground;
                }
                SniffState::Skip => match b {
                    0x07 => self.state = SniffState::Ground,
                    0x1b => self.state = SniffState::SkipEsc,
                    _ => {}
                },
                SniffState::SkipEsc => {
                    self.state = if b == 0x1b {
                        SniffState::SkipEsc
                    } else {
                        // `\` (ST) => fin, tout autre octet => on continue à jeter.
                        if b == b'\\' {
                            SniffState::Ground
                        } else {
                            SniffState::Skip
                        }
                    };
                }
            }
        }
        last
    }
}

/// Décode un payload OSC 7 (`file://<host>/<chemin>`) en chemin natif Windows.
/// `host` doit être vide ou `localhost` (insensible à la casse), sinon `None`
/// (repli : on ignore un host distant, dont le chemin n'a pas de sens local).
/// URL-décode les `%XX`. `None` si le payload n'est pas une URI `file://`
/// exploitable.
fn decode_osc7_uri(payload: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(payload).ok()?;
    let rest = s.strip_prefix("file://")?;
    // Séparer host (jusqu'au premier '/') du chemin (qui commence par ce '/').
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let decoded = percent_decode(&rest[slash..]);
    Some(uri_path_to_windows(&decoded))
}

/// URL-décodage minimal des `%XX` (les autres octets sont conservés tels quels).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Convertit un chemin d'URI `/C:/a/b` en chemin natif Windows `C:\a\b` : retire
/// le `/` de tête devant une lettre de lecteur, puis remplace `/` par `\`.
fn uri_path_to_windows(path: &str) -> String {
    let b = path.as_bytes();
    let trimmed = if b.len() >= 3 && b[0] == b'/' && b[2] == b':' && b[1].is_ascii_alphabetic() {
        &path[1..]
    } else {
        path
    };
    trimmed.replace('/', "\\")
}
```

- [ ] **Step 3: Ajouter les champs à `PaneState` + le hook dans `reader_loop` + `Pane::cwd()`**

Dans `struct PaneState { ... }`, après le champ `subscribers: Vec<...>,`, ajouter :

```rust
    /// cwd courant du volet, mis à jour par le renifleur OSC 7 (W3).
    cwd: Option<String>,
    /// État du renifleur OSC 7 (partiels d'une séquence coupée entre lectures).
    sniffer: Osc7Sniffer,
```

Dans `Pane::spawn_command`, littéral `PaneState { ... }`, après `subscribers: Vec::new(),`, ajouter :

```rust
                cwd: None,
                sniffer: Osc7Sniffer::default(),
```

Dans `reader_loop`, à l'intérieur du bloc `let rang = { let mut st = pane.state.lock().unwrap(); ... };`, **juste après** `st.terminal.advance(&buf[..n]);` et avant `let responses = st.terminal.take_responses();`, ajouter :

```rust
                    // Renifleur OSC 7 passif (W3) : sous le même verrou, sur les
                    // mêmes octets bruts ; met à jour le cwd sans toucher au reste.
                    if let Some(cwd) = st.sniffer.feed(&buf[..n]) {
                        st.cwd = Some(cwd);
                    }
```

Ajouter la méthode `Pane::cwd()` dans `impl Pane`, à côté de `size()` :

```rust
    /// cwd courant du volet (dernier OSC 7 capté), `None` si aucun (W3).
    pub fn cwd(&self) -> Option<String> {
        self.state.lock().unwrap().cwd.clone()
    }
```

- [ ] **Step 4: Vert renifleur + non-régression pane**

```bash
cargo test -p wimux-server --lib pane -- --test-threads=1 2>&1 | tail -30
cargo fmt
RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets 2>&1 | tail -20
```

PASS attendu : les 9 tests du renifleur/décodeur verts, et les tests `pane` existants (notifier, snapshot, kill…) toujours verts. Clippy propre.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "$(printf 'feat(server): renifleur OSC 7 -> Pane.cwd (W3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: `git_branch` (module `git.rs`)

**Files:**
- Create: `crates/wimux-server/src/git.rs`
- Modify: `crates/wimux-server/src/lib.rs`

**Interfaces:**
- Produces : `pub fn git_branch(cwd: &std::path::Path) -> Option<String>` — remonte les parents jusqu'à un `.git` répertoire ; lit `HEAD`. `ref: refs/heads/<b>` → `Some("<b>")` ; sha 40 hex détaché → `Some(sha[..7])` ; `.git` fichier (worktree) → `None` (premier jet) ; rien trouvé → `None`.

- [ ] **Step 1: Créer `git.rs` avec les tests (échouent : module absent)**

Créer `crates/wimux-server/src/git.rs` :

```rust
//! Détermination de la branche git d'un répertoire, par **lecture de fichier
//! uniquement** (sans lancer `git`). On remonte les dossiers parents depuis le
//! cwd jusqu'à trouver un dossier contenant un `.git` répertoire (car
//! `.git/HEAD` n'existe qu'à la RACINE d'un dépôt), puis on lit `HEAD`.

use std::path::Path;

/// Branche git du dépôt contenant `cwd`, ou `None` (hors dépôt / HEAD illisible /
/// `.git` de worktree — repli `None` au premier jet).
pub fn git_branch(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let git = d.join(".git");
        if git.is_dir() {
            return read_head_branch(&git);
        }
        if git.is_file() {
            // `.git` fichier = worktree lié (`gitdir: ...`). Repli `None`.
            return None;
        }
        dir = d.parent();
    }
    None
}

/// Lit `<git_dir>/HEAD` : `ref: refs/heads/<b>` → `<b>` ; sha 40 hex → sha court.
fn read_head_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(rest) = head.strip_prefix("ref: refs/heads/") {
        if rest.is_empty() {
            return None;
        }
        return Some(rest.to_string());
    }
    if head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Dossier temporaire unique auto-nettoyé (Drop).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!(
                "wimux-git-test-{}-{label}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Écrit `<root>/.git/HEAD` avec `content`.
    fn write_head(root: &Path, content: &str) {
        let git = root.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(git.join("HEAD"), content).unwrap();
    }

    #[test]
    fn branche_depuis_head_symbolique() {
        let t = TempDir::new("sym");
        write_head(t.path(), "ref: refs/heads/feat/x\n");
        assert_eq!(git_branch(t.path()).as_deref(), Some("feat/x"));
    }

    #[test]
    fn branche_depuis_sous_dossier_remonte() {
        let t = TempDir::new("sub");
        write_head(t.path(), "ref: refs/heads/feat/x\n");
        let sub = t.path().join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        // Le sous-dossier n'a pas de `.git` : on doit remonter jusqu'à la racine.
        assert_eq!(git_branch(&sub).as_deref(), Some("feat/x"));
    }

    #[test]
    fn head_detache_donne_sha_court() {
        let t = TempDir::new("det");
        write_head(t.path(), "0123456789abcdef0123456789abcdef01234567\n");
        assert_eq!(git_branch(t.path()).as_deref(), Some("0123456"));
    }

    #[test]
    fn sans_git_donne_none() {
        let t = TempDir::new("nogit");
        assert_eq!(git_branch(t.path()), None);
    }

    #[test]
    fn git_fichier_worktree_donne_none() {
        let t = TempDir::new("wt");
        fs::write(t.path().join(".git"), "gitdir: C:/repo/.git/worktrees/wt\n").unwrap();
        assert_eq!(git_branch(t.path()), None);
    }
}
```

Déclarer le module dans `crates/wimux-server/src/lib.rs`, en respectant l'ordre alphabétique (après `pub mod daemon;`) :

```rust
pub mod git;
```

Vérifier l'échec initial (avant d'ajouter `pub mod git;`, ou compilation du test) :

```bash
cargo test -p wimux-server --lib git -- --test-threads=1 2>&1 | tail -20
```

- [ ] **Step 2: Vert git + non-régression**

```bash
cargo test -p wimux-server --lib git -- --test-threads=1 2>&1 | tail -20
cargo fmt
RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets 2>&1 | tail -20
```

PASS attendu : 5 tests `git` verts, clippy propre.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "$(printf 'feat(server): git_branch par lecture de .git/HEAD (W3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: Plomberie `SessionInfo` (`session.rs` + `daemon.rs` + test d'intégration)

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Modify: `crates/wimux-server/src/daemon.rs`
- Modify: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces:**
- Produces : `Session::active_pane_cwd(&self) -> Option<String>` — cwd du volet actif de la fenêtre active.
- Consumes : `Pane::cwd()` (Task 2), `crate::git::git_branch` (Task 3), `SessionInfo.cwd/branch` (Task 1).

- [ ] **Step 1: `Session::active_pane_cwd()`**

Dans `crates/wimux-server/src/session.rs`, `impl Session`, juste après la méthode privée `active_pane()` (~ligne 133), ajouter :

```rust
    /// cwd du volet actif de la fenêtre active (source du cwd de session, W3).
    pub fn active_pane_cwd(&self) -> Option<String> {
        self.active_pane().and_then(|p| p.cwd())
    }
```

- [ ] **Step 2: `daemon.rs::list` renseigne `cwd` / `branch`**

Dans `crates/wimux-server/src/daemon.rs`, méthode `list()`, remplacer le stub `cwd: None, branch: None,` (posé en Task 1) par le calcul réel. La closure `.map(|s| { ... })` construit `SessionInfo` ; juste avant le littéral `SessionInfo { ... }`, ajouter le calcul, puis renseigner les deux champs :

```rust
                let cwd = s.active_pane_cwd();
                let branch = cwd
                    .as_deref()
                    .and_then(|c| crate::git::git_branch(std::path::Path::new(c)));
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
                    group: s.group(),
                    cwd,
                    branch,
                }
```

(Le reste de la closure — `name`, `activity`, `bell` — est inchangé.)

- [ ] **Step 3: Test d'intégration `cwd_et_branche_via_osc7` (déterministe, SANS le hook de prompt)**

> Rationale : Task 5 (injection) n'est pas encore câblée ici, donc le shell PowerShell des sessions **n'émet pas** de prompt OSC 7 concurrent. On injecte une séquence OSC 7 **FIXE** dans le flux de sortie du volet via `[Console]::Out.Write(...)`, en pointant sur un dépôt git temporaire de branche connue. Le `Set-Location` rend le test robuste même une fois Task 5 en place (le prompt émettrait alors le MÊME cwd → convergence). On sonde `list-sessions` jusqu'à voir `cwd` + `branch` attendus.

Ajouter à la fin de `crates/wimux-server/tests/gui_mode.rs` :

```rust
// --- W3 : cwd (OSC 7) + branche git dans SessionInfo ----------------------

/// Crée un dépôt git temporaire sur une branche nommée. `None` si git absent.
fn init_temp_git_repo_on_branch(label: &str, branch: &str) -> Option<std::path::PathBuf> {
    let git_ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("wimux-w3-repo-{}-{label}", std::process::id()));
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
            "-c", "user.email=t@t", "-c", "user.name=t", "commit", "--allow-empty", "-m", "init",
        ]),
        "commit initial a échoué"
    );
    assert!(git(&["checkout", "-b", branch]), "checkout -b a échoué");
    Some(dir)
}

#[test]
fn cwd_et_branche_via_osc7() {
    let Some(repo) = init_temp_git_repo_on_branch("cb", "w3-branch") else {
        eprintln!("git absent : test cwd_et_branche_via_osc7 ignoré");
        return;
    };
    let pipe = format!(r"\\.\pipe\wimux-test-{}-w3cwd", std::process::id());
    start_daemon(&pipe); // shell par défaut = powershell.exe

    create_detached(&pipe, "W3");

    // Chemin natif (pour comparaison) et URL (slashes avant, hôte vide).
    let repo_native = repo.to_string_lossy().into_owned(); // C:\...\repo
    let repo_url = repo_native.replace('\\', "/"); // C:/...:/repo

    // Une seule ligne PowerShell : se placer dans le dépôt PUIS émettre un OSC 7
    // FIXE (indépendant du hook de prompt) pointant sur ce dépôt.
    let cmd = format!(
        "Set-Location -LiteralPath '{repo_native}'; [Console]::Out.Write(\"$([char]27)]7;file:///{repo_url}$([char]7)\")\r"
    );
    send_keys(&pipe, "W3", cmd.as_bytes());

    let want_native = repo_native.clone();
    let ok = poll_list_until(&pipe, 25, |list| {
        list.iter().find(|s| s.name == "W3").is_some_and(|s| {
            s.cwd
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(&want_native))
                && s.branch.as_deref() == Some("w3-branch")
        })
    });
    assert!(
        ok,
        "W3 devrait exposer cwd={repo_native} et branch=w3-branch : {:?}",
        fetch_list(&pipe)
    );

    let c = Arc::new(connect_retry(&pipe));
    handshake(&c);
    {
        let mut w: &PipeConn = &c;
        let _ = send(&mut w, &ClientMessage::Kill { name: "W3".into() });
    }
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&repo);
}
```

- [ ] **Step 4: Vert intégration + workspace**

```bash
cargo test -p wimux-server --test gui_mode cwd_et_branche_via_osc7 -- --test-threads=1 --nocapture 2>&1 | tail -40
cargo fmt
RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets 2>&1 | tail -20
cargo test --workspace -- --test-threads=1 2>&1 | tail -30
```

PASS attendu : `cwd_et_branche_via_osc7` vert (ou ignoré proprement si git absent), et l'ensemble du workspace vert. `cargo fmt` a pu retoucher `gui_mode.rs` — c'est légitime ici (Task 4 le modifie), **ne pas** `git checkout`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "$(printf 'feat(server): SessionInfo.cwd/branch renseignes dans list (W3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: Injection OSC 7 au spawn PowerShell

**Files:**
- Modify: `crates/wimux-server/src/session.rs`

**Interfaces:**
- Produces :
  - `pub fn osc7_prompt_injection(shell: &str) -> Option<Vec<String>>` (pure) — pour un shell dont le basename (insensible à la casse, sans `.exe`) vaut `powershell` ou `pwsh`, renvoie `Some(vec!["-NoExit", "-Command", OSC7_PS_SNIPPET])` ; `None` sinon.
  - `const OSC7_PS_SNIPPET: &str` — hook de prompt PowerShell (une ligne, un seul argument `-Command`).
- Consumes : `Pane::spawn` / `Pane::spawn_command` (existants), `Session.shell`, `Session.notifier`.

- [ ] **Step 1: Tests de la fonction de décision (échouent : fn absente)**

Ajouter dans `#[cfg(test)] mod tests` de `crates/wimux-server/src/session.rs`, avant la `}` finale du module :

```rust
    #[test]
    fn injection_powershell_renvoie_les_args() {
        let args = osc7_prompt_injection("powershell.exe").expect("powershell -> Some");
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "-NoExit");
        assert_eq!(args[1], "-Command");
        assert_eq!(args[2], OSC7_PS_SNIPPET);
    }

    #[test]
    fn injection_pwsh_renvoie_les_args() {
        assert!(osc7_prompt_injection("pwsh").is_some());
        assert!(osc7_prompt_injection("pwsh.exe").is_some());
    }

    #[test]
    fn injection_insensible_casse_et_chemin() {
        assert!(osc7_prompt_injection("PowerShell.EXE").is_some());
        assert!(
            osc7_prompt_injection(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
                .is_some()
        );
    }

    #[test]
    fn injection_autre_shell_renvoie_none() {
        assert!(osc7_prompt_injection("cmd.exe").is_none());
        assert!(osc7_prompt_injection("bash").is_none());
        assert!(osc7_prompt_injection("cmd").is_none());
    }
```

Vérifier l'échec :

```bash
cargo test -p wimux-server --lib session::tests::injection 2>&1 | tail -20
```

FAIL attendu : `cannot find function osc7_prompt_injection` / `cannot find value OSC7_PS_SNIPPET`.

- [ ] **Step 2: Implémenter la fonction pure + le snippet + les helpers de spawn**

Dans `crates/wimux-server/src/session.rs`, ajouter en fin de fichier (après la fonction libre `grid_cells`/`draw_status_bar`, avant `#[cfg(test)]`) :

```rust
/// Hook de prompt PowerShell (une seule ligne = un seul argument `-Command`).
/// Capture le prompt courant (`$function:prompt`, déjà défini par le profil s'il
/// existe, car `-Command` s'exécute APRÈS le profil), puis le remplace par une
/// version qui, à chaque invite, émet `ESC ]7;file:///<cwd-url-encodé> BEL` sur
/// la console AVANT de rappeler l'ancien prompt (préservation). L'hôte est laissé
/// vide (`file:///…`, accepté par le renifleur). Pas de guillemets doubles :
/// évite tout double-échappement à la frontière du spawn (portable-pty).
const OSC7_PS_SNIPPET: &str = "$__wimux_op=$function:prompt;function global:prompt{$__wimux_u=[uri]::EscapeUriString(((Get-Location).ProviderPath -replace '\\\\','/'));[Console]::Write(([string][char]27)+']7;file:///'+$__wimux_u+([string][char]7));& $__wimux_op}";

/// Détermine les arguments de spawn injectant l'émission OSC 7 pour un shell
/// PowerShell/pwsh. `None` pour tout autre shell (cmd.exe, bash…) → pas
/// d'injection (repli sans régression : le cwd restera `None`).
pub fn osc7_prompt_injection(shell: &str) -> Option<Vec<String>> {
    let base = shell.rsplit(['\\', '/']).next().unwrap_or(shell);
    let base = base.to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    if base == "powershell" || base == "pwsh" {
        Some(vec![
            "-NoExit".to_string(),
            "-Command".to_string(),
            OSC7_PS_SNIPPET.to_string(),
        ])
    } else {
        None
    }
}

/// Spawn d'un volet shell avec injection OSC 7 conditionnelle (PowerShell/pwsh).
fn spawn_shell_pane_with(
    cols: u16,
    rows: u16,
    shell: &str,
    notifier: &Arc<Notifier>,
) -> Result<Arc<Pane>> {
    match osc7_prompt_injection(shell) {
        Some(args) => Pane::spawn_command(cols, rows, shell, &args, None, Arc::clone(notifier)),
        None => Pane::spawn(cols, rows, shell, Arc::clone(notifier)),
    }
}
```

Ajouter la méthode d'instance correspondante dans `impl Session` (près de `reflow`) :

```rust
    /// Spawn d'un volet du shell de la session, avec injection OSC 7 (W3).
    fn spawn_shell_pane(&self, cols: u16, rows: u16) -> Result<Arc<Pane>> {
        spawn_shell_pane_with(cols, rows, &self.shell, &self.notifier)
    }
```

- [ ] **Step 3: Câbler l'injection aux 3 sites de spawn du shell**

Dans `Session::new` (~ligne 49), remplacer :

```rust
        let pane = Pane::spawn(cols, content_rows(rows), shell, Arc::clone(&notifier))?;
```

par :

```rust
        let pane = spawn_shell_pane_with(cols, content_rows(rows), shell, &notifier)?;
```

Dans `gui_split` (~ligne 172), remplacer :

```rust
        let new_pane = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier)).ok()?;
```

par :

```rust
        let new_pane = self.spawn_shell_pane(1, 1).ok()?;
```

Dans `gui_new_window` (~ligne 278), remplacer :

```rust
        let new_pane = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier));
```

par :

```rust
        let new_pane = self.spawn_shell_pane(1, 1);
```

> Hors périmètre W3 : les chemins **TUI** `Session::split` (~679) et `Session::new_window` (~731) restent en `Pane::spawn` (le rail GUI n'en dépend pas) ; `new_agent` ne doit PAS injecter (il lance un programme, pas un shell). Ne pas les modifier.

- [ ] **Step 4: Vert décision + non-régression complète**

```bash
cargo test -p wimux-server --lib session -- --test-threads=1 2>&1 | tail -30
cargo fmt
RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets 2>&1 | tail -20
cargo test --workspace -- --test-threads=1 2>&1 | tail -30
```

PASS attendu : les 4 tests `injection_*` verts ; les suites `session` (dont `agent_*`, `gui_*`, `new_agent_*`) et l'intégration (dont `cwd_et_branche_via_osc7`) toujours vertes — le spawn PowerShell injecté doit démarrer normalement (aucune régression sur cmd.exe / agents).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "$(printf 'feat(server): injection OSC 7 au spawn PowerShell (W3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 6: Frontend — 2e ligne de rail (cwd + branche)

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`

**Interfaces:**
- Consumes : `SessionInfo.cwd/branch` (Task 1).
- Produces : `SessionDto` (Rust + TS) += `cwd`/`branch` ; rendu 2 lignes du rail.

- [ ] **Step 1: Passer `cwd`/`branch` dans le pont Tauri**

Dans `wimux-gui/src-tauri/src/lib.rs`, `struct SessionDto` (~ligne 89), après `group: Option<String>,` ajouter :

```rust
    cwd: Option<String>,
    branch: Option<String>,
```

Dans `list_sessions` (~ligne 121), au littéral `SessionDto { ... }`, après `group: s.group,` ajouter :

```rust
                    cwd: s.cwd,
                    branch: s.branch,
```

- [ ] **Step 2: Type TS + rendu 2 lignes**

Dans `wimux-gui/src/main.ts`, étendre le type `SessionDto` (~ligne 131) :

```ts
type SessionDto = {
  name: string;
  attached: boolean;
  activity: boolean;
  bell: boolean;
  agent: boolean;
  agent_status: string | null;
  group: string | null;
  cwd: string | null;
  branch: string | null;
};
```

Ajouter, juste avant `function renderSession(...)`, un abréviateur de cwd (sans dépendance, sans connaître le home réel — heuristique `~` sur `C:\Users\<user>`) :

```ts
function abbreviateCwd(cwd: string): string {
  // Remplace un préfixe de profil utilisateur par `~` (heuristique).
  let p = cwd.replace(/^[A-Za-z]:\\Users\\[^\\]+/i, "~");
  const MAX = 30;
  if (p.length > MAX) p = "…" + p.slice(p.length - (MAX - 1));
  return p;
}
```

Remplacer, dans `renderSession`, la création du `name` et les trois `el.append(...)` pour introduire une colonne `.session-main` (nom + méta) à la place du `name` seul. Le corps de `renderSession` devient :

```ts
function renderSession(s: SessionDto): HTMLElement {
  const el = document.createElement("div");
  el.className = "session" + (s.name === activeSession ? " active" : "");
  const main = document.createElement("div");
  main.className = "session-main";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = s.name;
  main.appendChild(name);
  // 2e ligne : cwd abrégé + branche. Masquée si cwd inconnu (cmd.exe, agent…).
  if (s.cwd) {
    const meta = document.createElement("div");
    meta.className = "session-meta";
    const cwd = document.createElement("span");
    cwd.className = "meta-cwd";
    cwd.textContent = abbreviateCwd(s.cwd);
    cwd.title = s.cwd;
    meta.appendChild(cwd);
    if (s.branch) {
      const br = document.createElement("span");
      br.className = "meta-branch";
      br.textContent = "⎇ " + s.branch;
      br.title = s.branch;
      meta.appendChild(br);
    }
    main.appendChild(meta);
  }
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
    const glyph = document.createElement("span");
    glyph.className = "agent-glyph " + agentStatusClass(s.agent_status);
    glyph.textContent = agentStatusGlyph(s.agent_status);
    glyph.title = s.agent_status ?? "agent";
    el.append(main, glyph, close);
  } else if (!isActive && (s.bell || s.activity)) {
    const dot = document.createElement("span");
    dot.className = "dot " + (s.bell ? "bell" : "activity");
    dot.textContent = s.bell ? "🔔" : "";
    el.append(main, dot, close);
  } else {
    el.append(main, close);
  }
  return el;
}
```

(Les fonctions `agentStatusGlyph` / `agentStatusClass` / `startRename` / `switchTo` sont inchangées. `startRename` remplace toujours les enfants de `el` par le champ de saisie : la méta disparaît le temps de l'édition, ce qui est acceptable.)

- [ ] **Step 3: Styles de la 2e ligne**

Dans `wimux-gui/src/styles.css`, après la règle `.session .name-edit { ... }` (~ligne 15), ajouter :

```css
.session .session-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; overflow: hidden; }
.session .session-main .name { flex: 0 0 auto; }
.session .session-meta { display: flex; gap: 6px; align-items: center; font-size: 11px; color: #888; overflow: hidden; white-space: nowrap; }
.session .session-meta .meta-cwd { overflow: hidden; text-overflow: ellipsis; }
.session .session-meta .meta-branch { flex: 0 0 auto; color: #6aa06a; }
```

- [ ] **Step 4: Build frontend (OK)**

```bash
cd wimux-gui && npm run build 2>&1 | tail -20 ; cd ..
```

PASS attendu : build TypeScript/Vite OK (aucune erreur de type).

- [ ] **Step 5: README — section « Vérification manuelle W3 »**

Ajouter à la fin de `wimux-gui/README.md` :

```markdown
## Vérification manuelle W3 (rail enrichi : cwd + branche git)

Prérequis : rebuild + **redémarrage du daemon détaché** (changement de protocole
`SessionInfo`), shell par défaut PowerShell/pwsh, puis `cd wimux-gui && npm run tauri dev`.

1. **Session PowerShell** : créer/attacher une session (shell par défaut). Sous
   son nom dans le rail apparaît une **2e ligne** : le cwd abrégé (`~` pour le
   profil utilisateur) et, si le dossier est un dépôt git, la branche (`⎇ <nom>`).
2. **Suivi des `cd`** : dans le terminal, `cd` vers un **dépôt git** (ex. le repo
   `wimux`). En ~1 s (sondage `list-sessions`), la 2e ligne se met à jour : le
   cwd suit, et la branche affiche la branche courante (ex. `⎇ main`).
   Changer de branche (`git switch -c essai`) → la ligne reflète `⎇ essai`.
3. **Hors dépôt** : `cd C:\Windows` → le cwd s'affiche, **sans** branche.
4. **Repli cmd.exe** : lancer une session avec un shell non-PowerShell
   (`set default-shell cmd.exe` dans `%USERPROFILE%\.wimux.conf`, ou
   `WIMUX_SHELL=cmd.exe`). Cette session n'émet pas d'OSC 7 → le rail affiche le
   **nom seul** (pas de 2e ligne, aucune erreur). Aucune régression.
5. **Non-régression indicateurs** : les pastilles d'activité/cloche (G4) et les
   glyphes d'agent (M-series) restent affichés à droite du nom, sur la 1re ligne.
```

- [ ] **Step 6: Vérification finale + commit**

```bash
cargo test --workspace -- --test-threads=1 2>&1 | tail -20
cd wimux-gui && npm run build 2>&1 | tail -5 ; cd ..
git add -A && git commit -m "$(printf 'feat(gui): rail 2e ligne cwd + branche git (W3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

Puis vérifier manuellement selon la nouvelle section README (rebuild + redémarrage du daemon avant `npm run tauri dev`).

---

## Récapitulatif de cohérence des signatures (inter-tâches)

- `SessionInfo.cwd: Option<String>` / `SessionInfo.branch: Option<String>` — Task 1 ; consommés Task 4 (serveur) et Task 6 (`SessionDto` Rust + TS).
- `Pane::cwd(&self) -> Option<String>` — Task 2 ; consommé par `Session::active_pane_cwd` (Task 4).
- `Osc7Sniffer::feed(&mut self, &[u8]) -> Option<String>` + `decode_osc7_uri(&[u8]) -> Option<String>` — Task 2 (renifleur/décodeur).
- `git_branch(&Path) -> Option<String>` — Task 3 ; consommé par `daemon.rs::list` (Task 4).
- `Session::active_pane_cwd(&self) -> Option<String>` — Task 4.
- `osc7_prompt_injection(&str) -> Option<Vec<String>>` + `OSC7_PS_SNIPPET` — Task 5 ; câblés à `Session::new` / `gui_split` / `gui_new_window` via `spawn_shell_pane[_with]`.
```
