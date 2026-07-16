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
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use wimux_vt::{Cell, Color, Grid, Pen, Terminal};

/// Résultat du traitement d'une touche en mode copie.
pub enum CopyAction {
    /// Rien de spécial (rester en mode copie).
    None,
    /// Quitter le mode copie sans copier.
    Exit,
    /// Texte sélectionné copié ; quitter le mode copie.
    Copied(String),
}

/// Recherche en cours dans le scrollback.
#[derive(Clone)]
struct Search {
    backward: bool,
    query: String,
}

/// État du mode copie d'un volet : vue défilée + curseur + sélection.
struct CopyMode {
    /// Ligne logique (historique puis écran) en haut de la vue.
    view_top: usize,
    cursor_line: usize,
    cursor_col: u16,
    /// Ancre de sélection (ligne, colonne), si une sélection est en cours.
    anchor: Option<(usize, u16)>,
    /// Saisie de recherche en cours (après `/` ou `?`).
    search_input: Option<Search>,
    /// Dernière recherche exécutée (pour `n` / `N`).
    last_search: Option<Search>,
}

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

/// Identifiant unique de volet.
pub type PaneId = u64;

/// Signal de changement d'affichage partagé par tous les volets d'une session.
pub struct Notifier {
    generation: Mutex<u64>,
    cond: Condvar,
    /// Cloche (BEL) en attente pour cette session (G4).
    bell: AtomicBool,
    /// Horodatage de la dernière sortie (dernier `bump`), pour le calcul du
    /// statut d'agent (M1).
    last_output_at: Mutex<Instant>,
}

impl Notifier {
    pub fn new() -> Arc<Notifier> {
        Arc::new(Notifier {
            generation: Mutex::new(0),
            cond: Condvar::new(),
            bell: AtomicBool::new(false),
            last_output_at: Mutex::new(Instant::now()),
        })
    }

    /// Signale un changement (nouvelle sortie, changement de layout...).
    pub fn bump(&self) {
        let mut g = self.generation.lock().unwrap();
        *g += 1;
        *self.last_output_at.lock().unwrap() = Instant::now();
        self.cond.notify_all();
    }

    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    /// Durée écoulée depuis la dernière sortie (dernier `bump`). Sert au calcul
    /// du statut d'agent (M1).
    pub fn last_output_elapsed(&self) -> Duration {
        self.last_output_at.lock().unwrap().elapsed()
    }

    pub fn notify(&self) {
        self.cond.notify_all();
    }

    /// Pose le drapeau cloche (appelé par le `reader_loop` d'un volet).
    pub fn signal_bell(&self) {
        self.bell.store(true, Ordering::Relaxed);
    }

    /// Lit le drapeau cloche.
    pub fn bell(&self) -> bool {
        self.bell.load(Ordering::Relaxed)
    }

    /// Efface le drapeau cloche (quand la session est vue).
    pub fn clear_bell(&self) {
        self.bell.store(false, Ordering::Relaxed);
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
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    cols: u16,
    rows: u16,
    exit_code: Option<u32>,
    copy: Option<CopyMode>,
    subscribers: Vec<std::sync::mpsc::Sender<(PaneId, Vec<u8>)>>,
}

pub struct Pane {
    pub id: PaneId,
    state: Mutex<PaneState>,
    notifier: Arc<Notifier>,
}

impl Pane {
    /// Crée un volet exécutant `shell` (jeton unique, sans args). Cas particulier
    /// de [`Pane::spawn_command`].
    pub fn spawn(cols: u16, rows: u16, shell: &str, notifier: Arc<Notifier>) -> Result<Arc<Pane>> {
        Pane::spawn_command(cols, rows, shell, &[], None, notifier)
    }

    /// Crée un volet : ouvre une pseudo-console, lance `program` avec `args` dans
    /// `cwd` (défaut = cwd du processus), démarre le thread lecteur. Le
    /// **programme est un seul jeton** (piège `portable-pty`) ; les args sont
    /// séparés. Sert de volet racine aux sessions agent (M2).
    pub fn spawn_command(
        cols: u16,
        rows: u16,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        notifier: Arc<Notifier>,
    ) -> Result<Arc<Pane>> {
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

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("lancement du programme")?;
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
                master: Some(pair.master),
                child: Some(child),
                cols,
                rows,
                exit_code: None,
                copy: None,
                subscribers: Vec::new(),
            }),
            notifier,
        });

        let reader_pane = Arc::clone(&pane);
        std::thread::spawn(move || reader_loop(reader_pane, reader));

        let waiter_pane = Arc::clone(&pane);
        std::thread::spawn(move || wait_for_exit(waiter_pane));
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
        if let Some(m) = st.master.as_ref() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
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

    /// Contenu visible du volet sous forme de texte (pour `capture-pane`).
    pub fn capture_text(&self) -> String {
        let st = self.state.lock().unwrap();
        let grid = st.terminal.grid();
        let mut lines = Vec::with_capacity(grid.rows() as usize);
        for row in 0..grid.rows() {
            let line: String = grid
                .row(row)
                .iter()
                .filter(|c| c.width != 0)
                .map(|c| c.ch)
                .collect();
            lines.push(line.trim_end().to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\r\n")
    }

    pub fn in_copy_mode(&self) -> bool {
        self.state.lock().unwrap().copy.is_some()
    }

    /// Indicateur de statut du mode copie : « COPIE n/total », ou la saisie de
    /// recherche en cours (« /motif »).
    pub fn copy_status(&self) -> Option<String> {
        let st = self.state.lock().unwrap();
        st.copy.as_ref().map(|cm| match &cm.search_input {
            Some(si) => {
                let prefix = if si.backward { '?' } else { '/' };
                format!("{prefix}{}", si.query)
            }
            None => {
                let total = total_lines(&st.terminal);
                format!("COPIE {}/{}", cm.cursor_line + 1, total)
            }
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
            search_input: None,
            last_search: None,
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

        // --- Saisie d'une recherche (après `/` ou `?`) ---
        if cm.search_input.is_some() {
            match byte {
                0x0d => {
                    // Entrée : exécuter la recherche.
                    let search = cm.search_input.take().unwrap();
                    if !search.query.is_empty() {
                        run_search(cm, &st.terminal, &search);
                        cm.last_search = Some(search);
                    }
                }
                0x1b => cm.search_input = None, // Échap : annuler
                0x08 | 0x7f => {
                    cm.search_input.as_mut().unwrap().query.pop();
                }
                b if (0x20..0x7f).contains(&b) => {
                    cm.search_input.as_mut().unwrap().query.push(b as char);
                }
                _ => {}
            }
            keep_cursor_visible(cm, rows);
            self.notifier.bump();
            return CopyAction::None;
        }

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
            b'w' => {
                cm.cursor_col = word_forward(
                    &logical_line(&st.terminal, cm.cursor_line),
                    cm.cursor_col,
                    cols,
                )
            }
            b'b' => {
                cm.cursor_col =
                    word_backward(&logical_line(&st.terminal, cm.cursor_line), cm.cursor_col)
            }
            b'/' => {
                cm.search_input = Some(Search {
                    backward: false,
                    query: String::new(),
                })
            }
            b'?' => {
                cm.search_input = Some(Search {
                    backward: true,
                    query: String::new(),
                })
            }
            b'n' | b'N' => {
                if let Some(mut search) = cm.last_search.clone() {
                    if byte == b'N' {
                        search.backward = !search.backward;
                    }
                    run_search(cm, &st.terminal, &search);
                }
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

        if let Some(cm) = st.copy.as_mut() {
            keep_cursor_visible(cm, rows);
        }
        self.notifier.bump();
        CopyAction::None
    }

    /// Défilement molette : entre en mode copie si nécessaire et déplace la vue.
    pub fn scroll(&self, up: bool, lines: u16) {
        let mut guard = self.state.lock().unwrap();
        let st = &mut *guard;
        if st.copy.is_none() {
            let history = st.terminal.history().len();
            let (ccol, crow) = st.terminal.cursor();
            let cursor_line = history + crow as usize;
            let view_top = (history + st.rows as usize).saturating_sub(st.rows as usize);
            st.copy = Some(CopyMode {
                view_top,
                cursor_line,
                cursor_col: ccol,
                anchor: None,
                search_input: None,
                last_search: None,
            });
        }
        let total = total_lines(&st.terminal);
        let rows = st.rows as usize;
        if let Some(cm) = st.copy.as_mut() {
            if up {
                cm.cursor_line = cm.cursor_line.saturating_sub(lines as usize);
            } else {
                cm.cursor_line = (cm.cursor_line + lines as usize).min(total.saturating_sub(1));
            }
            keep_cursor_visible(cm, rows);
        }
        drop(guard);
        self.notifier.bump();
    }

    pub fn is_alive(&self) -> bool {
        self.state.lock().unwrap().exit_code.is_none()
    }

    /// Code de sortie du processus du volet, ou `None` s'il tourne encore (M1).
    pub fn exit_code(&self) -> Option<u32> {
        self.state.lock().unwrap().exit_code
    }

    pub fn kill(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(child) = st.child.as_mut() {
            let _ = child.kill();
        }
        // Fermer la ConPTY (drop du master) provoque l'EOF du reader_loop parqué
        // sur read() : ConPTY n'EOF pas sur sortie propre, mais fermer le master
        // débloque le lecteur, qui se termine et relâche son Arc<Pane> (correctif
        // M1 : sinon tuer un agent terminé fuit un thread + un handle).
        st.master.take();
    }
}

/// Détecte la sortie du process racine indépendamment du flux de lecture PTY
/// (M1). Sous Windows, ConPTY ne ferme pas forcément le tuyau de sortie quand
/// le shell quitte proprement — le pseudo-terminal reste ouvert tant que le
/// `master` est vivant — donc `reader_loop` peut ne jamais observer d'EOF.
/// On sonde directement `try_wait()` sur le process.
fn wait_for_exit(pane: Arc<Pane>) {
    loop {
        let done = {
            let mut st = pane.state.lock().unwrap();
            if st.exit_code.is_some() {
                return;
            }
            st.child.as_mut().and_then(|c| c.try_wait().ok().flatten())
        };
        if let Some(status) = done {
            let mut st = pane.state.lock().unwrap();
            if st.exit_code.is_none() {
                st.exit_code = Some(status.exit_code());
                drop(st);
                pane.notifier.bump();
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn reader_loop(pane: Arc<Pane>, mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let rang = {
                    let mut st = pane.state.lock().unwrap();
                    st.terminal.advance(&buf[..n]);
                    let responses = st.terminal.take_responses();
                    if !responses.is_empty() {
                        let _ = st.writer.write_all(&responses);
                        let _ = st.writer.flush();
                    }
                    // Diffuser le flux brut aux clients GUI abonnés.
                    st.subscribers
                        .retain(|tx| tx.send((pane.id, buf[..n].to_vec())).is_ok());
                    // Cloche détectée par l'émulateur pendant l'advance (sous le verrou).
                    st.terminal.take_bell()
                };
                if rang {
                    // Hors du verrou du volet : le drapeau vit sur le Notifier partagé.
                    pane.notifier.signal_bell();
                }
                pane.notifier.bump();
            }
        }
    }

    // Filet de sécurité : si l'EOF du lecteur arrive avant `wait_for_exit`
    // (ex. pseudo-console fermée), on détecte quand même la sortie ici.
    let child = {
        let mut st = pane.state.lock().unwrap();
        if st.exit_code.is_some() {
            return;
        }
        st.child.take()
    };
    let code = child
        .and_then(|mut c| c.wait().ok())
        .map(|status| status.exit_code())
        .unwrap_or(0);

    let mut st = pane.state.lock().unwrap();
    if st.exit_code.is_none() {
        st.exit_code = Some(code);
        drop(st);
        pane.notifier.bump();
    }
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

/// Réajuste `view_top` pour garder le curseur de copie visible.
fn keep_cursor_visible(cm: &mut CopyMode, rows: usize) {
    if cm.cursor_line < cm.view_top {
        cm.view_top = cm.cursor_line;
    } else if rows > 0 && cm.cursor_line >= cm.view_top + rows {
        cm.view_top = cm.cursor_line + 1 - rows;
    }
}

fn is_word(c: char) -> bool {
    !c.is_whitespace()
}

/// Colonne du début du mot suivant sur la ligne.
fn word_forward(cells: &[Cell], col: u16, cols: u16) -> u16 {
    let n = cells.len().min(cols as usize);
    let mut i = col as usize;
    while i < n && is_word(cells[i].ch) {
        i += 1;
    }
    while i < n && !is_word(cells[i].ch) {
        i += 1;
    }
    i.min(n.saturating_sub(1)) as u16
}

/// Colonne du début du mot précédent sur la ligne.
fn word_backward(cells: &[Cell], col: u16) -> u16 {
    let mut i = col as usize;
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !is_word(cells[i].ch) {
        i -= 1;
    }
    while i > 0 && is_word(cells[i - 1].ch) {
        i -= 1;
    }
    i as u16
}

/// Exécute une recherche depuis le curseur ; déplace le curseur sur la première
/// correspondance trouvée (sinon le laisse en place).
fn run_search(cm: &mut CopyMode, term: &Terminal, search: &Search) {
    let total = total_lines(term);
    let needle: Vec<char> = search.query.chars().collect();
    if needle.is_empty() {
        return;
    }
    let chars_of = |idx: usize| -> Vec<char> {
        logical_line(term, idx)
            .iter()
            .filter(|c| c.width != 0)
            .map(|c| c.ch)
            .collect()
    };

    if !search.backward {
        for line in cm.cursor_line..total {
            let hay = chars_of(line);
            let from = if line == cm.cursor_line {
                cm.cursor_col as usize + 1
            } else {
                0
            };
            if let Some(pos) = find_sub(&hay, &needle, from) {
                cm.cursor_line = line;
                cm.cursor_col = pos as u16;
                return;
            }
        }
    } else {
        for line in (0..=cm.cursor_line).rev() {
            let hay = chars_of(line);
            let upto = if line == cm.cursor_line {
                cm.cursor_col as usize
            } else {
                hay.len()
            };
            if let Some(pos) = rfind_sub(&hay, &needle, upto) {
                cm.cursor_line = line;
                cm.cursor_col = pos as u16;
                return;
            }
        }
    }
}

fn find_sub(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn rfind_sub(hay: &[char], needle: &[char], upto: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let max_start = upto.min(hay.len() - needle.len() + 1);
    (0..max_start)
        .rev()
        .find(|&i| hay[i..i + needle.len()] == *needle)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_cloche() {
        let n = Notifier::new();
        assert!(!n.bell(), "cloche neuve à faux");
        n.signal_bell();
        assert!(n.bell());
        n.clear_bell();
        assert!(!n.bell());
    }

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

    #[test]
    fn grid_to_ansi_fidelite_couleurs_attributs() {
        let mut term = wimux_vt::Terminal::new(30, 2);
        // "AB" : fond bleu (44) + gras (1) ; "C" : retour au defaut ;
        // "D" : fg 256 couleurs (38;5;200) ; "E" : fg RGB (38;2;10;20;30).
        term.advance(b"\x1b[44;1mAB\x1b[0mC\x1b[38;5;200mD\x1b[0m\x1b[38;2;10;20;30mE\x1b[0m");

        let bytes = grid_to_ansi(term.grid(), term.cursor());
        let mut term2 = wimux_vt::Terminal::new(30, 2);
        term2.advance(&bytes);

        // La sortie re-parsee doit restituer char ET pen a l'identique.
        let g1 = term.grid();
        let g2 = term2.grid();
        for col in 0..5u16 {
            let c1 = g1.cell(col, 0).unwrap();
            let c2 = g2.cell(col, 0).unwrap();
            assert_eq!(c2.ch, c1.ch, "caractere different en colonne {col}");
            assert_eq!(c2.pen, c1.pen, "pen different en colonne {col}");
        }
    }

    #[test]
    fn notifier_horodatage_apres_bump() {
        let n = Notifier::new();
        n.bump();
        assert!(
            n.last_output_elapsed() < Duration::from_secs(1),
            "juste après un bump, l'écoulement doit être petit"
        );
    }

    #[test]
    fn notifier_neuf_a_un_horodatage_defini() {
        let n = Notifier::new();
        // Un Notifier neuf initialise last_output_at à Instant::now() : l'écoulement
        // est défini et petit tout de suite après la création.
        assert!(n.last_output_elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn exit_code_none_pour_volet_vivant() {
        let n = Notifier::new();
        let p = Pane::spawn(20, 5, "cmd.exe", n).unwrap();
        assert_eq!(
            p.exit_code(),
            None,
            "un volet vivant n'a pas de code de sortie"
        );
        p.kill();
    }

    #[test]
    fn kill_ferme_le_master_et_libere_le_lecteur() {
        let n = Notifier::new();
        let pane = Pane::spawn(20, 5, "cmd.exe", n).unwrap();
        // Faire sortir le shell proprement (code 0).
        pane.send_input(b"exit 0\r\n");
        // Attendre la détection de la sortie (wait_for_exit renseigne exit_code).
        let deadline = Instant::now() + Duration::from_secs(10);
        while pane.exit_code().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            pane.exit_code().is_some(),
            "le shell aurait dû sortir (exit 0)"
        );
        // À ce stade, le reader_loop reste parqué sur read() (ConPTY n'EOF pas sur
        // sortie propre) et retient un Arc<Pane> -> strong_count >= 2.
        // kill() ferme le master -> EOF -> le thread lecteur se termine.
        pane.kill();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&pane) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            Arc::strong_count(&pane),
            1,
            "après kill (master fermé), le thread lecteur doit relâcher son Arc<Pane>"
        );
    }
}
