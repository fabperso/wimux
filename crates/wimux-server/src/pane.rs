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
use wimux_vt::{Grid, Terminal};

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

    /// Copie de la grille visible et position du curseur (col, row) locale.
    pub fn snapshot(&self) -> (Grid, (u16, u16)) {
        let st = self.state.lock().unwrap();
        (st.terminal.grid().clone(), st.terminal.cursor())
    }

    pub fn size(&self) -> (u16, u16) {
        let st = self.state.lock().unwrap();
        (st.cols, st.rows)
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
