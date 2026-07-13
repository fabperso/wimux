//! Une fenêtre = un arbre binaire de découpes dont les feuilles sont des volets.
//! La fenêtre calcule la disposition (rectangles + bordures) dans une zone
//! donnée, redimensionne ses volets en conséquence, et se compose dans une
//! grille pour l'affichage.

use std::collections::HashMap;
use std::sync::Arc;

use wimux_vt::{Cell, Color, Grid, Pen};

use crate::pane::{Pane, PaneId};

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
    pub name: String,
    root: Node,
    panes: HashMap<PaneId, Arc<Pane>>,
    active: PaneId,
    rects: HashMap<PaneId, Rect>,
    borders: Vec<Border>,
}

impl Window {
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
        }
    }

    pub fn active_pane(&self) -> Arc<Pane> {
        Arc::clone(&self.panes[&self.active])
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Termine tous les volets de la fenêtre (pour `kill-session`).
    pub fn kill_all(&self) {
        for pane in self.panes.values() {
            pane.kill();
        }
    }

    /// Description des volets (pour `list-panes`).
    pub fn pane_list(&self) -> Vec<String> {
        let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
        ids.sort_unstable();
        ids.iter()
            .map(|id| {
                let (c, r) = self.panes[id].size();
                let active = if *id == self.active { " (actif)" } else { "" };
                format!("volet {id}: {c}x{r}{active}")
            })
            .collect()
    }

    /// Découpe le volet actif, en y insérant `new_pane` qui devient actif.
    pub fn split(&mut self, dir: SplitDir, new_pane: Arc<Pane>) {
        let new_id = new_pane.id;
        let active = self.active;
        Self::replace_leaf(&mut self.root, active, |old| Node::Split {
            dir,
            ratio: 0.5,
            a: Box::new(old),
            b: Box::new(Node::Leaf(new_id)),
        });
        self.panes.insert(new_id, new_pane);
        self.active = new_id;
    }

    /// Ferme le volet actif. Renvoie `true` si la fenêtre est désormais vide.
    pub fn close_active(&mut self) -> bool {
        let closing = self.active;
        if let Some(pane) = self.panes.remove(&closing) {
            pane.kill();
        }
        if self.panes.is_empty() {
            return true;
        }
        Self::remove_leaf(&mut self.root, closing);
        // Nouvel actif : un volet quelconque encore présent.
        self.active = *self.panes.keys().next().unwrap();
        false
    }

    /// Retire les volets dont le shell est mort. Renvoie `true` si vide.
    pub fn reap_dead(&mut self) -> bool {
        let dead: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| !p.is_alive())
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
        let mut rects = HashMap::new();
        let mut borders = Vec::new();
        layout(&self.root, area, &mut rects, &mut borders);
        for (&id, r) in &rects {
            if let Some(pane) = self.panes.get(&id) {
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
            if let Some(pane) = self.panes.get(&id) {
                let (grid, (cc, cr)) = pane.snapshot();
                into.blit(&grid, r.x, r.y);
                if id == self.active {
                    cursor = (
                        r.x + cc.min(r.w.saturating_sub(1)),
                        r.y + cr.min(r.h.saturating_sub(1)),
                    );
                }
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
        Node::Split { dir, ratio, a, b } => match dir {
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
