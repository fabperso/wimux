//! Binaire `wimux` : point d'entrée utilisateur. Il analyse la ligne de commande,
//! démarre le serveur s'il est absent, puis soit lance un client TUI (attach),
//! soit envoie une commande de contrôle au serveur.
//!
//! Phase 0 : gère uniquement `--version` / `-V` et `--help` / `-h`.

use wimux_protocol::PROTOCOL_VERSION;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!(
                "wimux {} (protocole {})",
                env!("CARGO_PKG_VERSION"),
                PROTOCOL_VERSION
            );
            std::process::ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | None => {
            print_help();
            std::process::ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("wimux : commande inconnue « {other} » (essayez `wimux --help`)");
            std::process::ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "wimux — multiplexeur de terminal natif Windows\n\
         \n\
         USAGE :\n    \
             wimux [COMMANDE]\n\
         \n\
         OPTIONS :\n    \
             -V, --version    Affiche la version\n    \
             -h, --help       Affiche cette aide\n\
         \n\
         Projet en construction (phase 0). Les commandes new/attach/list\n\
         arriveront aux prochaines phases."
    );
}
