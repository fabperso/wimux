// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
//! État d'un volet NAVIGATEUR (B1) : l'URL courante et la pile d'historique.
//! Aucun processus, aucune I/O — c'est de l'état pur, possédé par le serveur pour
//! que le volet et son URL survivent au redémarrage de la GUI.
//!
//! L'historique est celui des URL que **wimux** a posées (barre d'URL, ouverture,
//! et plus tard l'automatisation B2). Les navigations faites *dans* la page en
//! cross-origin nous sont invisibles : ce n'est donc pas l'historique du site.

use std::sync::Mutex;

use crate::pane::PaneId;

/// Volet navigateur : identité + pile d'URL avec un curseur.
pub struct WebPane {
    pub id: PaneId,
    state: Mutex<State>,
}

struct State {
    /// Pile des URL visitées, de la plus ancienne à la plus récente.
    history: Vec<String>,
    /// Position courante dans `history` (toujours un index valide).
    cursor: usize,
}

impl WebPane {
    /// Crée un volet navigateur positionné sur `url`.
    pub fn new(id: PaneId, url: String) -> WebPane {
        WebPane {
            id,
            state: Mutex::new(State {
                history: vec![url],
                cursor: 0,
            }),
        }
    }

    /// URL courante.
    pub fn url(&self) -> String {
        let st = self.state.lock().unwrap();
        st.history[st.cursor].clone()
    }

    /// Navigue vers `url` : tronque l'« avant » (comme un navigateur) puis empile.
    pub fn navigate(&self, url: String) {
        let mut st = self.state.lock().unwrap();
        let cursor_pos = st.cursor;
        st.history.truncate(cursor_pos + 1);
        st.history.push(url);
        st.cursor = st.history.len() - 1;
    }

    /// Recule d'un cran. `false` si on est déjà en tête de pile (no-op).
    pub fn back(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.cursor == 0 {
            return false;
        }
        st.cursor -= 1;
        true
    }

    /// Avance d'un cran. `false` si on est déjà en fin de pile (no-op).
    pub fn forward(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.cursor + 1 >= st.history.len() {
            return false;
        }
        st.cursor += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nouvelle_pile_commence_sur_l_url_initiale() {
        let w = WebPane::new(1, "http://a/".into());
        assert_eq!(w.url(), "http://a/");
        assert!(!w.back(), "rien avant la première URL");
        assert!(!w.forward(), "rien après la première URL");
    }

    #[test]
    fn navigate_empile_et_back_forward_parcourent() {
        let w = WebPane::new(1, "http://a/".into());
        w.navigate("http://b/".into());
        w.navigate("http://c/".into());
        assert_eq!(w.url(), "http://c/");

        assert!(w.back());
        assert_eq!(w.url(), "http://b/");
        assert!(w.back());
        assert_eq!(w.url(), "http://a/");
        assert!(!w.back(), "en tête de pile, back est un no-op");
        assert_eq!(w.url(), "http://a/");

        assert!(w.forward());
        assert_eq!(w.url(), "http://b/");
        assert!(w.forward());
        assert_eq!(w.url(), "http://c/");
        assert!(!w.forward(), "en fin de pile, forward est un no-op");
    }

    #[test]
    fn naviguer_apres_un_back_tronque_l_avant() {
        let w = WebPane::new(1, "http://a/".into());
        w.navigate("http://b/".into());
        assert!(w.back()); // sur a/
        w.navigate("http://z/".into());
        assert_eq!(w.url(), "http://z/");
        assert!(
            !w.forward(),
            "après une nouvelle navigation, l'avant est tronqué"
        );
        assert!(w.back());
        assert_eq!(w.url(), "http://a/");
    }
}
