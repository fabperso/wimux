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

/// Résumé d'une session, tel qu'affiché par `wimux list-sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
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
    /// Texte à placer dans le presse-papiers du système (suite à une copie).
    SetClipboard(String),
    /// Résultat textuel d'une commande scriptable.
    CommandResult(String),
    /// Erreur applicative.
    Error(String),
    Pong,
    /// Acquittement générique.
    Ok,
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
}
