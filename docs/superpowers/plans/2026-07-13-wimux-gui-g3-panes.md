# wimux GUI G3 — Volets graphiques : Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rendre dans `wimux-gui` l'arbre de découpes de la fenêtre active avec un xterm.js par volet (couleurs + curseur fidèles), et gérer découper / fermer / focaliser / glisser-bordure à la souris via des commandes de volet explicites.

**Architecture:** Approche A. Le serveur reste la source de vérité de la topologie (arbre `Window` déjà partagé TUI/GUI) ; la GUI possède le rendu et les entrées. À l'attache, le serveur envoie `WindowLayout` + un `PaneSnapshot` fidèle par volet, puis diffuse le `PaneOutput` de tous les volets par un canal fusionné. Les opérations (`SplitPane`/`ClosePane`/`FocusPane`/`SetSplitRatio`) mutent la fenêtre active, et le serveur repousse `WindowLayout`.

**Tech Stack:** Rust (workspace, edition 2024), Tauri 2 + TypeScript + xterm.js + FitAddon, Named Pipe overlapped, postcard, `wimux-vt`.

## Global Constraints

- Rust edition 2024. `cargo fmt` + `cargo clippy --workspace --all-targets` sous `RUSTFLAGS="-D warnings"` PROPRES à chaque tâche.
- Aucune régression : suites TUI + G1 + G2 vertes (`cargo test --workspace -- --test-threads=1`).
- Tests serveur = intégration dans `crates/wimux-server/tests/gui_mode.rs` (+ tests unitaires dans `window.rs`/`pane.rs`/`wimux-vt` là où pertinent). Frontend = build `npm run build` (tsc+vite) + vérif manuelle documentée dans `wimux-gui/README.md` (pas de test auto front).
- Outil shell : **Bash tool** (git bash) pour cargo/git ; les tests d'intégration sont lents (ConPTY), `--test-threads=1`, patience.
- Frontend Tauri : build depuis `wimux-gui` (`npm run build`) ; le crate Tauri `wimux-gui` a son propre `[workspace]` (exclu du workspace racine), donc `cargo build` Rust du pont se fait dans `wimux-gui/src-tauri`.
- Chaque commit se termine par le trailer : `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Types partagés : `PaneId = u64`, `node_id: u32`, ratio `f32` borné `[0.1,0.9]` côté serveur.

---

## File Structure

- `crates/wimux-protocol/src/lib.rs` — **modifier** : `SplitDir`, `LayoutNode`, 4 nouveaux `ClientMessage`, `ServerMessage::WindowLayout`, tests roundtrip.
- `crates/wimux-server/src/window.rs` — **modifier** : `node_id` sur `Node::Split`, `layout_tree`, `active_pane_id`, `split_pane`/`close_pane`/`set_ratio`/`pane_ids`, `From<protocol::SplitDir>`.
- `crates/wimux-server/src/pane.rs` — **modifier** : `grid_to_ansi` (remplace `grid_to_bytes`), abonnés tagués `(PaneId, Vec<u8>)`, `snapshot_and_subscribe_into`.
- `crates/wimux-server/src/session.rs` — **modifier** : `gui_attach_window`, `gui_split`, `gui_close`, `gui_focus`, `gui_set_ratio`, `gui_pane_resize`, `window_layout` ; retrait de `gui_attach`.
- `crates/wimux-server/src/daemon.rs` — **modifier** : stubs (Task 1) puis câblage GUI multi-volets (Task 6), `GuiAttachment` étendu, `PaneResize` honoré.
- `crates/wimux-server/tests/gui_mode.rs` — **modifier** : tests d'intégration attach/split/ratio/close.
- `wimux-gui/src-tauri/src/lib.rs` — **modifier** : commandes `split_pane`/`close_pane`/`focus_pane`/`set_split_ratio`/`pane_resize`, event `window-layout`.
- `wimux-gui/src/panes.ts` — **créer** : `PaneManager` + type `LayoutNode`.
- `wimux-gui/src/main.ts`, `wimux-gui/index.html`, `wimux-gui/src/styles.css` — **modifier** : brancher `PaneManager`, styles des volets/séparateurs/barre.
- `wimux-gui/README.md` — **modifier** : section « Vérification manuelle G3 ».

---

## Task 1: Protocole G3 (+ stubs démon)

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs` (stubs no-op pour rester exhaustif)
- Test: `crates/wimux-protocol/src/lib.rs` (module `tests`)

**Interfaces:**
- Consumes: rien.
- Produces:
  - `pub enum SplitDir { LeftRight, TopBottom }` (Serialize/Deserialize/Clone/Copy/Debug/PartialEq)
  - `pub enum LayoutNode { Leaf { pane_id: u64 }, Split { node_id: u32, dir: SplitDir, ratio: f32, a: Box<LayoutNode>, b: Box<LayoutNode> } }` (Serialize/Deserialize/Clone/Debug/PartialEq)
  - `ClientMessage::SplitPane { pane_id: u64, dir: SplitDir }`, `ClosePane { pane_id: u64 }`, `FocusPane { pane_id: u64 }`, `SetSplitRatio { node_id: u32, ratio: f32 }`
  - `ServerMessage::WindowLayout { tree: LayoutNode, active: u64 }`

- [ ] **Step 1: Écrire les tests roundtrip (échouent)**

Dans le module `tests` de `crates/wimux-protocol/src/lib.rs`, ajouter :

```rust
    #[test]
    fn aller_retour_split_pane() {
        let msg = ClientMessage::SplitPane {
            pane_id: 3,
            dir: SplitDir::TopBottom,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::SplitPane { pane_id, dir } => {
                assert_eq!(pane_id, 3);
                assert_eq!(dir, SplitDir::TopBottom);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_window_layout() {
        let tree = LayoutNode::Split {
            node_id: 1,
            dir: SplitDir::LeftRight,
            ratio: 0.5,
            a: Box::new(LayoutNode::Leaf { pane_id: 10 }),
            b: Box::new(LayoutNode::Leaf { pane_id: 11 }),
        };
        let msg = ServerMessage::WindowLayout {
            tree: tree.clone(),
            active: 10,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::WindowLayout { tree: got, active } => {
                assert_eq!(active, 10);
                assert_eq!(got, tree);
            }
            _ => panic!("mauvais variant"),
        }
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-protocol`
Expected: FAIL — `cannot find type SplitDir` / `no variant SplitPane`.

- [ ] **Step 3: Ajouter les types partagés**

Dans `crates/wimux-protocol/src/lib.rs`, après la définition de `SessionInfo` (avant `Frame`), insérer :

```rust
/// Sens d'une découpe de volet (miroir du `window::SplitDir` serveur).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SplitDir {
    LeftRight,
    TopBottom,
}

/// Arbre de disposition d'une fenêtre, sérialisable pour la GUI. Chaque `Split`
/// porte un `node_id` stable (attribué à la création) pour cibler `SetSplitRatio`
/// sans ambiguïté même si l'arbre a changé ailleurs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutNode {
    Leaf {
        pane_id: u64,
    },
    Split {
        node_id: u32,
        dir: SplitDir,
        ratio: f32,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
}
```

- [ ] **Step 4: Ajouter les variantes de messages**

Dans `enum ClientMessage`, après `PaneResize { .. }`, insérer :

```rust
    /// Découpe le volet désigné (mode GUI) ; le nouveau volet devient actif.
    SplitPane {
        pane_id: u64,
        dir: SplitDir,
    },
    /// Ferme le volet désigné (mode GUI).
    ClosePane {
        pane_id: u64,
    },
    /// Désigne le volet actif (mode GUI).
    FocusPane {
        pane_id: u64,
    },
    /// Fixe le ratio d'un nœud de découpe interne (glisser-bordure). Borné
    /// `[0.1, 0.9]` côté serveur.
    SetSplitRatio {
        node_id: u32,
        ratio: f32,
    },
```

Dans `enum ServerMessage`, après `PaneOutput { .. }`, insérer :

```rust
    /// Disposition de la fenêtre active (mode GUI). Envoyé à l'attache et après
    /// chaque changement de topologie ou de ratio.
    WindowLayout {
        tree: LayoutNode,
        active: u64,
    },
```

- [ ] **Step 5: Ajouter les stubs no-op dans le démon (garder le `match` exhaustif)**

Dans `crates/wimux-server/src/daemon.rs`, dans le `match msg` de `handle_client`, juste avant le bras `ClientMessage::SendKeys { .. }`, insérer :

```rust
            ClientMessage::SplitPane { .. } => {} // Task 6
            ClientMessage::ClosePane { .. } => {} // Task 6
            ClientMessage::FocusPane { .. } => {} // Task 6
            ClientMessage::SetSplitRatio { .. } => {} // Task 6
```

- [ ] **Step 6: Lancer les tests (attendu PASS) + build serveur**

Run: `cargo test -p wimux-protocol`
Expected: PASS (dont `aller_retour_split_pane`, `aller_retour_window_layout`).

Run: `cargo build -p wimux-server`
Expected: OK (match exhaustif grâce aux stubs).

- [ ] **Step 7: fmt + clippy**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-protocol -p wimux-server --all-targets`
Expected: OK.

- [ ] **Step 8: Commit**

```bash
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(protocol): SplitDir, LayoutNode et messages de volet G3\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 2: `window.rs` — arbre exposable + opérations par id

**Files:**
- Modify: `crates/wimux-server/src/window.rs`
- Test: `crates/wimux-server/src/window.rs` (module `tests`)

**Interfaces:**
- Consumes (Task 1): `wimux_protocol::{LayoutNode, SplitDir as ProtoSplitDir}`.
- Produces (utilisés par Task 5) :
  - `Window::layout_tree(&self) -> wimux_protocol::LayoutNode`
  - `Window::active_pane_id(&self) -> PaneId`
  - `Window::split_pane(&mut self, target: PaneId, dir: SplitDir, new_pane: Arc<Pane>)`
  - `Window::close_pane(&mut self, target: PaneId) -> bool`
  - `Window::set_ratio(&mut self, node_id: u32, ratio: f32)`
  - `Window::pane_ids(&self) -> Vec<PaneId>`
  - `impl From<wimux_protocol::SplitDir> for SplitDir` (utilisé par Task 6)

- [ ] **Step 1: Écrire les tests unitaires (échouent)**

Dans le module `tests` de `crates/wimux-server/src/window.rs`, ajouter :

```rust
    fn dummy_pane() -> Arc<Pane> {
        Pane::spawn(10, 5, "cmd.exe", crate::pane::Notifier::new()).unwrap()
    }

    #[test]
    fn layout_tree_dun_split() {
        let root = Node::Split {
            node_id: 7,
            dir: SplitDir::LeftRight,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        };
        match node_to_layout(&root) {
            wimux_protocol::LayoutNode::Split {
                node_id, a, b, ..
            } => {
                assert_eq!(node_id, 7);
                assert!(matches!(*a, wimux_protocol::LayoutNode::Leaf { pane_id: 1 }));
                assert!(matches!(*b, wimux_protocol::LayoutNode::Leaf { pane_id: 2 }));
            }
            _ => panic!("attendu un Split"),
        }
    }

    #[test]
    fn set_ratio_walk_change_le_bon_noeud() {
        let mut root = Node::Split {
            node_id: 42,
            dir: SplitDir::LeftRight,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        };
        assert!(set_ratio_walk(&mut root, 42, 0.7));
        match root {
            Node::Split { ratio, .. } => assert!((ratio - 0.7).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn split_et_close_par_id() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new("w".into(), p1);
        let p2 = dummy_pane();
        let id2 = p2.id;
        win.split_pane(id1, SplitDir::LeftRight, p2);
        assert_eq!(win.pane_ids().len(), 2);
        assert!(win.pane_ids().contains(&id1) && win.pane_ids().contains(&id2));
        assert!(matches!(
            win.layout_tree(),
            wimux_protocol::LayoutNode::Split { .. }
        ));
        assert!(!win.close_pane(id2));
        assert_eq!(win.pane_ids(), vec![id1]);
        assert!(matches!(
            win.layout_tree(),
            wimux_protocol::LayoutNode::Leaf { pane_id } if pane_id == id1
        ));
        win.kill_all();
    }

    #[test]
    fn set_ratio_borne() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new("w".into(), p1);
        win.split_pane(id1, SplitDir::TopBottom, dummy_pane());
        let node_id = match win.layout_tree() {
            wimux_protocol::LayoutNode::Split { node_id, .. } => node_id,
            _ => panic!(),
        };
        win.set_ratio(node_id, 5.0);
        match win.layout_tree() {
            wimux_protocol::LayoutNode::Split { ratio, .. } => assert!((ratio - 0.9).abs() < 1e-6),
            _ => panic!(),
        }
        win.kill_all();
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --lib window::tests`
Expected: FAIL — `no field node_id` / `no method layout_tree`.

- [ ] **Step 3: Ajouter `node_id` au `Node::Split` et le compteur**

En tête de `crates/wimux-server/src/window.rs`, remplacer l'import atomique manquant en ajoutant après `use std::sync::Arc;` :

```rust
use std::sync::atomic::{AtomicU32, Ordering};
```

Ajouter, sous les `use`, le compteur :

```rust
/// Attribue un identifiant stable à chaque nœud de découpe (par processus).
static NEXT_NODE_ID: AtomicU32 = AtomicU32::new(1);
```

Modifier la variante `Node::Split` pour porter `node_id` :

```rust
enum Node {
    Leaf(PaneId),
    Split {
        node_id: u32,
        dir: SplitDir,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}
```

- [ ] **Step 4: Réparer les sites qui déstructurent `Node::Split`**

Dans `fn resize_walk`, changer le bras :

```rust
        Node::Split { dir, ratio, a, b, .. } => {
```

Dans `fn layout`, changer le bras :

```rust
        Node::Split { dir, ratio, a, b, .. } => match dir {
```

Dans le module `tests`, la fonction `split_lr()` construit un `Node::Split` : lui ajouter `node_id: 1,` en première position.

(Les autres sites — `replace_leaf`, `remove_leaf`, `contains_leaf` — utilisent déjà `{ a, b, .. }` et restent valides.)

- [ ] **Step 5: Convertir `split` en délégation vers `split_pane`, ajouter les nouvelles méthodes**

Remplacer la méthode `split` existante par :

```rust
    /// Découpe le volet actif, en y insérant `new_pane` qui devient actif.
    pub fn split(&mut self, dir: SplitDir, new_pane: Arc<Pane>) {
        let active = self.active;
        self.split_pane(active, dir, new_pane);
    }

    /// Découpe le volet DÉSIGNÉ `target` ; le nouveau volet devient actif.
    pub fn split_pane(&mut self, target: PaneId, dir: SplitDir, new_pane: Arc<Pane>) {
        self.zoomed = false;
        let new_id = new_pane.id;
        let node_id = NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed);
        Self::replace_leaf(&mut self.root, target, |old| Node::Split {
            node_id,
            dir,
            ratio: 0.5,
            a: Box::new(old),
            b: Box::new(Node::Leaf(new_id)),
        });
        self.panes.insert(new_id, new_pane);
        self.active = new_id;
    }
```

Remplacer la méthode `close_active` par :

```rust
    /// Ferme le volet actif. Renvoie `true` si la fenêtre est désormais vide.
    pub fn close_active(&mut self) -> bool {
        let active = self.active;
        self.close_pane(active)
    }

    /// Ferme le volet DÉSIGNÉ `target`. Renvoie `true` si la fenêtre est vide.
    pub fn close_pane(&mut self, target: PaneId) -> bool {
        self.zoomed = false;
        if let Some(pane) = self.panes.remove(&target) {
            pane.kill();
        }
        if self.panes.is_empty() {
            return true;
        }
        Self::remove_leaf(&mut self.root, target);
        if !self.panes.contains_key(&self.active) {
            self.active = *self.panes.keys().next().unwrap();
        }
        false
    }
```

Ajouter (par exemple juste après `pane_count`) les accesseurs :

```rust
    /// Traduit l'arbre interne en `LayoutNode` sérialisable pour la GUI.
    pub fn layout_tree(&self) -> wimux_protocol::LayoutNode {
        node_to_layout(&self.root)
    }

    /// Identifiant du volet actif.
    pub fn active_pane_id(&self) -> PaneId {
        self.active
    }

    /// Fixe le ratio du nœud de découpe `node_id` (borné `[0.1, 0.9]`).
    pub fn set_ratio(&mut self, node_id: u32, ratio: f32) {
        set_ratio_walk(&mut self.root, node_id, ratio.clamp(0.1, 0.9));
    }

    /// Identifiants des volets, triés.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
        ids.sort_unstable();
        ids
    }
```

- [ ] **Step 6: Ajouter les fonctions libres + la conversion `From`**

À la fin du fichier (avant `#[cfg(test)]`), ajouter :

```rust
/// Traduit un `Node` interne en `LayoutNode` de protocole.
fn node_to_layout(node: &Node) -> wimux_protocol::LayoutNode {
    match node {
        Node::Leaf(id) => wimux_protocol::LayoutNode::Leaf { pane_id: *id },
        Node::Split {
            node_id,
            dir,
            ratio,
            a,
            b,
        } => wimux_protocol::LayoutNode::Split {
            node_id: *node_id,
            dir: match dir {
                SplitDir::LeftRight => wimux_protocol::SplitDir::LeftRight,
                SplitDir::TopBottom => wimux_protocol::SplitDir::TopBottom,
            },
            ratio: *ratio,
            a: Box::new(node_to_layout(a)),
            b: Box::new(node_to_layout(b)),
        },
    }
}

/// Fixe le ratio du nœud `target`. Renvoie `true` si trouvé.
fn set_ratio_walk(node: &mut Node, target: u32, ratio: f32) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split {
            node_id, ratio: r, a, b, ..
        } => {
            if *node_id == target {
                *r = ratio;
                true
            } else {
                set_ratio_walk(a, target, ratio) || set_ratio_walk(b, target, ratio)
            }
        }
    }
}

impl From<wimux_protocol::SplitDir> for SplitDir {
    fn from(d: wimux_protocol::SplitDir) -> Self {
        match d {
            wimux_protocol::SplitDir::LeftRight => SplitDir::LeftRight,
            wimux_protocol::SplitDir::TopBottom => SplitDir::TopBottom,
        }
    }
}
```

- [ ] **Step 7: Lancer les tests (attendu PASS)**

Run: `cargo test -p wimux-server --lib window -- --test-threads=1`
Expected: PASS (dont `split_et_close_par_id`, `set_ratio_borne`, `layout_tree_dun_split`).

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

```bash
git add crates/wimux-server/src/window.rs
git commit -m "$(printf 'feat(window): node_id stable, layout_tree et ops par id (G3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 3: Instantané fidèle `grid_to_ansi`

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Test: `crates/wimux-server/src/pane.rs` (module `tests`)

**Interfaces:**
- Consumes: `wimux_vt::{Grid, Color, Pen, Terminal}`.
- Produces (utilisé par Tasks 4/5) : `fn grid_to_ansi(grid: &Grid, cursor: (u16, u16)) -> Vec<u8>` ; `Pane::snapshot_and_subscribe` continue de renvoyer `(Vec<u8>, Receiver<Vec<u8>>)` mais le snapshot est désormais coloré.

- [ ] **Step 1: Écrire le test fidèle + adapter l'ancien (échouent)**

Dans le module `tests` de `crates/wimux-server/src/pane.rs`, remplacer le test `snapshot_reproduit_le_texte_visible` par :

```rust
    #[test]
    fn snapshot_reproduit_le_texte_visible() {
        let mut term = wimux_vt::Terminal::new(20, 3);
        term.advance(b"abc\r\ndef");
        let bytes = grid_to_ansi(term.grid(), term.cursor());
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("abc"));
        assert!(text.contains("def"));
    }

    #[test]
    fn grid_to_ansi_preserve_couleur_et_curseur() {
        let mut term = wimux_vt::Terminal::new(20, 3);
        term.advance(b"\x1b[31mRED\x1b[0m");
        let bytes = grid_to_ansi(term.grid(), term.cursor());

        let mut term2 = wimux_vt::Terminal::new(20, 3);
        term2.advance(&bytes);
        let cell = term2.grid().cell(0, 0).unwrap();
        assert_eq!(cell.ch, 'R');
        assert_eq!(cell.pen.fg, wimux_vt::Color::Indexed(1));
        assert_eq!(term2.cursor(), term.cursor());
    }
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --lib pane::tests`
Expected: FAIL — `cannot find function grid_to_ansi`.

- [ ] **Step 3: Ajouter `Color, Pen` aux imports**

Dans `crates/wimux-server/src/pane.rs`, remplacer la ligne :

```rust
use wimux_vt::{Cell, Grid, Terminal};
```

par :

```rust
use wimux_vt::{Cell, Color, Grid, Pen, Terminal};
```

- [ ] **Step 4: Remplacer `grid_to_bytes` par `grid_to_ansi`**

Supprimer la fonction `grid_to_bytes` (et son doc-comment) et la remplacer par :

```rust
/// Reconstruit une séquence d'octets rejouable et FIDÈLE (écran visible) depuis
/// la grille : efface l'écran, puis émet chaque ligne en groupant les runs de
/// `Pen` identique (couleurs SGR + attributs), avec reset avant chaque changement
/// de pen et en fin de ligne, puis positionne le curseur.
fn grid_to_ansi(grid: &Grid, cursor: (u16, u16)) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[2J\x1b[H");
    for row in 0..grid.rows() {
        let mut cur_pen: Option<Pen> = None;
        for cell in grid.row(row) {
            if cell.width == 0 {
                continue; // colonne de continuation d'un caractère large
            }
            if cur_pen != Some(cell.pen) {
                out.extend_from_slice(b"\x1b[0m");
                let sgr = pen_to_sgr(&cell.pen);
                if !sgr.is_empty() {
                    out.extend_from_slice(format!("\x1b[{sgr}m").as_bytes());
                }
                cur_pen = Some(cell.pen);
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(cell.ch.encode_utf8(&mut buf).as_bytes());
        }
        out.extend_from_slice(b"\x1b[0m");
        if row + 1 < grid.rows() {
            out.extend_from_slice(b"\r\n");
        }
    }
    let (col, row) = cursor;
    out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    out
}

/// Construit la liste de paramètres SGR (sans les `ESC[` / `m`) pour un `Pen`.
fn pen_to_sgr(pen: &Pen) -> String {
    let mut codes: Vec<String> = Vec::new();
    if pen.attrs.bold {
        codes.push("1".into());
    }
    if pen.attrs.italic {
        codes.push("3".into());
    }
    if pen.attrs.underline {
        codes.push("4".into());
    }
    if pen.attrs.reverse {
        codes.push("7".into());
    }
    match pen.fg {
        Color::Default => {}
        Color::Indexed(n @ 0..=7) => codes.push((30 + n as u16).to_string()),
        Color::Indexed(n @ 8..=15) => codes.push((90 + n as u16 - 8).to_string()),
        Color::Indexed(n) => codes.push(format!("38;5;{n}")),
        Color::Rgb(r, g, b) => codes.push(format!("38;2;{r};{g};{b}")),
    }
    match pen.bg {
        Color::Default => {}
        Color::Indexed(n @ 0..=7) => codes.push((40 + n as u16).to_string()),
        Color::Indexed(n @ 8..=15) => codes.push((100 + n as u16 - 8).to_string()),
        Color::Indexed(n) => codes.push(format!("48;5;{n}")),
        Color::Rgb(r, g, b) => codes.push(format!("48;2;{r};{g};{b}")),
    }
    codes.join(";")
}
```

- [ ] **Step 5: Utiliser `grid_to_ansi` dans `snapshot_and_subscribe`**

Dans `Pane::snapshot_and_subscribe`, remplacer :

```rust
        let snapshot = grid_to_bytes(&st.terminal);
```

par :

```rust
        let snapshot = grid_to_ansi(st.terminal.grid(), st.terminal.cursor());
```

- [ ] **Step 6: Lancer les tests (attendu PASS)**

Run: `cargo test -p wimux-server --lib pane -- --test-threads=1`
Expected: PASS (dont `grid_to_ansi_preserve_couleur_et_curseur`).

- [ ] **Step 7: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

```bash
git add crates/wimux-server/src/pane.rs
git commit -m "$(printf 'feat(pane): grid_to_ansi — snapshot GUI fidele (couleurs + curseur)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 4: `pane.rs` — abonnés tagués `(PaneId, Vec<u8>)`

**Files:**
- Modify: `crates/wimux-server/src/pane.rs`
- Modify: `crates/wimux-server/src/session.rs` (signature de `gui_attach`)
- Modify: `crates/wimux-server/src/daemon.rs` (destructurer le tuple dans la boucle `AttachGui`)

**Interfaces:**
- Consumes (Task 3) : `grid_to_ansi`.
- Produces (utilisés par Task 5) :
  - `subscribers: Vec<Sender<(PaneId, Vec<u8>)>>`
  - `Pane::snapshot_and_subscribe(&self) -> (Vec<u8>, Receiver<(PaneId, Vec<u8>)>)`
  - `Pane::snapshot_and_subscribe_into(&self, tx: Sender<(PaneId, Vec<u8>)>) -> Vec<u8>`

- [ ] **Step 1: Changer le type des abonnés**

Dans `struct PaneState`, remplacer :

```rust
    subscribers: Vec<std::sync::mpsc::Sender<Vec<u8>>>,
```

par :

```rust
    subscribers: Vec<std::sync::mpsc::Sender<(PaneId, Vec<u8>)>>,
```

- [ ] **Step 2: Diffuser en taguant le `pane_id` dans `reader_loop`**

Dans `fn reader_loop`, remplacer :

```rust
                    st.subscribers
                        .retain(|tx| tx.send(buf[..n].to_vec()).is_ok());
```

par :

```rust
                    st.subscribers
                        .retain(|tx| tx.send((pane.id, buf[..n].to_vec())).is_ok());
```

- [ ] **Step 3: Adapter `snapshot_and_subscribe` + ajouter `snapshot_and_subscribe_into`**

Remplacer la méthode `snapshot_and_subscribe` par :

```rust
    /// Sous un seul verrou : reconstruit l'instantané FIDÈLE ET inscrit un abonné
    /// au flux brut tagué `pane_id`. Atomique. Crée son propre canal.
    pub fn snapshot_and_subscribe(
        &self,
    ) -> (Vec<u8>, std::sync::mpsc::Receiver<(PaneId, Vec<u8>)>) {
        let mut st = self.state.lock().unwrap();
        let snapshot = grid_to_ansi(st.terminal.grid(), st.terminal.cursor());
        let (tx, rx) = std::sync::mpsc::channel();
        st.subscribers.push(tx);
        (snapshot, rx)
    }

    /// Comme `snapshot_and_subscribe`, mais inscrit un `tx` FOURNI (canal fusionné
    /// multi-volets). Renvoie le snapshot fidèle du volet.
    pub fn snapshot_and_subscribe_into(
        &self,
        tx: std::sync::mpsc::Sender<(PaneId, Vec<u8>)>,
    ) -> Vec<u8> {
        let mut st = self.state.lock().unwrap();
        let snapshot = grid_to_ansi(st.terminal.grid(), st.terminal.cursor());
        st.subscribers.push(tx);
        snapshot
    }
```

- [ ] **Step 4: Répercuter le type sur `Session::gui_attach`**

Dans `crates/wimux-server/src/session.rs`, dans `gui_attach`, changer la signature de retour :

```rust
    pub fn gui_attach(&self) -> Option<(u64, Vec<u8>, std::sync::mpsc::Receiver<(u64, Vec<u8>)>)> {
```

(Le corps reste `let (snapshot, rx) = pane.snapshot_and_subscribe();` puis `Some((pane.id, snapshot, rx))`.)

- [ ] **Step 5: Destructurer le tuple dans la boucle `AttachGui` du démon**

Dans `crates/wimux-server/src/daemon.rs`, dans le bras `AttachGui`, la boucle de retransmission fait actuellement `Ok(chunk) => { ... PaneOutput { pane_id, bytes: chunk } ... }`. Remplacer ce bras `Ok` par :

```rust
                                        Ok((pid, chunk)) => {
                                            let mut w: &PipeConn = &conn_out;
                                            if send(
                                                &mut w,
                                                &ServerMessage::PaneOutput {
                                                    pane_id: pid,
                                                    bytes: chunk,
                                                },
                                            )
                                            .is_err()
                                            {
                                                break;
                                            }
                                        }
```

(Ce bras sera entièrement réécrit en Task 6 ; ici on garde juste la compilation.)

- [ ] **Step 6: Build + tests lib**

Run: `cargo build -p wimux-server`
Expected: OK.

Run: `cargo test -p wimux-server --lib -- --test-threads=1`
Expected: PASS.

- [ ] **Step 7: Non-régression G1/G2 (le flux passe toujours)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS (dont `attach_gui_recoit_snapshot_puis_flux`, `bascule_gui_arrete_le_flux_precedent`).

- [ ] **Step 8: fmt + clippy + commit**

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy -p wimux-server --all-targets`
Expected: OK.

```bash
git add crates/wimux-server/src/pane.rs crates/wimux-server/src/session.rs crates/wimux-server/src/daemon.rs
git commit -m "$(printf 'feat(pane): abonnes tagues pane_id + snapshot_and_subscribe_into\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 5: `session.rs` — attache fenêtre + opérations GUI

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Test: `crates/wimux-server/src/session.rs` (module `tests`)

**Interfaces:**
- Consumes (Tasks 2/4) : `Window::{layout_tree, active_pane_id, pane_ids, split_pane, close_pane, set_ratio, set_active, pane}`, `Pane::snapshot_and_subscribe_into`, `wimux_protocol::LayoutNode`.
- Produces (utilisés par Task 6) :
  - `gui_attach_window(&self) -> Option<(LayoutNode, u64, Vec<(u64, Vec<u8>)>, Receiver<(PaneId, Vec<u8>)>, Sender<(PaneId, Vec<u8>)>)>`
  - `gui_split(&self, pane_id: u64, dir: SplitDir, tx: Sender<(PaneId, Vec<u8>)>) -> Option<(u64, Vec<u8>, LayoutNode, u64)>`
  - `gui_close(&self, pane_id: u64) -> Option<(LayoutNode, u64)>`
  - `gui_focus(&self, pane_id: u64) -> Option<(LayoutNode, u64)>`
  - `gui_set_ratio(&self, node_id: u32, ratio: f32) -> Option<(LayoutNode, u64)>`
  - `gui_pane_resize(&self, pane_id: u64, cols: u16, rows: u16)`
  - `window_layout(&self) -> Option<(LayoutNode, u64)>`

> **Note de conception (déviation signalée) :** la spec Task 5 décrivait un 4-tuple pour `gui_attach_window`. On renvoie EN PLUS le `Sender` (5-tuple) afin que le démon (Task 6) le stocke dans `GuiAttachment` et puisse abonner les volets créés au split au MÊME canal fusionné. Le canal est toujours créé dans `gui_attach_window` (propriété exigée par la spec).

- [ ] **Step 1: Écrire le test unitaire (échoue)**

Dans `crates/wimux-server/src/session.rs`, ajouter à la fin du fichier :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_layout_feuille_unique() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let (tree, active) = s.window_layout().unwrap();
        match tree {
            wimux_protocol::LayoutNode::Leaf { pane_id } => assert_eq!(pane_id, active),
            _ => panic!("attendu une feuille pour une session neuve"),
        }
        s.kill();
    }
}
```

- [ ] **Step 2: Lancer le test (attendu FAIL)**

Run: `cargo test -p wimux-server --lib session::tests`
Expected: FAIL — `no method window_layout`.

- [ ] **Step 3: Ajouter les imports**

Dans `crates/wimux-server/src/session.rs`, remplacer :

```rust
use crate::pane::{CopyAction, Notifier, Pane};
```

par :

```rust
use std::sync::mpsc::{Receiver, Sender};

use crate::pane::{CopyAction, Notifier, Pane, PaneId};
```

et ajouter à l'import protocole (ligne `use wimux_protocol::Frame;`) le type `LayoutNode` :

```rust
use wimux_protocol::{Frame, LayoutNode};
```

- [ ] **Step 4: Remplacer `gui_attach` par `gui_attach_window`**

Supprimer la méthode `gui_attach` (elle n'est plus utilisée après Task 6 — on la retire dès maintenant pour éviter le `dead_code`) et insérer, à sa place, les méthodes GUI G3 :

```rust
    /// Prépare l'attache GUI de la fenêtre active : crée UN canal fusionné, abonne
    /// chaque volet, renvoie la disposition, le volet actif, les snapshots par
    /// volet, le récepteur fusionné ET un `Sender` (pour abonner les futurs volets).
    pub fn gui_attach_window(
        &self,
    ) -> Option<(
        LayoutNode,
        u64,
        Vec<(u64, Vec<u8>)>,
        Receiver<(PaneId, Vec<u8>)>,
        Sender<(PaneId, Vec<u8>)>,
    )> {
        let inner = self.inner.lock().unwrap();
        let win = inner.windows.get(inner.active_window)?;
        let (tx, rx) = std::sync::mpsc::channel();
        let layout = win.layout_tree();
        let active = win.active_pane_id();
        let mut snaps = Vec::new();
        for id in win.pane_ids() {
            if let Some(pane) = win.pane(id) {
                let snap = pane.snapshot_and_subscribe_into(tx.clone());
                snaps.push((id, snap));
            }
        }
        Some((layout, active, snaps, rx, tx))
    }

    /// Découpe le volet désigné (mode GUI). Spawn hors verrou, puis abonne le
    /// nouveau volet au canal fusionné `tx`. Renvoie
    /// `(new_id, snapshot, layout, active)`.
    pub fn gui_split(
        &self,
        pane_id: u64,
        dir: SplitDir,
        tx: Sender<(PaneId, Vec<u8>)>,
    ) -> Option<(u64, Vec<u8>, LayoutNode, u64)> {
        let new_pane = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier)).ok()?;
        let new_id = new_pane.id;
        let (layout, active) = {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            inner.windows.get(aw)?;
            let area = content_area(inner.cols, inner.rows);
            inner.windows[aw].split_pane(pane_id, dir, Arc::clone(&new_pane));
            inner.windows[aw].reflow(area);
            (
                inner.windows[aw].layout_tree(),
                inner.windows[aw].active_pane_id(),
            )
        };
        let snapshot = new_pane.snapshot_and_subscribe_into(tx);
        self.notifier.bump();
        Some((new_id, snapshot, layout, active))
    }

    /// Ferme le volet désigné (mode GUI). Renvoie la nouvelle disposition, ou
    /// `None` si plus aucune fenêtre.
    pub fn gui_close(&self, pane_id: u64) -> Option<(LayoutNode, u64)> {
        {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            let empty = inner.windows.get_mut(aw).map(|w| w.close_pane(pane_id));
            if empty == Some(true) {
                inner.windows.remove(aw);
                if inner.active_window >= inner.windows.len() && !inner.windows.is_empty() {
                    inner.active_window = inner.windows.len() - 1;
                }
            }
        }
        self.reflow();
        self.notifier.bump();
        self.window_layout()
    }

    /// Désigne le volet actif (mode GUI).
    pub fn gui_focus(&self, pane_id: u64) -> Option<(LayoutNode, u64)> {
        {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            if let Some(win) = inner.windows.get_mut(aw) {
                win.set_active(pane_id);
            }
        }
        self.notifier.bump();
        self.window_layout()
    }

    /// Fixe le ratio d'un nœud de découpe (mode GUI, glisser-bordure).
    pub fn gui_set_ratio(&self, node_id: u32, ratio: f32) -> Option<(LayoutNode, u64)> {
        {
            let mut inner = self.inner.lock().unwrap();
            let area = content_area(inner.cols, inner.rows);
            let aw = inner.active_window;
            if let Some(win) = inner.windows.get_mut(aw) {
                win.set_ratio(node_id, ratio);
                win.reflow(area);
            }
        }
        self.notifier.bump();
        self.window_layout()
    }

    /// Redimensionne le PTY d'un volet désigné (mode GUI, `PaneResize` honoré).
    pub fn gui_pane_resize(&self, pane_id: u64, cols: u16, rows: u16) {
        let pane = {
            let inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            inner.windows.get(aw).and_then(|w| w.pane(pane_id))
        };
        if let Some(pane) = pane {
            pane.resize(cols, rows);
        }
    }

    /// Disposition courante de la fenêtre active.
    pub fn window_layout(&self) -> Option<(LayoutNode, u64)> {
        let inner = self.inner.lock().unwrap();
        let win = inner.windows.get(inner.active_window)?;
        Some((win.layout_tree(), win.active_pane_id()))
    }
```

- [ ] **Step 4b: Router l'entrée GUI par `pane_id` (et non vers le volet actif)**

En G3 chaque xterm porte son propre `pane_id` : `PaneInput` doit atteindre CE volet, pas le volet actif. Remplacer la méthode `gui_input` existante (reliquat G1 qui ignorait `pane_id`) par :

```rust
    /// Frappe GUI vers le volet DÉSIGNÉ de la fenêtre active (repli : volet actif
    /// si l'id est introuvable, ex. course avec une fermeture).
    pub fn gui_input(&self, pane_id: u64, bytes: &[u8]) {
        let pane = {
            let inner = self.inner.lock().unwrap();
            let win = inner.windows.get(inner.active_window);
            win.and_then(|w| w.pane(pane_id).or_else(|| Some(w.active_pane())))
        };
        if let Some(pane) = pane {
            pane.send_input(bytes);
        }
    }
```

(Le démon appelle déjà `s.gui_input(pane_id, &bytes)` dans le bras `PaneInput` — aucune modification du démon requise.)

- [ ] **Step 5: Lancer le test (attendu PASS)**

Run: `cargo test -p wimux-server --lib session -- --test-threads=1`
Expected: PASS (`window_layout_feuille_unique`).

> **Note :** `gui_input` (utilisé par le démon pour `PaneInput`) reste inchangé. Le démon référence encore `gui_attach` jusqu'à ce que Task 6 réécrive le bras `AttachGui` — c'est pourquoi cette Task 5 casse temporairement la compilation de `daemon.rs`. On ne compile donc PAS `-p wimux-server` complet ici ; seul le test `--lib` de `session` est exécuté (il compile la lib, dont `daemon.rs`). **Donc Task 5 et Task 6 doivent être réalisées d'un même tenant** : après avoir écrit le code de Task 5, enchaîner immédiatement Task 6 avant de recompiler la lib entière. Le test `--lib session::tests` de Step 5 échouera à la compilation tant que `daemon.rs` appelle l'ancien `gui_attach`. **Réordonnancement :** exécuter Step 1–4 de Task 5, puis Task 6 Steps 1–7, puis revenir valider Step 5–6 de Task 5.

- [ ] **Step 6: fmt + commit (après Task 6 compilée)**

```bash
git add crates/wimux-server/src/session.rs
git commit -m "$(printf 'feat(session): operations GUI de volets (attach/split/close/focus/ratio)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 6: `daemon.rs` — câblage GUI multi-volets

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/tests/gui_mode.rs` (+ `tests/common/mod.rs` déjà suffisant)

**Interfaces:**
- Consumes (Tasks 2/5) : `Session::{gui_attach_window, gui_split, gui_close, gui_focus, gui_set_ratio, gui_pane_resize}`, `From<protocol::SplitDir> for window::SplitDir`.
- Produces : les envois `ServerMessage::{WindowLayout, PaneSnapshot, PaneOutput}` selon le protocole G3.

- [ ] **Step 1: Écrire les tests d'intégration (échouent)**

Dans `crates/wimux-server/tests/gui_mode.rs`, changer l'import haut de fichier :

```rust
use wimux_protocol::{ClientMessage, LayoutNode, ServerMessage, SplitDir, send};
```

et ajouter `use std::sync::mpsc::Receiver;` sous les autres `use std::...`.

Ajouter, en fin de fichier, les helpers puis les 4 tests :

```rust
fn setup_attached(pipe: &str, name: &str) -> (Arc<PipeConn>, Receiver<ServerMessage>) {
    let owner = Arc::new(connect_retry(pipe));
    handshake(&owner);
    {
        let mut w: &PipeConn = &owner;
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
    let orx = spawn_reader(Arc::clone(&owner));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match orx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::Frame(_)) => break,
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de frame (shell non démarré)"),
        }
    }
    std::thread::sleep(Duration::from_millis(1000));
    // On laisse tomber `owner` : la session survit (détachée).
    let gui = Arc::new(connect_retry(pipe));
    handshake(&gui);
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::AttachGui { session: name.into() }).unwrap();
    }
    let grx = spawn_reader(Arc::clone(&gui));
    (gui, grx)
}

fn wait_layout(rx: &Receiver<ServerMessage>, secs: u64) -> (LayoutNode, u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowLayout { tree, active }) => return (tree, active),
            Ok(_) => {}
            Err(_) if Instant::now() < deadline => {}
            Err(_) => panic!("pas de WindowLayout"),
        }
    }
}

fn find_ratio(tree: &LayoutNode, node_id: u32) -> Option<f32> {
    match tree {
        LayoutNode::Leaf { .. } => None,
        LayoutNode::Split {
            node_id: nid,
            ratio,
            a,
            b,
            ..
        } => {
            if *nid == node_id {
                Some(*ratio)
            } else {
                find_ratio(a, node_id).or_else(|| find_ratio(b, node_id))
            }
        }
    }
}

fn wait_ratio_near(rx: &Receiver<ServerMessage>, node_id: u32, target: f32, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Ok(ServerMessage::WindowLayout { tree, .. }) =
            rx.recv_timeout(Duration::from_millis(200))
            && let Some(r) = find_ratio(&tree, node_id)
            && (r - target).abs() < 0.05
        {
            return true;
        }
    }
    false
}

#[test]
fn attach_gui_envoie_layout_et_snapshots() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3layout", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "L");

    let mut layout_pane: Option<u64> = None;
    let mut snap_pane: Option<u64> = None;
    let deadline = Instant::now() + Duration::from_secs(6);
    while (layout_pane.is_none() || snap_pane.is_none()) && Instant::now() < deadline {
        match grx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::WindowLayout { tree, active }) => match tree {
                LayoutNode::Leaf { pane_id } => {
                    assert_eq!(pane_id, active);
                    layout_pane = Some(pane_id);
                }
                _ => panic!("session neuve : arbre attendu = feuille"),
            },
            Ok(ServerMessage::PaneSnapshot { pane_id, .. }) => snap_pane = Some(pane_id),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(layout_pane.is_some(), "WindowLayout non reçu");
    assert_eq!(layout_pane, snap_pane, "layout et snapshot désignent des volets différents");

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "L".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn split_pane_ajoute_une_feuille() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3split", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "S");

    let (tree, active) = wait_layout(&grx, 6);
    let leaf = match tree {
        LayoutNode::Leaf { pane_id } => pane_id,
        _ => panic!("attendu une feuille"),
    };
    assert_eq!(leaf, active);

    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SplitPane {
                pane_id: leaf,
                dir: SplitDir::TopBottom,
            },
        )
        .unwrap();
    }

    let mut new_id: Option<u64> = None;
    let mut split_ok = false;
    let mut output_ok = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while (!split_ok || !output_ok) && Instant::now() < deadline {
        match grx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMessage::PaneSnapshot { pane_id, .. }) => {
                if pane_id != leaf {
                    new_id = Some(pane_id);
                }
            }
            Ok(ServerMessage::WindowLayout { tree, .. }) => {
                if let LayoutNode::Split { a, b, .. } = tree {
                    let mut ids = Vec::new();
                    if let LayoutNode::Leaf { pane_id } = *a {
                        ids.push(pane_id);
                    }
                    if let LayoutNode::Leaf { pane_id } = *b {
                        ids.push(pane_id);
                    }
                    if ids.contains(&leaf) && ids.iter().any(|&i| Some(i) == new_id) {
                        split_ok = true;
                    }
                }
            }
            Ok(ServerMessage::PaneOutput { pane_id, .. }) => {
                if Some(pane_id) == new_id {
                    output_ok = true;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(new_id.is_some(), "pas de snapshot du nouveau volet");
    assert!(split_ok, "le WindowLayout ne reflète pas le split");
    assert!(output_ok, "le nouveau volet ne diffuse pas de PaneOutput");

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "S".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn set_split_ratio_change_le_ratio() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3ratio", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "R");

    let (tree, _) = wait_layout(&grx, 6);
    let leaf = match tree {
        LayoutNode::Leaf { pane_id } => pane_id,
        _ => panic!("attendu une feuille"),
    };
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SplitPane {
                pane_id: leaf,
                dir: SplitDir::LeftRight,
            },
        )
        .unwrap();
    }
    // Récupérer le node_id du split.
    let node_id = {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    if let LayoutNode::Split { node_id, .. } = tree {
                        break node_id;
                    }
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de WindowLayout à un split"),
            }
        }
    };

    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::SetSplitRatio { node_id, ratio: 0.75 }).unwrap();
    }
    assert!(
        wait_ratio_near(&grx, node_id, 0.75, 8),
        "le ratio n'a pas été fixé à 0.75"
    );

    // Clamp : 5.0 -> 0.9.
    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::SetSplitRatio { node_id, ratio: 5.0 }).unwrap();
    }
    assert!(
        wait_ratio_near(&grx, node_id, 0.9, 8),
        "le ratio n'a pas été borné à 0.9"
    );

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "R".into() });
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn close_pane_retire_la_feuille() {
    let pipe = format!(r"\\.\pipe\wimux-test-{}-g3close", std::process::id());
    start_daemon(&pipe);
    let (gui, grx) = setup_attached(&pipe, "C");

    let (tree, _) = wait_layout(&grx, 6);
    let leaf = match tree {
        LayoutNode::Leaf { pane_id } => pane_id,
        _ => panic!("attendu une feuille"),
    };
    {
        let mut w: &PipeConn = &gui;
        send(
            &mut w,
            &ClientMessage::SplitPane {
                pane_id: leaf,
                dir: SplitDir::LeftRight,
            },
        )
        .unwrap();
    }
    // Capturer le nouvel id (snapshot != leaf).
    let new_id = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::PaneSnapshot { pane_id, .. }) if pane_id != leaf => break pane_id,
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => panic!("pas de snapshot du nouveau volet"),
            }
        }
    };

    {
        let mut w: &PipeConn = &gui;
        send(&mut w, &ClientMessage::ClosePane { pane_id: new_id }).unwrap();
    }
    // Attendre un WindowLayout redevenu une feuille == leaf.
    let back_to_leaf = {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match grx.recv_timeout(Duration::from_millis(200)) {
                Ok(ServerMessage::WindowLayout { tree, .. }) => {
                    if let LayoutNode::Leaf { pane_id } = tree {
                        break pane_id == leaf;
                    }
                }
                Ok(_) => {}
                Err(_) if Instant::now() < deadline => {}
                Err(_) => break false,
            }
        }
    };
    assert!(back_to_leaf, "après fermeture, l'arbre n'est pas redevenu la feuille restante");

    let mut w: &PipeConn = &gui;
    let _ = send(&mut w, &ClientMessage::Kill { name: "C".into() });
    std::thread::sleep(Duration::from_millis(200));
}
```

- [ ] **Step 2: Lancer les tests (attendu FAIL)**

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1 attach_gui_envoie_layout_et_snapshots`
Expected: FAIL — pas de `WindowLayout` reçu (le démon envoie encore l'ancien `PaneSnapshot` seul).

- [ ] **Step 3: Étendre `GuiAttachment`**

Dans `crates/wimux-server/src/daemon.rs`, remplacer la déclaration de `GuiAttachment` par :

```rust
struct GuiAttachment {
    keep_going: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    tx: std::sync::mpsc::Sender<(u64, Vec<u8>)>,
    session: Arc<Session>,
}
```

(Le `impl Drop for GuiAttachment` reste inchangé : il stoppe `keep_going` et joint `handle`.)

- [ ] **Step 4: Réécrire le bras `AttachGui`**

Remplacer entièrement le bras `ClientMessage::AttachGui { session } => { ... }` par :

```rust
            ClientMessage::AttachGui { session } => {
                // Arrêter proprement la diffusion précédente avant d'en démarrer une.
                gui_attach = None;
                gui_session = None;
                match server.get(&session) {
                    Some(s) => {
                        if let Some((tree, active, snaps, rx, tx)) = s.gui_attach_window() {
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                            for (pane_id, bytes) in snaps {
                                let mut wr: &PipeConn = &conn;
                                send(&mut wr, &ServerMessage::PaneSnapshot { pane_id, bytes })?;
                            }
                            let keep_going = Arc::new(AtomicBool::new(true));
                            let conn_out = Arc::clone(&conn);
                            let kg = Arc::clone(&keep_going);
                            let handle = std::thread::spawn(move || {
                                while kg.load(Ordering::Relaxed) {
                                    match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                                        Ok((pane_id, bytes)) => {
                                            let mut w: &PipeConn = &conn_out;
                                            if send(
                                                &mut w,
                                                &ServerMessage::PaneOutput { pane_id, bytes },
                                            )
                                            .is_err()
                                            {
                                                break;
                                            }
                                        }
                                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                                    }
                                }
                            });
                            gui_attach = Some(GuiAttachment {
                                keep_going,
                                handle: Some(handle),
                                tx,
                                session: Arc::clone(&s),
                            });
                            gui_session = Some(s);
                        } else {
                            let mut wr: &PipeConn = &conn;
                            send(
                                &mut wr,
                                &ServerMessage::Error(format!(
                                    "aucun volet dans la session : {session}"
                                )),
                            )?;
                        }
                    }
                    None => {
                        let mut wr: &PipeConn = &conn;
                        send(
                            &mut wr,
                            &ServerMessage::Error(format!("session introuvable : {session}")),
                        )?;
                    }
                }
            }
```

- [ ] **Step 5: Remplacer les 4 stubs + honorer `PaneResize`**

Remplacer les 4 bras stub (`SplitPane`/`ClosePane`/`FocusPane`/`SetSplitRatio`) par :

```rust
            ClientMessage::SplitPane { pane_id, dir } => {
                if let Some(ga) = &gui_attach
                    && let Some((new_id, snapshot, tree, active)) =
                        ga.session.gui_split(pane_id, dir.into(), ga.tx.clone())
                {
                    let mut wr: &PipeConn = &conn;
                    send(
                        &mut wr,
                        &ServerMessage::PaneSnapshot {
                            pane_id: new_id,
                            bytes: snapshot,
                        },
                    )?;
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
            ClientMessage::ClosePane { pane_id } => {
                if let Some(ga) = &gui_attach
                    && let Some((tree, active)) = ga.session.gui_close(pane_id)
                {
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
            ClientMessage::FocusPane { pane_id } => {
                if let Some(ga) = &gui_attach
                    && let Some((tree, active)) = ga.session.gui_focus(pane_id)
                {
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
            ClientMessage::SetSplitRatio { node_id, ratio } => {
                if let Some(ga) = &gui_attach
                    && let Some((tree, active)) = ga.session.gui_set_ratio(node_id, ratio)
                {
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
```

Remplacer le bras `ClientMessage::PaneResize { .. } => {}` par :

```rust
            ClientMessage::PaneResize { pane_id, cols, rows } => {
                if let Some(ga) = &gui_attach {
                    ga.session.gui_pane_resize(pane_id, cols, rows);
                }
            }
```

- [ ] **Step 6: Compiler la lib entière + lancer les tests G3 (attendu PASS)**

Run: `cargo build -p wimux-server`
Expected: OK (Task 5 + Task 6 cohérentes ; plus aucun appel à l'ancien `gui_attach`).

Run: `cargo test -p wimux-server --test gui_mode -- --test-threads=1`
Expected: PASS (dont `attach_gui_envoie_layout_et_snapshots`, `split_pane_ajoute_une_feuille`, `set_split_ratio_change_le_ratio`, `close_pane_retire_la_feuille`, et les tests G1/G2 existants).

- [ ] **Step 7: Non-régression complète + fmt + clippy**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS.

Run: `cargo fmt` puis `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`
Expected: OK.

- [ ] **Step 8: Commit (Task 5 + Task 6 ensemble)**

```bash
git add crates/wimux-server/src/session.rs crates/wimux-server/src/daemon.rs crates/wimux-server/tests/gui_mode.rs
git commit -m "$(printf 'feat(daemon): cablage GUI multi-volets (layout, split, close, focus, ratio)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 7: Pont Tauri — commandes de volet + event `window-layout`

**Files:**
- Modify: `wimux-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes (Tasks 1/6) : `ClientMessage::{SplitPane, ClosePane, FocusPane, SetSplitRatio, PaneResize}`, `wimux_protocol::SplitDir`, `ServerMessage::WindowLayout`.
- Produces (utilisés par Task 8) : commandes Tauri `split_pane`/`close_pane`/`focus_pane`/`set_split_ratio`/`pane_resize` ; event `window-layout` de payload `(LayoutNode, u64)` (LayoutNode sérialisé serde externally-tagged).

- [ ] **Step 1: Importer `SplitDir`**

Dans `wimux-gui/src-tauri/src/lib.rs`, remplacer l'import protocole par :

```rust
use wimux_protocol::{
    ClientMessage, Hello, HelloReply, PROTOCOL_VERSION, ServerMessage, SplitDir, recv, send,
};
```

- [ ] **Step 2: Émettre `window-layout` dans le thread lecteur**

Dans `attach_session`, dans le `match msg` du thread lecteur, ajouter un bras (après `PaneOutput`) :

```rust
                        ServerMessage::WindowLayout { tree, active } => {
                            let _ = app2.emit("window-layout", (tree, active));
                        }
```

- [ ] **Step 3: Ajouter les commandes de volet**

Après la commande `pane_input`, ajouter :

```rust
#[tauri::command]
fn split_pane(pane_id: u64, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::SplitPane { pane_id, dir }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::ClosePane { pane_id }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn focus_pane(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::FocusPane { pane_id }).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn set_split_ratio(node_id: u32, ratio: f32, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::SetSplitRatio { node_id, ratio })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn pane_resize(pane_id: u64, cols: u16, rows: u16, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(&mut w, &ClientMessage::PaneResize { pane_id, cols, rows })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
```

- [ ] **Step 4: Enregistrer les commandes**

Dans `run()`, remplacer le `generate_handler!` par :

```rust
        .invoke_handler(tauri::generate_handler![
            attach_session,
            pane_input,
            pane_resize,
            split_pane,
            close_pane,
            focus_pane,
            set_split_ratio,
            list_sessions,
            create_session,
            kill_session,
            rename_session
        ])
```

- [ ] **Step 5: Build (attendu OK)**

Run: `cd wimux-gui/src-tauri && cargo build`
Expected: OK.

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/src-tauri/src/lib.rs
git commit -m "$(printf 'feat(gui-bridge): commandes de volet + event window-layout (G3)\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 8: Moteur d'arbre frontend (`panes.ts`) + branchement

**Files:**
- Create: `wimux-gui/src/panes.ts`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`
- (Pas de test auto : cycle = code → `npm run build` → vérif manuelle.)

**Interfaces:**
- Consumes (Task 7) : events `window-layout` `[LayoutNode, number]`, `pane-snapshot`/`pane-output` `[number, number[]]` ; commandes `pane_input`/`pane_resize`.
- Produces (utilisés par Tasks 9/10) : `export type LayoutNode`, `export interface PaneCallbacks`, `export class PaneManager` avec `renderLayout(tree, active)`, `write(paneId, data)`, `reset()`.

- [ ] **Step 1: Créer `wimux-gui/src/panes.ts`**

```ts
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

/// Arbre de disposition, miroir du `LayoutNode` serveur (serde externally-tagged).
export type LayoutNode =
  | { Leaf: { pane_id: number } }
  | {
      Split: {
        node_id: number;
        dir: "LeftRight" | "TopBottom";
        ratio: number;
        a: LayoutNode;
        b: LayoutNode;
      };
    };

export interface PaneCallbacks {
  onInput: (paneId: number, bytes: number[]) => void;
  onResize: (paneId: number, cols: number, rows: number) => void;
}

interface PaneView {
  term: Terminal;
  fit: FitAddon;
  el: HTMLElement;
  observer: ResizeObserver;
}

export class PaneManager {
  private views = new Map<number, PaneView>();
  private mount: HTMLElement;
  private cb: PaneCallbacks;

  constructor(mount: HTMLElement, cb: PaneCallbacks) {
    this.mount = mount;
    this.cb = cb;
  }

  write(paneId: number, data: Uint8Array) {
    const v = this.views.get(paneId);
    if (v) v.term.write(data);
  }

  reset() {
    for (const v of this.views.values()) {
      v.observer.disconnect();
      v.term.dispose();
    }
    this.views.clear();
    this.mount.replaceChildren();
  }

  renderLayout(tree: LayoutNode, active: number) {
    const wanted = new Set<number>();
    this.collectIds(tree, wanted);
    // Disposer les volets disparus.
    for (const [id, v] of this.views) {
      if (!wanted.has(id)) {
        v.observer.disconnect();
        v.term.dispose();
        this.views.delete(id);
      }
    }
    // Reconstruire l'arbre DOM en RÉUTILISANT les wrappers existants.
    const root = this.buildNode(tree);
    this.mount.replaceChildren(root);
    // Marquer le volet actif + réajuster les tailles après reparentage.
    for (const [id, v] of this.views) {
      v.el.classList.toggle("active", id === active);
      try {
        v.fit.fit();
        this.cb.onResize(id, v.term.cols, v.term.rows);
      } catch {
        /* conteneur non mesurable (détaché) : ignoré */
      }
    }
  }

  private collectIds(tree: LayoutNode, into: Set<number>) {
    if ("Leaf" in tree) into.add(tree.Leaf.pane_id);
    else {
      this.collectIds(tree.Split.a, into);
      this.collectIds(tree.Split.b, into);
    }
  }

  private ensureView(paneId: number): PaneView {
    const existing = this.views.get(paneId);
    if (existing) return existing;
    const el = document.createElement("div");
    el.className = "pane";
    el.dataset.paneId = String(paneId);
    const term = new Terminal({
      fontFamily: "Cascadia Mono, Consolas, monospace",
      fontSize: 14,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    term.onData((data) => {
      const bytes = Array.from(new TextEncoder().encode(data));
      this.cb.onInput(paneId, bytes);
    });
    const observer = new ResizeObserver(() => {
      try {
        fit.fit();
        this.cb.onResize(paneId, term.cols, term.rows);
      } catch {
        /* non mesurable : ignoré */
      }
    });
    observer.observe(el);
    const view: PaneView = { term, fit, el, observer };
    this.views.set(paneId, view);
    return view;
  }

  private buildNode(tree: LayoutNode): HTMLElement {
    if ("Leaf" in tree) {
      return this.ensureView(tree.Leaf.pane_id).el;
    }
    const s = tree.Split;
    const container = document.createElement("div");
    container.className = "split " + (s.dir === "LeftRight" ? "split-row" : "split-col");
    const a = document.createElement("div");
    a.className = "split-child";
    a.style.flexGrow = String(s.ratio);
    a.appendChild(this.buildNode(s.a));
    const sep = document.createElement("div");
    sep.className = "separator " + (s.dir === "LeftRight" ? "sep-v" : "sep-h");
    sep.dataset.nodeId = String(s.node_id);
    const b = document.createElement("div");
    b.className = "split-child";
    b.style.flexGrow = String(1 - s.ratio);
    b.appendChild(this.buildNode(s.b));
    container.append(a, sep, b);
    return container;
  }
}
```

- [ ] **Step 2: Réécrire `wimux-gui/src/main.ts`**

Remplacer les lignes 1–35 (imports xterm/term jusqu'à la fin de `switchTo`) par :

```ts
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { PaneManager, type LayoutNode } from "./panes";

const mount = document.getElementById("terminal")!;
const paneManager = new PaneManager(mount, {
  onInput: (paneId, bytes) => {
    invoke("pane_input", { paneId, bytes }).catch(() => {});
  },
  onResize: (paneId, cols, rows) => {
    invoke("pane_resize", { paneId, cols, rows }).catch(() => {});
  },
});

let activeSession: string | null = null;

// Disposition + flux serveur -> volets.
listen<[LayoutNode, number]>("window-layout", (e) => {
  paneManager.renderLayout(e.payload[0], e.payload[1]);
});
listen<[number, number[]]>("pane-snapshot", (e) => {
  paneManager.write(e.payload[0], new Uint8Array(e.payload[1]));
});
listen<[number, number[]]>("pane-output", (e) => {
  paneManager.write(e.payload[0], new Uint8Array(e.payload[1]));
});
listen<string>("pane-error", (e) => {
  console.error("erreur serveur:", e.payload);
});

type SessionDto = { name: string; attached: boolean };

async function switchTo(name: string) {
  if (name === activeSession) return;
  activeSession = name;
  paneManager.reset();
  await invoke("attach_session", { session: name }).catch((e) =>
    console.error("attach:", e),
  );
  renderRail(lastSessions);
}
```

Le reste du fichier (`lastSessions`, `renderRail`, `startRename`, `refresh`, bouton `new-session`, sondage) demeure **inchangé** ; supprimer uniquement les anciennes lignes devenues mortes (`const term = ...`, `fit`, `term.open`, `window.addEventListener("resize", ...)`, `let activePane = 0;`, les 3 `listen` d'origine et le `term.onData`, remplacés ci-dessus).

- [ ] **Step 3: Styles des volets et séparateurs**

Dans `wimux-gui/src/styles.css`, remplacer la règle `#terminal { flex: 1; }` par :

```css
#terminal { flex: 1; display: flex; overflow: hidden; min-width: 0; min-height: 0; }
#terminal > * { flex: 1; display: flex; min-width: 0; min-height: 0; }
.split { display: flex; flex: 1; min-width: 0; min-height: 0; }
.split-row { flex-direction: row; }
.split-col { flex-direction: column; }
.split-child { display: flex; min-width: 0; min-height: 0; overflow: hidden; }
.pane { position: relative; flex: 1; min-width: 0; min-height: 0; overflow: hidden; }
.pane.active { outline: 1px solid #0a84ff; outline-offset: -1px; }
.separator { background: #333; flex: 0 0 4px; }
.separator:hover { background: #0a84ff; }
.sep-v { width: 4px; cursor: col-resize; }
.sep-h { height: 4px; cursor: row-resize; }
```

- [ ] **Step 4: Build (attendu OK)**

Run: `cd wimux-gui && npm run build`
Expected: OK (tsc sans erreur, vite build produit `dist/`).

- [ ] **Step 5: Documenter la vérif manuelle G3b dans le README**

Dans `wimux-gui/README.md`, ajouter une section :

```markdown
## Vérification manuelle des volets (G3b — rendu)

1. Construire le workspace et lancer un serveur avec une session découpée en TUI :
   ```bash
   cargo build --release
   target/release/wimux.exe new -s dev
   ```
   Dans la fenêtre TUI, découper : `Ctrl-b %` (gauche/droite) puis `Ctrl-b "`
   (haut/bas), puis se détacher `Ctrl-b d`.
2. Lancer la GUI : `cd wimux-gui && npm run tauri dev`.
3. **Attendu :** la session `dev` s'affiche avec UN xterm par volet, disposés
   selon l'arbre (proportions = ratios), **en couleur** dès l'attache, curseur au
   bon endroit. Taper dans chaque volet route l'entrée vers ce volet (chaque
   xterm porte son `pane_id`). Redimensionner la fenêtre reflow les volets.
```

- [ ] **Step 6: Commit**

```bash
git add wimux-gui/src/panes.ts wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "$(printf 'feat(gui): PaneManager — un xterm par volet, arbre flex depuis le layout\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 9: Barre d'actions au survol + clic-focus

**Files:**
- Modify: `wimux-gui/src/panes.ts`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/src/styles.css`
- Modify: `wimux-gui/README.md`

**Interfaces:**
- Consumes (Task 7) : commandes `split_pane`/`close_pane`/`focus_pane`.
- Produces : `PaneCallbacks` étendu avec `onFocus`, `onSplit`, `onClose`.

- [ ] **Step 1: Étendre `PaneCallbacks`**

Dans `wimux-gui/src/panes.ts`, remplacer l'interface `PaneCallbacks` par :

```ts
export interface PaneCallbacks {
  onInput: (paneId: number, bytes: number[]) => void;
  onResize: (paneId: number, cols: number, rows: number) => void;
  onFocus: (paneId: number) => void;
  onSplit: (paneId: number, dir: "LeftRight" | "TopBottom") => void;
  onClose: (paneId: number) => void;
}
```

- [ ] **Step 2: Ajouter la barre + le clic-focus dans `ensureView`**

Dans `ensureView`, juste après `term.open(el);`, insérer :

```ts
    const bar = document.createElement("div");
    bar.className = "pane-bar";
    const bSplitV = document.createElement("button");
    bSplitV.textContent = "⬍";
    bSplitV.title = "Découper haut/bas";
    bSplitV.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onSplit(paneId, "TopBottom");
    };
    const bSplitH = document.createElement("button");
    bSplitH.textContent = "⬌";
    bSplitH.title = "Découper gauche/droite";
    bSplitH.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onSplit(paneId, "LeftRight");
    };
    const bClose = document.createElement("button");
    bClose.textContent = "✕";
    bClose.title = "Fermer le volet";
    bClose.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onClose(paneId);
    };
    bar.append(bSplitV, bSplitH, bClose);
    el.appendChild(bar);
    el.addEventListener("mousedown", () => {
      this.cb.onFocus(paneId);
      term.focus();
    });
```

- [ ] **Step 3: Câbler les callbacks dans `main.ts`**

Dans `wimux-gui/src/main.ts`, remplacer l'objet passé à `new PaneManager(...)` par :

```ts
const paneManager = new PaneManager(mount, {
  onInput: (paneId, bytes) => {
    invoke("pane_input", { paneId, bytes }).catch(() => {});
  },
  onResize: (paneId, cols, rows) => {
    invoke("pane_resize", { paneId, cols, rows }).catch(() => {});
  },
  onFocus: (paneId) => {
    invoke("focus_pane", { paneId }).catch(() => {});
  },
  onSplit: (paneId, dir) => {
    invoke("split_pane", { paneId, dir }).catch(() => {});
  },
  onClose: (paneId) => {
    invoke("close_pane", { paneId }).catch(() => {});
  },
});
```

- [ ] **Step 4: Styles de la barre**

Dans `wimux-gui/src/styles.css`, ajouter :

```css
.pane-bar { position: absolute; top: 2px; right: 2px; z-index: 5; display: none; gap: 2px; }
.pane:hover .pane-bar { display: flex; }
.pane-bar button {
  border: none; background: #2d2d2dcc; color: #ddd; cursor: pointer;
  width: 22px; height: 20px; font-size: 12px; line-height: 1; border-radius: 3px;
}
.pane-bar button:hover { background: #0a84ff; color: #fff; }
```

- [ ] **Step 5: Build (attendu OK)**

Run: `cd wimux-gui && npm run build`
Expected: OK.

- [ ] **Step 6: Vérif manuelle G3c (1) dans le README**

Dans `wimux-gui/README.md`, ajouter :

```markdown
## Vérification manuelle des volets (G3c — opérations)

1. GUI lancée sur une session (`npm run tauri dev`).
2. Survoler un volet : une barre apparaît en haut à droite (⬍ ⬌ ✕).
3. **Attendu :**
   - ⬍ découpe le volet en haut/bas, ⬌ en gauche/droite (nouveau volet créé,
     shell démarré, snapshot coloré) ;
   - ✕ ferme le volet ; l'espace est repris par le volet frère ;
   - cliquer dans un volet le focalise (bordure bleue `.pane.active`) et la
     frappe va à ce volet.
```

- [ ] **Step 7: Commit**

```bash
git add wimux-gui/src/panes.ts wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/README.md
git commit -m "$(printf 'feat(gui): barre decouper/fermer au survol + clic-focus\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Task 10: Glisser les bordures (`SetSplitRatio`)

**Files:**
- Modify: `wimux-gui/src/panes.ts`
- Modify: `wimux-gui/src/main.ts`
- Modify: `wimux-gui/README.md`

**Interfaces:**
- Consumes (Task 7) : commande `set_split_ratio`.
- Produces : `PaneCallbacks` étendu avec `onRatio` ; séparateurs draggables (mise à jour optimiste + `set_split_ratio` throttlé ~50 ms).

- [ ] **Step 1: Étendre `PaneCallbacks` + état de throttle**

Dans `wimux-gui/src/panes.ts`, ajouter à `PaneCallbacks` :

```ts
  onRatio: (nodeId: number, ratio: number) => void;
```

Dans la classe `PaneManager`, ajouter les champs privés (sous `private cb: PaneCallbacks;`) :

```ts
  private ratioTimer: number | null = null;
  private pendingRatio: { nodeId: number; ratio: number } | null = null;
```

et la méthode privée de throttle :

```ts
  private emitRatio(nodeId: number, ratio: number) {
    this.pendingRatio = { nodeId, ratio };
    if (this.ratioTimer !== null) return;
    this.ratioTimer = window.setTimeout(() => {
      this.ratioTimer = null;
      if (this.pendingRatio) {
        this.cb.onRatio(this.pendingRatio.nodeId, this.pendingRatio.ratio);
        this.pendingRatio = null;
      }
    }, 50);
  }
```

- [ ] **Step 2: Rendre les séparateurs draggables dans `buildNode`**

Dans `buildNode`, juste avant `container.append(a, sep, b);`, insérer :

```ts
    sep.addEventListener("mousedown", (ev) => {
      ev.preventDefault();
      const isRow = s.dir === "LeftRight";
      const onMove = (m: MouseEvent) => {
        const rect = container.getBoundingClientRect();
        let ratio = isRow
          ? (m.clientX - rect.left) / rect.width
          : (m.clientY - rect.top) / rect.height;
        ratio = Math.max(0.1, Math.min(0.9, ratio));
        // Mise à jour optimiste locale ; le serveur ré-émettra window-layout.
        a.style.flexGrow = String(ratio);
        b.style.flexGrow = String(1 - ratio);
        this.emitRatio(s.node_id, ratio);
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    });
```

- [ ] **Step 3: Câbler `onRatio` dans `main.ts`**

Dans l'objet passé à `new PaneManager(...)`, ajouter la propriété :

```ts
  onRatio: (nodeId, ratio) => {
    invoke("set_split_ratio", { nodeId, ratio }).catch(() => {});
  },
```

- [ ] **Step 4: Build (attendu OK)**

Run: `cd wimux-gui && npm run build`
Expected: OK.

- [ ] **Step 5: Compléter la section « Vérification manuelle G3 » du README**

Dans `wimux-gui/README.md`, ajouter :

```markdown
## Vérification manuelle G3 (récapitulatif complet)

Avec la GUI attachée à une session découpée :
- **Couleurs à l'attache** : le contenu coloré (ex. `ls` colorisé, prompt) apparaît
  en couleur immédiatement, curseur au bon endroit.
- **Découper** : ⬍ (haut/bas) et ⬌ (gauche/droite) créent des volets vivants.
- **Fermer** : ✕ retire le volet, le frère reprend la place.
- **Focus** : clic → bordure bleue, la frappe suit le volet cliqué.
- **Taper dans chaque volet** : chaque volet exécute indépendamment (`whoami`, etc.).
- **Glisser les bordures** : tirer un séparateur redimensionne en direct (borné
  10 %–90 %) ; relâcher fixe le ratio côté serveur (le TUI attaché voit le même
  ratio).
```

- [ ] **Step 6: Non-régression build final + commit**

Run: `cd wimux-gui && npm run build`
Expected: OK.

```bash
git add wimux-gui/src/panes.ts wimux-gui/src/main.ts wimux-gui/README.md
git commit -m "$(printf 'feat(gui): glisser les bordures -> set_split_ratio throttle\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>')"
```

---

## Self-Review

**Spec coverage :**
- Protocole (`SplitDir`/`LayoutNode`/4 commandes/`WindowLayout`) → Task 1. `node_id` stable → Task 2. Snapshot fidèle (`grid_to_ansi`) → Task 3. Canal fusionné multi-volets + abonnés tagués → Tasks 4–6. Attache/split/close/focus/ratio serveur → Tasks 5–6. `PaneResize` honoré → Tasks 6/7. Pont Tauri + `window-layout` → Task 7. Moteur d'arbre + un xterm/volet + routage par `pane_id` → Task 8. Barre + clic-focus → Task 9. Glisser-bordure → Task 10. Vérifs manuelles README → Tasks 8/9/10. Jalons G3a (T1–6), G3b (T8), G3c (T9–10) couverts.

**Type consistency :** `gui_split(pane_id, window::SplitDir, Sender<(PaneId,Vec<u8>)>) -> (u64, Vec<u8>, LayoutNode, u64)` consommé tel quel en Task 6 ; `gui_attach_window` renvoie le 5-tuple (avec `Sender`) consommé en Task 6 ; `LayoutNode` serde externally-tagged ↔ type TS `{ Leaf: {...} } | { Split: {...} }`.

**Points signalés pour relecture (choix non spécifiés) :**
1. `gui_attach_window` renvoie un **5-tuple** (ajout du `Sender`) au lieu du 4-tuple de la spec, pour que `GuiAttachment` détienne le `tx` du canal fusionné (exigé par Task 6). Le canal reste créé dans `gui_attach_window`.
2. **Ordonnancement Task 5 ↔ Task 6** : retirer `Session::gui_attach` (Task 5) casse la compilation de `daemon.rs` tant que Task 6 n'a pas réécrit le bras `AttachGui`. Le plan impose donc de réaliser Task 5 (code) puis Task 6 avant de recompiler la lib, et commit conjoint. À valider en relecture.
3. `Window::split`/`close_active` refactorés en **wrappers** de `split_pane`/`close_pane` (préserve le comportement TUI, DRY).
4. Shell des tests unitaires `window.rs`/`session.rs` = **`"cmd.exe"`** (léger ; non spécifié par la spec).
5. `renderLayout(tree, active)` applique la classe `.active` **dès Task 8** (la spec la plaçait en Task 9) pour garder la signature stable ; Task 9 n'ajoute que la bordure CSS + interactions.
6. Émission de `window-layout` via `app.emit("window-layout", (tree, active))` en s'appuyant sur `Serialize` de `LayoutNode` (pas de DTO Rust séparé — option explicitement permise par la spec).
