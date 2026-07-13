//! Un volet (`Pane`) : une pseudo-console ConPTY exécutant un shell, plus le
//! terminal virtuel (`wimux-vt`) qui en reflète l'affichage. C'est l'unité de
//! base ; une fenêtre en dispose plusieurs selon un arbre de découpes.
//!
//! Les volets d'une même session partagent un [`Notifier`] : dès qu'un volet
//! produit de la sortie, il incrémente une génération globale et réveille les
//! clients attachés, qui redemandent alors une composition de la session.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use wimux_vt::{Cell, Grid, Terminal};

/// Résultat du traitement d'une touche en mode copie.
pub enum CopyAction {
    /// Rien de spécial (rester en mode copie).
    None,
    /// Quitter le mode copie sans copier.
    Exit,
    /// Texte sélectionné copié ; quitter le mode copie.
    Copied(String),
}

/// État du mode copie d'un volet : vue défilée + curseur + sélection.
struct CopyMode {
    /// Ligne logique (historique puis écran) en haut de la vue.
    view_top: usize,
    cursor_line: usize,
    cursor_col: u16,
    /// Ancre de sélection (ligne, colonne), si une sélection est en cours.
    anchor: Option<(usize, u16)>,
}

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

/// Identifiant unique de volet.
pub type PaneId = u64;

/// Signal de changement d'affichage partagé par tous les volets d'une session.
pub struct Notifier {
    generation: Mutex<u64>,
    cond: Condvar,
}

impl Notifier {
    pub fn new() -> Arc<Notifier> {
        Arc::new(Notifier {
            generation: Mutex::new(0),
            cond: Condvar::new(),
        })
    }

    /// Signale un changement (nouvelle sortie, changement de layout...).
    pub fn bump(&self) {
        let mut g = self.generation.lock().unwrap();
        *g += 1;
        self.cond.notify_all();
    }

    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    pub fn notify(&self) {
        self.cond.notify_all();
    }

    /// Bloque jusqu'à un changement au-delà de `last_seen`, ou jusqu'à ce que
    /// `keep_going` passe à faux. Renvoie la génération courante.
    pub fn wait_change(&self, last_seen: u64, keep_going: &AtomicBool) -> u64 {
        let mut g = self.generation.lock().unwrap();
        while *g == last_seen && keep_going.load(Ordering::Relaxed) {
            let (guard, _timeout) = self
                .cond
                .wait_timeout(g, Duration::from_millis(200))
                .unwrap();
            g = guard;
        }
        *g
    }
}

struct PaneState {
    terminal: Terminal,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    cols: u16,
    rows: u16,
    exit_code: Option<u32>,
    copy: Option<CopyMode>,
}

pub struct Pane {
    pub id: PaneId,
    state: Mutex<PaneState>,
    notifier: Arc<Notifier>,
}

impl Pane {
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
                master: pair.master,
                child: Some(child),
                cols,
                rows,
                exit_code: None,
                copy: None,
            }),
            notifier,
        });

        let reader_pane = Arc::clone(&pane);
        std::thread::spawn(move || reader_loop(reader_pane, reader));
        Ok(pane)
    }

    pub fn send_input(&self, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap();
        let _ = st.writer.write_all(bytes);
        let _ = st.writer.flush();
    }

    /// Redimensionne le volet (pseudo-console + terminal) à `cols` x `rows`.
    pub fn resize(&self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut st = self.state.lock().unwrap();
        if st.cols == cols && st.rows == rows {
            return;
        }
        let _ = st.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        st.terminal.resize(cols, rows);
        st.cols = cols;
        st.rows = rows;
    }

    /// Vue courante du volet et position du curseur (col, row) locale. En mode
    /// copie, renvoie la vue défilée avec la sélection surlignée.
    pub fn snapshot(&self) -> (Grid, (u16, u16)) {
        let st = self.state.lock().unwrap();
        match &st.copy {
            Some(cm) => render_copy_view(cm, &st.terminal, st.cols, st.rows),
            None => (st.terminal.grid().clone(), st.terminal.cursor()),
        }
    }

    pub fn size(&self) -> (u16, u16) {
        let st = self.state.lock().unwrap();
        (st.cols, st.rows)
    }

    pub fn in_copy_mode(&self) -> bool {
        self.state.lock().unwrap().copy.is_some()
    }

    /// Indicateur de statut du mode copie, ex. « COPIE 12/340 ».
    pub fn copy_status(&self) -> Option<String> {
        let st = self.state.lock().unwrap();
        st.copy.as_ref().map(|cm| {
            let total = total_lines(&st.terminal);
            format!("COPIE {}/{}", cm.cursor_line + 1, total)
        })
    }

    /// Entre en mode copie, curseur placé sur le curseur courant de l'écran.
    pub fn enter_copy_mode(&self) {
        let mut st = self.state.lock().unwrap();
        if st.copy.is_some() {
            return;
        }
        let history = st.terminal.history().len();
        let (ccol, crow) = st.terminal.cursor();
        let rows = st.rows;
        let cursor_line = history + crow as usize;
        let view_top = (history + st.rows as usize).saturating_sub(rows as usize);
        st.copy = Some(CopyMode {
            view_top,
            cursor_line,
            cursor_col: ccol,
            anchor: None,
        });
        self.notifier.bump();
    }

    /// Traite une touche en mode copie. Sans effet si le volet n'y est pas.
    pub fn copy_key(&self, byte: u8) -> CopyAction {
        let mut guard = self.state.lock().unwrap();
        // Déréférencer en `&mut PaneState` autorise les emprunts disjoints des
        // champs (impossible directement à travers le MutexGuard).
        let st = &mut *guard;
        let rows = st.rows as usize;
        let cols = st.cols;
        let total = total_lines(&st.terminal);
        let Some(cm) = st.copy.as_mut() else {
            return CopyAction::None;
        };

        match byte {
            b'q' | 0x1b => {
                st.copy = None;
                self.notifier.bump();
                return CopyAction::Exit;
            }
            b'j' => cm.cursor_line = (cm.cursor_line + 1).min(total.saturating_sub(1)),
            b'k' => cm.cursor_line = cm.cursor_line.saturating_sub(1),
            b'h' => cm.cursor_col = cm.cursor_col.saturating_sub(1),
            b'l' => cm.cursor_col = (cm.cursor_col + 1).min(cols.saturating_sub(1)),
            b'0' => cm.cursor_col = 0,
            b'$' => cm.cursor_col = cols.saturating_sub(1),
            b'g' => cm.cursor_line = 0,
            b'G' => cm.cursor_line = total.saturating_sub(1),
            0x15 => cm.cursor_line = cm.cursor_line.saturating_sub(rows / 2), // Ctrl-u
            0x04 => {
                cm.cursor_line = (cm.cursor_line + rows / 2).min(total.saturating_sub(1)); // Ctrl-d
            }
            b' ' => cm.anchor = Some((cm.cursor_line, cm.cursor_col)),
            b'y' | 0x0d => {
                let text = extract_selection(cm, &st.terminal, cols);
                st.copy = None;
                self.notifier.bump();
                return CopyAction::Copied(text);
            }
            _ => return CopyAction::None,
        }

        // Réajuster la fenêtre pour garder le curseur visible.
        if let Some(cm) = st.copy.as_mut() {
            if cm.cursor_line < cm.view_top {
                cm.view_top = cm.cursor_line;
            } else if cm.cursor_line >= cm.view_top + rows {
                cm.view_top = cm.cursor_line + 1 - rows;
            }
        }
        self.notifier.bump();
        CopyAction::None
    }

    pub fn is_alive(&self) -> bool {
        self.state.lock().unwrap().exit_code.is_none()
    }

    pub fn kill(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(child) = st.child.as_mut() {
            let _ = child.kill();
        }
    }
}

fn reader_loop(pane: Arc<Pane>, mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                {
                    let mut st = pane.state.lock().unwrap();
                    st.terminal.advance(&buf[..n]);
                    let responses = st.terminal.take_responses();
                    if !responses.is_empty() {
                        let _ = st.writer.write_all(&responses);
                        let _ = st.writer.flush();
                    }
                }
                pane.notifier.bump();
            }
        }
    }

    let child = {
        let mut st = pane.state.lock().unwrap();
        st.child.take()
    };
    let code = child
        .and_then(|mut c| c.wait().ok())
        .map(|status| status.exit_code())
        .unwrap_or(0);

    pane.state.lock().unwrap().exit_code = Some(code);
    pane.notifier.bump();
}

// --- Mode copie : vue défilée, sélection, extraction ----------------------

/// Nombre total de lignes logiques : historique (scrollback) + écran visible.
fn total_lines(term: &Terminal) -> usize {
    term.history().len() + term.grid().rows() as usize
}

/// Cellules de la ligne logique `idx` (historique puis écran).
fn logical_line(term: &Terminal, idx: usize) -> Vec<Cell> {
    let h = term.history().len();
    if idx < h {
        term.history()[idx].clone()
    } else {
        term.grid().row((idx - h) as u16).to_vec()
    }
}

/// Construit la grille affichée en mode copie (vue défilée + sélection).
fn render_copy_view(cm: &CopyMode, term: &Terminal, cols: u16, rows: u16) -> (Grid, (u16, u16)) {
    let mut grid = Grid::new(cols, rows);
    let total = total_lines(term);

    let sel = cm.anchor.map(|a| {
        let c = (cm.cursor_line, cm.cursor_col);
        if a <= c { (a, c) } else { (c, a) }
    });

    for r in 0..rows {
        let line_idx = cm.view_top + r as usize;
        if line_idx >= total {
            break;
        }
        let cells = logical_line(term, line_idx);
        for (col, cell) in cells.iter().enumerate() {
            let col = col as u16;
            if col >= cols {
                break;
            }
            let mut c = *cell;
            if let Some(((sl, sc), (el, ec))) = sel
                && in_selection(line_idx, col, sl, sc, el, ec)
            {
                c.pen.attrs.reverse = true;
            }
            grid.set(col, r, c);
        }
    }

    let cy =
        (cm.cursor_line.saturating_sub(cm.view_top)).min(rows.saturating_sub(1) as usize) as u16;
    let cx = cm.cursor_col.min(cols.saturating_sub(1));
    (grid, (cx, cy))
}

/// Vrai si la cellule (line, col) est dans la sélection [(sl,sc), (el,ec)].
fn in_selection(line: usize, col: u16, sl: usize, sc: u16, el: usize, ec: u16) -> bool {
    if line < sl || line > el {
        return false;
    }
    if sl == el {
        col >= sc && col <= ec
    } else if line == sl {
        col >= sc
    } else if line == el {
        col <= ec
    } else {
        true
    }
}

/// Extrait le texte de la sélection (ou la ligne courante si aucune ancre).
fn extract_selection(cm: &CopyMode, term: &Terminal, cols: u16) -> String {
    let Some(anchor) = cm.anchor else {
        let cells = logical_line(term, cm.cursor_line);
        return line_text(&cells, 0, cols.saturating_sub(1));
    };

    let c = (cm.cursor_line, cm.cursor_col);
    let ((sl, sc), (el, ec)) = if anchor <= c {
        (anchor, c)
    } else {
        (c, anchor)
    };

    let mut lines = Vec::new();
    for line in sl..=el {
        let cells = logical_line(term, line);
        let (from, to) = if sl == el {
            (sc, ec)
        } else if line == sl {
            (sc, cols.saturating_sub(1))
        } else if line == el {
            (0, ec)
        } else {
            (0, cols.saturating_sub(1))
        };
        lines.push(line_text(&cells, from, to));
    }
    lines.join("\r\n")
}

/// Texte d'une ligne entre les colonnes `from` et `to` (incluses), rogné à droite.
fn line_text(cells: &[Cell], from: u16, to: u16) -> String {
    let mut s = String::new();
    for col in from..=to {
        if let Some(cell) = cells.get(col as usize)
            && cell.width != 0
        {
            s.push(cell.ch);
        }
    }
    s.trim_end().to_string()
}
