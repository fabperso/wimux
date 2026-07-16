# wimux GUI W2 — Onglets terminaux par workspace : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Exposer les fenêtres (« windows » tmux) d'une session GUI-attachée comme une barre d'onglets permettant de créer / basculer / fermer / renommer une fenêtre, chaque onglet réutilisant l'arbre de volets G3.

**Architecture:** Un onglet = une `Window` de la session courante (`Vec<Window>` + `active_window`). Quatre nouveaux `ClientMessage` (`NewWindow`, `SelectWindow`, `CloseWindow`, `RenameWindow`) mutent la session de l'attache GUI, sans paramètre `session` (le serveur connaît la session via `GuiAttachment`). À chaque bascule de fenêtre, le serveur rejoue le cycle d'attache GUI (coupe le flux fusionné précédent, réabonne les volets de la nouvelle fenêtre, réémet `WindowLayout` + `PaneSnapshot`) via un helper mutualisé `reattach_active_window`. Un nouveau `ServerMessage::WindowList` décrit les onglets ; la GUI le rend en barre au-dessus de `#terminal`.

**Tech Stack:** Rust edition 2024 (workspace `wimux-protocol` / `wimux-server`), postcard (sérialisation par index de position), Named Pipe Windows, Tauri 2 + TypeScript/Vite + xterm.js (`wimux-gui`).

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : suites TUI + G1→G4 + M1→M3 vertes (`cargo test --workspace -- --test-threads=1`) ; `npm run build` OK.
- **Postcard : nouveaux variants EN FIN des enums** (compat fil de fer par index de position).
- Toutes les écritures GUI passent par le verrou `gui_write` (G3).
- Ordre d'envoi : `WindowLayout` AVANT `PaneSnapshot` (le frontend crée le xterm à la réception du layout).
- `cargo fmt` peut reformater `crates/wimux-server/tests/gui_mode.rs` hors périmètre — le rétablir (`git checkout -- crates/wimux-server/tests/gui_mode.rs`) avant commit si la tâche ne le modifie pas. **Task 4 le modifie légitimement : ne pas le rétablir.**
- Outil shell : **Bash tool** (git bash). Tests lents (ConPTY) : `--test-threads=1`, patience.
- Piège daemon détaché : rebuild + redémarrage du serveur après tout changement de protocole (manuel seulement ; sans objet pour les tests).
- Chaque commit finit par `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, via `git commit -m "$(printf '...')"`.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — ajoute `struct WindowInfo`, `ServerMessage::WindowList`, et 4 `ClientMessage` (`NewWindow`, `SelectWindow`, `CloseWindow`, `RenameWindow`) en fin d'enum + tests roundtrip. (Task 1)
- `crates/wimux-server/src/window.rs` — `Window.name: Option<String>` + `name()`/`set_name()` ; `Window::new` ne prend plus de nom (init `None`). (Task 2)
- `crates/wimux-server/src/session.rs` — mise à jour des appels `Window::new` ; méthodes `gui_new_window`, `gui_select_window`, `gui_close_window`, `gui_rename_window`, `window_list`. (Task 2 pour les appels ; Task 3 pour les méthodes)
- `crates/wimux-server/src/daemon.rs` — helper `reattach_active_window` (refactor du corps d'`AttachGui`) + câblage des 4 messages + `WindowList` à l'attache. (Task 4)
- `crates/wimux-server/tests/gui_mode.rs` — test d'intégration du cycle de vie des onglets. (Task 4)
- `wimux-gui/src-tauri/src/lib.rs` — émission de l'événement `window-list` + commandes `new_window`/`select_window`/`close_window`/`rename_window`. (Task 5)
- `wimux-gui/index.html` — conteneur `#workspace` (colonne) enveloppant `#tabs` + `#terminal`. (Task 6)
- `wimux-gui/src/main.ts` — écoute `window-list`, rendu de la barre d'onglets, câblage des commandes, `reset` à la bascule, édition inline du renommage. (Task 6)
- `wimux-gui/src/styles.css` — styles de `#workspace` / `#tabs` / `.tab`. (Task 6)
- `wimux-gui/README.md` — section « Vérification manuelle W2 ». (Task 6)

---

## Task 1: Protocole — `WindowInfo`, `WindowList`, 4 `ClientMessage`

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify (stubs seulement) : `crates/wimux-server/src/daemon.rs`

**Interfaces:**
- Produces:
  - `pub struct WindowInfo { pub name: Option<String> }` (derive `Debug, Clone, PartialEq, Serialize, Deserialize`).
  - `ClientMessage::NewWindow` (variant unité).
  - `ClientMessage::SelectWindow { index: u32 }`.
  - `ClientMessage::CloseWindow { index: u32 }`.
  - `ClientMessage::RenameWindow { index: u32, name: String }`.
  - `ServerMessage::WindowList { windows: Vec<WindowInfo>, active: u32 }`.
  - Ces 5 variants sont ajoutés **en dernier** dans leur enum respectif (après `CreateAgentBatch` pour `ClientMessage`, après `BatchCreated` pour `ServerMessage`).

- [ ] **Step 1: Écrire les tests roundtrip (échouent : types absents)**

Ajouter dans le module `#[cfg(test)] mod tests` de `crates/wimux-protocol/src/lib.rs`, avant la `}` finale du module :

```rust
    #[test]
    fn aller_retour_window_info_et_liste() {
        let msg = ServerMessage::WindowList {
            windows: vec![
                WindowInfo { name: Some("build".into()) },
                WindowInfo { name: None },
            ],
            active: 1,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::WindowList { windows, active } => {
                assert_eq!(active, 1);
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].name.as_deref(), Some("build"));
                assert_eq!(windows[1].name, None);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_new_window() {
        let mut buf = Vec::new();
        send(&mut buf, &ClientMessage::NewWindow).unwrap();
        let mut cur = io::Cursor::new(buf);
        assert!(matches!(
            recv::<_, ClientMessage>(&mut cur).unwrap(),
            ClientMessage::NewWindow
        ));
    }

    #[test]
    fn aller_retour_select_et_close_window() {
        let mut buf = Vec::new();
        send(&mut buf, &ClientMessage::SelectWindow { index: 2 }).unwrap();
        send(&mut buf, &ClientMessage::CloseWindow { index: 3 }).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::SelectWindow { index } => assert_eq!(index, 2),
            _ => panic!("mauvais variant"),
        }
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CloseWindow { index } => assert_eq!(index, 3),
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_rename_window() {
        let msg = ClientMessage::RenameWindow {
            index: 0,
            name: "build".into(),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::RenameWindow { index, name } => {
                assert_eq!(index, 0);
                assert_eq!(name, "build");
            }
            _ => panic!("mauvais variant"),
        }
    }
```

- [ ] **Step 2: Lancer les tests pour vérifier l'échec**

Run: `cargo test -p wimux-protocol`
Expected: FAIL à la compilation — `cannot find type WindowInfo`, `no variant NewWindow`, etc.

- [ ] **Step 3: Ajouter `WindowInfo`**

Dans `crates/wimux-protocol/src/lib.rs`, juste après la définition de `struct Frame` (avant `/// Messages client -> serveur.`), insérer :

```rust
/// Résumé d'une fenêtre (onglet) d'une session GUI-attachée (W2). La GUI affiche
/// `name` s'il est présent, sinon la position (1-based).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Nom explicite de la fenêtre, ou `None` (la GUI affiche alors la position).
    pub name: Option<String>,
}
```

- [ ] **Step 4: Ajouter les 4 `ClientMessage` en fin d'enum**

Dans `enum ClientMessage`, après le variant `CreateAgentBatch { ... },` et **avant** la `}` de fermeture de l'enum, ajouter :

```rust
    /// Crée une fenêtre (onglet) dans la session GUI-attachée et la rend active (W2).
    NewWindow,
    /// Rend active la fenêtre `index` de la session GUI-attachée (W2).
    SelectWindow {
        index: u32,
    },
    /// Ferme la fenêtre `index` (tue ses volets) ; no-op s'il ne reste qu'une
    /// fenêtre (W2).
    CloseWindow {
        index: u32,
    },
    /// Nomme la fenêtre `index` ; un nom vide efface le nom (W2).
    RenameWindow {
        index: u32,
        name: String,
    },
```

- [ ] **Step 5: Ajouter `WindowList` en fin de `ServerMessage`**

Dans `enum ServerMessage`, après le variant `BatchCreated { ... },` et **avant** la `}` de fermeture de l'enum, ajouter :

```rust
    /// Liste des fenêtres (onglets) de la session GUI-attachée (W2). Émis à
    /// l'attache (état initial) et après chaque opération de fenêtre.
    WindowList {
        windows: Vec<WindowInfo>,
        active: u32,
    },
```

- [ ] **Step 6: Lancer les tests pour vérifier le succès**

Run: `cargo test -p wimux-protocol`
Expected: PASS (tous les tests roundtrip, dont les 4 nouveaux).

- [ ] **Step 7: Ajouter les stubs d'exhaustivité dans `handle_client`**

`cargo build -p wimux-server` échoue désormais (`match msg` non exhaustif). Dans `crates/wimux-server/src/daemon.rs`, dans le `match msg { ... }` de `handle_client`, juste après le bras `ClientMessage::Hello(_) => {}` (dernier bras avant la `}` du match), ajouter les 4 stubs temporaires (remplis en Task 4) :

```rust
            ClientMessage::NewWindow => {}
            ClientMessage::SelectWindow { .. } => {}
            ClientMessage::CloseWindow { .. } => {}
            ClientMessage::RenameWindow { .. } => {}
```

- [ ] **Step 8: Vérifier la compilation du serveur**

Run: `cargo build -p wimux-server`
Expected: succès (match exhaustif).

- [ ] **Step 9: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets`
Expected: aucun warning.

- [ ] **Step 10: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(protocol): WindowInfo + WindowList + 4 ClientMessage onglets (W2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: `window.rs` — `Window.name: Option<String>` + accesseurs

**Files:**
- Modify: `crates/wimux-server/src/window.rs`
- Modify: `crates/wimux-server/src/session.rs` (appels `Window::new` mis à jour pour compiler)

**Interfaces:**
- Produces:
  - `Window::new(pane: Arc<Pane>) -> Window` (signature changée : **plus de paramètre `name`** ; `name` initialisé à `None`).
  - `Window::name(&self) -> Option<String>`.
  - `Window::set_name(&mut self, name: Option<String>)`.
- Consumes: rien de nouveau.

- [ ] **Step 1: Écrire le test unitaire (échoue : accesseurs absents / signature)**

Dans `crates/wimux-server/src/window.rs`, module `#[cfg(test)] mod tests`, ajouter avant la `}` finale du module :

```rust
    #[test]
    fn window_name_defaut_none_et_set() {
        let p = dummy_pane();
        let mut win = Window::new(p);
        assert_eq!(win.name(), None);
        win.set_name(Some("build".into()));
        assert_eq!(win.name().as_deref(), Some("build"));
        win.set_name(None);
        assert_eq!(win.name(), None);
        win.kill_all();
    }
```

- [ ] **Step 2: Lancer le test pour vérifier l'échec**

Run: `cargo test -p wimux-server --lib window`
Expected: FAIL à la compilation — `Window::new` attend 2 arguments / `no method named name`.

- [ ] **Step 3: Changer le champ `name` en `Option<String>`**

Dans `crates/wimux-server/src/window.rs`, dans `pub struct Window`, remplacer :

```rust
    pub name: String,
```

par :

```rust
    name: Option<String>,
```

- [ ] **Step 4: Adapter `Window::new` (plus de paramètre `name`) + ajouter les accesseurs**

Remplacer le constructeur existant :

```rust
    pub fn new(name: String, pane: Arc<Pane>) -> Window {
        let id = pane.id;
        let mut panes = HashMap::new();
        panes.insert(id, pane);
        Window {
            name,
            root: Node::Leaf(id),
            panes,
            active: id,
            rects: HashMap::new(),
            borders: Vec::new(),
            zoomed: false,
        }
    }
```

par :

```rust
    pub fn new(pane: Arc<Pane>) -> Window {
        let id = pane.id;
        let mut panes = HashMap::new();
        panes.insert(id, pane);
        Window {
            name: None,
            root: Node::Leaf(id),
            panes,
            active: id,
            rects: HashMap::new(),
            borders: Vec::new(),
            zoomed: false,
        }
    }

    /// Nom explicite de la fenêtre, ou `None` (W2).
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// Fixe (ou efface avec `None`) le nom de la fenêtre (W2).
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
```

- [ ] **Step 5: Mettre à jour les appels `Window::new` des tests de `window.rs`**

Toujours dans `window.rs`, remplacer les deux occurrences `Window::new("w".into(), p1)` par `Window::new(p1)` (dans `split_et_close_par_id` et `set_ratio_borne`).

- [ ] **Step 6: Mettre à jour les appels `Window::new` de `session.rs`**

Dans `crates/wimux-server/src/session.rs` :
- Dans `Session::new` : remplacer `let window = Window::new("win".to_string(), pane);` par `let window = Window::new(pane);`.
- Dans `Session::new_agent` : remplacer `let window = Window::new("win".to_string(), pane);` par `let window = Window::new(pane);`.
- Dans `Session::new_window` (TUI), remplacer le bloc :

```rust
    pub fn new_window(&self) {
        if let Ok(pane) = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier)) {
            let mut inner = self.inner.lock().unwrap();
            let n = inner.windows.len();
            inner.windows.push(Window::new(format!("win{n}"), pane));
            inner.active_window = inner.windows.len() - 1;
            let area = content_area(inner.cols, inner.rows);
            let aw = inner.active_window;
            inner.windows[aw].reflow(area);
            drop(inner);
            self.notifier.bump();
        }
    }
```

par (suppression du `n` désormais inutile, sinon warning `unused variable` sous `-D warnings`) :

```rust
    pub fn new_window(&self) {
        if let Ok(pane) = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier)) {
            let mut inner = self.inner.lock().unwrap();
            inner.windows.push(Window::new(pane));
            inner.active_window = inner.windows.len() - 1;
            let area = content_area(inner.cols, inner.rows);
            let aw = inner.active_window;
            inner.windows[aw].reflow(area);
            drop(inner);
            self.notifier.bump();
        }
    }
```

- [ ] **Step 7: Lancer le test unitaire pour vérifier le succès**

Run: `cargo test -p wimux-server --lib window`
Expected: PASS (dont `window_name_defaut_none_et_set`).

- [ ] **Step 8: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: aucun warning (pas de `pub name` mort, pas de `n` inutilisé).

- [ ] **Step 9: Commit**

```bash
git add crates/wimux-server/src/window.rs crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(window): name Option<String> + name()/set_name() (W2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: `session.rs` — méthodes GUI de fenêtre

**Files:**
- Modify: `crates/wimux-server/src/session.rs`

**Interfaces:**
- Consumes: `WindowInfo` (Task 1) ; `Window::new` / `Window::name` / `Window::set_name` (Task 2) ; helpers existants `content_area`, `Window::kill_all`, `Window::reflow`.
- Produces (méthodes de `impl Session`) :
  - `pub fn window_list(&self) -> (Vec<WindowInfo>, u32)`.
  - `pub fn gui_new_window(&self) -> (Vec<WindowInfo>, u32)`.
  - `pub fn gui_select_window(&self, index: u32) -> Option<(Vec<WindowInfo>, u32)>`.
  - `pub fn gui_close_window(&self, index: u32) -> Option<(Vec<WindowInfo>, u32)>`.
  - `pub fn gui_rename_window(&self, index: u32, name: String)`.

- [ ] **Step 1: Écrire les tests lib (échouent : méthodes absentes)**

Dans `crates/wimux-server/src/session.rs`, module `#[cfg(test)] mod tests`, ajouter avant la `}` finale du module :

```rust
    #[test]
    fn window_list_reflete_une_fenetre_neuve() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let (windows, active) = s.window_list();
        assert_eq!(windows.len(), 1);
        assert_eq!(active, 0);
        assert_eq!(windows[0].name, None);
        s.kill();
    }

    #[test]
    fn gui_new_window_ajoute_un_onglet_actif() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let (windows, active) = s.gui_new_window();
        assert_eq!(windows.len(), 2);
        assert_eq!(active, 1);
        s.kill();
    }

    #[test]
    fn gui_select_window_hors_borne_est_none() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        assert!(s.gui_select_window(5).is_none());
        // Index valide : active mis à jour.
        let _ = s.gui_new_window(); // 2 fenêtres, active = 1
        let (_, active) = s.gui_select_window(0).unwrap();
        assert_eq!(active, 0);
        s.kill();
    }

    #[test]
    fn gui_close_window_refuse_la_derniere() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        assert!(
            s.gui_close_window(0).is_none(),
            "fermer l'unique fenêtre doit être refusé"
        );
        s.kill();
    }

    #[test]
    fn gui_close_window_retire_et_reajuste() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let _ = s.gui_new_window(); // 2 fenêtres, active = 1
        let (windows, active) = s.gui_close_window(1).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(active, 0);
        s.kill();
    }

    #[test]
    fn gui_rename_window_reflete_le_nom() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.gui_rename_window(0, "build".into());
        let (windows, _) = s.window_list();
        assert_eq!(windows[0].name.as_deref(), Some("build"));
        // Nom vide -> None.
        s.gui_rename_window(0, String::new());
        let (windows, _) = s.window_list();
        assert_eq!(windows[0].name, None);
        s.kill();
    }
```

- [ ] **Step 2: Lancer les tests pour vérifier l'échec**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: FAIL à la compilation — `no method named window_list` / `gui_new_window`, etc.

- [ ] **Step 3: Importer `WindowInfo`**

Dans `crates/wimux-server/src/session.rs`, remplacer la ligne d'import :

```rust
use wimux_protocol::{AgentStatus, Frame, LayoutNode};
```

par :

```rust
use wimux_protocol::{AgentStatus, Frame, LayoutNode, WindowInfo};
```

- [ ] **Step 4: Implémenter les 5 méthodes**

Dans `impl Session`, insérer ce bloc juste après la méthode `window_layout` (celle qui se termine par `Some((win.layout_tree(), win.active_pane_id()))` puis `}`) :

```rust
    /// Projette les fenêtres en `WindowInfo` (nom de chaque fenêtre) + l'index de
    /// la fenêtre active (W2).
    pub fn window_list(&self) -> (Vec<WindowInfo>, u32) {
        let inner = self.inner.lock().unwrap();
        let windows = inner
            .windows
            .iter()
            .map(|w| WindowInfo { name: w.name() })
            .collect();
        (windows, inner.active_window as u32)
    }

    /// Crée une fenêtre (onglet) : spawn un volet shell HORS verrou (comme
    /// `gui_split`), pousse une `Window` neuve (comme `Session::new`) et la rend
    /// active. Renvoie la nouvelle `window_list()` (W2).
    pub fn gui_new_window(&self) -> (Vec<WindowInfo>, u32) {
        // Ne pas tenir le verrou pendant le spawn (qui lance un processus).
        let new_pane = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier));
        let result = {
            let mut inner = self.inner.lock().unwrap();
            if let Ok(pane) = new_pane {
                inner.windows.push(Window::new(pane));
                inner.active_window = inner.windows.len() - 1;
                let area = content_area(inner.cols, inner.rows);
                let aw = inner.active_window;
                inner.windows[aw].reflow(area);
            }
            let windows = inner
                .windows
                .iter()
                .map(|w| WindowInfo { name: w.name() })
                .collect();
            (windows, inner.active_window as u32)
        };
        self.notifier.bump();
        result
    }

    /// Rend active la fenêtre `index` (bornée). `None` si l'index est hors borne (W2).
    pub fn gui_select_window(&self, index: u32) -> Option<(Vec<WindowInfo>, u32)> {
        let result = {
            let mut inner = self.inner.lock().unwrap();
            let idx = index as usize;
            if idx >= inner.windows.len() {
                return None;
            }
            inner.active_window = idx;
            let area = content_area(inner.cols, inner.rows);
            inner.windows[idx].reflow(area);
            let windows = inner
                .windows
                .iter()
                .map(|w| WindowInfo { name: w.name() })
                .collect();
            (windows, inner.active_window as u32)
        };
        self.notifier.bump();
        Some(result)
    }

    /// Ferme la fenêtre `index` (tue ses volets) et réajuste `active_window`.
    /// `None` (no-op) si l'index est hors borne OU s'il ne reste qu'une fenêtre (W2).
    pub fn gui_close_window(&self, index: u32) -> Option<(Vec<WindowInfo>, u32)> {
        let result = {
            let mut inner = self.inner.lock().unwrap();
            let idx = index as usize;
            if idx >= inner.windows.len() || inner.windows.len() == 1 {
                return None;
            }
            inner.windows[idx].kill_all();
            inner.windows.remove(idx);
            // Borne `active_window` comme `gui_close` (il reste ≥ 1 fenêtre ici).
            if inner.active_window >= inner.windows.len() {
                inner.active_window = inner.windows.len() - 1;
            }
            let area = content_area(inner.cols, inner.rows);
            let aw = inner.active_window;
            inner.windows[aw].reflow(area);
            let windows = inner
                .windows
                .iter()
                .map(|w| WindowInfo { name: w.name() })
                .collect();
            (windows, inner.active_window as u32)
        };
        self.notifier.bump();
        Some(result)
    }

    /// Nomme la fenêtre `index` ; un nom vide efface le nom (`None`) (W2).
    pub fn gui_rename_window(&self, index: u32, name: String) {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(win) = inner.windows.get_mut(index as usize) {
                let name = if name.is_empty() { None } else { Some(name) };
                win.set_name(name);
            }
        }
        self.notifier.bump();
    }
```

- [ ] **Step 5: Lancer les tests pour vérifier le succès**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (dont les 6 nouveaux tests). Note : ces tests spawnent de vrais volets ConPTY — comptez plusieurs secondes.

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: aucun warning.

- [ ] **Step 7: Commit**

```bash
git add crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(session): gui_new/select/close/rename_window + window_list (W2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: `daemon.rs` — helper `reattach_active_window` + câblage + test d'intégration

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Modify: `crates/wimux-server/tests/gui_mode.rs`

**Interfaces:**
- Consumes: `Session::{gui_new_window, gui_select_window, gui_close_window, gui_rename_window, window_list}` (Task 3) ; `Session::gui_attach_window` (existant) ; `ServerMessage::WindowList` (Task 1) ; `GuiAttachment` (existant).
- Produces (helper libre dans `daemon.rs`) :
  - `fn reattach_active_window(s: &Arc<Session>, conn: &Arc<PipeConn>, gui_write: &Arc<Mutex<()>>, gui_attach: &mut Option<GuiAttachment>) -> std::io::Result<()>` : (1) `*gui_attach = None` (le `Drop` coupe le flux précédent), (2) `s.gui_attach_window()`, (3) sous `gui_write` : `WindowLayout` puis les `PaneSnapshot`, (4) spawn le thread de pompe `rx.recv_timeout → PaneOutput`, (5) `*gui_attach = Some(GuiAttachment{..})`. Renvoie une `Error` GUI si `gui_attach_window()` est `None` (session dégénérée sans volet).

- [ ] **Step 1: Écrire le test d'intégration (échoue : câblage absent)**

Dans `crates/wimux-server/tests/gui_mode.rs` :

1. Ajouter `WindowInfo` à l'import du haut de fichier. Remplacer :

```rust
use wimux_protocol::{ClientMessage, LayoutNode, ServerMessage, SessionInfo, SplitDir, recv, send};
```

par :

```rust
use wimux_protocol::{
    ClientMessage, LayoutNode, ServerMessage, SessionInfo, SplitDir, WindowInfo, recv, send,
};
```

2. Ajouter le helper `wait_window_list` juste après la fonction `wait_layout` :

```rust
fn wait_window_list(rx: &Receiver<ServerMessage>, secs: u64) -> (Vec<WindowInfo>, u32) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowList { windows, active }) => return (windows, active),
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de WindowList"),
        }
    }
}
```

3. Ajouter le test de cycle de vie à la fin du fichier (après `create_agent_batch_base_non_git_renvoie_error`) :

```rust
#[test]
fn onglets_cycle_de_vie() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-w2tabs", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "W");

    // À l'attache : WindowList avec 1 fenêtre, active = 0.
    let (windows, active) = wait_window_list(&grx, 8);
    assert_eq!(windows.len(), 1);
    assert_eq!(active, 0);

    // Capturer le pane_id de la 1re fenêtre (via WindowLayout).
    let (tree0, _) = wait_layout(&grx, 8);
    let pane0 = match tree0 {
        LayoutNode::Leaf { pane_id } => pane_id,
        _ => panic!("session neuve : arbre attendu = feuille"),
    };

    // NewWindow -> 2 fenêtres, active = 1 ; WindowLayout d'une nouvelle feuille.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::NewWindow).unwrap();
    }
    let (windows, active) = wait_window_list(&grx, 8);
    assert_eq!(windows.len(), 2);
    assert_eq!(active, 1);
    let (tree1, _) = wait_layout(&grx, 8);
    let pane1 = match tree1 {
        LayoutNode::Leaf { pane_id } => pane_id,
        _ => panic!("nouvelle fenêtre : arbre attendu = feuille"),
    };
    assert_ne!(pane0, pane1, "les deux fenêtres partagent un pane_id");

    // SelectWindow { index: 0 } -> active = 0.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::SelectWindow { index: 0 }).unwrap();
    }
    let (_windows, active) = wait_window_list(&grx, 8);
    assert_eq!(active, 0);

    // RenameWindow { index: 0, name: "build" } -> WindowList reflète le nom.
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::RenameWindow {
                index: 0,
                name: "build".into(),
            },
        )
        .unwrap();
    }
    let named = {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowList { windows, .. }) => {
                    break windows.first().and_then(|w| w.name.clone());
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de WindowList après RenameWindow"),
            }
        }
    };
    assert_eq!(named.as_deref(), Some("build"));

    // CloseWindow { index: 1 } -> 1 fenêtre, active = 0.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::CloseWindow { index: 1 }).unwrap();
    }
    let (windows, active) = wait_window_list(&grx, 8);
    assert_eq!(windows.len(), 1);
    assert_eq!(active, 0);

    // 2e CloseWindow sur l'unique fenêtre : no-op (toujours 1 fenêtre).
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::CloseWindow { index: 0 }).unwrap();
    }
    let (windows, _) = wait_window_list(&grx, 8);
    assert_eq!(
        windows.len(),
        1,
        "fermer la dernière fenêtre doit être no-op"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "W".into() });
    std::thread::sleep(Duration::from_millis(200));
}
```

- [ ] **Step 2: Lancer le test pour vérifier l'échec**

Run: `cargo test -p wimux-server --test gui_mode onglets_cycle_de_vie -- --test-threads=1`
Expected: FAIL — les stubs `=> {}` ne renvoient aucun `WindowList` ; `wait_window_list` panique (« pas de WindowList »).

- [ ] **Step 3: Ajouter le helper `reattach_active_window`**

Dans `crates/wimux-server/src/daemon.rs`, insérer cette fonction libre juste après le bloc `impl Drop for GuiAttachment { ... }` (avant `struct PrefixState`) :

```rust
/// Rejoue le cycle d'attache GUI pour la fenêtre **active** de `s` : coupe
/// proprement la diffusion précédente (via le `Drop` de `GuiAttachment`, qui
/// arrête et joint le thread de pompe), réabonne les volets de la fenêtre active,
/// envoie `WindowLayout` PUIS un `PaneSnapshot` frais par volet (sous `gui_write`),
/// puis relance le thread de pompe `PaneOutput`. Mutualisé entre l'attache
/// initiale (`AttachGui`) et les bascules de fenêtre `NewWindow`/`SelectWindow`/
/// `CloseWindow` (W2).
fn reattach_active_window(
    s: &Arc<Session>,
    conn: &Arc<PipeConn>,
    gui_write: &Arc<Mutex<()>>,
    gui_attach: &mut Option<GuiAttachment>,
) -> std::io::Result<()> {
    // (1) Couper la diffusion précédente (Drop => join du thread de pompe).
    *gui_attach = None;

    // (2) Réabonner les volets de la fenêtre active.
    let Some((tree, active, snaps, rx, tx)) = s.gui_attach_window() else {
        let _g = gui_write.lock().unwrap();
        let mut wr: &PipeConn = conn;
        return send(
            &mut wr,
            &ServerMessage::Error("aucun volet dans la fenêtre active".into()),
        );
    };

    // (3) WindowLayout D'ABORD (le frontend crée le xterm à sa réception), puis
    //     les snapshots. Tout sous le verrou d'écriture GUI.
    {
        let _g = gui_write.lock().unwrap();
        let mut wr: &PipeConn = conn;
        send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
        for (pane_id, bytes) in snaps {
            let mut wr: &PipeConn = conn;
            send(&mut wr, &ServerMessage::PaneSnapshot { pane_id, bytes })?;
        }
    }

    // (4) Thread de pompe : rx.recv_timeout -> PaneOutput (sérialisé par gui_write).
    let keep_going = Arc::new(AtomicBool::new(true));
    let conn_out = Arc::clone(conn);
    let kg = Arc::clone(&keep_going);
    let gw = Arc::clone(gui_write);
    let handle = std::thread::spawn(move || {
        while kg.load(Ordering::Relaxed) {
            match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok((pane_id, bytes)) => {
                    let mut w: &PipeConn = &conn_out;
                    let sent = {
                        let _g = gw.lock().unwrap();
                        send(&mut w, &ServerMessage::PaneOutput { pane_id, bytes })
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // (5) Stocker la nouvelle attache.
    *gui_attach = Some(GuiAttachment {
        keep_going,
        handle: Some(handle),
        tx,
        session: Arc::clone(s),
    });
    Ok(())
}
```

- [ ] **Step 4: Refactorer le handler `AttachGui` pour utiliser le helper + envoyer `WindowList`**

Remplacer TOUT le bras `ClientMessage::AttachGui { session } => { ... }` existant (depuis `gui_attach = None;` jusqu'à sa `}` fermante) par :

```rust
            ClientMessage::AttachGui { session } => {
                // Arrêter proprement la diffusion précédente avant d'en démarrer une.
                gui_attach = None;
                gui_session = None;
                // La vue précédente se termine ; aucune session n'est regardée tant
                // que la nouvelle attache n'a pas réussi.
                server.set_gui_viewed(None);
                match server.get(&session) {
                    Some(s) => {
                        // WindowList (état initial des onglets) AVANT layout/snapshots.
                        {
                            let (windows, active) = s.window_list();
                            let _g = gui_write.lock().unwrap();
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                        }
                        reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                        // G4 : cette session devient « vue » ; on efface ses indicateurs.
                        server.set_gui_viewed(Some(session.clone()));
                        s.mark_seen();
                        viewed_set = true;
                        gui_session = Some(s);
                    }
                    None => {
                        let _g = gui_write.lock().unwrap();
                        let mut wr: &PipeConn = &conn;
                        send(
                            &mut wr,
                            &ServerMessage::Error(format!("session introuvable : {session}")),
                        )?;
                    }
                }
            }
```

- [ ] **Step 5: Remplacer les 4 stubs par les vrais handlers**

Remplacer les 4 bras stubs (`ClientMessage::NewWindow => {}` … `ClientMessage::RenameWindow { .. } => {}`) par :

```rust
            ClientMessage::NewWindow => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    let (windows, active) = s.gui_new_window();
                    {
                        let _g = gui_write.lock().unwrap();
                        let mut wr: &PipeConn = &conn;
                        send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                    }
                    reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                }
            }
            ClientMessage::SelectWindow { index } => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    match s.gui_select_window(index) {
                        Some((windows, active)) => {
                            {
                                let _g = gui_write.lock().unwrap();
                                let mut wr: &PipeConn = &conn;
                                send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                            }
                            reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                        }
                        None => {
                            // Hors borne : renvoyer la liste courante sans réattache.
                            let (windows, active) = s.window_list();
                            let _g = gui_write.lock().unwrap();
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                        }
                    }
                }
            }
            ClientMessage::CloseWindow { index } => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    match s.gui_close_window(index) {
                        Some((windows, active)) => {
                            {
                                let _g = gui_write.lock().unwrap();
                                let mut wr: &PipeConn = &conn;
                                send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                            }
                            reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                        }
                        None => {
                            // Refusé (une seule fenêtre) ou hors borne : liste courante.
                            let (windows, active) = s.window_list();
                            let _g = gui_write.lock().unwrap();
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                        }
                    }
                }
            }
            ClientMessage::RenameWindow { index, name } => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    s.gui_rename_window(index, name);
                    // Renommage : seule la WindowList change (contenu/fenêtre inchangés).
                    let (windows, active) = s.window_list();
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                }
            }
```

- [ ] **Step 6: Lancer le test d'intégration pour vérifier le succès**

Run: `cargo test -p wimux-server --test gui_mode onglets_cycle_de_vie -- --test-threads=1`
Expected: PASS. Test lent (ConPTY + plusieurs réattaches) — laissez-lui le temps.

- [ ] **Step 7: Non-régression de la suite serveur**

Run: `cargo test -p wimux-server -- --test-threads=1`
Expected: PASS (G1→G4, M1→M3, onglets). Vérifie en particulier que `bascule_gui_arrete_le_flux_precedent` et `bascule_efface_activite` restent verts après le refactor.

- [ ] **Step 8: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: aucun warning. (Task 4 modifie légitimement `gui_mode.rs` : ne PAS le rétablir via `git checkout`.)

- [ ] **Step 9: Commit**

```bash
git add crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs
git commit -m "$(printf 'feat(daemon): reattach_active_window + cablage onglets (W2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: Pont Tauri — événement `window-list` + commandes

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `ClientMessage::{NewWindow, SelectWindow, CloseWindow, RenameWindow}`, `ServerMessage::WindowList` (Task 1) ; `Bridge` + connexion persistante (existants).
- Produces (commandes Tauri, sur la connexion persistante) :
  - `new_window()`, `select_window(index: u32)`, `close_window(index: u32)`, `rename_window(index: u32, name: String)`.
  - Événement Tauri `"window-list"` de charge utile `(Vec<WindowInfo>, u32)` (WindowInfo dérive `Serialize`/`Clone`).

- [ ] **Step 1: Émettre l'événement `window-list` dans la boucle de lecture**

Dans `wimux-gui/src-tauri/src/lib.rs`, dans le thread lecteur de `attach_session` (le `match msg { ... }`), ajouter un bras juste après le bras `ServerMessage::WindowLayout { tree, active } => { ... }` :

```rust
                        ServerMessage::WindowList { windows, active } => {
                            let _ = app2.emit("window-list", (windows, active));
                        }
```

- [ ] **Step 2: Ajouter les 4 commandes**

Toujours dans `lib.rs`, après la commande `pane_resize` (avant `#[cfg_attr(mobile, tauri::mobile_entry_point)]`), ajouter :

```rust
#[tauri::command]
fn new_window(bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::NewWindow).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn select_window(index: u32, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::SelectWindow { index }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_window(index: u32, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::CloseWindow { index }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn rename_window(index: u32, name: String, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::RenameWindow { index, name }).map_err(|e| e.to_string())?;
    }
    Ok(())
}
```

- [ ] **Step 3: Enregistrer les commandes dans `generate_handler!`**

Dans `run()`, dans la macro `tauri::generate_handler![ ... ]`, ajouter les 4 noms. Remplacer la fin de la liste :

```rust
            list_agent_templates,
            create_agent,
            create_batch
        ])
```

par :

```rust
            list_agent_templates,
            create_agent,
            create_batch,
            new_window,
            select_window,
            close_window,
            rename_window
        ])
```

- [ ] **Step 4: Compiler le crate Tauri**

Run: `cd wimux-gui/src-tauri && cargo build`
Expected: succès (les 4 commandes + le bras `WindowList` compilent ; `WindowInfo` est `Serialize`/`Clone` pour `emit`).

- [ ] **Step 5: clippy**

Run: `cd wimux-gui/src-tauri && RUSTFLAGS="-D warnings" cargo clippy --all-targets`
Expected: aucun warning.

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/src-tauri/src/lib.rs
git commit -m "$(printf 'feat(gui-bridge): evenement window-list + commandes onglets (W2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 6: Frontend — barre d'onglets + styles + README

**Files:**
- Modify: `wimux-gui/index.html`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`

**Interfaces:**
- Consumes: commandes `new_window`/`select_window`/`close_window`/`rename_window` + événement `window-list` (Task 5) ; `paneManager.reset()` (existant).
- Produces: barre d'onglets `#tabs` rendue depuis `window-list`.

- [ ] **Step 1: Envelopper `#terminal` dans un conteneur colonne `#workspace`**

Dans `wimux-gui/index.html`, remplacer :

```html
      <div id="terminal"></div>
    </div>
```

par :

```html
      <div id="workspace">
        <div id="tabs"></div>
        <div id="terminal"></div>
      </div>
    </div>
```

(La `</div>` conservée ferme `#app`.)

- [ ] **Step 2: Ajouter le rendu des onglets dans `main.ts`**

Dans `wimux-gui/src/main.ts`, juste après le bloc des `listen(...)` d'événements volets (après le `listen<string>("pane-error", ...)`), ajouter :

```ts
// --- W2 : barre d'onglets (fenêtres de la session GUI-attachée) -------------
type WindowInfo = { name: string | null };

const tabsEl = document.getElementById("tabs")!;
// Dernière fenêtre active connue : une bascule (changement d'`active`) impose un
// `reset()` du PaneManager avant le prochain window-layout (pane_id globaux, les
// snapshots frais repeignent le contenu).
let lastActiveWindow = -1;

listen<[WindowInfo[], number]>("window-list", (e) => {
  const [windows, active] = e.payload;
  if (active !== lastActiveWindow) {
    paneManager.reset();
  }
  lastActiveWindow = active;
  renderTabs(windows, active);
});

function renderTabs(windows: WindowInfo[], active: number) {
  tabsEl.innerHTML = "";
  windows.forEach((win, i) => {
    const tab = document.createElement("div");
    tab.className = "tab" + (i === active ? " active" : "");
    const label = document.createElement("span");
    label.className = "tab-label";
    label.textContent = win.name ?? String(i + 1);
    tab.appendChild(label);
    let clickTimer: number | null = null;
    label.ondblclick = (ev) => {
      ev.stopPropagation();
      if (clickTimer !== null) { clearTimeout(clickTimer); clickTimer = null; }
      startTabRename(tab, i, win.name ?? "");
    };
    // Le `×` est masqué s'il ne reste qu'une fenêtre (fermeture interdite).
    if (windows.length > 1) {
      const close = document.createElement("span");
      close.className = "tab-close";
      close.textContent = "×";
      close.onclick = (ev) => {
        ev.stopPropagation();
        invoke("close_window", { index: i }).catch(() => {});
      };
      tab.appendChild(close);
    }
    tab.onclick = () => {
      if (clickTimer !== null) return; // 2e clic d'un double-clic : laisse ondblclick
      clickTimer = window.setTimeout(() => {
        clickTimer = null;
        invoke("select_window", { index: i }).catch(() => {});
      }, 200);
    };
    tabsEl.appendChild(tab);
  });
  const add = document.createElement("button");
  add.className = "tab-add";
  add.textContent = "+";
  add.title = "Nouvel onglet";
  add.onclick = () => { invoke("new_window", {}).catch(() => {}); };
  tabsEl.appendChild(add);
}

function startTabRename(tab: HTMLElement, index: number, oldName: string) {
  const input = document.createElement("input");
  input.className = "tab-edit";
  input.value = oldName;
  tab.replaceChildren(input);
  input.focus();
  input.select();
  let committed = false;
  const commit = () => {
    if (committed) return;
    committed = true;
    const name = input.value.trim();
    // Nom vide => le serveur remet le nom à None (affiche la position).
    invoke("rename_window", { index, name }).catch(() => {});
  };
  input.onkeydown = (ev) => {
    if (ev.key === "Enter") commit();
    else if (ev.key === "Escape") { committed = true; input.blur(); }
  };
  input.onblur = () => commit();
}
```

- [ ] **Step 3: Réinitialiser `lastActiveWindow` à chaque bascule de session**

Dans `main.ts`, dans la fonction `switchTo`, juste après `paneManager.reset();`, ajouter :

```ts
  lastActiveWindow = -1; // force le rendu des onglets + reset au 1er window-list de la nouvelle session
```

- [ ] **Step 4: Ajouter les styles**

Dans `wimux-gui/src/styles.css`, après la règle `#terminal > * { ... }` (ligne du bloc terminal), ajouter :

```css
#workspace { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; }
#tabs { display: flex; align-items: stretch; background: #252526; border-bottom: 1px solid #333; min-height: 32px; overflow-x: auto; }
.tab { display: flex; align-items: center; gap: 6px; padding: 4px 10px; cursor: pointer; color: #ccc; border-right: 1px solid #333; border-top: 2px solid transparent; white-space: nowrap; }
.tab:hover { background: #2a2d2e; }
.tab.active { background: #1e1e1e; border-top-color: #0a84ff; color: #fff; }
.tab .tab-label { line-height: 1; }
.tab .tab-close { visibility: hidden; color: #999; }
.tab:hover .tab-close { visibility: visible; }
.tab .tab-edit { background: #1e1e1e; color: #fff; border: 1px solid #0a84ff; width: 90px; }
.tab-add { background: transparent; color: #ccc; border: none; cursor: pointer; padding: 4px 12px; font-size: 16px; line-height: 1; }
.tab-add:hover { background: #2a2d2e; }
```

- [ ] **Step 5: Construire le frontend**

Run: `cd wimux-gui && npm run build`
Expected: build OK (TypeScript sans erreur, Vite produit le bundle).

- [ ] **Step 6: Ajouter la section « Vérification manuelle W2 » au README**

Dans `wimux-gui/README.md`, ajouter à la fin du fichier :

```markdown
## Vérification manuelle W2 (onglets terminaux)

Prérequis : rebuild + redémarrage du daemon (changement de protocole), puis
`npm run tauri dev`. S'attacher à une session.

1. **État initial** : une session neuve affiche un seul onglet (libellé « 1 »),
   sans `×` (fermeture de la dernière fenêtre interdite), et un bouton `+`.
2. **Créer** : cliquer `+` → un 2e onglet apparaît (« 2 »), devient actif, et la
   zone de volets se réinitialise sur le shell de la nouvelle fenêtre.
3. **Basculer** : cliquer l'onglet « 1 » → le contenu revient à la 1re fenêtre
   (les volets et leur sortie suivent la bascule). L'onglet actif est surligné
   (bordure haute bleue).
4. **Renommer** : double-cliquer un onglet → champ d'édition inline ; taper
   « build » + Entrée → le libellé devient « build ». Vider le nom + Entrée →
   le libellé revient à la position.
5. **Fermer** : avec ≥ 2 onglets, survoler un onglet → un `×` apparaît ; cliquer
   → l'onglet disparaît. Quand il ne reste qu'un onglet, le `×` est masqué
   (le dernier onglet ne peut pas être fermé).
6. **Non-régression volets** : dans un onglet, découper un volet (W1/G3),
   redimensionner la bordure, fermer un volet — tout fonctionne comme avant, par
   onglet.
```

- [ ] **Step 7: Commit**

```bash
git add wimux-gui/index.html wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "$(printf 'feat(gui): barre onglets terminaux par workspace (W2)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Vérification finale (non-régression globale)

- [ ] **Suite complète Rust**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS (TUI + G1→G4 + M1→M3 + W2).

- [ ] **fmt + clippy workspace**

Run: `cargo fmt --check` puis `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
Expected: aucun warning, aucun diff de format.

- [ ] **Build frontend**

Run: `cd wimux-gui && npm run build`
Expected: OK.

- [ ] **Vérification manuelle** : suivre la section « Vérification manuelle W2 » du README (après rebuild + redémarrage du daemon détaché).
