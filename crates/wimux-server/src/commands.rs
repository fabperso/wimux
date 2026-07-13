//! Interpréteur de commandes textuelles, partagé par l'invite (`Ctrl-b :`) et la
//! CLI scriptable (`wimux <commande> -t <session>`). Syntaxe façon tmux.

use crate::session::Session;
use crate::window::SplitDir;

/// Résultat de l'exécution d'une commande.
pub enum CommandResult {
    /// Action effectuée, sans sortie.
    None,
    /// Sortie textuelle à renvoyer (ex. `list-panes`, `capture-pane`).
    Text(String),
}

/// Exécute une commande sur la session donnée.
pub fn run(session: &Session, input: &str) -> CommandResult {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    match tokens.as_slice() {
        ["split-window", "-h"] | ["splitw", "-h"] => {
            session.split(SplitDir::LeftRight);
            CommandResult::None
        }
        ["split-window"] | ["split-window", "-v"] | ["splitw"] => {
            session.split(SplitDir::TopBottom);
            CommandResult::None
        }
        ["new-window"] | ["neww"] => {
            session.new_window();
            CommandResult::None
        }
        ["kill-pane"] | ["killp"] => {
            session.close_active_pane();
            CommandResult::None
        }
        ["next-window"] | ["next"] => {
            session.next_window();
            CommandResult::None
        }
        ["previous-window"] | ["prev"] => {
            session.prev_window();
            CommandResult::None
        }
        ["select-window", n] => {
            if let Ok(i) = n.parse::<usize>() {
                session.select_window(i);
            }
            CommandResult::None
        }
        ["list-panes"] | ["lsp"] => CommandResult::Text(session.list_panes_text()),
        ["capture-pane"] | ["capturep"] => CommandResult::Text(session.capture_active_pane()),
        _ => CommandResult::None,
    }
}
