//! Démon `wimux-server` : détient les sessions, pilote les ConPTY, maintient les
//! grilles VT et sert les clients via Named Pipe. Un seul serveur par
//! utilisateur, détaché du terminal qui l'a lancé (il survit à sa fermeture).
//!
//! Phase 0 : squelette. Le serveur ne fait encore qu'annoncer sa version.

use wimux_protocol::PROTOCOL_VERSION;

fn main() {
    println!(
        "wimux-server {} (protocole {})",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION
    );
    // TODO(J2) : créer le Named Pipe, boucle d'acceptation des clients,
    // gestionnaire de sessions, persistance.
}
