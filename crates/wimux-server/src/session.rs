//! Une session : un ensemble de fenêtres, chacune contenant un arbre de volets.
//! La session est **la source de vérité** : ses volets tournent en continu, et à
//! chaque changement les clients attachés sont réveillés puis reçoivent une
//! composition (volets + bordures + barre de statut) rendue par le serveur.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use wimux_protocol::Frame;
use wimux_vt::{Color, Grid, Pen};

use crate::pane::{Notifier, Pane};
use crate::window::{Move, Rect, SplitDir, Window};

struct Inner {
    windows: Vec<Window>,
    active_window: usize,
    cols: u16,
    rows: u16,
}

pub struct Session {
    pub name: String,
    notifier: Arc<Notifier>,
    shell: String,
    inner: Mutex<Inner>,
    attached: AtomicUsize,
}

impl Session {
    pub fn new(name: String, cols: u16, rows: u16, shell: &str) -> Result<Arc<Session>> {
        let notifier = Notifier::new();
        let pane = Pane::spawn(cols, content_rows(rows), shell, Arc::clone(&notifier))?;
        let window = Window::new("win".to_string(), pane);

        let session = Arc::new(Session {
            name,
            notifier,
            shell: shell.to_string(),
            inner: Mutex::new(Inner {
                windows: vec![window],
                active_window: 0,
                cols,
                rows,
            }),
            attached: AtomicUsize::new(0),
        });
        session.reflow();
        Ok(session)
    }

    pub fn notifier(&self) -> Arc<Notifier> {
        Arc::clone(&self.notifier)
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
            draw_status_bar(&mut grid, &self.name, &inner, rows - 1);
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

fn draw_status_bar(grid: &mut Grid, name: &str, inner: &Inner, row: u16) {
    let bar = Pen {
        fg: Color::Indexed(0),
        bg: Color::Indexed(2),
        ..Pen::default()
    };
    // Fond de la barre.
    for col in 0..grid.cols() {
        grid.set(col, row, wimux_vt::Cell::blank_with(bar));
    }
    let mut text = format!(" [{name}] ");
    for (i, _win) in inner.windows.iter().enumerate() {
        if i == inner.active_window {
            text.push_str(&format!("{i}* "));
        } else {
            text.push_str(&format!("{i}  "));
        }
    }
    grid.set_str(0, row, &text, bar);
}
