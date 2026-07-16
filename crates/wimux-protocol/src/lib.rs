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
}

/// Sens d'une découpe de volet (miroir du `window::SplitDir` serveur).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SplitDir {
    LeftRight,
    TopBottom,
}

/// Arbre de disposition d'une fenêtre, sérialisable pour la GUI. Chaque `Split`
/// porte un `node_id` stable (attribué à la création) pour cibler `SetSplitRatio`
/// sans ambiguïté même si l'arbre a changé ailleurs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutNode {
    Leaf {
        pane_id: u64,
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
    /// Nom explicite de la fenêtre, ou `None` (la GUI affiche alors la position).
    pub name: Option<String>,
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
            a: Box::new(LayoutNode::Leaf { pane_id: 10 }),
            b: Box::new(LayoutNode::Leaf { pane_id: 11 }),
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
                },
                WindowInfo { name: None },
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
}
