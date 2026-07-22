//! Protocole RPC partagé entre le client `wimux` et le démon `wimux-server`.
//!
//! Transport : Named Pipe Windows. Cadrage des messages : préfixe de longueur
//! `u32` little-endian suivi du corps sérialisé avec `postcard`. Le premier
//! message d'une connexion est **toujours** un [`ClientMessage::Hello`] qui
//! négocie la version ; en cas d'incompatibilité de version majeure, le serveur
//! refuse proprement au lieu de corrompre l'affichage. C'est ce qui permet de
//! mettre à jour le serveur (qui survit en arrière-plan) sans casser les
//! clients, ou l'inverse.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use wimux_vt::Cell;

pub mod transport;

/// Version du protocole. Incrémenter `MAJOR` casse la compatibilité fil de fer ;
/// `MINOR` ajoute des messages rétro-compatibles.
pub const PROTOCOL_VERSION: Version = Version { major: 0, minor: 1 };

/// Préfixe du Named Pipe. Le chemin complet est `\\.\pipe\wimux-<user>` afin
/// d'isoler les serveurs par utilisateur.
pub const PIPE_PREFIX: &str = r"\\.\pipe\wimux";

/// Taille maximale acceptée pour un message entrant (garde-fou anti-abus).
const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Version {
    /// Deux versions sont compatibles si elles partagent la même majeure.
    pub fn is_compatible_with(self, other: Version) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Message d'ouverture envoyé par le client dès la connexion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub client_version: Version,
    /// Version du binaire client, à titre informatif (diagnostic).
    pub client_build: String,
}

/// Réponse du serveur au [`Hello`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HelloReply {
    Ok {
        server_version: Version,
    },
    VersionMismatch {
        server_version: Version,
        reason: String,
    },
}

/// Modèle d'agent configuré côté serveur (M2). Le frontend n'en lit que le nom ;
/// le serveur possède `program`/`args` et effectue la substitution `{prompt}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}

/// Statut calculé d'une session agent (M1). Sérialisé sur [`SessionInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// Le volet racine produit de la sortie récemment.
    Working,
    /// Vivant mais silencieux au-delà du seuil d'inactivité.
    Idle,
    /// Une cloche (BEL) est en attente d'être vue.
    Attention,
    /// Le volet racine a quitté avec le code 0.
    Done,
    /// Le volet racine a quitté avec un code non nul.
    Error,
}

/// Résumé d'une session, tel qu'affiché par `wimux list-sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    /// Sortie non vue depuis la dernière vue GUI (G4).
    pub activity: bool,
    /// BEL explicite reçu depuis la dernière vue GUI (G4).
    pub bell: bool,
    /// Est-ce une session agent ? (M1)
    pub agent: bool,
    /// Statut de l'agent ; `None` si `agent == false` (M1).
    pub agent_status: Option<AgentStatus>,
    /// Identifiant de lot (M3) : les sessions d'un même fan-out le partagent
    /// (`batch<N>`). `None` pour une session hors lot.
    pub group: Option<String>,
    /// cwd courant du volet actif (chemin natif affichable), `None` si inconnu (W3).
    pub cwd: Option<String>,
    /// Branche git du cwd, `None` si hors repo / inconnu (W3).
    pub branch: Option<String>,
    /// Couleur d'accent du workspace (hex `#rrggbb`), `None` = défaut (W5).
    pub color: Option<String>,
    /// Workspace épinglé : trié en tête du rail (W5).
    pub pinned: bool,
    /// Compteur de révision de la topologie de volets (A1) : bumpé à chaque
    /// création/fermeture de volet. La GUI le compare pour se réattacher et
    /// refléter en direct les volets créés via la CLI.
    pub layout_rev: u64,
}

/// Sens d'une découpe de volet (miroir du `window::SplitDir` serveur).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SplitDir {
    LeftRight,
    TopBottom,
}

/// Nature d'une feuille de disposition (B1) : terminal, ou navigateur portant son
/// URL courante. C'est ce qui dit au frontend quoi rendre pour cette feuille.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaneKind {
    Terminal,
    Web { url: String },
}

/// Arbre de disposition d'une fenêtre, sérialisable pour la GUI. Chaque `Split`
/// porte un `node_id` stable (attribué à la création) pour cibler `SetSplitRatio`
/// sans ambiguïté même si l'arbre a changé ailleurs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutNode {
    Leaf {
        pane_id: u64,
        /// B1 : terminal ou navigateur (+ URL). **Ajout de champ assumé** : il
        /// change tous les encodages de `Leaf`, d'où rebuild + redémarrage daemon.
        kind: PaneKind,
    },
    Split {
        node_id: u32,
        dir: SplitDir,
        ratio: f32,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
}

/// Instantané complet de la grille d'un volet, envoyé au client pour affichage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub cursor_col: u16,
    pub cursor_row: u16,
    /// Cellules en ordre ligne par ligne (`rows * cols` éléments).
    pub cells: Vec<Cell>,
}

/// Résumé d'une fenêtre (onglet) d'une session GUI-attachée (W2). La GUI affiche
/// `name` s'il est présent, sinon la position (1-based).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Nom explicite de la fenêtre, ou `None` (la GUI affiche alors, à défaut, le
    /// nom du répertoire courant `cwd`, sinon la position).
    pub name: Option<String>,
    /// Répertoire courant du volet actif de la fenêtre (W3/W4), ou `None`. Sert de
    /// libellé d'onglet par défaut (basename) façon CMUX.
    pub cwd: Option<String>,
}

/// Notification émise par un programme dans une session via `OSC 9` / `OSC 777`,
/// remontée à la GUI (toast OS + panneau) (W6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationInfo {
    /// Session (workspace) d'origine.
    pub session: String,
    /// Titre (OSC 777), ou `None` (OSC 9).
    pub title: Option<String>,
    /// Corps du message.
    pub body: String,
}

/// Résumé d'un volet, pour l'orchestration agent (A1). Renvoyé par `ListPanes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Identifiant global du volet.
    pub pane_id: u64,
    /// cwd courant (dernier OSC 7 capté), `None` si inconnu.
    pub cwd: Option<String>,
    /// Le processus du volet est-il encore vivant ?
    pub running: bool,
    /// Code de sortie si terminé, `None` s'il tourne encore.
    pub exit_code: Option<i32>,
    /// Chemin du fichier journal si ce volet est journalisé (volet agent).
    pub log_path: Option<String>,
}

/// Résumé d'un lot d'agents (M4). Renvoyé par `ListBatches`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchInfo {
    /// Identifiant de lot (`batch<N>`), partagé par les sessions membres.
    pub group: String,
    /// Noms de session des membres, dans l'ordre des index.
    pub sessions: Vec<String>,
    /// Dépôt de base du lot (chemin natif).
    pub base_repo: String,
    /// Branche du dépôt de base au lancement — cible des futures PR.
    pub base_branch: String,
}

/// Résultat produit par un agent d'un lot (M4). Renvoyé par `ReviewBatch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResult {
    pub session: String,
    /// Rang de l'agent dans son lot (dérivé du suffixe du nom de session).
    pub index: u32,
    pub branch: String,
    /// Statut d'agent (M1), `None` si indisponible.
    pub status: Option<AgentStatus>,
    /// Fichiers suivis modifiés vs la base (commité + en cours).
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
    /// Nombre de fichiers NON suivis (comptés à part : aucun double comptage).
    pub untracked: u32,
    /// L'agent a-t-il au moins un commit au-delà de la base ?
    pub has_commits: bool,
}

/// Messages client -> serveur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Hello(Hello),
    /// Créer une session (nom optionnel -> généré si absent) et s'y attacher.
    NewSession {
        name: Option<String>,
        cols: u16,
        rows: u16,
    },
    /// S'attacher à une session existante.
    Attach {
        name: String,
        cols: u16,
        rows: u16,
    },
    /// Lister les sessions.
    List,
    /// Détruire une session.
    Kill {
        name: String,
    },
    /// Frappe(s) clavier à transmettre au volet actif.
    Input(Vec<u8>),
    /// S'attacher en mode GUI (flux bruts par volet).
    AttachGui {
        session: String,
    },
    /// Frappe(s) vers un volet précis (mode GUI).
    PaneInput {
        pane_id: u64,
        bytes: Vec<u8>,
    },
    /// Un volet a changé de taille dans la GUI.
    PaneResize {
        pane_id: u64,
        cols: u16,
        rows: u16,
    },
    /// Découpe le volet désigné (mode GUI) ; le nouveau volet devient actif.
    SplitPane {
        pane_id: u64,
        dir: SplitDir,
    },
    /// Ferme le volet désigné (mode GUI).
    ClosePane {
        pane_id: u64,
    },
    /// Désigne le volet actif (mode GUI).
    FocusPane {
        pane_id: u64,
    },
    /// Fixe le ratio d'un nœud de découpe interne (glisser-bordure). Borné
    /// `[0.1, 0.9]` côté serveur.
    SetSplitRatio {
        node_id: u32,
        ratio: f32,
    },
    /// Crée une session sans s'y attacher (mode GUI). Nom auto si `None`.
    CreateSession {
        name: Option<String>,
    },
    /// Renomme une session.
    RenameSession {
        from: String,
        to: String,
    },
    /// Commande scriptable : injecte des octets dans le volet actif d'une session
    /// nommée (comme `tmux send-keys -t <session>`).
    SendKeys {
        session: String,
        keys: Vec<u8>,
    },
    /// Commande textuelle scriptable (split-window, list-panes, capture-pane...).
    Command {
        session: String,
        command: String,
    },
    /// Le client (donc le volet actif) a changé de taille.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Se détacher (la session survit).
    Detach,
    /// Demander l'arrêt du serveur.
    Shutdown,
    /// Vérifier que le serveur répond.
    Ping,
    /// Lister les modèles d'agents configurés (mode GUI, pour le lanceur).
    ListAgentTemplates,
    /// Crée une session agent depuis un modèle. Le serveur substitue `{prompt}`
    /// dans les args s'il est présent, sinon envoie le prompt + Entrée sur le
    /// stdin du volet racine après le spawn. Nom auto `<template>-<n>` si `None`.
    CreateAgentSession {
        name: Option<String>,
        template: String,
        prompt: String,
        cwd: Option<String>,
    },
    /// Crée un **lot** (M3) : `count` sessions agent depuis un même modèle, chacune
    /// dans un worktree git de `base_repo`. Orchestration côté serveur (atomique).
    CreateAgentBatch {
        template: String,
        prompt: String,
        base_repo: String,
        count: u32,
    },
    /// Crée une fenêtre (onglet) dans la session GUI-attachée et la rend active (W2).
    NewWindow,
    /// Rend active la fenêtre `index` de la session GUI-attachée (W2).
    SelectWindow {
        index: u32,
    },
    /// Ferme la fenêtre `index` (tue ses volets) ; no-op s'il ne reste qu'une
    /// fenêtre (W2).
    CloseWindow {
        index: u32,
    },
    /// Nomme la fenêtre `index` ; un nom vide efface le nom (W2).
    RenameWindow {
        index: u32,
        name: String,
    },
    /// Demande la liste courante des fenêtres (onglets) de la session GUI-attachée
    /// (W4, sondage périodique) : le serveur répond par `WindowList` sur la
    /// connexion persistante, ce qui rafraîchit les libellés d'onglet (cwd).
    ListWindows,
    /// Réordonne les sessions du rail selon `names` (glisser-déposer, W4/W5) :
    /// chaque session prend l'ordre de sa position dans la liste.
    ReorderSessions {
        names: Vec<String>,
    },
    /// Fixe la couleur d'accent d'un workspace (hex `#rrggbb`), `None` = défaut (W5).
    SetSessionColor {
        name: String,
        color: Option<String>,
    },
    /// Épingle ou désépingle un workspace (trié en tête du rail) (W5).
    SetSessionPinned {
        name: String,
        pinned: bool,
    },
    /// Récupère (et draine) les notifications OSC 9/777 en attente, toutes sessions
    /// confondues (W6, sondage). Le serveur répond par `Notifications`.
    TakeNotifications,
    /// Marque un workspace comme lu (efface activité + cloche) sans l'attacher (W6).
    MarkSessionRead {
        name: String,
    },
    /// Marque un workspace comme non lu (repose le drapeau cloche) (W6).
    MarkSessionUnread {
        name: String,
    },
    /// A1 : découpe la fenêtre active de `session` (à partir de `from_pane`, défaut
    /// volet actif) et lance `program`/`args` dans le nouveau volet (journalisé).
    SpawnPane {
        session: String,
        from_pane: Option<u64>,
        dir: SplitDir,
        cwd: Option<String>,
        program: String,
        args: Vec<String>,
    },
    /// A1 : capture le contenu visible du volet `pane` de `session`.
    CapturePane {
        session: String,
        pane: u64,
    },
    /// A1 : liste les volets de `session` (structuré).
    ListPanes {
        session: String,
    },
    /// A1 : envoie des octets au volet `pane` de `session`.
    SendKeysPane {
        session: String,
        pane: u64,
        keys: Vec<u8>,
    },
    /// A1 : ferme le volet `pane` de `session`.
    KillPane {
        session: String,
        pane: u64,
    },
    /// M4 : lister les lots d'agents en cours.
    ListBatches,
    /// M4 : résumé par agent des résultats d'un lot.
    ReviewBatch {
        group: String,
    },
    /// M4 : diff complet du travail d'un agent.
    DiffAgent {
        session: String,
    },
    /// M4 : intégrer le travail d'un agent par Pull Request (commit du WIP,
    /// push, `gh pr create`), puis nettoyer les perdants du lot.
    OpenPr {
        session: String,
        title: Option<String>,
        body: Option<String>,
    },
    /// B1 : ouvre un volet NAVIGATEUR en découpant depuis `from_pane` (défaut :
    /// volet actif). Réponse : `PaneSpawned { pane_id }`.
    OpenWebPane {
        session: String,
        from_pane: Option<u64>,
        dir: SplitDir,
        url: String,
    },
    /// B1 : fait naviguer un volet navigateur vers `url` (empile l'historique).
    WebNavigate {
        session: String,
        pane: u64,
        url: String,
    },
    /// B1 : recule d'un cran dans la pile d'URL du volet.
    WebBack {
        session: String,
        pane: u64,
    },
    /// B1 : avance d'un cran dans la pile d'URL du volet.
    WebForward {
        session: String,
        pane: u64,
    },
    /// B2.1 : lance le navigateur pilotable (no-op s'il tourne déjà).
    BrowserLaunch,
    /// B2.1 : ferme le navigateur pilotable.
    BrowserClose,
    /// B2.1 : état du navigateur (lancé ? URL courante ?).
    BrowserStatus,
    /// B2.1 : navigue (lance au besoin) ; refuse les schémas non http(s).
    BrowserNavigate {
        url: String,
    },
    /// B2.1 : URL courante (erreur si non lancé).
    BrowserUrl,
    /// B2.1 : arbre d'accessibilité de la page (erreur si non lancé).
    BrowserSnapshot,
    /// B2.1 : capture PNG écrite sur disque, renvoie le chemin (erreur si non lancé).
    BrowserScreenshot,
    /// B2.2 : clic gauche sur l'élément désigné par une ref de snapshot.
    BrowserClick {
        ref_: String,
    },
    /// B2.2 : vide le champ puis saisit du texte.
    BrowserType {
        ref_: String,
        text: String,
    },
}

/// Messages serveur -> client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Hello(HelloReply),
    Sessions(Vec<SessionInfo>),
    /// Attachement réussi à la session nommée.
    Attached {
        name: String,
    },
    /// Nouvel état d'affichage du volet actif.
    Frame(Frame),
    /// Le processus du volet actif s'est terminé.
    PaneExited {
        code: u32,
    },
    /// Le serveur a détaché ce client (`Ctrl-b d`) ; le client doit quitter le
    /// mode plein écran. La session survit.
    Detached,
    /// Contenu initial d'un volet (mode GUI), pour restaurer l'affichage.
    PaneSnapshot {
        pane_id: u64,
        bytes: Vec<u8>,
    },
    /// Flux brut d'un volet (mode GUI).
    PaneOutput {
        pane_id: u64,
        bytes: Vec<u8>,
    },
    /// Disposition de la fenêtre active (mode GUI). Envoyé à l'attache et après
    /// chaque changement de topologie ou de ratio.
    WindowLayout {
        tree: LayoutNode,
        active: u64,
    },
    /// Session créée (réponse à `CreateSession`).
    SessionCreated {
        name: String,
    },
    /// Texte à placer dans le presse-papiers du système (suite à une copie).
    SetClipboard(String),
    /// Résultat textuel d'une commande scriptable.
    CommandResult(String),
    /// Erreur applicative.
    Error(String),
    Pong,
    /// Acquittement générique.
    Ok,
    /// Liste des modèles d'agents (réponse à `ListAgentTemplates`).
    AgentTemplates(Vec<AgentTemplate>),
    /// Lot créé (réponse à `CreateAgentBatch`) : identifiant de groupe + noms des
    /// sessions membres.
    BatchCreated {
        group: String,
        sessions: Vec<String>,
    },
    /// Liste des fenêtres (onglets) de la session GUI-attachée (W2). Émis à
    /// l'attache (état initial) et après chaque opération de fenêtre.
    WindowList {
        windows: Vec<WindowInfo>,
        active: u32,
    },
    /// Notifications OSC 9/777 en attente (réponse à `TakeNotifications`) (W6).
    Notifications(Vec<NotificationInfo>),
    /// A1 : réponse à `SpawnPane` — identifiant du volet créé.
    PaneSpawned {
        pane_id: u64,
    },
    /// A1 : réponse à `CapturePane` — contenu visible du volet.
    PaneCapture(String),
    /// A1 : réponse à `ListPanes`.
    PaneList(Vec<PaneInfo>),
    /// M4 : réponse à `ListBatches`.
    Batches(Vec<BatchInfo>),
    /// M4 : réponse à `ReviewBatch`.
    BatchReview(Vec<AgentResult>),
    /// M4 : réponse à `DiffAgent`.
    AgentDiff(String),
    /// M4 : réponse à `OpenPr` — URL de la Pull Request créée.
    PrOpened {
        url: String,
    },
    /// B2.1 : réponse à `BrowserStatus`.
    BrowserState {
        running: bool,
        url: Option<String>,
    },
    /// B2.1 : réponse texte (url / navigate / snapshot).
    BrowserText(String),
    /// B2.1 : réponse à `BrowserScreenshot` — chemin du PNG.
    BrowserShot {
        path: String,
    },
}

// --- Cadrage (framing) longueur + postcard --------------------------------

/// Sérialise et envoie un message précédé de sa longueur.
pub fn send<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body =
        postcard::to_allocvec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message trop grand"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Reçoit et désérialise un message. Renvoie une erreur `UnexpectedEof` propre
/// quand le pair a fermé la connexion.
pub fn recv<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message dépassant la taille maximale",
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meme_majeure_est_compatible() {
        let a = Version { major: 1, minor: 0 };
        let b = Version { major: 1, minor: 7 };
        assert!(a.is_compatible_with(b));
    }

    #[test]
    fn majeure_differente_est_incompatible() {
        let a = Version { major: 1, minor: 0 };
        let b = Version { major: 2, minor: 0 };
        assert!(!a.is_compatible_with(b));
    }

    #[test]
    fn version_saffiche() {
        assert_eq!(PROTOCOL_VERSION.to_string(), "0.1");
    }

    #[test]
    fn aller_retour_message() {
        let msg = ClientMessage::NewSession {
            name: Some("dev".into()),
            cols: 80,
            rows: 24,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();

        let mut cursor = io::Cursor::new(buf);
        let decoded: ClientMessage = recv(&mut cursor).unwrap();
        match decoded {
            ClientMessage::NewSession { name, cols, rows } => {
                assert_eq!(name.as_deref(), Some("dev"));
                assert_eq!((cols, rows), (80, 24));
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn eof_propre_quand_le_pair_ferme() {
        let mut cursor = io::Cursor::new(Vec::new());
        let res: io::Result<ClientMessage> = recv(&mut cursor);
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn aller_retour_attach_gui() {
        let msg = ClientMessage::AttachGui {
            session: "dev".into(),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::AttachGui { session } => assert_eq!(session, "dev"),
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_pane_output() {
        let msg = ServerMessage::PaneOutput {
            pane_id: 7,
            bytes: b"hello".to_vec(),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::PaneOutput { pane_id, bytes } => {
                assert_eq!(pane_id, 7);
                assert_eq!(bytes, b"hello");
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_create_session() {
        let msg = ClientMessage::CreateSession {
            name: Some("dev".into()),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CreateSession { name } => assert_eq!(name.as_deref(), Some("dev")),
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_rename_session() {
        let msg = ClientMessage::RenameSession {
            from: "a".into(),
            to: "b".into(),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::RenameSession { from, to } => {
                assert_eq!(from, "a");
                assert_eq!(to, "b");
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_split_pane() {
        let msg = ClientMessage::SplitPane {
            pane_id: 3,
            dir: SplitDir::TopBottom,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::SplitPane { pane_id, dir } => {
                assert_eq!(pane_id, 3);
                assert_eq!(dir, SplitDir::TopBottom);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_window_layout() {
        let tree = LayoutNode::Split {
            node_id: 1,
            dir: SplitDir::LeftRight,
            ratio: 0.5,
            a: Box::new(LayoutNode::Leaf {
                pane_id: 10,
                kind: PaneKind::Terminal,
            }),
            b: Box::new(LayoutNode::Leaf {
                pane_id: 11,
                kind: PaneKind::Terminal,
            }),
        };
        let msg = ServerMessage::WindowLayout {
            tree: tree.clone(),
            active: 10,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::WindowLayout { tree: got, active } => {
                assert_eq!(active, 10);
                assert_eq!(got, tree);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_session_info_activite() {
        let info = SessionInfo {
            name: "dev".into(),
            windows: 2,
            attached: true,
            activity: true,
            bell: false,
            agent: false,
            agent_status: None,
            group: None,
            cwd: None,
            branch: None,
            color: None,
            pinned: false,
            layout_rev: 0,
        };
        let msg = ServerMessage::Sessions(vec![info]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::Sessions(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "dev");
                assert_eq!(v[0].windows, 2);
                assert!(v[0].attached);
                assert!(v[0].activity);
                assert!(!v[0].bell);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_session_info_agent() {
        let info = SessionInfo {
            name: "bot".into(),
            windows: 1,
            attached: false,
            activity: false,
            bell: false,
            agent: true,
            agent_status: Some(AgentStatus::Working),
            group: Some("batch0".into()),
            cwd: None,
            branch: None,
            color: None,
            pinned: false,
            layout_rev: 0,
        };
        let msg = ServerMessage::Sessions(vec![info]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::Sessions(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "bot");
                assert!(v[0].agent);
                assert_eq!(v[0].agent_status, Some(AgentStatus::Working));
                assert_eq!(v[0].group.as_deref(), Some("batch0"));
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_agent_template() {
        let tpl = AgentTemplate {
            name: "claude".into(),
            program: "claude".into(),
            args: vec!["-p".into(), "{prompt}".into()],
        };
        let msg = ServerMessage::AgentTemplates(vec![tpl.clone()]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::AgentTemplates(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], tpl);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_list_agent_templates() {
        let msg = ClientMessage::ListAgentTemplates;
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        assert!(matches!(
            recv::<_, ClientMessage>(&mut cur).unwrap(),
            ClientMessage::ListAgentTemplates
        ));
    }

    #[test]
    fn aller_retour_create_agent_session() {
        let msg = ClientMessage::CreateAgentSession {
            name: Some("bot".into()),
            template: "claude".into(),
            prompt: "corrige le bug".into(),
            cwd: Some("C:\\proj".into()),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CreateAgentSession {
                name,
                template,
                prompt,
                cwd,
            } => {
                assert_eq!(name.as_deref(), Some("bot"));
                assert_eq!(template, "claude");
                assert_eq!(prompt, "corrige le bug");
                assert_eq!(cwd.as_deref(), Some("C:\\proj"));
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_create_agent_batch() {
        let msg = ClientMessage::CreateAgentBatch {
            template: "echo".into(),
            prompt: "corrige le bug".into(),
            base_repo: "C:\\proj".into(),
            count: 3,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CreateAgentBatch {
                template,
                prompt,
                base_repo,
                count,
            } => {
                assert_eq!(template, "echo");
                assert_eq!(prompt, "corrige le bug");
                assert_eq!(base_repo, "C:\\proj");
                assert_eq!(count, 3);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_batch_created() {
        let msg = ServerMessage::BatchCreated {
            group: "batch0".into(),
            sessions: vec!["echo-batch0-0".into(), "echo-batch0-1".into()],
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::BatchCreated { group, sessions } => {
                assert_eq!(group, "batch0");
                assert_eq!(sessions, vec!["echo-batch0-0", "echo-batch0-1"]);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_window_info_et_liste() {
        let msg = ServerMessage::WindowList {
            windows: vec![
                WindowInfo {
                    name: Some("build".into()),
                    cwd: Some("C:\\proj".into()),
                },
                WindowInfo {
                    name: None,
                    cwd: None,
                },
            ],
            active: 1,
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::WindowList { windows, active } => {
                assert_eq!(active, 1);
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].name.as_deref(), Some("build"));
                assert_eq!(windows[1].name, None);
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_new_window() {
        let mut buf = Vec::new();
        send(&mut buf, &ClientMessage::NewWindow).unwrap();
        let mut cur = io::Cursor::new(buf);
        assert!(matches!(
            recv::<_, ClientMessage>(&mut cur).unwrap(),
            ClientMessage::NewWindow
        ));
    }

    #[test]
    fn aller_retour_select_et_close_window() {
        let mut buf = Vec::new();
        send(&mut buf, &ClientMessage::SelectWindow { index: 2 }).unwrap();
        send(&mut buf, &ClientMessage::CloseWindow { index: 3 }).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::SelectWindow { index } => assert_eq!(index, 2),
            _ => panic!("mauvais variant"),
        }
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::CloseWindow { index } => assert_eq!(index, 3),
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_rename_window() {
        let msg = ClientMessage::RenameWindow {
            index: 0,
            name: "build".into(),
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ClientMessage>(&mut cur).unwrap() {
            ClientMessage::RenameWindow { index, name } => {
                assert_eq!(index, 0);
                assert_eq!(name, "build");
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_session_info_cwd_branche() {
        let info = SessionInfo {
            name: "dev".into(),
            windows: 1,
            attached: true,
            activity: false,
            bell: false,
            agent: false,
            agent_status: None,
            group: None,
            cwd: Some("C:\\proj\\wimux".into()),
            branch: Some("main".into()),
            color: Some("#e8833a".into()),
            pinned: true,
            layout_rev: 0,
        };
        let msg = ServerMessage::Sessions(vec![info]);
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut cur = io::Cursor::new(buf);
        match recv::<_, ServerMessage>(&mut cur).unwrap() {
            ServerMessage::Sessions(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].cwd.as_deref(), Some("C:\\proj\\wimux"));
                assert_eq!(v[0].branch.as_deref(), Some("main"));
            }
            _ => panic!("mauvais variant"),
        }
    }

    #[test]
    fn aller_retour_spawn_pane_et_pane_list() {
        let msg = ClientMessage::SpawnPane {
            session: "s".into(),
            from_pane: Some(3),
            dir: SplitDir::LeftRight,
            cwd: Some("C:\\repo".into()),
            program: "claude".into(),
            args: vec!["-p".into(), "tache".into()],
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        match postcard::from_bytes::<ClientMessage>(&bytes).unwrap() {
            ClientMessage::SpawnPane {
                program,
                args,
                from_pane,
                ..
            } => {
                assert_eq!(program, "claude");
                assert_eq!(args, vec!["-p".to_string(), "tache".to_string()]);
                assert_eq!(from_pane, Some(3));
            }
            _ => panic!("variante inattendue"),
        }

        let info = PaneInfo {
            pane_id: 7,
            cwd: Some("C:\\repo".into()),
            running: true,
            exit_code: None,
            log_path: Some("C:\\log\\7.log".into()),
        };
        let reply = ServerMessage::PaneList(vec![info.clone()]);
        let bytes = postcard::to_allocvec(&reply).unwrap();
        match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
            ServerMessage::PaneList(v) => assert_eq!(v[0], info),
            _ => panic!("variante inattendue"),
        }
    }

    #[test]
    fn aller_retour_review_batch_et_open_pr() {
        let msg = ClientMessage::OpenPr {
            session: "claude-batch0-1".into(),
            title: Some("fix: gérer le payload vide".into()),
            body: None,
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        match postcard::from_bytes::<ClientMessage>(&bytes).unwrap() {
            ClientMessage::OpenPr {
                session,
                title,
                body,
            } => {
                assert_eq!(session, "claude-batch0-1");
                assert_eq!(title.as_deref(), Some("fix: gérer le payload vide"));
                assert_eq!(body, None);
            }
            _ => panic!("variante inattendue"),
        }

        let res = AgentResult {
            session: "claude-batch0-1".into(),
            index: 1,
            branch: "wimux/batch0/1".into(),
            status: Some(AgentStatus::Done),
            files_changed: 3,
            insertions: 42,
            deletions: 7,
            untracked: 2,
            has_commits: true,
        };
        let reply = ServerMessage::BatchReview(vec![res.clone()]);
        let bytes = postcard::to_allocvec(&reply).unwrap();
        match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
            ServerMessage::BatchReview(v) => assert_eq!(v[0], res),
            _ => panic!("variante inattendue"),
        }

        let info = BatchInfo {
            group: "batch0".into(),
            sessions: vec!["claude-batch0-0".into(), "claude-batch0-1".into()],
            base_repo: "C:\\repo".into(),
            base_branch: "main".into(),
        };
        let bytes = postcard::to_allocvec(&ServerMessage::Batches(vec![info.clone()])).unwrap();
        match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
            ServerMessage::Batches(v) => assert_eq!(v[0], info),
            _ => panic!("variante inattendue"),
        }
    }

    #[test]
    fn aller_retour_leaf_web_et_messages_navigateur() {
        // Une feuille NAVIGATEUR transporte son URL.
        let tree = LayoutNode::Leaf {
            pane_id: 4,
            kind: PaneKind::Web {
                url: "http://localhost:5173/".into(),
            },
        };
        let bytes = postcard::to_allocvec(&tree).unwrap();
        match postcard::from_bytes::<LayoutNode>(&bytes).unwrap() {
            LayoutNode::Leaf { pane_id, kind } => {
                assert_eq!(pane_id, 4);
                assert_eq!(
                    kind,
                    PaneKind::Web {
                        url: "http://localhost:5173/".into()
                    }
                );
            }
            _ => panic!("attendu une feuille"),
        }

        // Une feuille TERMINAL reste distinguable.
        let tree = LayoutNode::Leaf {
            pane_id: 1,
            kind: PaneKind::Terminal,
        };
        let bytes = postcard::to_allocvec(&tree).unwrap();
        match postcard::from_bytes::<LayoutNode>(&bytes).unwrap() {
            LayoutNode::Leaf { kind, .. } => assert_eq!(kind, PaneKind::Terminal),
            _ => panic!("attendu une feuille"),
        }

        let msg = ClientMessage::OpenWebPane {
            session: "s".into(),
            from_pane: Some(2),
            dir: SplitDir::LeftRight,
            url: "http://localhost:3000/".into(),
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        match postcard::from_bytes::<ClientMessage>(&bytes).unwrap() {
            ClientMessage::OpenWebPane { from_pane, url, .. } => {
                assert_eq!(from_pane, Some(2));
                assert_eq!(url, "http://localhost:3000/");
            }
            _ => panic!("variante inattendue"),
        }

        let msg = ClientMessage::WebBack {
            session: "s".into(),
            pane: 4,
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        assert!(matches!(
            postcard::from_bytes::<ClientMessage>(&bytes).unwrap(),
            ClientMessage::WebBack { pane: 4, .. }
        ));
    }

    #[test]
    fn aller_retour_messages_navigateur() {
        let msg = ClientMessage::BrowserNavigate {
            url: "http://localhost:8899/".into(),
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        assert!(matches!(
            postcard::from_bytes::<ClientMessage>(&bytes).unwrap(),
            ClientMessage::BrowserNavigate { url } if url == "http://localhost:8899/"
        ));

        let reply = ServerMessage::BrowserState {
            running: true,
            url: Some("http://localhost:8899/".into()),
        };
        let bytes = postcard::to_allocvec(&reply).unwrap();
        match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
            ServerMessage::BrowserState { running, url } => {
                assert!(running);
                assert_eq!(url.as_deref(), Some("http://localhost:8899/"));
            }
            _ => panic!("variante inattendue"),
        }

        let shot = ServerMessage::BrowserShot {
            path: "C:\\x\\1.png".into(),
        };
        let bytes = postcard::to_allocvec(&shot).unwrap();
        assert!(matches!(
            postcard::from_bytes::<ServerMessage>(&bytes).unwrap(),
            ServerMessage::BrowserShot { path } if path == "C:\\x\\1.png"
        ));
    }
}
