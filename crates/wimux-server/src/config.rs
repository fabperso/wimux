//! Configuration du serveur, chargée au démarrage depuis un fichier optionnel.
//!
//! Format proche de tmux, une directive par ligne (`#` pour les commentaires) :
//! ```text
//! set prefix C-a
//! set default-shell pwsh.exe
//! bind | split-window -h
//! bind - split-window -v
//! ```
//! Emplacement : `%USERPROFILE%\.wimux.conf`, puis
//! `%APPDATA%\wimux\wimux.conf`.

use std::collections::HashMap;

/// Action déclenchée par une touche de préfixe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Detach,
    SplitH,
    SplitV,
    NextPane,
    SelectLeft,
    SelectDown,
    SelectUp,
    SelectRight,
    KillPane,
    NewWindow,
    NextWindow,
    PrevWindow,
    CopyMode,
    Paste,
}

/// Configuration résolue.
#[derive(Debug, Clone)]
pub struct Config {
    /// Octet de la touche de préfixe (Ctrl-b = 0x02 par défaut).
    pub prefix: u8,
    pub default_shell: String,
    /// Table des raccourcis de préfixe (octet -> action).
    pub bindings: HashMap<u8, Action>,
}

impl Default for Config {
    fn default() -> Self {
        let mut bindings = HashMap::new();
        bindings.insert(b'd', Action::Detach);
        bindings.insert(b'%', Action::SplitH);
        bindings.insert(b'"', Action::SplitV);
        bindings.insert(b'o', Action::NextPane);
        bindings.insert(b'h', Action::SelectLeft);
        bindings.insert(b'j', Action::SelectDown);
        bindings.insert(b'k', Action::SelectUp);
        bindings.insert(b'l', Action::SelectRight);
        bindings.insert(b'x', Action::KillPane);
        bindings.insert(b'c', Action::NewWindow);
        bindings.insert(b'n', Action::NextWindow);
        bindings.insert(b'p', Action::PrevWindow);
        bindings.insert(b'[', Action::CopyMode);
        bindings.insert(b']', Action::Paste);
        Config {
            prefix: 0x02,
            default_shell: std::env::var("WIMUX_SHELL")
                .unwrap_or_else(|_| "powershell.exe".to_string()),
            bindings,
        }
    }
}

impl Config {
    /// Charge la configuration (défauts + fichier utilisateur s'il existe).
    pub fn load() -> Config {
        let mut config = Config::default();
        if let Some(path) = config_path()
            && let Ok(contents) = std::fs::read_to_string(&path)
        {
            config.apply(&contents);
        }
        config
    }

    /// Applique le contenu d'un fichier de configuration.
    pub fn apply(&mut self, contents: &str) {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let tokens: Vec<&str> = line.split_whitespace().collect();
            match tokens.as_slice() {
                ["set", "prefix", key] => {
                    if let Some(b) = parse_key(key) {
                        self.prefix = b;
                    }
                }
                ["set", "default-shell", shell] => self.default_shell = shell.to_string(),
                ["bind", key, rest @ ..] => {
                    if let (Some(b), Some(action)) = (parse_key(key), parse_action(rest)) {
                        self.bindings.insert(b, action);
                    }
                }
                _ => {} // directive inconnue : ignorée
            }
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let p = std::path::Path::new(&profile).join(".wimux.conf");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = std::path::Path::new(&appdata)
            .join("wimux")
            .join("wimux.conf");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Convertit une description de touche en octet : `C-a`..`C-z`, `Space`, `Enter`,
/// ou un caractère unique.
pub fn parse_key(s: &str) -> Option<u8> {
    match s {
        "Space" => Some(0x20),
        "Enter" => Some(0x0d),
        "Tab" => Some(0x09),
        _ => {
            if let Some(rest) = s.strip_prefix("C-") {
                let c = rest.chars().next()?.to_ascii_lowercase();
                if c.is_ascii_lowercase() {
                    return Some((c as u8) - b'a' + 1);
                }
                None
            } else if s.chars().count() == 1 {
                let c = s.chars().next()?;
                c.is_ascii().then_some(c as u8)
            } else {
                None
            }
        }
    }
}

/// Convertit une commande de binding en [`Action`].
fn parse_action(tokens: &[&str]) -> Option<Action> {
    match tokens {
        ["split-window", "-h"] | ["split-h"] => Some(Action::SplitH),
        ["split-window", "-v"] | ["split-v"] => Some(Action::SplitV),
        ["new-window"] => Some(Action::NewWindow),
        ["next-window"] => Some(Action::NextWindow),
        ["previous-window"] | ["prev-window"] => Some(Action::PrevWindow),
        ["next-pane"] | ["select-pane", "-t", ":.+"] => Some(Action::NextPane),
        ["kill-pane"] => Some(Action::KillPane),
        ["copy-mode"] => Some(Action::CopyMode),
        ["paste-buffer"] | ["paste"] => Some(Action::Paste),
        ["detach-client"] | ["detach"] => Some(Action::Detach),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_par_defaut_est_ctrl_b() {
        assert_eq!(Config::default().prefix, 0x02);
    }

    #[test]
    fn parse_key_controle() {
        assert_eq!(parse_key("C-a"), Some(1));
        assert_eq!(parse_key("C-b"), Some(2));
        assert_eq!(parse_key("Space"), Some(0x20));
        assert_eq!(parse_key("%"), Some(b'%'));
    }

    #[test]
    fn set_prefix_modifie_le_prefixe() {
        let mut c = Config::default();
        c.apply("set prefix C-a\n");
        assert_eq!(c.prefix, 1);
    }

    #[test]
    fn bind_ajoute_un_raccourci() {
        let mut c = Config::default();
        c.apply("bind | split-window -h\n");
        assert_eq!(c.bindings.get(&b'|'), Some(&Action::SplitH));
    }

    #[test]
    fn commentaires_et_lignes_vides_ignores() {
        let mut c = Config::default();
        c.apply("# commentaire\n\nset default-shell pwsh.exe\n");
        assert_eq!(c.default_shell, "pwsh.exe");
    }
}
