//! Protocole RPC partagé entre le client `wimux` et le démon `wimux-server`.
//!
//! Tous les échanges passent par un Named Pipe. Le premier message d'une
//! connexion est **toujours** un [`Hello`] qui négocie la version : si le
//! client et le serveur ne parlent pas la même version majeure, le serveur
//! refuse proprement au lieu de corrompre l'affichage. C'est ce qui permet de
//! mettre à jour le serveur (qui survit en arrière-plan) sans casser les
//! clients déjà attachés ou l'inverse.

use serde::{Deserialize, Serialize};

/// Version du protocole. Incrémenter `MAJOR` casse la compatibilité fil de fer ;
/// `MINOR` ajoute des messages rétro-compatibles.
pub const PROTOCOL_VERSION: Version = Version { major: 0, minor: 1 };

/// Nom de base du Named Pipe. Le chemin complet est
/// `\\.\pipe\wimux-<user>-<socket>` pour isoler les serveurs par utilisateur.
pub const PIPE_PREFIX: &str = r"\\.\pipe\wimux";

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
    /// Handshake accepté.
    Ok { server_version: Version },
    /// Versions de protocole incompatibles ; la connexion sera fermée.
    VersionMismatch {
        server_version: Version,
        reason: String,
    },
}

/// Messages client -> serveur (au-delà du handshake). Volontairement minimal
/// en phase 0 ; enrichi au fil des phases (Attach, Input, Resize, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Demande la liste des sessions.
    ListSessions,
    /// Demande l'arrêt propre du serveur (si aucune session).
    Shutdown,
    /// Vérifie que le serveur répond.
    Ping,
}

/// Messages serveur -> client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Sessions(Vec<SessionInfo>),
    Pong,
    /// Erreur applicative renvoyée au client.
    Error(String),
}

/// Résumé d'une session, tel qu'affiché par `wimux list-sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
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
}
