//! Démon : boucle d'acceptation des clients sur le Named Pipe, gestion des
//! sessions et dialogue avec chaque client (un thread par connexion, plus un
//! thread émetteur de frames par attachement).
//!
//! Le **préfixe** `Ctrl-b` et sa table de commandes sont interprétés ici, côté
//! serveur (modèle tmux) : le client transmet toutes les frappes, et le serveur
//! décide de les router vers le volet actif ou d'exécuter une commande (découpe,
//! navigation, détachement...).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use wimux_protocol::transport::{PipeConn, PipeListener, user_pipe_name};
use wimux_protocol::{
    AgentResult, AgentTemplate, BatchInfo, ClientMessage, Hello, HelloReply, NotificationInfo,
    PROTOCOL_VERSION, ServerMessage, SessionInfo, recv, send,
};

use crate::config::{Action, Config};
use crate::pane::CopyAction;
use crate::session::Session;
use crate::window::{Move, SplitDir};
use crate::worktree::{self, Worktree};

/// Compteur de lots (M3), unique par démon : garantit des `group` (donc des
/// branches `wimux/<group>/<i>`) distincts entre fan-outs successifs.
static NEXT_BATCH_ID: AtomicU64 = AtomicU64::new(0);

pub struct Server {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    config: Config,
    /// Session actuellement affichée par la connexion GUI persistante (G4).
    gui_viewed: Mutex<Option<String>>,
    /// Moteur navigateur pilotable (B2.1), unique au daemon, lancé paresseusement.
    browser: crate::browser::BrowserEngine,
}

impl Server {
    fn new() -> Arc<Server> {
        Self::with_config(Config::load())
    }

    /// Construit un serveur avec une config donnée (utile aux tests qui doivent
    /// injecter des modèles d'agents sans toucher au fichier utilisateur).
    fn with_config(config: Config) -> Arc<Server> {
        let browser = crate::browser::BrowserEngine::new(config.browser_headless);
        Arc::new(Server {
            sessions: Mutex::new(HashMap::new()),
            config,
            gui_viewed: Mutex::new(None),
            browser,
        })
    }

    fn reap(&self) {
        self.sessions.lock().unwrap().retain(|_, s| s.is_alive());
    }

    fn get(&self, name: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(name).cloned()
    }

    fn set_gui_viewed(&self, v: Option<String>) {
        *self.gui_viewed.lock().unwrap() = v;
    }

    fn list(&self) -> Vec<SessionInfo> {
        self.reap();
        let viewed = self.gui_viewed.lock().unwrap().clone();

        // Collecter les Arc<Session> SOUS le verrou, puis dropper le guard
        // avant tout calcul lent (git_branch fait de l'I/O disque).
        let mut sessions_vec = {
            let sessions = self.sessions.lock().unwrap();
            sessions.values().cloned().collect::<Vec<_>>()
        };
        // Ordre d'affichage du rail : ordre custom (glisser-déposer, W4/W5) puis
        // nom en tiebreak (déterministe).
        sessions_vec.sort_by(|a, b| {
            b.pinned()
                .cmp(&a.pinned())
                .then_with(|| a.order().cmp(&b.order()))
                .then_with(|| a.name().cmp(&b.name()))
        });

        // Construire les SessionInfo HORS verrou global, dans cet ordre.
        let infos: Vec<SessionInfo> = sessions_vec
            .iter()
            .map(|s| {
                let name = s.name();
                // La session vue n'a jamais d'indicateur : on efface et on
                // rafraîchit son baseline à chaque sondage (paresseux).
                let (activity, bell) = if Some(&name) == viewed.as_ref() {
                    s.mark_seen();
                    (false, false)
                } else {
                    (s.has_activity(), s.has_bell())
                };
                let cwd = s.active_pane_cwd();
                let branch = cwd
                    .as_deref()
                    .and_then(|c| crate::git::git_branch(std::path::Path::new(c)));
                SessionInfo {
                    name,
                    windows: s.window_count() as u32,
                    attached: s.attached_count() > 0,
                    activity,
                    bell,
                    agent: s.is_agent(),
                    agent_status: s.agent_status(std::time::Duration::from_secs(
                        self.config.agent_idle_seconds,
                    )),
                    group: s.group(),
                    cwd,
                    branch,
                    color: s.color(),
                    pinned: s.pinned(),
                    layout_rev: s.layout_rev(),
                }
            })
            .collect();
        infos
    }

    /// Réaffecte l'ordre d'affichage des sessions du rail selon la liste `names`
    /// (glisser-déposer, W4/W5) : chaque session nommée prend l'ordre de sa
    /// position ; les sessions absentes de `names` gardent leur ordre (rare).
    fn reorder_sessions(&self, names: &[String]) {
        let sessions = self.sessions.lock().unwrap();
        for (i, name) in names.iter().enumerate() {
            if let Some(s) = sessions.get(name) {
                s.set_order(i as u64);
            }
        }
    }

    /// Fixe la couleur d'accent d'un workspace (W5).
    fn set_session_color(&self, name: &str, color: Option<String>) {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            s.set_color(color);
        }
    }

    /// Épingle/désépingle un workspace (W5).
    fn set_session_pinned(&self, name: &str, pinned: bool) {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            s.set_pinned(pinned);
        }
    }

    /// Draine les notifications OSC 9/777 de toutes les sessions, taguées de leur
    /// nom (W6). Collecte les Arc sous verrou puis draine hors verrou global.
    fn take_notifications(&self) -> Vec<NotificationInfo> {
        let sessions = {
            let s = self.sessions.lock().unwrap();
            s.values().cloned().collect::<Vec<_>>()
        };
        let mut out = Vec::new();
        for s in sessions {
            let name = s.name();
            for n in s.drain_notifications() {
                out.push(NotificationInfo {
                    session: name.clone(),
                    title: n.title,
                    body: n.body,
                });
            }
        }
        out
    }

    /// Marque un workspace comme lu (efface activité + cloche) (W6).
    fn mark_session_read(&self, name: &str) {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            s.mark_seen();
        }
    }

    /// Marque un workspace comme non lu (repose la cloche) (W6).
    fn mark_session_unread(&self, name: &str) {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            s.mark_unread();
        }
    }

    fn kill(&self, name: &str) -> bool {
        let session = self.sessions.lock().unwrap().remove(name);
        match session {
            Some(s) => {
                s.kill();
                true
            }
            None => false,
        }
    }

    fn create_session(
        &self,
        name: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Session>, String> {
        self.reap();
        let mut sessions = self.sessions.lock().unwrap();

        let name = match name {
            Some(n) => {
                if sessions.contains_key(&n) {
                    return Err(format!("la session « {n} » existe déjà"));
                }
                n
            }
            None => {
                let mut i = 0;
                loop {
                    let candidate = i.to_string();
                    if !sessions.contains_key(&candidate) {
                        break candidate;
                    }
                    i += 1;
                }
            }
        };

        let session = Session::new(name.clone(), cols, rows, &self.config.default_shell)
            .map_err(|e| format!("création de la session : {e}"))?;
        sessions.insert(name, Arc::clone(&session));
        Ok(session)
    }

    /// Modèles d'agents configurés (M2).
    fn agent_templates(&self) -> Vec<AgentTemplate> {
        self.config.agent_templates.clone()
    }

    /// Crée une session agent depuis un modèle. Substitue `{prompt}` dans les
    /// args s'il est présent ; sinon signale un envoi stdin. Nom auto
    /// `<template>-<n>` si `name` absent. Insère la session et, en cas de
    /// livraison stdin, envoie `prompt` + `\r` au volet racine après le spawn.
    fn create_agent_session(
        &self,
        name: Option<String>,
        template: &str,
        prompt: &str,
        cwd: Option<&str>,
    ) -> Result<Arc<Session>, String> {
        // Résoudre le modèle par son nom.
        let tpl = self
            .config
            .agent_templates
            .iter()
            .find(|t| t.name == template)
            .cloned()
            .ok_or_else(|| format!("modèle d'agent inconnu : {template}"))?;

        // Substituer {prompt} dans les args ; noter s'il reste à livrer sur stdin.
        let mut has_placeholder = false;
        let args: Vec<String> = tpl
            .args
            .iter()
            .map(|a| {
                if a.contains("{prompt}") {
                    has_placeholder = true;
                    a.replace("{prompt}", prompt)
                } else {
                    a.clone()
                }
            })
            .collect();
        let stdin_prompt = !has_placeholder;

        self.reap();
        let mut sessions = self.sessions.lock().unwrap();

        // Nom : fourni (doit être libre) ou auto `<template>-<n>`.
        let name = match name {
            Some(n) => {
                if sessions.contains_key(&n) {
                    return Err(format!("la session « {n} » existe déjà"));
                }
                n
            }
            None => {
                let mut i = 0;
                loop {
                    let candidate = format!("{template}-{i}");
                    if !sessions.contains_key(&candidate) {
                        break candidate;
                    }
                    i += 1;
                }
            }
        };

        let session = Session::new_agent(name.clone(), 80, 24, &tpl.program, &args, cwd)
            .map_err(|e| format!("création de la session agent : {e}"))?;
        sessions.insert(name, Arc::clone(&session));
        drop(sessions);

        if stdin_prompt && !prompt.is_empty() {
            let mut line = prompt.as_bytes().to_vec();
            line.push(b'\r');
            session.send_input(&line);
        }
        Ok(session)
    }

    /// Crée un **lot** de `count` sessions agent (M3), chacune dans un worktree
    /// git de `base_repo`, sur une branche `wimux/<group>/<i>`. Le travail lourd
    /// (git + spawn) se fait hors verrou `sessions` ; l'insertion des N membres
    /// est atomique (un seul verrou). Rollback complet en cas d'échec partiel.
    fn create_agent_batch(
        &self,
        template: &str,
        prompt: &str,
        base_repo: &str,
        count: u32,
    ) -> Result<(String, Vec<String>), String> {
        // (1) La base doit être un dépôt git.
        let base = std::path::PathBuf::from(base_repo);
        if !worktree::is_git_repo(&base) {
            return Err(format!(
                "le répertoire de base n'est pas un dépôt git : {base_repo}"
            ));
        }

        // (1 bis) Référence de comparaison du lot (M4) : sha + branche de la base
        // au lancement. Sans HEAD (dépôt sans commit), le lot n'a pas de base.
        let base_sha = worktree::head_sha(&base).ok_or_else(|| {
            format!("le dépôt de base n'a pas de HEAD (aucun commit ?) : {base_repo}")
        })?;
        let base_branch = worktree::current_branch(&base).ok_or_else(|| {
            format!("branche courante du dépôt de base introuvable : {base_repo}")
        })?;

        // (2) Résoudre le modèle par son nom.
        let tpl = self
            .config
            .agent_templates
            .iter()
            .find(|t| t.name == template)
            .cloned()
            .ok_or_else(|| format!("modèle d'agent inconnu : {template}"))?;

        // Substituer {prompt} dans les args (une fois, commun aux N agents) ;
        // sinon livraison stdin après spawn (comme M2).
        let mut has_placeholder = false;
        let args: Vec<String> = tpl
            .args
            .iter()
            .map(|a| {
                if a.contains("{prompt}") {
                    has_placeholder = true;
                    a.replace("{prompt}", prompt)
                } else {
                    a.clone()
                }
            })
            .collect();
        let stdin_prompt = !has_placeholder;

        // (3) Identifiant de lot unique.
        let group = format!("batch{}", NEXT_BATCH_ID.fetch_add(1, Ordering::Relaxed));
        let root = &self.config.agent_worktree_root;

        // (4) Boucle i : worktree + spawn, HORS verrou `sessions`. Rollback des
        // sessions déjà créées (chacune retire son worktree via kill).
        let rollback = |created: &[(String, Arc<Session>)]| {
            for (_, s) in created {
                s.kill();
            }
        };
        let mut created: Vec<(String, Arc<Session>)> = Vec::new();
        for i in 0..count {
            let name = format!("{template}-{group}-{i}");
            let branch = format!("wimux/{group}/{i}");
            let path = root.join(format!("{group}-{i}"));
            let Some(path_str) = path.to_str().map(|s| s.to_string()) else {
                rollback(&created);
                return Err(format!("chemin de worktree non-UTF-8 : {}", path.display()));
            };

            // Créer le worktree.
            if let Err(e) = worktree::add(&base, &path, &branch) {
                rollback(&created);
                return Err(format!("échec de création du worktree {i} : {e}"));
            }

            // Spawn l'agent dans le worktree.
            match Session::new_agent(name.clone(), 80, 24, &tpl.program, &args, Some(&path_str)) {
                Ok(session) => {
                    session.set_group(group.clone());
                    session.set_worktree(Worktree {
                        base_repo: base.clone(),
                        path: path.clone(),
                        branch: branch.clone(),
                        base_sha: base_sha.clone(),
                        base_branch: base_branch.clone(),
                    });
                    created.push((name, session));
                }
                Err(e) => {
                    // Worktree orphelin (aucune session ne le porte) : le retirer,
                    // puis rollback des sessions déjà créées.
                    worktree::remove(&base, &path, &branch);
                    rollback(&created);
                    return Err(format!("échec du lancement de l'agent {i} : {e}"));
                }
            }
        }

        // (5) Insertion atomique des N membres sous un unique verrou.
        {
            let mut sessions = self.sessions.lock().unwrap();
            for (name, session) in &created {
                sessions.insert(name.clone(), Arc::clone(session));
            }
        }

        // (6) Livraison stdin si pas de placeholder (comme M2).
        if stdin_prompt && !prompt.is_empty() {
            let mut line = prompt.as_bytes().to_vec();
            line.push(b'\r');
            for (_, session) in &created {
                session.send_input(&line);
            }
        }

        let names = created.into_iter().map(|(n, _)| n).collect();
        Ok((group, names))
    }

    /// Rang d'un agent dans son lot, dérivé du nom de session M3
    /// `<template>-<group>-<i>` : on lit le suffixe après le dernier `-`.
    fn agent_index(name: &str) -> u32 {
        name.rsplit('-')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// M4 : lots en cours, construits depuis les sessions portant un `group`.
    fn list_batches(&self) -> Vec<BatchInfo> {
        let sessions = {
            let s = self.sessions.lock().unwrap();
            s.values().cloned().collect::<Vec<_>>()
        };
        let mut by_group: std::collections::BTreeMap<String, Vec<Arc<Session>>> =
            std::collections::BTreeMap::new();
        for s in sessions {
            if let Some(g) = s.group() {
                by_group.entry(g).or_default().push(s);
            }
        }
        by_group
            .into_iter()
            .map(|(group, mut members)| {
                members.sort_by_key(|s| Self::agent_index(&s.name()));
                // base_repo / base_branch : pris sur le premier membre (identiques
                // pour tout le lot, posés à la création).
                let wt = members.first().and_then(|s| s.worktree());
                BatchInfo {
                    group,
                    sessions: members.iter().map(|s| s.name()).collect(),
                    base_repo: wt
                        .as_ref()
                        .map(|w| w.base_repo.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    base_branch: wt.map(|w| w.base_branch).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// M4 : résumé par agent des résultats d'un lot.
    fn review_batch(&self, group: &str) -> Result<Vec<AgentResult>, String> {
        let members = {
            let s = self.sessions.lock().unwrap();
            let mut v: Vec<Arc<Session>> = s
                .values()
                .filter(|s| s.group().as_deref() == Some(group))
                .cloned()
                .collect();
            v.sort_by_key(|s| Self::agent_index(&s.name()));
            v
        };
        if members.is_empty() {
            return Err(format!("lot introuvable : {group}"));
        }
        let idle = std::time::Duration::from_secs(self.config.agent_idle_seconds);
        let mut out = Vec::new();
        for s in members {
            let name = s.name();
            let Some(wt) = s.worktree() else {
                return Err(format!("la session « {name} » n'a pas de worktree"));
            };
            let stats = crate::batch::diff_stats(&wt.path, &wt.base_sha)
                .map_err(|e| format!("diff de « {name} » : {e}"))?;
            out.push(AgentResult {
                session: name.clone(),
                index: Self::agent_index(&name),
                branch: wt.branch.clone(),
                status: s.agent_status(idle),
                files_changed: stats.files_changed,
                insertions: stats.insertions,
                deletions: stats.deletions,
                untracked: crate::batch::untracked(&wt.path).len() as u32,
                has_commits: crate::batch::has_commits(&wt.path, &wt.base_sha),
            });
        }
        Ok(out)
    }

    /// M4 : diff complet du travail d'un agent.
    fn diff_agent(&self, session: &str) -> Result<String, String> {
        let s = self
            .get(session)
            .ok_or_else(|| format!("session introuvable : {session}"))?;
        let wt = s
            .worktree()
            .ok_or_else(|| format!("la session « {session} » n'a pas de worktree"))?;
        crate::batch::full_diff(&wt.path, &wt.base_sha)
    }

    /// M4 : intègre le travail d'un agent par Pull Request, puis nettoie les
    /// PERDANTS du lot (le gagnant reste vivant pour itérer sur la revue).
    ///
    /// Ordre volontaire : toutes les gardes AVANT le moindre effet de bord.
    fn open_pr(
        &self,
        session: &str,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<String, String> {
        // (1) Résolution + gardes, sans effet de bord.
        let s = self
            .get(session)
            .ok_or_else(|| format!("session introuvable : {session}"))?;
        let wt = s
            .worktree()
            .ok_or_else(|| format!("la session « {session} » n'a pas de worktree"))?;
        let group = s
            .group()
            .ok_or_else(|| format!("la session « {session} » n'appartient à aucun lot"))?;

        // Un agent qui produit ENCORE de la sortie peut être au milieu d'une
        // écriture : `commit_wip` (`git add -A`) capturerait un fichier à moitié
        // écrit. On refuse ce seul cas. Idle/Attention/Done/Error passent — un
        // agent interactif reste vivant indéfiniment, exiger sa mort rendrait
        // l'intégration impossible pour lui.
        let idle = std::time::Duration::from_secs(self.config.agent_idle_seconds);
        if s.agent_status(idle) == Some(wimux_protocol::AgentStatus::Working) {
            return Err(format!(
                "l'agent « {session} » produit encore de la sortie : attends qu'il se stabilise \
                 (ou tue-le) avant d'intégrer son travail"
            ));
        }

        let stats = crate::batch::diff_stats(&wt.path, &wt.base_sha)?;
        let untracked_n = crate::batch::untracked(&wt.path).len();
        if stats.files_changed == 0 && untracked_n == 0 {
            return Err(format!(
                "l'agent « {session} » n'a rien produit : pas de PR"
            ));
        }
        crate::batch::gh_ready(&wt.path)?;

        // (2) Effets de bord : commit du WIP, push, PR.
        crate::batch::commit_wip(
            &wt.path,
            &format!("wimux: travail de l'agent {session} (lot {group})"),
        )?;
        crate::batch::push_branch(&wt.path, &wt.branch)?;

        let title = title.unwrap_or_else(|| format!("wimux: résultat de l'agent {session}"));
        let footer = format!(
            "\n\n---\nOuvert par wimux — lot `{group}`, agent `{session}`, branche `{}`.\n\
             {} fichier(s) suivi(s) modifié(s), +{} / -{}, {} non suivi(s).",
            wt.branch, stats.files_changed, stats.insertions, stats.deletions, untracked_n
        );
        let body = format!("{}{footer}", body.unwrap_or_default());

        // Le push a réussi : si la PR échoue, on le dit SANS nettoyer quoi que ce soit.
        let url = crate::batch::create_pr(&wt.path, &wt.base_branch, &wt.branch, &title, &body)
            .map_err(|e| {
                format!(
                    "{e}\n(la branche « {} » EST poussée sur origin ; la PR reste à ouvrir, \
                     rien n'a été nettoyé)",
                    wt.branch
                )
            })?;

        // (3) Nettoyage des PERDANTS uniquement.
        let losers: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .values()
                .filter(|o| o.group().as_deref() == Some(group.as_str()) && o.name() != session)
                .map(|o| o.name())
                .collect()
        };
        for name in losers {
            self.kill(&name);
        }
        Ok(url)
    }

    fn rename_session(&self, from: &str, to: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().unwrap();
        if !sessions.contains_key(from) {
            return Err(format!("session introuvable : {from}"));
        }
        if sessions.contains_key(to) {
            return Err(format!("la session « {to} » existe déjà"));
        }
        if let Some(s) = sessions.remove(from) {
            s.set_name(to.to_string());
            sessions.insert(to.to_string(), s);
        }
        Ok(())
    }
}

/// Lance le démon sur le pipe de l'utilisateur courant.
pub fn run() -> Result<()> {
    run_on(&user_pipe_name())
}

/// Lance le démon sur un pipe nommé donné (utile pour les tests isolés).
pub fn run_on(pipe_name: &str) -> Result<()> {
    serve(Server::new(), pipe_name)
}

/// Lance le démon avec une configuration injectée (tests : modèles d'agents).
pub fn run_on_with_config(pipe_name: &str, config: Config) -> Result<()> {
    serve(Server::with_config(config), pipe_name)
}

/// Boucle d'acceptation partagée par `run_on` et `run_on_with_config`.
fn serve(server: Arc<Server>, pipe_name: &str) -> Result<()> {
    let listener = PipeListener::bind(pipe_name);

    loop {
        match listener.accept() {
            Ok(conn) => {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(server, conn) {
                        eprintln!("wimux-server : client terminé sur erreur : {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("wimux-server : échec d'acceptation : {e}");
            }
        }
    }
}

/// Lien actif client<->session : garde d'attachement + thread émetteur de frames.
struct Attachment {
    keep_going: Arc<AtomicBool>,
    sender: Option<JoinHandle<()>>,
    session: Arc<Session>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.keep_going.store(false, Ordering::Relaxed);
        self.session.notifier().notify();
        if let Some(handle) = self.sender.take() {
            let _ = handle.join();
        }
        self.session.decr_attached();
    }
}

fn attach(session: Arc<Session>, conn: Arc<PipeConn>) -> Attachment {
    session.incr_attached();
    let keep_going = Arc::new(AtomicBool::new(true));

    let sender = {
        let session = Arc::clone(&session);
        let keep_going = Arc::clone(&keep_going);
        std::thread::spawn(move || sender_loop(session, conn, keep_going))
    };

    Attachment {
        keep_going,
        sender: Some(sender),
        session,
    }
}

/// Boucle émettrice : envoie une frame composée à chaque changement.
fn sender_loop(session: Arc<Session>, conn: Arc<PipeConn>, keep_going: Arc<AtomicBool>) {
    let notifier = session.notifier();
    let mut last_gen = 0u64;
    loop {
        let generation = notifier.wait_change(last_gen, &keep_going);
        if !keep_going.load(Ordering::Relaxed) {
            break;
        }
        last_gen = generation;

        let frame = session.composite();
        let mut w: &PipeConn = &conn;
        if send(&mut w, &ServerMessage::Frame(frame)).is_err() {
            break;
        }
        if !session.is_alive() {
            let mut w: &PipeConn = &conn;
            let _ = send(&mut w, &ServerMessage::PaneExited { code: 0 });
            break;
        }
    }
}

/// Attachement GUI suivi (une session diffusée sur cette connexion). Arrêt propre
/// du thread de retransmission au drop (permet la bascule d'une session à l'autre).
struct GuiAttachment {
    keep_going: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    tx: std::sync::mpsc::Sender<(u64, Vec<u8>)>,
    session: Arc<Session>,
}

impl Drop for GuiAttachment {
    fn drop(&mut self) {
        self.keep_going.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Rejoue le cycle d'attache GUI pour la fenêtre **active** de `s` : coupe
/// proprement la diffusion précédente (via le `Drop` de `GuiAttachment`, qui
/// arrête et joint le thread de pompe), réabonne les volets de la fenêtre active,
/// envoie `WindowLayout` PUIS un `PaneSnapshot` frais par volet (sous `gui_write`),
/// puis relance le thread de pompe `PaneOutput`. Mutualisé entre l'attache
/// initiale (`AttachGui`) et les bascules de fenêtre `NewWindow`/`SelectWindow`/
/// `CloseWindow` (W2).
fn reattach_active_window(
    s: &Arc<Session>,
    conn: &Arc<PipeConn>,
    gui_write: &Arc<Mutex<()>>,
    gui_attach: &mut Option<GuiAttachment>,
) -> std::io::Result<()> {
    // (1) Couper la diffusion précédente (Drop => join du thread de pompe).
    *gui_attach = None;

    // (2) Réabonner les volets de la fenêtre active.
    let Some((tree, active, snaps, rx, tx)) = s.gui_attach_window() else {
        let _g = gui_write.lock().unwrap();
        let mut wr: &PipeConn = conn;
        return send(
            &mut wr,
            &ServerMessage::Error("aucun volet dans la fenêtre active".into()),
        );
    };

    // (3) WindowLayout D'ABORD (le frontend crée le xterm à sa réception), puis
    //     les snapshots. Tout sous le verrou d'écriture GUI.
    {
        let _g = gui_write.lock().unwrap();
        let mut wr: &PipeConn = conn;
        send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
        for (pane_id, bytes) in snaps {
            let mut wr: &PipeConn = conn;
            send(&mut wr, &ServerMessage::PaneSnapshot { pane_id, bytes })?;
        }
    }

    // (4) Thread de pompe : rx.recv_timeout -> PaneOutput (sérialisé par gui_write).
    let keep_going = Arc::new(AtomicBool::new(true));
    let conn_out = Arc::clone(conn);
    let kg = Arc::clone(&keep_going);
    let gw = Arc::clone(gui_write);
    let handle = std::thread::spawn(move || {
        while kg.load(Ordering::Relaxed) {
            match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok((pane_id, bytes)) => {
                    let mut w: &PipeConn = &conn_out;
                    let sent = {
                        let _g = gw.lock().unwrap();
                        send(&mut w, &ServerMessage::PaneOutput { pane_id, bytes })
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // (5) Stocker la nouvelle attache.
    *gui_attach = Some(GuiAttachment {
        keep_going,
        handle: Some(handle),
        tx,
        session: Arc::clone(s),
    });
    Ok(())
}

/// Ces messages arrivent de la GUI sur sa connexion persistante, où la session
/// attachée est déjà connue : la GUI envoie donc une chaîne vide. On retombe
/// alors sur la session de l'attachement (et on laisse la valeur telle quelle
/// quand elle est fournie, ce que fait la CLI).
///
/// `None` si `session` est vide ET qu'aucune GUI n'est attachée : il n'y a
/// alors aucune session à résoudre (pas même une chaîne vide à chercher).
fn resolve_gui_session(session: String, gui_attach: &Option<GuiAttachment>) -> Option<String> {
    if session.is_empty() {
        gui_attach.as_ref().map(|ga| ga.session.name())
    } else {
        Some(session)
    }
}

/// Résout la session ciblée par un message B1 (`OpenWebPane`/`WebNavigate`/
/// `WebBack`/`WebForward`), ou renvoie directement le message d'erreur à
/// répondre si la connexion n'a ni nom de session fourni ni GUI attachée.
/// Centralise ce message pour ne pas le dupliquer dans les quatre handlers.
fn require_gui_session(
    session: String,
    gui_attach: &Option<GuiAttachment>,
) -> Result<String, ServerMessage> {
    resolve_gui_session(session, gui_attach).ok_or_else(|| {
        ServerMessage::Error("aucune session : cette connexion n'est pas attachée".into())
    })
}

/// Repousse la disposition de la fenêtre active à la connexion GUI courante, si
/// cette connexion EST une GUI attachée. Utilisé après une navigation web : le
/// `WindowLayout` transporte l'URL, la GUI n'a plus qu'à refléter le `src`.
fn push_layout_if_gui(
    gui_attach: &Option<GuiAttachment>,
    conn: &Arc<PipeConn>,
    gui_write: &Arc<Mutex<()>>,
) -> std::io::Result<()> {
    if let Some(ga) = gui_attach
        && let Some((tree, active)) = ga.session.window_layout()
    {
        let _g = gui_write.lock().unwrap();
        let mut wr: &PipeConn = conn;
        send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
    }
    Ok(())
}

/// Traduit une réponse du moteur navigateur (B2.1) en message serveur.
fn browser_reply(res: Result<crate::browser::BrowserReply, String>) -> ServerMessage {
    use crate::browser::BrowserReply;
    match res {
        Ok(BrowserReply::Ok) => ServerMessage::Ok,
        Ok(BrowserReply::Status { running, url }) => ServerMessage::BrowserState { running, url },
        Ok(BrowserReply::Text(t)) => ServerMessage::BrowserText(t),
        Ok(BrowserReply::Shot(path)) => ServerMessage::BrowserShot { path },
        Err(e) => ServerMessage::Error(e),
    }
}

/// État du décodage du préfixe pour un client.
#[derive(Default)]
struct PrefixState {
    armed: bool,
}

fn handle_client(server: Arc<Server>, conn: PipeConn) -> Result<()> {
    let conn = Arc::new(conn);

    // Handshake de version.
    let mut rd: &PipeConn = &conn;
    let hello: ClientMessage = match recv(&mut rd) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let ok = matches!(
        &hello,
        ClientMessage::Hello(Hello { client_version, .. })
            if client_version.is_compatible_with(PROTOCOL_VERSION)
    );
    let mut wr: &PipeConn = &conn;
    if !ok {
        let _ = send(
            &mut wr,
            &ServerMessage::Hello(HelloReply::VersionMismatch {
                server_version: PROTOCOL_VERSION,
                reason: "version de protocole incompatible".into(),
            }),
        );
        return Ok(());
    }
    send(
        &mut wr,
        &ServerMessage::Hello(HelloReply::Ok {
            server_version: PROTOCOL_VERSION,
        }),
    )?;

    let mut attachment: Option<Attachment> = None;
    let mut gui_attach: Option<GuiAttachment> = None;
    let mut gui_session: Option<Arc<Session>> = None;
    // A-t-on posé l'état « vue » global ? (Seule cette connexion l'efface au drop.)
    let mut viewed_set = false;
    let mut prefix = PrefixState::default();
    // Sérialise TOUTES les écritures GUI sur `conn` (WindowLayout/PaneSnapshot
    // envoyés par la boucle principale, PaneOutput envoyé par le thread de
    // retransmission) : deux `write_all` concurrents sur le même PipeConn
    // peuvent entrelacer leurs frames et désynchroniser le client.
    let gui_write = Arc::new(Mutex::new(()));

    loop {
        let mut rd: &PipeConn = &conn;
        let msg = match recv::<_, ClientMessage>(&mut rd) {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            ClientMessage::NewSession { name, cols, rows } => {
                match server.create_session(name, cols, rows) {
                    Ok(session) => {
                        let mut wr: &PipeConn = &conn;
                        send(
                            &mut wr,
                            &ServerMessage::Attached {
                                name: session.name(),
                            },
                        )?;
                        attachment = Some(attach(session, Arc::clone(&conn)));
                    }
                    Err(e) => {
                        let mut wr: &PipeConn = &conn;
                        send(&mut wr, &ServerMessage::Error(e))?;
                    }
                }
            }
            ClientMessage::Attach { name, cols, rows } => match server.get(&name) {
                Some(session) => {
                    session.resize(cols, rows);
                    let mut wr: &PipeConn = &conn;
                    send(
                        &mut wr,
                        &ServerMessage::Attached {
                            name: session.name(),
                        },
                    )?;
                    attachment = Some(attach(session, Arc::clone(&conn)));
                }
                None => {
                    let mut wr: &PipeConn = &conn;
                    send(
                        &mut wr,
                        &ServerMessage::Error(format!("session introuvable : {name}")),
                    )?;
                }
            },
            ClientMessage::List => {
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Sessions(server.list()))?;
            }
            ClientMessage::Kill { name } => {
                server.kill(&name);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::ReorderSessions { names } => {
                server.reorder_sessions(&names);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::SetSessionColor { name, color } => {
                server.set_session_color(&name, color);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::SetSessionPinned { name, pinned } => {
                server.set_session_pinned(&name, pinned);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::TakeNotifications => {
                let notifs = server.take_notifications();
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Notifications(notifs))?;
            }
            ClientMessage::MarkSessionRead { name } => {
                server.mark_session_read(&name);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::MarkSessionUnread { name } => {
                server.mark_session_unread(&name);
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
            }
            ClientMessage::AttachGui { session } => {
                // Arrêter proprement la diffusion précédente avant d'en démarrer une.
                gui_attach = None;
                gui_session = None;
                // La vue précédente se termine ; aucune session n'est regardée tant
                // que la nouvelle attache n'a pas réussi.
                server.set_gui_viewed(None);
                match server.get(&session) {
                    Some(s) => {
                        // WindowList (état initial des onglets) AVANT layout/snapshots.
                        {
                            let (windows, active) = s.window_list();
                            let _g = gui_write.lock().unwrap();
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                        }
                        reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                        // G4 : cette session devient « vue » ; on efface ses indicateurs.
                        server.set_gui_viewed(Some(session.clone()));
                        s.mark_seen();
                        viewed_set = true;
                        gui_session = Some(s);
                    }
                    None => {
                        let _g = gui_write.lock().unwrap();
                        let mut wr: &PipeConn = &conn;
                        send(
                            &mut wr,
                            &ServerMessage::Error(format!("session introuvable : {session}")),
                        )?;
                    }
                }
            }
            ClientMessage::PaneInput { pane_id, bytes } => {
                if let Some(s) = &gui_session {
                    s.gui_input(pane_id, &bytes);
                }
            }
            ClientMessage::PaneResize {
                pane_id,
                cols,
                rows,
            } => {
                if let Some(ga) = &gui_attach {
                    ga.session.gui_pane_resize(pane_id, cols, rows);
                }
            }
            ClientMessage::SplitPane { pane_id, dir } => {
                if let Some(ga) = &gui_attach
                    && let Some((new_id, snapshot, tree, active)) =
                        ga.session.gui_split(pane_id, dir.into(), ga.tx.clone())
                {
                    // WindowLayout D'ABORD : le frontend crée le xterm du volet à
                    // sa réception ; un PaneSnapshot reçu avant serait perdu.
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                    let mut wr: &PipeConn = &conn;
                    send(
                        &mut wr,
                        &ServerMessage::PaneSnapshot {
                            pane_id: new_id,
                            bytes: snapshot,
                        },
                    )?;
                }
            }
            ClientMessage::ClosePane { pane_id } => {
                if let Some(ga) = &gui_attach
                    && let Some((tree, active)) = ga.session.gui_close(pane_id)
                {
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
            ClientMessage::FocusPane { pane_id } => {
                if let Some(ga) = &gui_attach
                    && let Some((tree, active)) = ga.session.gui_focus(pane_id)
                {
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
            ClientMessage::SetSplitRatio { node_id, ratio } => {
                if let Some(ga) = &gui_attach
                    && let Some((tree, active)) = ga.session.gui_set_ratio(node_id, ratio)
                {
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
                }
            }
            ClientMessage::SendKeys { session, keys } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        s.send_input(&keys);
                        ServerMessage::Ok
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Command { session, command } => {
                let reply = match server.get(&session) {
                    Some(s) => match crate::commands::run(&s, &command) {
                        crate::commands::CommandResult::Text(t) => ServerMessage::CommandResult(t),
                        crate::commands::CommandResult::None => {
                            ServerMessage::CommandResult(String::new())
                        }
                    },
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::SpawnPane {
                session,
                from_pane,
                dir,
                cwd,
                program,
                args,
            } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        match s.spawn_pane(from_pane, dir.into(), cwd.as_deref(), &program, &args) {
                            Some(pane_id) => ServerMessage::PaneSpawned { pane_id },
                            None => ServerMessage::Error("échec du spawn de volet".into()),
                        }
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::CapturePane { session, pane } => {
                let reply = match server.get(&session) {
                    Some(s) => match s.capture_pane(pane) {
                        Some(text) => ServerMessage::PaneCapture(text),
                        None => ServerMessage::Error(format!("volet introuvable : {pane}")),
                    },
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::ListPanes { session } => {
                let reply = match server.get(&session) {
                    Some(s) => ServerMessage::PaneList(s.pane_infos()),
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::SendKeysPane {
                session,
                pane,
                keys,
            } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        if s.send_keys_pane(pane, &keys) {
                            ServerMessage::Ok
                        } else {
                            ServerMessage::Error(format!("volet introuvable : {pane}"))
                        }
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::KillPane { session, pane } => {
                let reply = match server.get(&session) {
                    Some(s) => {
                        if s.kill_pane(pane) {
                            ServerMessage::Ok
                        } else {
                            ServerMessage::Error(format!("volet introuvable : {pane}"))
                        }
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::ListBatches => {
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Batches(server.list_batches()))?;
            }
            ClientMessage::ReviewBatch { group } => {
                let reply = match server.review_batch(&group) {
                    Ok(v) => ServerMessage::BatchReview(v),
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::DiffAgent { session } => {
                let reply = match server.diff_agent(&session) {
                    Ok(text) => ServerMessage::AgentDiff(text),
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::OpenPr {
                session,
                title,
                body,
            } => {
                let reply = match server.open_pr(&session, title, body) {
                    Ok(url) => ServerMessage::PrOpened { url },
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::OpenWebPane {
                session,
                from_pane,
                dir,
                url,
            } => {
                let reply = match require_gui_session(session, &gui_attach) {
                    // Fix 5 : re-teste la garde AVANT `open_web_pane` pour
                    // distinguer, dans le message, une URL refusée (schéma ou
                    // origine) d'un échec « normal » (fenêtre introuvable) —
                    // sinon un utilisateur qui tape `example.com` sans schéma
                    // est envoyé sur une fausse piste (« échec d'ouverture du
                    // volet web »).
                    Ok(_) if !crate::session::url_autorisee(&url) => ServerMessage::Error(
                        "URL refusée : seules les adresses http(s) externes sont autorisées".into(),
                    ),
                    Ok(session) => match server.get(&session) {
                        Some(s) => match s.open_web_pane(from_pane, dir.into(), url) {
                            Some(pane_id) => ServerMessage::PaneSpawned { pane_id },
                            None => ServerMessage::Error("échec d'ouverture du volet web".into()),
                        },
                        None => ServerMessage::Error(format!("session introuvable : {session}")),
                    },
                    Err(e) => e,
                };
                // Réponse sous `gui_write` : cette connexion peut être la GUI
                // persistante, où le thread de pompe écrit des `PaneOutput` en
                // continu (voir le commentaire sur `gui_write` plus haut).
                let _g = gui_write.lock().unwrap();
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::WebNavigate { session, pane, url } => {
                let reply = match require_gui_session(session, &gui_attach) {
                    // Fix 5 : voir le commentaire équivalent sur `OpenWebPane`
                    // — sans ce test préalable, une URL refusée renverrait ici
                    // « volet navigateur introuvable », un message trompeur
                    // pour l'utilisateur qui a simplement oublié le schéma.
                    Ok(_) if !crate::session::url_autorisee(&url) => ServerMessage::Error(
                        "URL refusée : seules les adresses http(s) externes sont autorisées".into(),
                    ),
                    Ok(session) => match server.get(&session) {
                        Some(s) => {
                            if s.web_navigate(pane, url) {
                                ServerMessage::Ok
                            } else {
                                ServerMessage::Error(format!(
                                    "volet navigateur introuvable : {pane}"
                                ))
                            }
                        }
                        None => ServerMessage::Error(format!("session introuvable : {session}")),
                    },
                    Err(e) => e,
                };
                {
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &reply)?;
                }
                // Verrou relâché avant cet appel : `push_layout_if_gui` reprend
                // `gui_write` lui-même, et un `Mutex` std n'est pas réentrant.
                push_layout_if_gui(&gui_attach, &conn, &gui_write)?;
            }
            ClientMessage::WebBack { session, pane } => {
                let reply = match require_gui_session(session, &gui_attach) {
                    Ok(session) => match server.get(&session) {
                        Some(s) => match s.web_back(pane) {
                            Some(_) => ServerMessage::Ok,
                            None => ServerMessage::Error(format!(
                                "volet navigateur introuvable : {pane}"
                            )),
                        },
                        None => ServerMessage::Error(format!("session introuvable : {session}")),
                    },
                    Err(e) => e,
                };
                {
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &reply)?;
                }
                // Verrou relâché avant cet appel : `push_layout_if_gui` reprend
                // `gui_write` lui-même, et un `Mutex` std n'est pas réentrant.
                push_layout_if_gui(&gui_attach, &conn, &gui_write)?;
            }
            ClientMessage::WebForward { session, pane } => {
                let reply = match require_gui_session(session, &gui_attach) {
                    Ok(session) => match server.get(&session) {
                        Some(s) => match s.web_forward(pane) {
                            Some(_) => ServerMessage::Ok,
                            None => ServerMessage::Error(format!(
                                "volet navigateur introuvable : {pane}"
                            )),
                        },
                        None => ServerMessage::Error(format!("session introuvable : {session}")),
                    },
                    Err(e) => e,
                };
                {
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &reply)?;
                }
                // Verrou relâché avant cet appel : `push_layout_if_gui` reprend
                // `gui_write` lui-même, et un `Mutex` std n'est pas réentrant.
                push_layout_if_gui(&gui_attach, &conn, &gui_write)?;
            }
            // B2.1 : handlers du moteur navigateur (Task 6).
            ClientMessage::BrowserLaunch => {
                let reply =
                    browser_reply(server.browser.exec(crate::browser::BrowserCommand::Launch));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserClose => {
                let reply =
                    browser_reply(server.browser.exec(crate::browser::BrowserCommand::Close));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserStatus => {
                let reply =
                    browser_reply(server.browser.exec(crate::browser::BrowserCommand::Status));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserNavigate { url } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Navigate(url)),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserUrl => {
                let reply = browser_reply(server.browser.exec(crate::browser::BrowserCommand::Url));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserSnapshot => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Snapshot),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserScreenshot => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Screenshot),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserClick { ref_ } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Click { ref_ }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserType { ref_, text } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Type { ref_, text }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserPress { key, ref_ } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Press { key, ref_ }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserScroll { ref_, dy } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Scroll { ref_, dy }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserWait { text, ms, settle } => {
                let reply =
                    browser_reply(server.browser.exec(crate::browser::BrowserCommand::Wait {
                        text,
                        ms,
                        settle,
                    }));
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserEval { js } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Eval { js }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::BrowserSelect { ref_, value } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Select { ref_, value }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Input(bytes) => {
                if let Some(a) = &attachment {
                    let outcome = route_input(&a.session, &server.config, &mut prefix, &bytes);
                    if let Some(text) = outcome.clipboard {
                        let mut wr: &PipeConn = &conn;
                        let _ = send(&mut wr, &ServerMessage::SetClipboard(text));
                    }
                    if outcome.detach {
                        let mut wr: &PipeConn = &conn;
                        let _ = send(&mut wr, &ServerMessage::Detached);
                        attachment = None;
                        break;
                    }
                }
            }
            ClientMessage::Resize { cols, rows } => {
                if let Some(a) = &attachment {
                    a.session.resize(cols, rows);
                }
            }
            ClientMessage::Detach => {
                attachment = None;
                break;
            }
            ClientMessage::Shutdown => {
                for info in server.list() {
                    server.kill(&info.name);
                }
                std::process::exit(0);
            }
            ClientMessage::Ping => {
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Pong)?;
            }
            ClientMessage::CreateSession { name } => {
                let reply = match server.create_session(name, 80, 24) {
                    Ok(s) => ServerMessage::SessionCreated { name: s.name() },
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::RenameSession { from, to } => {
                let reply = match server.rename_session(&from, &to) {
                    Ok(()) => ServerMessage::Ok,
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::ListAgentTemplates => {
                let mut wr: &PipeConn = &conn;
                send(
                    &mut wr,
                    &ServerMessage::AgentTemplates(server.agent_templates()),
                )?;
            }
            ClientMessage::CreateAgentSession {
                name,
                template,
                prompt,
                cwd,
            } => {
                let reply =
                    match server.create_agent_session(name, &template, &prompt, cwd.as_deref()) {
                        Ok(s) => ServerMessage::SessionCreated { name: s.name() },
                        Err(e) => ServerMessage::Error(e),
                    };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::CreateAgentBatch {
                template,
                prompt,
                base_repo,
                count,
            } => {
                let reply = match server.create_agent_batch(&template, &prompt, &base_repo, count) {
                    Ok((group, sessions)) => ServerMessage::BatchCreated { group, sessions },
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::Hello(_) => {}
            ClientMessage::NewWindow => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    let (windows, active) = s.gui_new_window();
                    {
                        let _g = gui_write.lock().unwrap();
                        let mut wr: &PipeConn = &conn;
                        send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                    }
                    reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                }
            }
            ClientMessage::SelectWindow { index } => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    match s.gui_select_window(index) {
                        Some((windows, active)) => {
                            {
                                let _g = gui_write.lock().unwrap();
                                let mut wr: &PipeConn = &conn;
                                send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                            }
                            reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                        }
                        None => {
                            // Hors borne : renvoyer la liste courante sans réattache.
                            let (windows, active) = s.window_list();
                            let _g = gui_write.lock().unwrap();
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                        }
                    }
                }
            }
            ClientMessage::CloseWindow { index } => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    match s.gui_close_window(index) {
                        Some((windows, active)) => {
                            {
                                let _g = gui_write.lock().unwrap();
                                let mut wr: &PipeConn = &conn;
                                send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                            }
                            reattach_active_window(&s, &conn, &gui_write, &mut gui_attach)?;
                        }
                        None => {
                            // Refusé (une seule fenêtre) ou hors borne : liste courante.
                            let (windows, active) = s.window_list();
                            let _g = gui_write.lock().unwrap();
                            let mut wr: &PipeConn = &conn;
                            send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                        }
                    }
                }
            }
            ClientMessage::ListWindows => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    let (windows, active) = s.window_list();
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                }
            }
            ClientMessage::RenameWindow { index, name } => {
                if let Some(s) = gui_attach.as_ref().map(|ga| Arc::clone(&ga.session)) {
                    s.gui_rename_window(index, name);
                    // Renommage : seule la WindowList change (contenu/fenêtre inchangés).
                    let (windows, active) = s.window_list();
                    let _g = gui_write.lock().unwrap();
                    let mut wr: &PipeConn = &conn;
                    send(&mut wr, &ServerMessage::WindowList { windows, active })?;
                }
            }
        }
    }

    drop(attachment);
    drop(gui_attach);
    // G4 : n'effacer l'état « vue » que si CETTE connexion l'a posé (les
    // connexions `List` jetables ne doivent pas l'effacer).
    if viewed_set {
        server.set_gui_viewed(None);
    }
    Ok(())
}

/// Résultat du routage d'un fragment d'entrée.
#[derive(Default)]
struct RouteOutcome {
    detach: bool,
    clipboard: Option<String>,
}

/// Évènement souris décodé (protocole SGR 1006).
struct MouseEvent {
    button: u16,
    col: u16,
    row: u16,
    pressed: bool,
}

/// Extrait les séquences souris SGR (`ESC[<b;x;y M|m`) du flux d'entrée et
/// renvoie les évènements décodés ainsi que les octets restants.
fn extract_mouse_events(bytes: &[u8]) -> (Vec<MouseEvent>, Vec<u8>) {
    let mut events = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 2 < bytes.len() && bytes[i + 1] == b'[' && bytes[i + 2] == b'<' {
            let start = i + 3;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'M' && bytes[j] != b'm' {
                j += 1;
            }
            if j < bytes.len() {
                if let Some(ev) = parse_mouse(&bytes[start..j], bytes[j] == b'M') {
                    events.push(ev);
                }
                i = j + 1;
                continue;
            }
            // Séquence incomplète : conserver telle quelle.
            rest.extend_from_slice(&bytes[i..]);
            break;
        }
        rest.push(bytes[i]);
        i += 1;
    }
    (events, rest)
}

fn parse_mouse(params: &[u8], pressed: bool) -> Option<MouseEvent> {
    let s = std::str::from_utf8(params).ok()?;
    let mut it = s.split(';');
    let button = it.next()?.parse().ok()?;
    let col = it.next()?.parse().ok()?;
    let row = it.next()?.parse().ok()?;
    Some(MouseEvent {
        button,
        col,
        row,
        pressed,
    })
}

/// Traite un évènement souris : molette -> scrollback, clic gauche -> sélection.
fn handle_mouse(session: &Session, ev: &MouseEvent) {
    let col = ev.col.saturating_sub(1);
    let row = ev.row.saturating_sub(1);
    match ev.button {
        64 => session.mouse_scroll_at(col, row, true),
        65 => session.mouse_scroll_at(col, row, false),
        0 if ev.pressed => session.select_pane_at(col, row),
        _ => {}
    }
}

/// Exécute une action de préfixe. Renvoie `true` pour un détachement.
fn execute_action(session: &Session, action: Action) -> bool {
    match action {
        Action::Detach => return true,
        Action::SplitH => session.split(SplitDir::LeftRight),
        Action::SplitV => session.split(SplitDir::TopBottom),
        Action::NextPane => session.next_pane(),
        Action::SelectLeft => session.select(Move::Left),
        Action::SelectDown => session.select(Move::Down),
        Action::SelectUp => session.select(Move::Up),
        Action::SelectRight => session.select(Move::Right),
        Action::KillPane => session.close_active_pane(),
        Action::NewWindow => session.new_window(),
        Action::NextWindow => session.next_window(),
        Action::PrevWindow => session.prev_window(),
        Action::CopyMode => session.enter_copy_mode(),
        Action::Paste => session.paste(),
        Action::Zoom => session.toggle_zoom(),
        Action::ResizeLeft => session.resize_pane(Move::Left),
        Action::ResizeDown => session.resize_pane(Move::Down),
        Action::ResizeUp => session.resize_pane(Move::Up),
        Action::ResizeRight => session.resize_pane(Move::Right),
    }
    false
}

/// Décode le flux d'entrée : mode copie, commande de préfixe (selon la config),
/// ou frappe vers le volet actif.
fn route_input(
    session: &Session,
    config: &Config,
    prefix: &mut PrefixState,
    bytes: &[u8],
) -> RouteOutcome {
    let mut outcome = RouteOutcome::default();

    // Souris : extraire et traiter les évènements SGR avant tout le reste.
    let owned;
    let bytes: &[u8] = if config.mouse {
        let (events, rest) = extract_mouse_events(bytes);
        for ev in &events {
            handle_mouse(session, ev);
        }
        owned = rest;
        &owned
    } else {
        bytes
    };

    // En mode copie, les touches pilotent la vue et ne vont pas au shell.
    if session.active_in_copy_mode() {
        for &b in bytes {
            if let CopyAction::Copied(text) = session.copy_key(b) {
                outcome.clipboard = Some(text);
            }
        }
        return outcome;
    }

    // Invite de commande : les touches construisent la ligne ; Entrée l'exécute.
    if session.in_command_prompt() {
        for &b in bytes {
            if let Some(cmd) = session.command_key(b) {
                let _ = crate::commands::run(session, &cmd);
            }
        }
        return outcome;
    }

    let mut forward: Vec<u8> = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        if prefix.armed {
            prefix.armed = false;
            // Vider ce qui précède avant d'exécuter la commande.
            if !forward.is_empty() {
                session.send_input(&forward);
                forward.clear();
            }
            if byte == config.prefix {
                forward.push(config.prefix); // préfixe deux fois -> préfixe littéral
            } else if byte == b':' {
                session.enter_command_prompt();
            } else if byte.is_ascii_digit() {
                session.select_window((byte - b'0') as usize);
            } else if let Some(&action) = config.bindings.get(&byte) {
                if execute_action(session, action) {
                    outcome.detach = true;
                    return outcome;
                }
            }
        } else if byte == config.prefix {
            prefix.armed = true;
        } else {
            forward.push(byte);
        }
    }

    if !forward.is_empty() {
        session.send_input(&forward);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_molette_haut() {
        let (events, rest) = extract_mouse_events(b"\x1b[<64;10;5M");
        assert!(rest.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].button, 64);
        assert_eq!(events[0].col, 10);
        assert_eq!(events[0].row, 5);
        assert!(events[0].pressed);
    }

    #[test]
    fn separe_souris_et_texte() {
        let (events, rest) = extract_mouse_events(b"ab\x1b[<0;3;4Mcd");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].button, 0);
        assert_eq!(rest, b"abcd");
    }

    #[test]
    fn flux_sans_souris_intact() {
        let (events, rest) = extract_mouse_events(b"bonjour\r");
        assert!(events.is_empty());
        assert_eq!(rest, b"bonjour\r");
    }

    #[test]
    fn list_batches_vide_sans_lot() {
        let server = Server::new();
        assert!(server.list_batches().is_empty(), "aucun lot au démarrage");
    }

    #[test]
    fn review_batch_groupe_inconnu_est_erreur() {
        let server = Server::new();
        assert!(server.review_batch("batch-inexistant").is_err());
    }

    #[test]
    fn diff_agent_session_inconnue_est_erreur() {
        let server = Server::new();
        assert!(server.diff_agent("session-inexistante").is_err());
    }

    #[test]
    fn open_pr_session_inconnue_est_erreur() {
        let server = Server::new();
        let err = server
            .open_pr("session-inexistante", None, None)
            .expect_err("session inconnue doit échouer");
        assert!(err.contains("introuvable"), "message : {err}");
    }

    #[test]
    fn list_expose_layout_rev() {
        let server = Server::new();
        let s = server.create_session(Some("lr".into()), 80, 24).unwrap();
        let before = server
            .list()
            .iter()
            .find(|i| i.name == "lr")
            .unwrap()
            .layout_rev;
        s.spawn_pane(
            None,
            crate::window::SplitDir::LeftRight,
            None,
            "cmd.exe",
            &[],
        );
        let after = server
            .list()
            .iter()
            .find(|i| i.name == "lr")
            .unwrap()
            .layout_rev;
        assert!(
            after > before,
            "layout_rev remonté par list() après spawn_pane"
        );
        server.kill("lr");
    }

    #[test]
    fn server_ouvre_un_volet_navigateur() {
        let server = Server::new();
        let s = server.create_session(Some("wb".into()), 80, 24).unwrap();
        let id = s
            .open_web_pane(None, crate::window::SplitDir::LeftRight, "http://a/".into())
            .expect("un id");
        assert!(
            s.capture_pane(id).unwrap().contains("[navigateur]"),
            "le volet créé est bien un navigateur"
        );
        server.kill("wb");
    }

    /// B2.1 : le statut du moteur navigateur répond « non lancé » sans lancer
    /// de fenêtre — test pur, aucun effet de bord.
    #[test]
    fn browser_status_sans_lancement() {
        let server = Server::new();
        match server
            .browser
            .exec(crate::browser::BrowserCommand::Status)
            .unwrap()
        {
            crate::browser::BrowserReply::Status { running, .. } => assert!(!running),
            _ => panic!("Status attendu"),
        }
    }
}
