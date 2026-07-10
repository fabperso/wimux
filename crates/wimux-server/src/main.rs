//! Démon `wimux-server` : détient les sessions, pilote les ConPTY, maintient les
//! grilles VT et sert les clients via Named Pipe. Un seul serveur par
//! utilisateur, détaché du terminal qui l'a lancé (il survit à sa fermeture).
//!
//! Phase 1 : squelette + démonstration du PoC ConPTY. Lancé avec l'argument
//! `poc`, le serveur exécute une commande à travers ConPTY et affiche sa sortie
//! visible — de quoi vérifier la chaîne à la main.

use anyhow::Result;
use portable_pty::{CommandBuilder, PtySize};
use wimux_protocol::PROTOCOL_VERSION;
use wimux_server::pty::run_capture;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("serve") => wimux_server::daemon::run(),
        Some("poc") => run_poc(),
        _ => {
            println!(
                "wimux-server {} (protocole {})",
                env!("CARGO_PKG_VERSION"),
                PROTOCOL_VERSION
            );
            println!("Usage : wimux-server serve   (démarre le démon)");
            Ok(())
        }
    }
}

/// Démonstration manuelle du PoC ConPTY (phase 1).
fn run_poc() -> Result<()> {
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };

    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-Command",
        "Write-Output 'Bonjour depuis ConPTY'",
    ]);

    let capture = run_capture(cmd, size, None)?;
    println!("--- sortie visible ---");
    print!("{}", capture.visible_text());
    println!("--- code de sortie : {} ---", capture.exit_code);
    Ok(())
}
