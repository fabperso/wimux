//! Une session : un ensemble de fenêtres, chacune contenant un arbre de volets.
//! La session est **la source de vérité** : ses volets tournent en continu, et à
//! chaque changement les clients attachés sont réveillés puis reçoivent une
//! composition (volets + bordures + barre de statut) rendue par le serveur.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use wimux_protocol::{Frame, LayoutNode};
use wimux_vt::{Color, Grid, Pen};

use crate::pane::{CopyAction, Notifier, Pane, PaneId};
use crate::window::{Move, Rect, SplitDir, Window};

struct Inner {
    windows: Vec<Window>,
    active_window: usize,
    cols: u16,
    rows: u16,
    /// Ligne de commande en cours de saisie (`Ctrl-b :`), sans le `:`.
    command_line: Option<String>,
}

pub struct Session {
    name: Mutex<String>,
    notifier: Arc<Notifier>,
    shell: String,
    inner: Mutex<Inner>,
    attached: AtomicUsize,
    /// Génération du Notifier vue par la GUI la dernière fois (G4).
    last_seen_gen: AtomicU64,
    paste_buffer: Mutex<String>,
}

impl Session {
    pub fn new(name: String, cols: u16, rows: u16, shell: &str) -> Result<Arc<Session>> {
        let notifier = Notifier::new();
        let pane = Pane::spawn(cols, content_rows(rows), shell, Arc::clone(&notifier))?;
        let window = Window::new("win".to_string(), pane);

        let session = Arc::new(Session {
            name: Mutex::new(name),
            notifier,
            shell: shell.to_string(),
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
        });
        session.reflow();
        Ok(session)
    }

    pub fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }

    pub fn set_name(&self, new: String) {
        *self.name.lock().unwrap() = new;
    }

    fn active_pane(&self) -> Option<Arc<Pane>> {
        let inner = self.inner.lock().unwrap();
        inner
            .windows
            .get(inner.active_window)
            .map(|w| w.active_pane())
    }

    /// Prépare l'attache GUI de la fenêtre active : crée UN canal fusionné, abonne
    /// chaque volet, renvoie la disposition, le volet actif, les snapshots par
    /// volet, le récepteur fusionné ET un `Sender` (pour abonner les futurs volets).
    #[allow(clippy::type_complexity)]
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
            if inner.windows.get(aw).is_none() {
                drop(inner);
                new_pane.kill();
                return None;
            }
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

    /// Entre en mode copie sur le volet actif.
    pub fn enter_copy_mode(&self) {
        if let Some(p) = self.active_pane() {
            p.enter_copy_mode();
        }
    }

    pub fn active_in_copy_mode(&self) -> bool {
        self.active_pane()
            .map(|p| p.in_copy_mode())
            .unwrap_or(false)
    }

    /// Traite une touche de mode copie. Une copie remplit le tampon de collage.
    pub fn copy_key(&self, byte: u8) -> CopyAction {
        let Some(pane) = self.active_pane() else {
            return CopyAction::None;
        };
        let action = pane.copy_key(byte);
        if let CopyAction::Copied(text) = &action {
            *self.paste_buffer.lock().unwrap() = text.clone();
        }
        action
    }

    /// Colle le tampon dans le volet actif.
    pub fn paste(&self) {
        let buffer = self.paste_buffer.lock().unwrap().clone();
        if !buffer.is_empty()
            && let Some(pane) = self.active_pane()
        {
            pane.send_input(buffer.as_bytes());
        }
    }

    fn active_copy_status(&self) -> Option<String> {
        self.active_pane().and_then(|p| p.copy_status())
    }

    // --- Invite de commande (Ctrl-b :) ---------------------------------------

    pub fn enter_command_prompt(&self) {
        self.inner.lock().unwrap().command_line = Some(String::new());
        self.notifier.bump();
    }

    pub fn in_command_prompt(&self) -> bool {
        self.inner.lock().unwrap().command_line.is_some()
    }

    /// Traite une touche de l'invite. Renvoie la commande à exécuter sur Entrée.
    pub fn command_key(&self, byte: u8) -> Option<String> {
        let mut result = None;
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(line) = inner.command_line.as_mut() {
                match byte {
                    0x0d => {
                        result = Some(std::mem::take(line));
                        inner.command_line = None;
                    }
                    0x1b => inner.command_line = None,
                    0x08 | 0x7f => {
                        line.pop();
                    }
                    b if (0x20..0x7f).contains(&b) => line.push(b as char),
                    _ => {}
                }
            }
        }
        self.notifier.bump();
        result
    }

    fn command_status(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .command_line
            .as_ref()
            .map(|l| format!(":{l}"))
    }

    /// Contenu visible du volet actif (pour `capture-pane`).
    pub fn capture_active_pane(&self) -> String {
        self.active_pane()
            .map(|p| p.capture_text())
            .unwrap_or_default()
    }

    /// Description des volets de la fenêtre active (pour `list-panes`).
    pub fn list_panes_text(&self) -> String {
        let inner = self.inner.lock().unwrap();
        inner
            .windows
            .get(inner.active_window)
            .map(|w| w.pane_list().join("\r\n"))
            .unwrap_or_default()
    }

    // --- Zoom & redimensionnement (phase 6) ----------------------------------

    pub fn toggle_zoom(&self) {
        let mut inner = self.inner.lock().unwrap();
        let area = content_area(inner.cols, inner.rows);
        let aw = inner.active_window;
        if let Some(win) = inner.windows.get_mut(aw) {
            win.toggle_zoom();
            win.reflow(area);
        }
        drop(inner);
        self.notifier.bump();
    }

    pub fn resize_pane(&self, mv: Move) {
        let mut inner = self.inner.lock().unwrap();
        let area = content_area(inner.cols, inner.rows);
        let aw = inner.active_window;
        if let Some(win) = inner.windows.get_mut(aw) {
            win.resize_active(mv);
            win.reflow(area);
        }
        drop(inner);
        self.notifier.bump();
    }

    // --- Souris (phase 6) ----------------------------------------------------

    /// Rend actif le volet situé sous le clic (coordonnées de contenu 0-based).
    pub fn select_pane_at(&self, col: u16, row: u16) {
        {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            if let Some(win) = inner.windows.get_mut(aw)
                && let Some(id) = win.pane_at(col, row)
            {
                win.set_active(id);
            }
        }
        self.notifier.bump();
    }

    /// Molette : fait défiler (et active) le volet sous le curseur.
    pub fn mouse_scroll_at(&self, col: u16, row: u16, up: bool) {
        let pane = {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            let Some(win) = inner.windows.get_mut(aw) else {
                return;
            };
            let Some(id) = win.pane_at(col, row) else {
                return;
            };
            win.set_active(id);
            win.pane(id)
        };
        if let Some(p) = pane {
            p.scroll(up, 3);
        }
        self.notifier.bump();
    }

    fn active_zoomed(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .windows
            .get(inner.active_window)
            .map(|w| w.is_zoomed())
            .unwrap_or(false)
    }

    pub fn notifier(&self) -> Arc<Notifier> {
        Arc::clone(&self.notifier)
    }

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

    pub fn attached_count(&self) -> usize {
        self.attached.load(Ordering::Relaxed)
    }

    pub fn incr_attached(&self) {
        self.attached.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decr_attached(&self) {
        let prev = self.attached.load(Ordering::Relaxed);
        if prev > 0 {
            self.attached.store(prev - 1, Ordering::Relaxed);
        }
    }

    fn reflow(&self) {
        let mut inner = self.inner.lock().unwrap();
        let area = content_area(inner.cols, inner.rows);
        let aw = inner.active_window;
        if let Some(win) = inner.windows.get_mut(aw) {
            win.reflow(area);
        }
    }

    /// Transmet des octets au volet actif de la fenêtre active.
    pub fn send_input(&self, bytes: &[u8]) {
        let pane = {
            let inner = self.inner.lock().unwrap();
            inner
                .windows
                .get(inner.active_window)
                .map(|w| w.active_pane())
        };
        if let Some(pane) = pane {
            pane.send_input(bytes);
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.cols == cols && inner.rows == rows {
                return;
            }
            inner.cols = cols;
            inner.rows = rows;
        }
        self.reflow();
        self.notifier.bump();
    }

    pub fn split(&self, dir: SplitDir) {
        // Ne pas tenir le verrou pendant le spawn (qui lance un processus).
        let new_pane = Pane::spawn(1, 1, &self.shell, Arc::clone(&self.notifier));
        if let Ok(pane) = new_pane {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            if let Some(win) = inner.windows.get_mut(aw) {
                win.split(dir, pane);
                let area = content_area(inner.cols, inner.rows);
                inner.windows[aw].reflow(area);
            }
            drop(inner);
            self.notifier.bump();
        }
    }

    pub fn select(&self, mv: Move) {
        let mut inner = self.inner.lock().unwrap();
        let aw = inner.active_window;
        if let Some(win) = inner.windows.get_mut(aw) {
            win.select(mv);
        }
        drop(inner);
        self.notifier.bump();
    }

    pub fn next_pane(&self) {
        let mut inner = self.inner.lock().unwrap();
        let aw = inner.active_window;
        if let Some(win) = inner.windows.get_mut(aw) {
            win.next_pane();
        }
        drop(inner);
        self.notifier.bump();
    }

    /// Ferme le volet actif ; retire la fenêtre si elle devient vide.
    pub fn close_active_pane(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            let empty = inner.windows.get_mut(aw).map(|w| w.close_active());
            if empty == Some(true) {
                inner.windows.remove(aw);
                if inner.active_window >= inner.windows.len() && !inner.windows.is_empty() {
                    inner.active_window = inner.windows.len() - 1;
                }
            }
        }
        self.reflow();
        self.notifier.bump();
    }

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

    pub fn next_window(&self) {
        self.switch_window(1);
    }

    pub fn prev_window(&self) {
        self.switch_window(-1);
    }

    fn switch_window(&self, delta: i32) {
        {
            let mut inner = self.inner.lock().unwrap();
            let n = inner.windows.len();
            if n <= 1 {
                return;
            }
            let cur = inner.active_window as i32;
            inner.active_window = (cur + delta).rem_euclid(n as i32) as usize;
        }
        self.reflow();
        self.notifier.bump();
    }

    pub fn select_window(&self, index: usize) {
        {
            let mut inner = self.inner.lock().unwrap();
            if index >= inner.windows.len() {
                return;
            }
            inner.active_window = index;
        }
        self.reflow();
        self.notifier.bump();
    }

    /// Retire les volets/fenêtres morts. Renvoie `true` s'il reste de la vie.
    fn reap(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let mut i = 0;
        while i < inner.windows.len() {
            if inner.windows[i].reap_dead() {
                inner.windows.remove(i);
            } else {
                i += 1;
            }
        }
        if inner.active_window >= inner.windows.len() && !inner.windows.is_empty() {
            inner.active_window = inner.windows.len() - 1;
        }
        !inner.windows.is_empty()
    }

    pub fn is_alive(&self) -> bool {
        !self.inner.lock().unwrap().windows.is_empty()
    }

    pub fn window_count(&self) -> usize {
        self.inner.lock().unwrap().windows.len()
    }

    pub fn kill(&self) {
        let inner = self.inner.lock().unwrap();
        for win in &inner.windows {
            win.kill_all();
        }
    }

    /// Compose l'état d'affichage courant en une frame pour le client.
    pub fn composite(&self) -> Frame {
        self.reap();
        // Calculés avant le verrou principal (évite un verrouillage réentrant).
        let copy_status = self.active_copy_status();
        let command_status = self.command_status();
        let zoomed = self.active_zoomed();
        let name = self.name();
        let mut inner = self.inner.lock().unwrap();
        let (cols, rows) = (inner.cols.max(1), inner.rows.max(1));
        let area = content_area(cols, rows);

        let mut grid = Grid::new(cols, rows);
        let aw = inner.active_window;

        // Redimensionner au cas où (idempotent) puis composer la fenêtre active.
        let cursor = if let Some(win) = inner.windows.get_mut(aw) {
            win.reflow(area);
            win.render(&mut grid)
        } else {
            (0, 0)
        };

        // Barre de statut sur la dernière ligne.
        if rows >= 2 {
            draw_status_bar(
                &mut grid,
                &name,
                &inner,
                rows - 1,
                copy_status.as_deref(),
                command_status.as_deref(),
                zoomed,
            );
        }

        Frame {
            cols,
            rows,
            cursor_col: cursor.0,
            cursor_row: cursor.1,
            cells: grid_cells(&grid),
        }
    }
}

/// Nombre de lignes réservées au contenu (hors barre de statut).
fn content_rows(rows: u16) -> u16 {
    if rows >= 2 { rows - 1 } else { rows }
}

fn content_area(cols: u16, rows: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: cols.max(1),
        h: content_rows(rows).max(1),
    }
}

fn grid_cells(grid: &Grid) -> Vec<wimux_vt::Cell> {
    let mut cells = Vec::with_capacity(grid.cols() as usize * grid.rows() as usize);
    for row in 0..grid.rows() {
        cells.extend_from_slice(grid.row(row));
    }
    cells
}

fn draw_status_bar(
    grid: &mut Grid,
    name: &str,
    inner: &Inner,
    row: u16,
    copy_status: Option<&str>,
    command_status: Option<&str>,
    zoomed: bool,
) {
    let bar = Pen {
        fg: Color::Indexed(0),
        bg: Color::Indexed(2),
        ..Pen::default()
    };
    // Fond de la barre.
    for col in 0..grid.cols() {
        grid.set(col, row, wimux_vt::Cell::blank_with(bar));
    }

    // L'invite de commande, si active, occupe toute la barre.
    if let Some(prompt) = command_status {
        grid.set_str(0, row, prompt, bar);
        return;
    }

    let zoom_flag = if zoomed { "Z " } else { "" };
    let mut text = format!(" [{name}] {zoom_flag}");
    for (i, _win) in inner.windows.iter().enumerate() {
        if i == inner.active_window {
            text.push_str(&format!("{i}* "));
        } else {
            text.push_str(&format!("{i}  "));
        }
    }
    grid.set_str(0, row, &text, bar);

    // Indicateur de mode copie, aligné à droite.
    if let Some(status) = copy_status {
        let x = grid.cols().saturating_sub(status.len() as u16 + 1);
        grid.set_str(x, row, status, bar);
    }
}

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
}
