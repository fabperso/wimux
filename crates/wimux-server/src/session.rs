//! Une session : un ensemble de fenêtres, chacune contenant un arbre de volets.
//! La session est **la source de vérité** : ses volets tournent en continu, et à
//! chaque changement les clients attachés sont réveillés puis reçoivent une
//! composition (volets + bordures + barre de statut) rendue par le serveur.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use wimux_protocol::{AgentStatus, Frame, LayoutNode};
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
    /// Drapeau « session agent » (M1) : déclenche le calcul de statut et le
    /// non-reap. Posé par `mark_agent` (aucun chemin client en M1 ; c'est M2).
    agent: AtomicBool,
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
            agent: AtomicBool::new(false),
        });
        session.reflow();
        Ok(session)
    }

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
    ///
    /// M1 : pour une **session agent**, court-circuite sans rien retirer — la
    /// fenêtre morte est conservée (statut `Done`/`Error` visible) jusqu'à un
    /// `kill` manuel.
    fn reap(&self) -> bool {
        if self.is_agent() {
            return true;
        }
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
        assert!(
            s.is_agent(),
            "une session créée via new_agent doit être agent"
        );
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
}
