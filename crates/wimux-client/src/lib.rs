//! Client TUI `wimux` : s'exécute dans n'importe quel terminal VT, se connecte
//! au serveur, affiche la grille reçue et transmet les entrées clavier/souris.
//! Volontairement « jetable » : le fermer = se détacher.
//!
//! Phase 0 : squelette. Le rendu (crossterm) et l'attach arrivent au jalon J2.

use wimux_protocol::Version;

/// Vérifie côté client que notre version de protocole est compatible avec
/// celle annoncée par le serveur.
pub fn is_server_compatible(server: Version) -> bool {
    wimux_protocol::PROTOCOL_VERSION.is_compatible_with(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serveur_meme_majeure_compatible() {
        let server = Version {
            major: wimux_protocol::PROTOCOL_VERSION.major,
            minor: wimux_protocol::PROTOCOL_VERSION.minor + 5,
        };
        assert!(is_server_compatible(server));
    }
}
