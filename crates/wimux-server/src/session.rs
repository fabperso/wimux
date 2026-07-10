//! Une session = un volet (pour la phase 2) : une pseudo-console ConPTY exécutant
//! un shell, plus le terminal virtuel (`wimux-vt`) qui en reflète l'affichage.
//!
//! La session est **la source de vérité** : un thread lecteur alimente en continu
//! le terminal à partir de la sortie du shell, même sans client attaché. Les
//! clients attachés sont réveillés via une `Condvar` à chaque changement.

use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use wimux_protocol::Frame;
use wimux_vt::Terminal;

/// État interne mutable d'une session, protégé par un `Mutex`.
struct State {
    terminal: Terminal,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    cols: u16,
    rows: u16,
    /// Incrémenté à chaque changement d'affichage ; les clients comparent leur
    /// dernière génération vue pour savoir s'il faut renvoyer une frame.
    generation: u64,
    /// Code de sortie du shell une fois terminé.
    exit_code: Option<u32>,
    /// Nombre de clients actuellement attachés.
    attached: usize,
}

pub struct Session {
    pub name: String,
    state: Mutex<State>,
    cond: Condvar,
}

impl Session {
    /// Crée une session : ouvre une pseudo-console, lance le shell, démarre le
    /// thread lecteur, et renvoie la session partagée.
    pub fn new(name: String, cols: u16, rows: u16, shell: &str) -> Result<Arc<Session>> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("ouverture de la pseudo-console")?;

        let cmd = CommandBuilder::new(shell);
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("lancement du shell")?;

        let reader = pair
            .master
            .try_clone_reader()
            .context("clonage du lecteur PTY")?;
        let writer = pair
            .master
            .take_writer()
            .context("prise de l'écrivain PTY")?;

        // Le côté esclave ne doit plus être détenu par le serveur.
        drop(pair.slave);

        let session = Arc::new(Session {
            name,
            state: Mutex::new(State {
                terminal: Terminal::new(cols, rows),
                writer,
                master: pair.master,
                child: Some(child),
                cols,
                rows,
                generation: 0,
                exit_code: None,
                attached: 0,
            }),
            cond: Condvar::new(),
        });

        // Thread lecteur : alimente le terminal et réveille les clients.
        let reader_session = Arc::clone(&session);
        std::thread::spawn(move || reader_loop(reader_session, reader));

        Ok(session)
    }

    /// Transmet des octets (frappes clavier) à l'entrée du shell.
    pub fn send_input(&self, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap();
        let _ = st.writer.write_all(bytes);
        let _ = st.writer.flush();
    }

    /// Redimensionne la pseudo-console et le terminal.
    pub fn resize(&self, cols: u16, rows: u16) {
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
        st.generation += 1;
        drop(st);
        self.cond.notify_all();
    }

    /// Instantané courant de l'affichage, à envoyer à un client.
    pub fn snapshot(&self) -> Frame {
        let st = self.state.lock().unwrap();
        build_frame(&st)
    }

    pub fn is_alive(&self) -> bool {
        self.state.lock().unwrap().exit_code.is_none()
    }

    /// Réveille les clients en attente (par ex. pour les faire réévaluer un
    /// drapeau d'arrêt).
    pub fn notify(&self) {
        self.cond.notify_all();
    }

    pub fn attached_count(&self) -> usize {
        self.state.lock().unwrap().attached
    }

    fn incr_attached(&self) {
        self.state.lock().unwrap().attached += 1;
    }

    fn decr_attached(&self) {
        let mut st = self.state.lock().unwrap();
        st.attached = st.attached.saturating_sub(1);
    }

    /// Termine le shell de la session (utilisé par `kill-session`).
    pub fn kill(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(child) = st.child.as_mut() {
            let _ = child.kill();
        }
    }

    /// Bloque jusqu'à ce que la génération d'affichage dépasse `last_seen` (ou
    /// que la session se termine / que `keep_going` passe à faux). Renvoie la
    /// nouvelle génération et l'éventuel code de sortie.
    pub fn wait_change(
        &self,
        last_seen: u64,
        keep_going: &std::sync::atomic::AtomicBool,
    ) -> (u64, Frame, Option<u32>) {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let mut st = self.state.lock().unwrap();
        while st.generation == last_seen
            && st.exit_code.is_none()
            && keep_going.load(Ordering::Relaxed)
        {
            let (guard, _timeout) = self
                .cond
                .wait_timeout(st, Duration::from_millis(200))
                .unwrap();
            st = guard;
        }
        (st.generation, build_frame(&st), st.exit_code)
    }
}

fn build_frame(st: &State) -> Frame {
    let grid = st.terminal.grid();
    let (cursor_col, cursor_row) = st.terminal.cursor();
    let mut cells = Vec::with_capacity(st.cols as usize * st.rows as usize);
    for row in 0..grid.rows() {
        cells.extend_from_slice(grid.row(row));
    }
    Frame {
        cols: grid.cols(),
        rows: grid.rows(),
        cursor_col,
        cursor_row,
        cells,
    }
}

/// Boucle du thread lecteur : lit la sortie du shell, la fait passer dans le
/// terminal, renvoie les réponses aux requêtes (DSR/CPR...) et réveille les
/// clients. À la fin du shell, enregistre le code de sortie.
fn reader_loop(session: Arc<Session>, mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut st = session.state.lock().unwrap();
                st.terminal.advance(&buf[..n]);
                let responses = st.terminal.take_responses();
                if !responses.is_empty() {
                    let _ = st.writer.write_all(&responses);
                    let _ = st.writer.flush();
                }
                st.generation += 1;
                drop(st);
                session.cond.notify_all();
            }
        }
    }

    // Le shell est terminé : récupérer son code de sortie.
    let child = {
        let mut st = session.state.lock().unwrap();
        st.child.take()
    };
    let code = child
        .and_then(|mut c| c.wait().ok())
        .map(|status| status.exit_code())
        .unwrap_or(0);

    let mut st = session.state.lock().unwrap();
    st.exit_code = Some(code);
    st.generation += 1;
    drop(st);
    session.cond.notify_all();
}

/// Garde RAII d'attachement : incrémente à la création, décrémente au drop.
/// Détient un `Arc<Session>` pour être utilisable au travers des threads.
pub struct AttachGuard {
    session: Arc<Session>,
}

impl AttachGuard {
    pub fn new(session: Arc<Session>) -> AttachGuard {
        session.incr_attached();
        AttachGuard { session }
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.session.decr_attached();
    }
}
