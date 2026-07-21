//! Une fenêtre = un arbre binaire de découpes dont les feuilles sont des volets.
//! La fenêtre calcule la disposition (rectangles + bordures) dans une zone
//! donnée, redimensionne ses volets en conséquence, et se compose dans une
//! grille pour l'affichage.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use wimux_vt::{Cell, Color, Grid, Pen};

use crate::pane::{Pane, PaneId};
use crate::webpane::WebPane;

/// Attribue un identifiant stable à chaque nœud de découpe (par processus).
static NEXT_NODE_ID: AtomicU32 = AtomicU32::new(1);

/// Contenu d'une feuille de disposition (B1) : un terminal, ou un navigateur.
/// C'est une ÉNUMÉRATION (et non une table parallèle) pour que le compilateur
/// recense tous les sites d'appel lors de l'ajout d'une nature.
pub enum PaneSlot {
    Term(Arc<Pane>),
    Web(Arc<WebPane>),
}

impl PaneSlot {
    /// Le volet terminal, ou `None` si cette feuille est un navigateur.
    fn term(&self) -> Option<Arc<Pane>> {
        match self {
            PaneSlot::Term(p) => Some(Arc::clone(p)),
            PaneSlot::Web(_) => None,
        }
    }
}

/// Sens d'une découpe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Volets côte à côte (séparés par une ligne verticale) — `Ctrl-b %`.
    LeftRight,
    /// Volets empilés (séparés par une ligne horizontale) — `Ctrl-b "`.
    TopBottom,
}

/// Direction de navigation entre volets.
#[derive(Debug, Clone, Copy)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

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

struct Border {
    vertical: bool,
    x: u16,
    y: u16,
    len: u16,
}

pub struct Window {
    name: Option<String>,
    root: Node,
    panes: HashMap<PaneId, PaneSlot>,
    active: PaneId,
    rects: HashMap<PaneId, Rect>,
    borders: Vec<Border>,
    /// Volet actif affiché en plein écran (`Ctrl-b z`).
    zoomed: bool,
}

impl Window {
    pub fn new(pane: Arc<Pane>) -> Window {
        let id = pane.id;
        let mut panes = HashMap::new();
        panes.insert(id, PaneSlot::Term(pane));
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

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Bascule le zoom du volet actif (sans effet s'il n'y a qu'un volet).
    pub fn toggle_zoom(&mut self) {
        if self.panes.len() > 1 || self.zoomed {
            self.zoomed = !self.zoomed;
        }
    }

    /// Redimensionne le volet actif dans la direction donnée.
    pub fn resize_active(&mut self, mv: Move) {
        let step = 0.05;
        let (horizontal, grow) = match mv {
            Move::Right => (true, step),
            Move::Left => (true, -step),
            Move::Down => (false, step),
            Move::Up => (false, -step),
        };
        resize_walk(&mut self.root, self.active, horizontal, grow);
    }

    /// Volet TERMINAL actif, ou `None` si le volet actif est un navigateur.
    pub fn active_term_pane(&self) -> Option<Arc<Pane>> {
        self.panes.get(&self.active).and_then(|s| s.term())
    }

    /// Volet situé à la position `(col, row)` (coordonnées de contenu, 0-based).
    pub fn pane_at(&self, col: u16, row: u16) -> Option<PaneId> {
        self.rects
            .iter()
            .find(|(_, r)| col >= r.x && col < r.x + r.w && row >= r.y && row < r.y + r.h)
            .map(|(&id, _)| id)
    }

    /// Volet TERMINAL d'identifiant `id` (`None` si absent ou si c'est un
    /// navigateur) — sémantique volontairement inchangée pour les appelants.
    pub fn pane(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.get(&id).and_then(|s| s.term())
    }

    /// Volet NAVIGATEUR d'identifiant `id`, s'il en est un.
    pub fn web_pane(&self, id: PaneId) -> Option<Arc<WebPane>> {
        match self.panes.get(&id) {
            Some(PaneSlot::Web(w)) => Some(Arc::clone(w)),
            _ => None,
        }
    }

    /// La fenêtre contient-elle cette feuille, quelle que soit sa nature ?
    pub fn contains(&self, id: PaneId) -> bool {
        self.panes.contains_key(&id)
    }

    pub fn set_active(&mut self, id: PaneId) {
        if self.panes.contains_key(&id) {
            self.active = id;
        }
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Traduit l'arbre interne en `LayoutNode` sérialisable pour la GUI.
    pub fn layout_tree(&self) -> wimux_protocol::LayoutNode {
        self.node_to_layout(&self.root)
    }

    /// Traduit un `Node` interne, en renseignant la NATURE de chaque feuille.
    fn node_to_layout(&self, node: &Node) -> wimux_protocol::LayoutNode {
        match node {
            Node::Leaf(id) => wimux_protocol::LayoutNode::Leaf {
                pane_id: *id,
                kind: match self.panes.get(id) {
                    Some(PaneSlot::Web(w)) => wimux_protocol::PaneKind::Web { url: w.url() },
                    _ => wimux_protocol::PaneKind::Terminal,
                },
            },
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
                a: Box::new(self.node_to_layout(a)),
                b: Box::new(self.node_to_layout(b)),
            },
        }
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

    /// Termine tous les volets de la fenêtre (pour `kill-session`).
    /// Un volet navigateur n'a pas de processus : rien à tuer.
    pub fn kill_all(&self) {
        for slot in self.panes.values() {
            if let PaneSlot::Term(p) = slot {
                p.kill();
            }
        }
    }

    /// Description des volets (pour `list-panes`).
    pub fn pane_list(&self) -> Vec<String> {
        let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
        ids.sort_unstable();
        ids.iter()
            .map(|id| {
                let active = if *id == self.active { " (actif)" } else { "" };
                match &self.panes[id] {
                    PaneSlot::Term(p) => {
                        let (c, r) = p.size();
                        format!("volet {id}: {c}x{r}{active}")
                    }
                    PaneSlot::Web(w) => format!("volet {id}: navigateur {}{active}", w.url()),
                }
            })
            .collect()
    }

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
        self.panes.insert(new_id, PaneSlot::Term(new_pane));
        self.active = new_id;
    }

    /// Découpe le volet `target` en y insérant un volet NAVIGATEUR, qui devient
    /// actif (même mécanique que `split_pane`, autre nature de feuille).
    pub fn split_web(&mut self, target: PaneId, dir: SplitDir, web: Arc<WebPane>) {
        self.zoomed = false;
        let new_id = web.id;
        let node_id = NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed);
        Self::replace_leaf(&mut self.root, target, |old| Node::Split {
            node_id,
            dir,
            ratio: 0.5,
            a: Box::new(old),
            b: Box::new(Node::Leaf(new_id)),
        });
        self.panes.insert(new_id, PaneSlot::Web(web));
        self.active = new_id;
    }

    /// Ferme le volet actif. Renvoie `true` si la fenêtre est désormais vide.
    pub fn close_active(&mut self) -> bool {
        let active = self.active;
        self.close_pane(active)
    }

    /// Ferme le volet DÉSIGNÉ `target`. Renvoie `true` si la fenêtre est vide.
    pub fn close_pane(&mut self, target: PaneId) -> bool {
        self.zoomed = false;
        if let Some(PaneSlot::Term(p)) = self.panes.remove(&target) {
            p.kill();
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

    /// Retire les volets dont le shell est mort. Renvoie `true` si vide.
    ///
    /// A1 : les volets JOURNALISÉS (agents, `log_path().is_some()`) sont exemptés
    /// même morts — ils doivent rester lisibles (`list`/`capture`/`exit_code`)
    /// jusqu'à un `kill_pane` explicite, comme le fait déjà `Session::reap` pour
    /// une session agent entière (M1).
    pub fn reap_dead(&mut self) -> bool {
        let dead: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, s)| match s {
                // Un volet navigateur n'a pas de processus : il ne meurt jamais.
                PaneSlot::Web(_) => false,
                PaneSlot::Term(p) => !p.is_alive() && p.log_path().is_none(),
            })
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.panes.remove(&id);
            Self::remove_leaf(&mut self.root, id);
            if self.active == id {
                self.active = self.panes.keys().next().copied().unwrap_or(0);
            }
        }
        self.panes.is_empty()
    }

    /// Passe au volet suivant (ordre trié des identifiants).
    pub fn next_pane(&mut self) {
        let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
        ids.sort_unstable();
        if let Some(pos) = ids.iter().position(|&id| id == self.active) {
            self.active = ids[(pos + 1) % ids.len()];
        }
    }

    /// Sélectionne le volet adjacent dans la direction donnée (via les rects).
    pub fn select(&mut self, mv: Move) {
        let Some(cur) = self.rects.get(&self.active).copied() else {
            return;
        };
        let (cx, cy) = (cur.x + cur.w / 2, cur.y + cur.h / 2);
        let mut best: Option<(PaneId, i32)> = None;
        for (&id, r) in &self.rects {
            if id == self.active {
                continue;
            }
            let ok = match mv {
                Move::Left => r.x + r.w <= cur.x && overlaps_v(r, &cur),
                Move::Right => r.x >= cur.x + cur.w && overlaps_v(r, &cur),
                Move::Up => r.y + r.h <= cur.y && overlaps_h(r, &cur),
                Move::Down => r.y >= cur.y + cur.h && overlaps_h(r, &cur),
            };
            if !ok {
                continue;
            }
            let d = (r.x as i32 + (r.w / 2) as i32 - cx as i32).abs()
                + (r.y as i32 + (r.h / 2) as i32 - cy as i32).abs();
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((id, d));
            }
        }
        if let Some((id, _)) = best {
            self.active = id;
        }
    }

    /// Recalcule la disposition dans `area` et redimensionne chaque volet à son
    /// rectangle. À appeler dès que le layout ou la taille de la vue change.
    pub fn reflow(&mut self, area: Rect) {
        self.rects.clear();
        self.borders.clear();

        // Zoom : seul le volet actif est visible, à pleine taille.
        if self.zoomed && self.panes.contains_key(&self.active) {
            if let Some(PaneSlot::Term(pane)) = self.panes.get(&self.active) {
                pane.resize(area.w, area.h);
            }
            self.rects.insert(self.active, area);
            return;
        }

        let mut rects = HashMap::new();
        let mut borders = Vec::new();
        layout(&self.root, area, &mut rects, &mut borders);
        for (&id, r) in &rects {
            if let Some(PaneSlot::Term(pane)) = self.panes.get(&id) {
                pane.resize(r.w, r.h);
            }
        }
        self.rects = rects;
        self.borders = borders;
    }

    /// Compose la fenêtre dans `into`. Renvoie la position absolue du curseur du
    /// volet actif.
    pub fn render(&self, into: &mut Grid) -> (u16, u16) {
        let border_pen = Pen {
            fg: Color::Indexed(8),
            ..Pen::default()
        };
        // Volets.
        let mut cursor = (0, 0);
        for (&id, r) in &self.rects {
            match self.panes.get(&id) {
                Some(PaneSlot::Term(pane)) => {
                    let (grid, (cc, cr)) = pane.snapshot();
                    into.blit(&grid, r.x, r.y);
                    if id == self.active {
                        cursor = (
                            r.x + cc.min(r.w.saturating_sub(1)),
                            r.y + cr.min(r.h.saturating_sub(1)),
                        );
                    }
                }
                // Un client TEXTE ne peut pas afficher une page : on dessine un
                // substitut lisible plutôt que de laisser un trou.
                Some(PaneSlot::Web(web)) => draw_web_placeholder(into, *r, &web.url()),
                None => {}
            }
        }
        // Bordures.
        for b in &self.borders {
            if b.vertical {
                for i in 0..b.len {
                    into.set(b.x, b.y + i, Cell::new('│', border_pen, 1));
                }
            } else {
                for i in 0..b.len {
                    into.set(b.x + i, b.y, Cell::new('─', border_pen, 1));
                }
            }
        }
        cursor
    }

    // --- utilitaires d'arbre -------------------------------------------------

    /// Remplace la feuille `target` par le résultat de `f(ancienne_feuille)`.
    fn replace_leaf(node: &mut Node, target: PaneId, f: impl FnOnce(Node) -> Node) {
        match node {
            Node::Leaf(id) if *id == target => {
                let old = std::mem::replace(node, Node::Leaf(0));
                *node = f(old);
            }
            Node::Leaf(_) => {}
            Node::Split { a, b, .. } => {
                // On tente à gauche puis à droite (une seule feuille correspond).
                if contains_leaf(a, target) {
                    Self::replace_leaf(a, target, f);
                } else {
                    Self::replace_leaf(b, target, f);
                }
            }
        }
    }

    /// Retire la feuille `target` en remplaçant son parent par le frère.
    fn remove_leaf(node: &mut Node, target: PaneId) {
        if let Node::Split { a, b, .. } = node {
            let a_has = matches!(a.as_ref(), Node::Leaf(id) if *id == target);
            let b_has = matches!(b.as_ref(), Node::Leaf(id) if *id == target);
            if a_has {
                let sibling = std::mem::replace(b.as_mut(), Node::Leaf(0));
                *node = sibling;
                return;
            }
            if b_has {
                let sibling = std::mem::replace(a.as_mut(), Node::Leaf(0));
                *node = sibling;
                return;
            }
            Self::remove_leaf(a, target);
            Self::remove_leaf(b, target);
        }
    }
}

fn contains_leaf(node: &Node, target: PaneId) -> bool {
    match node {
        Node::Leaf(id) => *id == target,
        Node::Split { a, b, .. } => contains_leaf(a, target) || contains_leaf(b, target),
    }
}

/// Ajuste le ratio de la découpe pertinente pour redimensionner le volet actif.
/// `horizontal` cible les découpes gauche/droite ; `grow > 0` agrandit le volet.
fn resize_walk(node: &mut Node, target: PaneId, horizontal: bool, grow: f32) -> Option<bool> {
    match node {
        Node::Leaf(id) => (*id == target).then_some(false),
        Node::Split {
            dir, ratio, a, b, ..
        } => {
            let matches = axis_matches(*dir, horizontal);
            if let Some(done) = resize_walk(a, target, horizontal, grow) {
                if !done && matches {
                    *ratio = (*ratio + grow).clamp(0.1, 0.9);
                    return Some(true);
                }
                return Some(done);
            }
            if let Some(done) = resize_walk(b, target, horizontal, grow) {
                if !done && matches {
                    *ratio = (*ratio - grow).clamp(0.1, 0.9);
                    return Some(true);
                }
                return Some(done);
            }
            None
        }
    }
}

fn axis_matches(dir: SplitDir, horizontal: bool) -> bool {
    matches!(
        (dir, horizontal),
        (SplitDir::LeftRight, true) | (SplitDir::TopBottom, false)
    )
}

fn overlaps_v(a: &Rect, b: &Rect) -> bool {
    a.y < b.y + b.h && b.y < a.y + a.h
}

fn overlaps_h(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w
}

/// Calcule récursivement les rectangles des feuilles et les segments de bordure.
fn layout(node: &Node, area: Rect, rects: &mut HashMap<PaneId, Rect>, borders: &mut Vec<Border>) {
    match node {
        Node::Leaf(id) => {
            rects.insert(*id, area);
        }
        Node::Split {
            dir, ratio, a, b, ..
        } => match dir {
            SplitDir::LeftRight => {
                if area.w < 3 {
                    // Trop étroit pour une bordure : on empile sans découper.
                    layout(a, area, rects, borders);
                    return;
                }
                let usable = area.w - 1;
                let left = ((usable as f32 * ratio).round() as u16).clamp(1, usable - 1);
                let a_rect = Rect {
                    x: area.x,
                    y: area.y,
                    w: left,
                    h: area.h,
                };
                let border_x = area.x + left;
                let b_rect = Rect {
                    x: border_x + 1,
                    y: area.y,
                    w: area.w - left - 1,
                    h: area.h,
                };
                borders.push(Border {
                    vertical: true,
                    x: border_x,
                    y: area.y,
                    len: area.h,
                });
                layout(a, a_rect, rects, borders);
                layout(b, b_rect, rects, borders);
            }
            SplitDir::TopBottom => {
                if area.h < 3 {
                    layout(a, area, rects, borders);
                    return;
                }
                let usable = area.h - 1;
                let top = ((usable as f32 * ratio).round() as u16).clamp(1, usable - 1);
                let a_rect = Rect {
                    x: area.x,
                    y: area.y,
                    w: area.w,
                    h: top,
                };
                let border_y = area.y + top;
                let b_rect = Rect {
                    x: area.x,
                    y: border_y + 1,
                    w: area.w,
                    h: area.h - top - 1,
                };
                borders.push(Border {
                    vertical: false,
                    x: area.x,
                    y: border_y,
                    len: area.w,
                });
                layout(a, a_rect, rects, borders);
                layout(b, b_rect, rects, borders);
            }
        },
    }
}

/// Dessine le substitut d'un volet navigateur pour les clients texte : une
/// étiquette et l'URL, tronquées à la largeur disponible.
fn draw_web_placeholder(into: &mut Grid, r: Rect, url: &str) {
    let pen = Pen {
        fg: Color::Indexed(6),
        ..Pen::default()
    };
    if r.h == 0 || r.w == 0 {
        return;
    }
    let clip = |s: &str| -> String { s.chars().take(r.w as usize).collect() };
    into.set_str(r.x, r.y, &clip("[navigateur]"), pen);
    if r.h >= 2 {
        into.set_str(r.x, r.y + 1, &clip(url), pen);
    }
}

/// Fixe le ratio du nœud `target`. Renvoie `true` si trouvé.
fn set_ratio_walk(node: &mut Node, target: u32, ratio: f32) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split {
            node_id,
            ratio: r,
            a,
            b,
            ..
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

#[cfg(test)]
mod tests {
    use super::*;

    fn split_lr() -> Node {
        Node::Split {
            node_id: 1,
            dir: SplitDir::LeftRight,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        }
    }

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

    #[test]
    fn layout_tree_dun_split() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        win.split_pane(id1, SplitDir::LeftRight, dummy_pane());
        match win.layout_tree() {
            wimux_protocol::LayoutNode::Split { a, b, .. } => {
                assert!(matches!(
                    *a,
                    wimux_protocol::LayoutNode::Leaf {
                        kind: wimux_protocol::PaneKind::Terminal,
                        ..
                    }
                ));
                assert!(matches!(
                    *b,
                    wimux_protocol::LayoutNode::Leaf {
                        kind: wimux_protocol::PaneKind::Terminal,
                        ..
                    }
                ));
            }
            _ => panic!("attendu un Split"),
        }
        win.kill_all();
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
        let mut win = Window::new(p1);
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
            wimux_protocol::LayoutNode::Leaf { pane_id, .. } if pane_id == id1
        ));
        win.kill_all();
    }

    #[test]
    fn set_ratio_borne() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
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

    #[test]
    fn resize_agrandit_le_volet_gauche() {
        let mut root = split_lr();
        resize_walk(&mut root, 1, true, 0.1);
        match root {
            Node::Split { ratio, .. } => assert!((ratio - 0.6).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn resize_agrandit_le_volet_droit() {
        let mut root = split_lr();
        resize_walk(&mut root, 2, true, 0.1);
        match root {
            Node::Split { ratio, .. } => assert!((ratio - 0.4).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn resize_ignore_le_mauvais_axe() {
        let mut root = split_lr();
        resize_walk(&mut root, 1, false, 0.1);
        match root {
            Node::Split { ratio, .. } => assert!((ratio - 0.5).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn axis_matches_correct() {
        assert!(axis_matches(SplitDir::LeftRight, true));
        assert!(axis_matches(SplitDir::TopBottom, false));
        assert!(!axis_matches(SplitDir::LeftRight, false));
    }

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

    fn dummy_web() -> Arc<crate::webpane::WebPane> {
        Arc::new(crate::webpane::WebPane::new(
            9_001,
            "http://localhost:5173/".into(),
        ))
    }

    #[test]
    fn volet_web_est_une_feuille_mais_pas_un_volet_terminal() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        let web = dummy_web();
        let idw = web.id;
        win.split_web(id1, SplitDir::LeftRight, Arc::clone(&web));

        // Il est dans la fenêtre…
        assert!(win.contains(idw), "le volet web est présent");
        assert!(win.pane_ids().contains(&idw));
        // …mais ce n'est pas un volet TERMINAL.
        assert!(
            win.pane(idw).is_none(),
            "pane() ne doit renvoyer que des terminaux"
        );
        assert!(win.web_pane(idw).is_some());
        // La disposition l'annonce comme navigateur, avec son URL.
        let tree = win.layout_tree();
        let kinds = collect_kinds(&tree);
        assert!(
            kinds.contains(&wimux_protocol::PaneKind::Web {
                url: "http://localhost:5173/".into()
            }),
            "la feuille web doit porter son URL : {kinds:?}"
        );
        win.kill_all();
    }

    #[test]
    fn volet_web_survit_au_reap() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        let web = dummy_web();
        let idw = web.id;
        win.split_web(id1, SplitDir::TopBottom, web);

        // Tuer le volet terminal puis reaper : le web doit rester.
        win.pane(id1).unwrap().kill();
        // `reap_dead` retire les terminaux morts ; il ne doit PAS toucher au web.
        let _ = win.reap_dead();
        assert!(win.contains(idw), "un volet web n'est jamais reapé");
        win.kill_all();
    }

    #[test]
    fn active_term_pane_est_none_sur_un_volet_web() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        let web = dummy_web();
        let idw = web.id;
        // `split_web` rend le nouveau volet actif.
        win.split_web(id1, SplitDir::LeftRight, web);
        assert_eq!(win.active_pane_id(), idw);
        assert!(
            win.active_term_pane().is_none(),
            "le volet actif est un navigateur : pas de volet terminal actif"
        );
        win.set_active(id1);
        assert!(win.active_term_pane().is_some());
        win.kill_all();
    }

    /// Aplatit les `PaneKind` d'un arbre de disposition (ordre non garanti).
    fn collect_kinds(node: &wimux_protocol::LayoutNode) -> Vec<wimux_protocol::PaneKind> {
        match node {
            wimux_protocol::LayoutNode::Leaf { kind, .. } => vec![kind.clone()],
            wimux_protocol::LayoutNode::Split { a, b, .. } => {
                let mut v = collect_kinds(a);
                v.extend(collect_kinds(b));
                v
            }
        }
    }
}
