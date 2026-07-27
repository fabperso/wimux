// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
//! Une session : un ensemble de fenêtres, chacune contenant un arbre de volets.
//! La session est **la source de vérité** : ses volets tournent en continu, et à
//! chaque changement les clients attachés sont réveillés puis reçoivent une
//! composition (volets + bordures + barre de statut) rendue par le serveur.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use wimux_protocol::{AgentStatus, Frame, LayoutNode, PaneInfo, WindowInfo};
use wimux_vt::{Color, Grid, Pen};

use crate::pane::{CopyAction, Notifier, Pane, PaneId};
use crate::window::{Move, Rect, SplitDir, Window};

/// Compteur d'ordre d'affichage des sessions dans le rail (W4/W5). À la création
/// une session prend le prochain numéro (donc s'ajoute EN FIN) ; le glisser-déposer
/// réaffecte les ordres via `set_order`.
static NEXT_SESSION_ORDER: AtomicU64 = AtomicU64::new(0);

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
    /// Identifiant de lot (M3), posé par `set_group` à la création du lot.
    group: Mutex<Option<String>>,
    /// Worktree git isolé de cette session de lot (M3), posé par `set_worktree`.
    /// Nettoyé (`worktree::remove`) au `kill`.
    worktree: Mutex<Option<crate::worktree::Worktree>>,
    /// Ordre d'affichage dans le rail (W4/W5) : `list` trie dessus, le
    /// glisser-déposer le réaffecte.
    order: AtomicU64,
    /// Couleur d'accent du workspace (hex `#rrggbb`), `None` = défaut (W5).
    color: Mutex<Option<String>>,
    /// Workspace épinglé : trié en tête du rail (W5).
    pinned: AtomicBool,
    /// Compteur de révision de topologie de volets (A1) : bumpé aux créations/
    /// fermetures de volet ; lu par la GUI pour se réattacher et refléter les
    /// volets créés en CLI.
    layout_rev: AtomicU64,
}

impl Session {
    pub fn new(name: String, cols: u16, rows: u16, shell: &str) -> Result<Arc<Session>> {
        let notifier = Notifier::new();
        let pane = spawn_shell_pane_with(cols, content_rows(rows), shell, &notifier, &name)?;
        let window = Window::new(pane);

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
            group: Mutex::new(None),
            worktree: Mutex::new(None),
            order: AtomicU64::new(NEXT_SESSION_ORDER.fetch_add(1, Ordering::Relaxed)),
            color: Mutex::new(None),
            pinned: AtomicBool::new(false),
            layout_rev: AtomicU64::new(0),
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
            crate::pane::PaneSpawnCtx::shell(&name),
        )?;
        let window = Window::new(pane);

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
            group: Mutex::new(None),
            worktree: Mutex::new(None),
            order: AtomicU64::new(NEXT_SESSION_ORDER.fetch_add(1, Ordering::Relaxed)),
            color: Mutex::new(None),
            pinned: AtomicBool::new(false),
            layout_rev: AtomicU64::new(0),
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

    /// Ordre d'affichage de la session dans le rail (W4/W5).
    pub fn order(&self) -> u64 {
        self.order.load(Ordering::Relaxed)
    }

    /// Réaffecte l'ordre d'affichage (glisser-déposer).
    pub fn set_order(&self, order: u64) {
        self.order.store(order, Ordering::Relaxed);
    }

    /// Couleur d'accent du workspace (hex `#rrggbb`), `None` = défaut (W5).
    pub fn color(&self) -> Option<String> {
        self.color.lock().unwrap().clone()
    }

    /// Fixe (ou efface avec `None`) la couleur d'accent du workspace (W5).
    pub fn set_color(&self, color: Option<String>) {
        *self.color.lock().unwrap() = color;
    }

    /// Le workspace est-il épinglé (trié en tête du rail) ? (W5)
    pub fn pinned(&self) -> bool {
        self.pinned.load(Ordering::Relaxed)
    }

    /// Épingle ou désépingle le workspace (W5).
    pub fn set_pinned(&self, pinned: bool) {
        self.pinned.store(pinned, Ordering::Relaxed);
    }

    /// Révision courante de la topologie de volets (A1).
    pub fn layout_rev(&self) -> u64 {
        self.layout_rev.load(Ordering::Relaxed)
    }

    /// Incrémente la révision de topologie (à chaque création/fermeture de volet).
    fn bump_layout_rev(&self) {
        self.layout_rev.fetch_add(1, Ordering::Relaxed);
    }

    fn active_pane(&self) -> Option<Arc<Pane>> {
        let inner = self.inner.lock().unwrap();
        inner
            .windows
            .get(inner.active_window)
            .and_then(|w| w.active_term_pane())
    }

    /// cwd à afficher pour la fenêtre active (source du cwd de session, W3).
    /// Passe par `Window::label_cwd()` : un volet navigateur actif ne doit pas
    /// faire disparaître le cwd si un autre volet terminal de la fenêtre en a
    /// un (régression B1 corrigée).
    pub fn active_pane_cwd(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .windows
            .get(inner.active_window)
            .and_then(|w| w.label_cwd())
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
        let new_pane = self.spawn_shell_pane(1, 1).ok()?;
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

    /// Projette les fenêtres en `WindowInfo` (nom de chaque fenêtre) + l'index de
    /// la fenêtre active (W2).
    pub fn window_list(&self) -> (Vec<WindowInfo>, u32) {
        let inner = self.inner.lock().unwrap();
        let windows = inner
            .windows
            .iter()
            .map(|w| WindowInfo {
                name: w.name(),
                cwd: w.label_cwd(),
            })
            .collect();
        (windows, inner.active_window as u32)
    }

    /// Crée une fenêtre (onglet) : spawn un volet shell HORS verrou (comme
    /// `gui_split`), pousse une `Window` neuve (comme `Session::new`) et la rend
    /// active. Renvoie la nouvelle `window_list()` (W2).
    pub fn gui_new_window(&self) -> (Vec<WindowInfo>, u32) {
        // Ne pas tenir le verrou pendant le spawn (qui lance un processus).
        let new_pane = self.spawn_shell_pane(1, 1);
        let result = {
            let mut inner = self.inner.lock().unwrap();
            if let Ok(pane) = new_pane {
                inner.windows.push(Window::new(pane));
                inner.active_window = inner.windows.len() - 1;
                let area = content_area(inner.cols, inner.rows);
                let aw = inner.active_window;
                inner.windows[aw].reflow(area);
            }
            let windows = inner
                .windows
                .iter()
                .map(|w| WindowInfo {
                    name: w.name(),
                    cwd: w.label_cwd(),
                })
                .collect();
            (windows, inner.active_window as u32)
        };
        self.notifier.bump();
        result
    }

    /// Rend active la fenêtre `index` (bornée). `None` si l'index est hors borne (W2).
    pub fn gui_select_window(&self, index: u32) -> Option<(Vec<WindowInfo>, u32)> {
        let result = {
            let mut inner = self.inner.lock().unwrap();
            let idx = index as usize;
            if idx >= inner.windows.len() {
                return None;
            }
            inner.active_window = idx;
            let area = content_area(inner.cols, inner.rows);
            inner.windows[idx].reflow(area);
            let windows = inner
                .windows
                .iter()
                .map(|w| WindowInfo {
                    name: w.name(),
                    cwd: w.label_cwd(),
                })
                .collect();
            (windows, inner.active_window as u32)
        };
        self.notifier.bump();
        Some(result)
    }

    /// Ferme la fenêtre `index` (tue ses volets) et réajuste `active_window`.
    /// `None` (no-op) si l'index est hors borne OU s'il ne reste qu'une fenêtre (W2).
    pub fn gui_close_window(&self, index: u32) -> Option<(Vec<WindowInfo>, u32)> {
        let result = {
            let mut inner = self.inner.lock().unwrap();
            let idx = index as usize;
            if idx >= inner.windows.len() || inner.windows.len() == 1 {
                return None;
            }
            inner.windows[idx].kill_all();
            inner.windows.remove(idx);
            // Borne `active_window` comme `gui_close` (il reste ≥ 1 fenêtre ici).
            if inner.active_window >= inner.windows.len() {
                inner.active_window = inner.windows.len() - 1;
            }
            let area = content_area(inner.cols, inner.rows);
            let aw = inner.active_window;
            inner.windows[aw].reflow(area);
            let windows = inner
                .windows
                .iter()
                .map(|w| WindowInfo {
                    name: w.name(),
                    cwd: w.label_cwd(),
                })
                .collect();
            (windows, inner.active_window as u32)
        };
        self.notifier.bump();
        Some(result)
    }

    /// Nomme la fenêtre `index` ; un nom vide efface le nom (`None`) (W2).
    pub fn gui_rename_window(&self, index: u32, name: String) {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(win) = inner.windows.get_mut(index as usize) {
                let name = if name.is_empty() { None } else { Some(name) };
                win.set_name(name);
            }
        }
        self.notifier.bump();
    }

    /// Frappe GUI vers le volet DÉSIGNÉ de la fenêtre active (repli : volet actif
    /// si l'id est introuvable, ex. course avec une fermeture).
    ///
    /// Le repli sur le volet actif ne doit jouer QUE si `pane_id` est vraiment
    /// introuvable (course avec une fermeture) — pas si c'est un volet
    /// NAVIGATEUR de cette fenêtre : `w.pane(pane_id)` renvoie `None` dans les
    /// deux cas (il ne connaît que les terminaux), donc sans ce distinguo une
    /// frappe destinée à un navigateur serait routée vers le terminal actif au
    /// lieu d'être ignorée.
    pub fn gui_input(&self, pane_id: u64, bytes: &[u8]) {
        let pane = {
            let inner = self.inner.lock().unwrap();
            let win = inner.windows.get(inner.active_window);
            win.and_then(|w| {
                w.pane(pane_id).or_else(|| {
                    if w.contains(pane_id) {
                        None
                    } else {
                        w.active_term_pane()
                    }
                })
            })
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

    /// A1 : découpe la fenêtre active à partir de `from_pane` (défaut : volet
    /// actif) et lance `program`/`args` (journalisé) dans le nouveau volet.
    /// Renvoie l'id du volet créé, ou `None` si le spawn échoue / plus de fenêtre.
    pub fn spawn_pane(
        &self,
        from_pane: Option<u64>,
        dir: SplitDir,
        cwd: Option<&str>,
        program: &str,
        args: &[String],
    ) -> Option<u64> {
        // Spawn HORS verrou (lance un processus).
        let ctx = crate::pane::PaneSpawnCtx::agent(&self.name());
        let new_pane =
            Pane::spawn_command(1, 1, program, args, cwd, Arc::clone(&self.notifier), ctx).ok()?;
        let new_id = new_pane.id;
        {
            let mut inner = self.inner.lock().unwrap();
            // Le volet d'origine peut vivre dans une fenêtre NON active (un Claude
            // orchestrateur dans un onglet d'arrière-plan) : on découpe là où il
            // se trouve, pour que l'agent apparaisse à côté de lui. À défaut
            // (`from_pane` absent ou introuvable), on retombe sur la fenêtre active.
            let target_window = from_pane
                .and_then(|id| inner.windows.iter().position(|w| w.pane(id).is_some()))
                .unwrap_or(inner.active_window);
            let Some(win) = inner.windows.get_mut(target_window) else {
                drop(inner);
                new_pane.kill();
                return None;
            };
            let target = from_pane
                .filter(|id| win.pane(*id).is_some())
                .unwrap_or_else(|| win.active_pane_id());
            win.split_pane(target, dir, Arc::clone(&new_pane));
            let area = content_area(inner.cols, inner.rows);
            inner.windows[target_window].reflow(area);
        }
        self.bump_layout_rev();
        self.notifier.bump();
        Some(new_id)
    }

    /// B1 : ouvre un volet NAVIGATEUR en découpant depuis `from_pane` (défaut :
    /// volet actif de la fenêtre active). Renvoie l'id du volet créé.
    ///
    /// Comme `spawn_pane`, on découpe la fenêtre où se trouve réellement
    /// `from_pane`, pour que le navigateur apparaisse à côté de son demandeur.
    ///
    /// SÉCURITÉ : refuse toute URL dont le schéma n'est pas `http(s)` — voir
    /// `url_autorisee`.
    pub fn open_web_pane(&self, from_pane: Option<u64>, dir: SplitDir, url: String) -> Option<u64> {
        if !url_autorisee(&url) {
            return None;
        }
        let web = Arc::new(crate::webpane::WebPane::new(
            crate::pane::next_pane_id(),
            url,
        ));
        let new_id = web.id;
        {
            let mut inner = self.inner.lock().unwrap();
            let target_window = from_pane
                .and_then(|id| inner.windows.iter().position(|w| w.contains(id)))
                .unwrap_or(inner.active_window);
            let win = inner.windows.get_mut(target_window)?;
            let target = from_pane
                .filter(|id| win.contains(*id))
                .unwrap_or_else(|| win.active_pane_id());
            win.split_web(target, dir, Arc::clone(&web));
            let area = content_area(inner.cols, inner.rows);
            inner.windows[target_window].reflow(area);
        }
        self.bump_layout_rev();
        self.notifier.bump();
        Some(new_id)
    }

    /// B1 : fait naviguer un volet navigateur. `false` si l'id n'est pas un
    /// volet navigateur de cette session.
    ///
    /// SÉCURITÉ : refuse toute URL dont le schéma n'est pas `http(s)` — voir
    /// `url_autorisee`. Dans ce cas l'URL courante du volet n'est PAS modifiée.
    pub fn web_navigate(&self, pane_id: u64, url: String) -> bool {
        if !url_autorisee(&url) {
            return false;
        }
        let Some(web) = self.find_web(pane_id) else {
            return false;
        };
        web.navigate(url);
        self.notifier.bump();
        true
    }

    /// B1 : recule d'un cran. `None` si l'id n'est pas un volet navigateur de
    /// cette session ; `Some(false)` si on est déjà en tête de pile (no-op
    /// légitime, PAS une erreur) ; `Some(true)` si le déplacement a eu lieu.
    pub fn web_back(&self, pane_id: u64) -> Option<bool> {
        let web = self.find_web(pane_id)?;
        let moved = web.back();
        if moved {
            self.notifier.bump();
        }
        Some(moved)
    }

    /// B1 : avance d'un cran. Mêmes conventions que `web_back`.
    pub fn web_forward(&self, pane_id: u64) -> Option<bool> {
        let web = self.find_web(pane_id)?;
        let moved = web.forward();
        if moved {
            self.notifier.bump();
        }
        Some(moved)
    }

    /// Cherche un volet navigateur par id dans toutes les fenêtres.
    fn find_web(&self, pane_id: u64) -> Option<Arc<crate::webpane::WebPane>> {
        let inner = self.inner.lock().unwrap();
        inner.windows.iter().find_map(|w| w.web_pane(pane_id))
    }

    /// A1 : contenu visible du volet `pane_id` (n'importe quelle fenêtre), texte.
    /// B1 : pour un volet navigateur, renvoie le substitut textuel plutôt qu'une
    /// erreur — l'appelant obtient une réponse utile, pas un échec.
    pub fn capture_pane(&self, pane_id: u64) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        for win in &inner.windows {
            if let Some(p) = win.pane(pane_id) {
                return Some(p.capture_text());
            }
            if let Some(w) = win.web_pane(pane_id) {
                return Some(format!("[navigateur]\r\n{}", w.url()));
            }
        }
        None
    }

    /// A1 : inventaire structuré des volets de toutes les fenêtres.
    pub fn pane_infos(&self) -> Vec<PaneInfo> {
        let inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        for win in &inner.windows {
            for id in win.pane_ids() {
                if let Some(p) = win.pane(id) {
                    out.push(PaneInfo {
                        pane_id: id,
                        cwd: p.cwd(),
                        running: p.is_alive(),
                        exit_code: p.exit_code().map(|c| c as i32),
                        log_path: p.log_path(),
                    });
                }
            }
        }
        out
    }

    /// A1 : envoie des octets au volet `pane_id`. `false` si l'id est introuvable.
    pub fn send_keys_pane(&self, pane_id: u64, bytes: &[u8]) -> bool {
        let pane = {
            let inner = self.inner.lock().unwrap();
            inner.windows.iter().find_map(|w| w.pane(pane_id))
        };
        match pane {
            Some(p) => {
                p.send_input(bytes);
                true
            }
            None => false,
        }
    }

    /// A1 : ferme le volet `pane_id` (dans quelque fenêtre que ce soit). Retire la
    /// fenêtre si elle devient vide. `false` si l'id est introuvable.
    ///
    /// B1 : porte sur `win.contains` (et non `win.pane`) pour fermer aussi bien
    /// un volet TERMINAL qu'un volet NAVIGATEUR — `win.pane` ne reconnaît que les
    /// terminaux, et un volet navigateur qu'on ne peut jamais fermer serait un
    /// volet fantôme qu'un agent aurait créé sans pouvoir le refermer.
    /// `Window::close_pane` gère déjà les deux natures (il ne tue le processus
    /// que pour un terminal).
    ///
    /// Purge best-effort du journal (spec : « purge best-effort au kill ») : le
    /// chemin est capturé AVANT la fermeture (pendant qu'on peut encore atteindre
    /// le volet), puis le fichier est supprimé APRÈS — sans jamais faire échouer
    /// l'appel si la suppression échoue (fichier déjà absent, verrou, etc.).
    pub fn kill_pane(&self, pane_id: u64) -> bool {
        let (found, log_path) = {
            let mut inner = self.inner.lock().unwrap();
            let mut hit = None;
            let mut log_path = None;
            for (wi, win) in inner.windows.iter_mut().enumerate() {
                if win.contains(pane_id) {
                    // `None` pour un volet navigateur (pas de journal à purger).
                    log_path = win.pane(pane_id).and_then(|p| p.log_path());
                    let empty = win.close_pane(pane_id);
                    hit = Some((wi, empty));
                    break;
                }
            }
            if let Some((wi, empty)) = hit {
                if empty {
                    inner.windows.remove(wi);
                    if inner.active_window >= inner.windows.len() && !inner.windows.is_empty() {
                        inner.active_window = inner.windows.len() - 1;
                    }
                }
                (true, log_path)
            } else {
                (false, None)
            }
        };
        if found {
            if let Some(path) = log_path {
                let _ = std::fs::remove_file(path);
            }
            self.reflow();
            self.bump_layout_rev();
            self.notifier.bump();
        }
        found
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

    /// W6 : marque la session comme « non lue » (repose le drapeau cloche pour
    /// afficher un indicateur dans le rail).
    pub fn mark_unread(&self) {
        self.notifier.signal_bell();
    }

    /// G4 : la session a-t-elle produit de la sortie depuis la dernière vue ?
    pub fn has_activity(&self) -> bool {
        self.notifier.generation() > self.last_seen_gen.load(Ordering::Relaxed)
    }

    /// G4 : une cloche (BEL) est-elle en attente sur cette session ?
    pub fn has_bell(&self) -> bool {
        self.notifier.bell()
    }

    /// W6 : draine les notifications OSC 9/777 émises par les volets de la session.
    pub fn drain_notifications(&self) -> Vec<crate::pane::PaneNotif> {
        self.notifier.drain_notifications()
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

    /// M3 : rattache cette session à un lot (identifiant de groupe).
    pub fn set_group(&self, group: String) {
        *self.group.lock().unwrap() = Some(group);
    }

    /// M3 : identifiant de lot de cette session, ou `None` hors lot.
    pub fn group(&self) -> Option<String> {
        self.group.lock().unwrap().clone()
    }

    /// M3 : associe un worktree git à cette session (nettoyé au `kill`).
    pub fn set_worktree(&self, wt: crate::worktree::Worktree) {
        *self.worktree.lock().unwrap() = Some(wt);
    }

    /// M3 : worktree git de cette session, ou `None` s'il n'y en a pas.
    pub fn worktree(&self) -> Option<crate::worktree::Worktree> {
        self.worktree.lock().unwrap().clone()
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

    /// Spawn d'un volet du shell de la session, avec injection OSC 7 (W3).
    fn spawn_shell_pane(&self, cols: u16, rows: u16) -> Result<Arc<Pane>> {
        let name = self.name();
        spawn_shell_pane_with(cols, rows, &self.shell, &self.notifier, &name)
    }

    /// Transmet des octets au volet actif de la fenêtre active.
    pub fn send_input(&self, bytes: &[u8]) {
        let pane = {
            let inner = self.inner.lock().unwrap();
            inner
                .windows
                .get(inner.active_window)
                .and_then(|w| w.active_term_pane())
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
        let new_pane = Pane::spawn(
            1,
            1,
            &self.shell,
            Arc::clone(&self.notifier),
            crate::pane::PaneSpawnCtx::shell(&self.name()),
        );
        if let Ok(pane) = new_pane {
            let mut inner = self.inner.lock().unwrap();
            let aw = inner.active_window;
            if let Some(win) = inner.windows.get_mut(aw) {
                win.split(dir, pane);
                let area = content_area(inner.cols, inner.rows);
                inner.windows[aw].reflow(area);
            }
            drop(inner);
            self.bump_layout_rev();
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
        self.bump_layout_rev();
        self.notifier.bump();
    }

    pub fn new_window(&self) {
        if let Ok(pane) = Pane::spawn(
            1,
            1,
            &self.shell,
            Arc::clone(&self.notifier),
            crate::pane::PaneSpawnCtx::shell(&self.name()),
        ) {
            let mut inner = self.inner.lock().unwrap();
            inner.windows.push(Window::new(pane));
            inner.active_window = inner.windows.len() - 1;
            let area = content_area(inner.cols, inner.rows);
            let aw = inner.active_window;
            inner.windows[aw].reflow(area);
            drop(inner);
            self.bump_layout_rev();
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
        // Tuer les volets sous le verrou `inner`, puis le RELÂCHER avant le
        // nettoyage git (une commande externe lente ne doit pas tenir `inner`).
        {
            let inner = self.inner.lock().unwrap();
            for win in &inner.windows {
                win.kill_all();
            }
        }
        // M3 : retirer le worktree une fois les volets tués (git worktree remove
        // --force suffit même si le process racine vient de mourir).
        if let Some(wt) = self.worktree.lock().unwrap().take() {
            crate::worktree::remove(&wt.base_repo, &wt.path, &wt.branch);
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

/// Hook de prompt PowerShell (une seule ligne = un seul argument `-Command`).
/// Capture le prompt courant (`$function:prompt`, déjà défini par le profil s'il
/// existe, car `-Command` s'exécute APRÈS le profil), puis le remplace par une
/// version qui, à chaque invite, émet `ESC ]7;file:///<cwd-url-encodé> BEL` sur
/// la console AVANT de rappeler l'ancien prompt (préservation). L'hôte est laissé
/// vide (`file:///…`, accepté par le renifleur). Pas de guillemets doubles :
/// évite tout double-échappement à la frontière du spawn (portable-pty).
const OSC7_PS_SNIPPET: &str = "$__wimux_op=$function:prompt;function global:prompt{$__wimux_u=[uri]::EscapeUriString(((Get-Location).ProviderPath -replace '\\\\','/'));[Console]::Write(([string][char]27)+']7;file:///'+$__wimux_u+([string][char]7));& $__wimux_op}";

/// Détermine les arguments de spawn injectant l'émission OSC 7 pour un shell
/// PowerShell/pwsh. `None` pour tout autre shell (cmd.exe, bash…) → pas
/// d'injection (repli sans régression : le cwd restera `None`).
pub fn osc7_prompt_injection(shell: &str) -> Option<Vec<String>> {
    let base = shell.rsplit(['\\', '/']).next().unwrap_or(shell);
    let base = base.to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    if base == "powershell" || base == "pwsh" {
        Some(vec![
            "-NoExit".to_string(),
            "-Command".to_string(),
            OSC7_PS_SNIPPET.to_string(),
        ])
    } else {
        None
    }
}

/// N'autorise, pour un volet navigateur, qu'une URL de schéma `http://` ou
/// `https://` (comparaison insensible à la casse) qui ne pointe PAS sur
/// l'origine de l'application elle-même.
///
/// POURQUOI (schéma) : cette URL est posée dans le `src` d'un `<iframe>` de la
/// GUI Tauri (`tauri.conf.json` a `csp: null` et `withGlobalTauri: true`). Une
/// URL de schéma `javascript:` posée en `src` d'iframe s'exécute en HÉRITANT DE
/// L'ORIGINE DU PARENT (contrairement à une iframe HTTP cross-origin, isolée) :
/// le script atteindrait donc `parent.__TAURI__.core.invoke` et toute la
/// surface de commandes de l'application (`kill_session`, `create_agent`, …).
///
/// POURQUOI (origine de l'application) : filtrer le schéma ne suffit PAS. La
/// GUI est servie sur `http://localhost:1420` en dev (`devUrl` de
/// `tauri.conf.json`) et sur `http(s)://tauri.localhost` en production
/// (WebView2/Tauri v2 Windows). Une iframe pointée sur CETTE origine est
/// same-origin avec son parent : elle atteint donc, elle aussi,
/// `window.parent.__TAURI__.core.invoke` et toute la surface de commandes —
/// exactement la même faille que `javascript:`, juste par un autre chemin. On
/// refuse donc en plus l'hôte `tauri.localhost` (toute casse) et
/// `localhost`/`127.0.0.1`/`[::1]` sur le port `1420` précisément (pas un
/// simple préfixe de port : `http://localhost:1420x/` doit rester autorisé, et
/// surtout `http://localhost:5173/`, le cas d'usage visé — un serveur de dev
/// quelconque — ne doit PAS être bloqué).
///
/// Aujourd'hui l'URL vient de l'utilisateur ou de la CLI locale, mais la phase
/// B2 fera poser des URL par un agent à partir de contenu lu sur le web — sans
/// ce garde-fou ce serait un chemin d'exécution de commandes hôte piloté par du
/// contenu non fiable. Ce filtre doit être appliqué aux DEUX points d'entrée
/// (`open_web_pane` et `web_navigate`) car la CLI n'entre pas par le frontend.
///
/// Visibilité `pub(crate)` (Fix 5) : `daemon.rs` doit pouvoir la ré-appeler en
/// amont de `open_web_pane`/`web_navigate` pour distinguer, dans le message
/// d'erreur renvoyé au client, une URL refusée d'un volet/fenêtre introuvable.
pub(crate) fn url_autorisee(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    !cible_origine_application(&lower)
}

/// `url_lower` (déjà en minuscules, schéma http(s) déjà vérifié) désigne-t-elle
/// l'origine de l'application Tauri elle-même ? Voir `url_autorisee` pour le
/// pourquoi.
fn cible_origine_application(url_lower: &str) -> bool {
    let Some((_, after_scheme)) = url_lower.split_once("://") else {
        return false;
    };
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    // Retire un éventuel `user:pass@` avant l'hôte (rare, par prudence).
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = split_host_port(authority);

    if host == "tauri.localhost" {
        return true;
    }
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") && port == Some("1420")
}

/// Sépare une autorité `hôte[:port]` en `(hôte, port)`. Gère l'hôte IPv6 entre
/// crochets (`[::1]:1420`), qui contient lui-même des `:`.
fn split_host_port(authority: &str) -> (&str, Option<&str>) {
    if authority.starts_with('[') {
        if let Some(close) = authority.find(']') {
            let host = &authority[..=close];
            let port = authority[close + 1..].strip_prefix(':');
            return (host, port);
        }
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    }
}

/// Spawn d'un volet shell avec injection OSC 7 conditionnelle (PowerShell/pwsh).
fn spawn_shell_pane_with(
    cols: u16,
    rows: u16,
    shell: &str,
    notifier: &Arc<Notifier>,
    session: &str,
) -> Result<Arc<Pane>> {
    let ctx = crate::pane::PaneSpawnCtx::shell(session);
    match osc7_prompt_injection(shell) {
        Some(args) => {
            Pane::spawn_command(cols, rows, shell, &args, None, Arc::clone(notifier), ctx)
        }
        None => Pane::spawn(cols, rows, shell, Arc::clone(notifier), ctx),
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
            wimux_protocol::LayoutNode::Leaf { pane_id, .. } => assert_eq!(pane_id, active),
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

    #[test]
    fn agent_sans_worktree_group_none_et_kill_ne_panique_pas() {
        let s = Session::new_agent(
            "a".into(),
            40,
            12,
            "cmd.exe",
            &["/c".into(), "echo".into(), "hi".into()],
            None,
        )
        .unwrap();
        // Aucun group ni worktree posé : group() est None.
        assert_eq!(s.group(), None);
        assert!(s.worktree().is_none());
        // set_group est visible via group().
        s.set_group("batch0".into());
        assert_eq!(s.group().as_deref(), Some("batch0"));
        // kill() sans worktree ne panique pas (rien à nettoyer).
        s.kill();
    }

    #[test]
    fn window_list_reflete_une_fenetre_neuve() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let (windows, active) = s.window_list();
        assert_eq!(windows.len(), 1);
        assert_eq!(active, 0);
        assert_eq!(windows[0].name, None);
        s.kill();
    }

    #[test]
    fn gui_new_window_ajoute_un_onglet_actif() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let (windows, active) = s.gui_new_window();
        assert_eq!(windows.len(), 2);
        assert_eq!(active, 1);
        s.kill();
    }

    #[test]
    fn gui_select_window_hors_borne_est_none() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        assert!(s.gui_select_window(5).is_none());
        // Index valide : active mis à jour.
        let _ = s.gui_new_window(); // 2 fenêtres, active = 1
        let (_, active) = s.gui_select_window(0).unwrap();
        assert_eq!(active, 0);
        s.kill();
    }

    #[test]
    fn gui_close_window_refuse_la_derniere() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        assert!(
            s.gui_close_window(0).is_none(),
            "fermer l'unique fenêtre doit être refusé"
        );
        s.kill();
    }

    #[test]
    fn gui_close_window_retire_et_reajuste() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        let _ = s.gui_new_window(); // 2 fenêtres, active = 1
        let (windows, active) = s.gui_close_window(1).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(active, 0);
        s.kill();
    }

    #[test]
    fn gui_rename_window_reflete_le_nom() {
        let s = Session::new("t".into(), 40, 12, "cmd.exe").unwrap();
        s.gui_rename_window(0, "build".into());
        let (windows, _) = s.window_list();
        assert_eq!(windows[0].name.as_deref(), Some("build"));
        // Nom vide -> None.
        s.gui_rename_window(0, String::new());
        let (windows, _) = s.window_list();
        assert_eq!(windows[0].name, None);
        s.kill();
    }

    #[test]
    fn injection_powershell_renvoie_les_args() {
        let args = osc7_prompt_injection("powershell.exe").expect("powershell -> Some");
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "-NoExit");
        assert_eq!(args[1], "-Command");
        assert_eq!(args[2], OSC7_PS_SNIPPET);
    }

    #[test]
    fn injection_pwsh_renvoie_les_args() {
        assert!(osc7_prompt_injection("pwsh").is_some());
        assert!(osc7_prompt_injection("pwsh.exe").is_some());
    }

    #[test]
    fn injection_insensible_casse_et_chemin() {
        assert!(osc7_prompt_injection("PowerShell.EXE").is_some());
        assert!(
            osc7_prompt_injection(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
                .is_some()
        );
    }

    #[test]
    fn injection_autre_shell_renvoie_none() {
        assert!(osc7_prompt_injection("cmd.exe").is_none());
        assert!(osc7_prompt_injection("bash").is_none());
        assert!(osc7_prompt_injection("cmd").is_none());
    }

    /// Sonde `capture_pane` jusqu'à ce qu'il contienne `needle`, dans la limite du délai.
    fn poll_capture_contains(s: &Session, pane_id: u64, needle: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if let Some(txt) = s.capture_pane(pane_id) {
                if txt.contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn spawn_pane_injecte_wimux_session_dans_l_env() {
        // Vérifie l'injection d'env de Task 2 (observable seulement via spawn_pane).
        let s = Session::new("envtest".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .spawn_pane(
                None,
                SplitDir::LeftRight,
                None,
                "cmd.exe",
                &["/c".into(), "echo".into(), "%WIMUX_SESSION%".into()],
            )
            .expect("spawn_pane doit renvoyer un id");
        assert!(
            poll_capture_contains(&s, id, "envtest", 20),
            "la sortie du volet doit contenir le nom de session injecté"
        );
        s.kill();
    }

    #[test]
    fn spawn_pane_journalise_la_sortie() {
        // Vérifie le tee de journalisation de Task 3.
        let s = Session::new("logtest".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .spawn_pane(
                None,
                SplitDir::LeftRight,
                None,
                "cmd.exe",
                &["/c".into(), "echo".into(), "HELLO_LOG".into()],
            )
            .expect("spawn_pane id");
        let info = s
            .pane_infos()
            .into_iter()
            .find(|p| p.pane_id == id)
            .expect("le volet doit être listé");
        let path = info.log_path.expect("un volet agent doit être journalisé");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("HELLO_LOG") {
                    found = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        s.kill();
        let _ = std::fs::remove_file(&path);
        assert!(found, "le journal doit contenir la sortie du volet");
    }

    #[test]
    fn spawn_pane_ajoute_un_volet_et_bump_layout_rev() {
        let s = Session::new("orch".into(), 80, 24, "cmd.exe").unwrap();
        let rev0 = s.layout_rev();
        let before = s.pane_infos().len();
        let id = s
            .spawn_pane(None, SplitDir::LeftRight, None, "cmd.exe", &[])
            .expect("un id de volet");
        assert_eq!(s.pane_infos().len(), before + 1, "un volet en plus");
        assert!(s.pane_infos().iter().any(|p| p.pane_id == id));
        assert!(s.layout_rev() > rev0, "layout_rev doit être bumpé");
        // kill_pane retire le volet et bump encore.
        let rev1 = s.layout_rev();
        assert!(s.kill_pane(id), "kill_pane doit trouver le volet");
        assert!(s.layout_rev() > rev1);
        s.kill();
    }

    #[test]
    fn volet_agent_termine_survit_au_reap() {
        let s = Session::new("keep".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .spawn_pane(
                None,
                SplitDir::LeftRight,
                None,
                "cmd.exe",
                &["/c".into(), "exit".into(), "0".into()],
            )
            .expect("spawn id");
        // Attendre la sortie du process, puis déclencher un reap via composite().
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut done = false;
        while std::time::Instant::now() < deadline {
            let _ = s.composite(); // composite() appelle reap()
            if let Some(info) = s.pane_infos().into_iter().find(|p| p.pane_id == id) {
                if !info.running {
                    assert_eq!(info.exit_code, Some(0), "exit_code conservé après reap");
                    done = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            done,
            "le volet agent terminé doit survivre au reap avec son exit_code"
        );
        s.kill();
    }

    #[test]
    fn spawn_pane_decoupe_la_fenetre_du_volet_dorigine() {
        let s = Session::new("xw".into(), 80, 24, "cmd.exe").unwrap();
        // Volet de la fenêtre de départ.
        let origin = match s.window_layout().unwrap().0 {
            LayoutNode::Leaf { pane_id, .. } => pane_id,
            _ => panic!("une session neuve n'a qu'un volet"),
        };
        // Une seconde fenêtre, qui devient active.
        let _ = s.gui_new_window();
        assert!(
            matches!(s.window_layout().unwrap().0, LayoutNode::Leaf { .. }),
            "la nouvelle fenêtre active n'a qu'un volet"
        );

        // On vise le volet de la PREMIÈRE fenêtre : la découpe doit s'y faire.
        let id = s
            .spawn_pane(Some(origin), SplitDir::LeftRight, None, "cmd.exe", &[])
            .expect("un id de volet");
        assert!(
            matches!(s.window_layout().unwrap().0, LayoutNode::Leaf { .. }),
            "la fenêtre ACTIVE ne doit pas avoir été découpée"
        );
        assert!(
            s.pane_infos().iter().any(|p| p.pane_id == id),
            "le volet agent doit exister (dans la fenêtre d'origine)"
        );
        s.kill();
    }

    #[test]
    fn send_keys_pane_cible_le_bon_volet() {
        let s = Session::new("sk".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .spawn_pane(None, SplitDir::LeftRight, None, "cmd.exe", &[])
            .unwrap();
        // Écrire "echo PONG\r" dans le volet agent, pas ailleurs.
        assert!(s.send_keys_pane(id, b"echo PONG\r\n"));
        assert!(
            poll_capture_contains(&s, id, "PONG", 20),
            "la frappe doit atteindre le volet ciblé"
        );
        assert!(!s.send_keys_pane(999_999, b"x"), "id inconnu -> false");
        s.kill();
    }

    #[test]
    fn open_web_pane_ajoute_une_feuille_navigateur() {
        let s = Session::new("web".into(), 80, 24, "cmd.exe").unwrap();
        let rev0 = s.layout_rev();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://localhost:5173/".into())
            .expect("un id de volet");
        assert!(s.layout_rev() > rev0, "layout_rev doit être bumpé");

        // La disposition annonce la feuille comme navigateur, avec son URL.
        let (tree, _) = s.window_layout().unwrap();
        assert!(
            layout_contient_web(&tree, id, "http://localhost:5173/"),
            "la feuille {id} doit être un navigateur sur la bonne URL : {tree:?}"
        );
        s.kill();
    }

    #[test]
    fn navigation_web_met_a_jour_l_url_et_l_historique() {
        let s = Session::new("web2".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://a/".into())
            .unwrap();

        assert!(s.web_navigate(id, "http://b/".into()));
        let (tree, _) = s.window_layout().unwrap();
        assert!(layout_contient_web(&tree, id, "http://b/"));

        assert_eq!(s.web_back(id), Some(true));
        let (tree, _) = s.window_layout().unwrap();
        assert!(layout_contient_web(&tree, id, "http://a/"));

        assert_eq!(s.web_forward(id), Some(true));
        let (tree, _) = s.window_layout().unwrap();
        assert!(layout_contient_web(&tree, id, "http://b/"));

        // En bout de pile (avant), reculer davantage est un no-op légitime :
        // Some(false), pas une erreur.
        assert_eq!(s.web_back(id), Some(true)); // -> http://a/ (début de pile)
        assert_eq!(s.web_back(id), Some(false), "déjà en tête de pile");

        // Un id inconnu ou un volet terminal ne sont pas navigables : None.
        assert!(!s.web_navigate(999_999, "http://x/".into()));
        assert_eq!(s.web_back(999_999), None, "id inconnu -> None");
        assert_eq!(s.web_forward(999_999), None, "id inconnu -> None");
        s.kill();
    }

    #[test]
    fn capture_pane_dun_volet_web_renvoie_le_substitut() {
        let s = Session::new("web3".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://localhost:9/".into())
            .unwrap();
        let txt = s.capture_pane(id).expect("une capture");
        assert!(txt.contains("[navigateur]"), "substitut attendu : {txt}");
        assert!(txt.contains("http://localhost:9/"), "URL attendue : {txt}");
        s.kill();
    }

    #[test]
    fn le_volet_web_est_toujours_la_apres_une_re_attache() {
        // La persistance est la raison d'être du choix « volet possédé par le
        // serveur » : une nouvelle attache doit retrouver le volet ET son URL.
        let s = Session::new("web4".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://persist/".into())
            .unwrap();
        s.web_navigate(id, "http://persist/2".into());

        // `gui_attach_window` est le cycle d'attache complet joué par la GUI.
        let (tree, _, _, _, _) = s.gui_attach_window().expect("une fenêtre active");
        assert!(
            layout_contient_web(&tree, id, "http://persist/2"),
            "après ré-attache, le volet et son URL courante sont retrouvés : {tree:?}"
        );
        s.kill();
    }

    // --- Fix 1 (sécurité) : validation du schéma d'URL --------------------

    #[test]
    fn url_autorisee_accepte_http_et_https() {
        assert!(url_autorisee("http://a/"));
        assert!(url_autorisee("https://a/"));
    }

    #[test]
    fn url_autorisee_refuse_javascript_meme_en_majuscules() {
        assert!(!url_autorisee("javascript:alert(1)"));
        assert!(
            !url_autorisee("JavaScript:alert(1)"),
            "le schéma en majuscules doit aussi être refusé"
        );
    }

    #[test]
    fn url_autorisee_refuse_lorigine_de_lapp_en_dev() {
        assert!(
            !url_autorisee("http://localhost:1420/"),
            "l'origine de dev de la GUI (devUrl) doit être refusée : iframe same-origin -> __TAURI__"
        );
    }

    #[test]
    fn url_autorisee_refuse_tauri_localhost_meme_en_majuscules() {
        assert!(
            !url_autorisee("http://TAURI.localhost/x"),
            "l'hôte de prod (WebView2) doit être refusé quelle que soit la casse"
        );
    }

    #[test]
    fn url_autorisee_accepte_un_serveur_de_dev_quelconque() {
        assert!(
            url_autorisee("http://localhost:5173/"),
            "le cas d'usage visé (serveur de dev sur un AUTRE port) doit rester autorisé"
        );
    }

    #[test]
    fn url_autorisee_ne_confond_pas_un_prefixe_de_port_avec_le_port() {
        assert!(
            url_autorisee("http://localhost:1420x/"),
            "« 1420x » n'est pas le port 1420 : ne pas bloquer sur un simple préfixe"
        );
    }

    #[test]
    fn open_web_pane_refuse_un_schema_javascript() {
        let s = Session::new("secu1".into(), 80, 24, "cmd.exe").unwrap();
        assert!(
            s.open_web_pane(None, SplitDir::LeftRight, "javascript:alert(1)".into())
                .is_none(),
            "un schéma javascript: ne doit jamais créer de volet navigateur"
        );
        s.kill();
    }

    #[test]
    fn open_web_pane_accepte_http_et_https() {
        let s = Session::new("secu2".into(), 80, 24, "cmd.exe").unwrap();
        assert!(
            s.open_web_pane(None, SplitDir::LeftRight, "http://a/".into())
                .is_some()
        );
        s.kill();
        let s2 = Session::new("secu3".into(), 80, 24, "cmd.exe").unwrap();
        assert!(
            s2.open_web_pane(None, SplitDir::LeftRight, "https://a/".into())
                .is_some()
        );
        s2.kill();
    }

    #[test]
    fn web_navigate_refuse_un_schema_javascript_et_ne_change_pas_l_url() {
        let s = Session::new("secu4".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://a/".into())
            .unwrap();
        assert!(!s.web_navigate(id, "javascript:alert(1)".into()));
        let (tree, _) = s.window_layout().unwrap();
        assert!(
            layout_contient_web(&tree, id, "http://a/"),
            "l'URL courante ne doit pas avoir changé après un refus : {tree:?}"
        );
        s.kill();
    }

    // --- Fix 2 : gui_input ne confond plus « volet inconnu » et « volet
    // navigateur » ---------------------------------------------------------

    #[test]
    fn gui_input_vers_un_volet_navigateur_nest_pas_route_au_terminal_actif() {
        let s = Session::new("gi".into(), 80, 24, "cmd.exe").unwrap();
        let term_id = match s.window_layout().unwrap().0 {
            LayoutNode::Leaf { pane_id, .. } => pane_id,
            _ => panic!("une session neuve n'a qu'un volet"),
        };
        let web_id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://a/".into())
            .unwrap();
        // Le volet navigateur vient de devenir actif (split_web) ; on refocalise
        // le terminal pour reproduire le cas où le volet navigateur n'est PAS
        // le volet actif de la fenêtre (ex. l'utilisateur a reporté le focus
        // ailleurs sans jamais avoir pu focaliser l'iframe, cf. Fix 3).
        s.gui_focus(term_id);

        s.gui_input(web_id, b"PAYLOAD");
        // L'ancien comportement retombait sur le terminal actif : on vérifie que
        // ce n'est plus le cas.
        std::thread::sleep(Duration::from_millis(300));
        let txt = s.capture_pane(term_id).unwrap();
        assert!(
            !txt.contains("PAYLOAD"),
            "une frappe adressée à un volet navigateur ne doit pas atteindre le terminal actif : {txt}"
        );
        s.kill();
    }

    // --- Fix 3 : kill_pane ferme aussi un volet navigateur -----------------

    #[test]
    fn kill_pane_ferme_un_volet_navigateur() {
        let s = Session::new("kw".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://a/".into())
            .unwrap();
        assert!(
            s.kill_pane(id),
            "kill_pane doit trouver et fermer un volet navigateur"
        );
        assert!(
            s.capture_pane(id).is_none(),
            "le volet navigateur ne doit plus être trouvable après kill_pane"
        );
        s.kill();
    }

    /// La disposition contient-elle une feuille `id` de nature web sur `url` ?
    fn layout_contient_web(node: &LayoutNode, id: u64, url: &str) -> bool {
        match node {
            LayoutNode::Leaf { pane_id, kind } => {
                *pane_id == id
                    && matches!(kind, wimux_protocol::PaneKind::Web { url: u } if u == url)
            }
            LayoutNode::Split { a, b, .. } => {
                layout_contient_web(a, id, url) || layout_contient_web(b, id, url)
            }
        }
    }
}
