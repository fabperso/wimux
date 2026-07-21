# B1 — Volet navigateur intégré — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permettre qu'un volet de la disposition soit un navigateur (iframe) au lieu d'un terminal, possédé par le serveur — donc composable, persistant, et adressable par la future automatisation B2.

**Architecture:** Le daemon possède un `WebPane` (id + historique d'URL, sans processus) ; la table de volets de `Window` devient une énumération `PaneSlot { Term | Web }` ; `LayoutNode::Leaf` gagne un `kind` qui dit à la GUI quoi rendre et transporte l'URL ; la navigation passe par le serveur, qui repousse un `WindowLayout`.

**Tech Stack:** Rust (`wimux-protocol` postcard/serde, `wimux-server`, `wimux-cli`), TypeScript/Vite + Tauri v2 (`wimux-gui`), `<iframe>`.

## Global Constraints

- **Compat postcard** : toute nouvelle variante d'enum / nouveau champ de struct s'ajoute **EN FIN**. Exception assumée et unique : `LayoutNode::Leaf` gagne un champ `kind`, ce qui change **tous** les encodages de `Leaf` — d'où rebuild complet + redémarrage du daemon.
- **Daemon persistant** : après tout changement de `wimux-protocol`/`wimux-server`, **rebuild release + redémarrer le daemon détaché**, sinon échec silencieux.
- **Le serveur est la source de vérité de la navigation** : `navigate`/`back`/`forward` passent par lui, qui repousse un `WindowLayout`. **Exception** : *recharger* est purement client (réassigner `src`), sans aller-retour.
- **Un volet web n'est jamais reapé** (il ne meurt pas) ; `reflow` lui donne son rectangle sans redimensionner de PTY ; les frappes qui lui seraient routées côté TUI sont **ignorées**.
- **Périmètre manuel** : barre d'URL + recharger + précédent/suivant. **Pas de zoom, pas de devtools** (écartés : non livrables ou dégradés sur un iframe).
- **Historique honnête** : la pile est celle des URL que *wimux* a posées ; les navigations internes à la page en cross-origin sont invisibles. À documenter tel quel, jamais présenté comme un historique de navigateur.
- **Nommage** : `PaneSlot` côté serveur (porte les objets), `PaneKind` côté protocole (étiquette sérialisée).
- **Qualité** : `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, **plus** `cargo clippy --all-targets -- -D warnings` et `cargo fmt -- --check` dans `wimux-gui/src-tauri` (ce crate n'est **pas** membre du workspace), et `npm run build`.
- **Langue** : commentaires et messages en français.

---

## File Structure

**Créés :**
- `crates/wimux-server/src/webpane.rs` — l'état d'un volet navigateur (URL courante + pile d'historique + curseur) et ses transitions. Pur, sans I/O ni processus, entièrement testable en unitaire.

**Modifiés :**
- `crates/wimux-protocol/src/lib.rs` — `PaneKind`, `LayoutNode::Leaf.kind`, 4 messages.
- `crates/wimux-server/src/lib.rs` — déclarer `pub mod webpane;`.
- `crates/wimux-server/src/window.rs` — table `PaneSlot`, `active_term_pane`, rendu du substitut TUI, reflow/reap/close/pane_list, `layout_tree` émettant le `kind`.
- `crates/wimux-server/src/session.rs` — `open_web_pane`/`web_navigate`/`web_back`/`web_forward`, substitut dans `capture_pane`, mise à jour des appelants de `active_pane`.
- `crates/wimux-server/src/daemon.rs` — 4 handlers.
- `wimux-gui/src/panes.ts` — rendu `.pane-web` (barre de chrome + iframe) et callbacks.
- `wimux-gui/src/main.ts` — câblage des callbacks vers les commandes Tauri.
- `wimux-gui/src/styles.css` — styles de `.pane-web`.
- `wimux-gui/src-tauri/src/lib.rs` — 4 commandes sur la connexion persistante.
- `crates/wimux-cli/src/main.rs` — `wimux browser open` + ligne d'aide.

**Interfaces clés (verrouillées ici) :**

```rust
// wimux-protocol
pub enum PaneKind { Terminal, Web { url: String } }
// LayoutNode::Leaf { pane_id: u64, kind: PaneKind }
// ClientMessage (en fin) :
//   OpenWebPane { session: String, from_pane: Option<u64>, dir: SplitDir, url: String }  -> PaneSpawned{pane_id}
//   WebNavigate { session: String, pane: u64, url: String }                              -> Ok/Error
//   WebBack { session: String, pane: u64 }                                               -> Ok/Error
//   WebForward { session: String, pane: u64 }                                            -> Ok/Error

// crates/wimux-server/src/webpane.rs
pub struct WebPane { pub id: PaneId }
impl WebPane {
    pub fn new(id: PaneId, url: String) -> WebPane;
    pub fn url(&self) -> String;
    pub fn navigate(&self, url: String);
    pub fn back(&self) -> bool;      // false si déjà en tête de pile
    pub fn forward(&self) -> bool;   // false si déjà en fin de pile
}

// crates/wimux-server/src/window.rs
pub enum PaneSlot { Term(Arc<Pane>), Web(Arc<WebPane>) }
impl Window {
    pub fn pane(&self, id: PaneId) -> Option<Arc<Pane>>;        // TERMINAL uniquement (inchangé pour les appelants)
    pub fn web_pane(&self, id: PaneId) -> Option<Arc<WebPane>>;
    pub fn contains(&self, id: PaneId) -> bool;                 // n'importe quelle nature
    pub fn active_term_pane(&self) -> Option<Arc<Pane>>;        // remplace active_pane()
    pub fn split_web(&mut self, target: PaneId, dir: SplitDir, web: Arc<WebPane>);
}

// crates/wimux-server/src/session.rs
impl Session {
    pub fn open_web_pane(&self, from_pane: Option<u64>, dir: SplitDir, url: String) -> Option<u64>;
    pub fn web_navigate(&self, pane_id: u64, url: String) -> bool;
    pub fn web_back(&self, pane_id: u64) -> bool;
    pub fn web_forward(&self, pane_id: u64) -> bool;
}
```

---

## Phase B1.1 — Protocole

### Task 1 : `PaneKind`, `Leaf.kind` et les 4 messages

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/window.rs` (garder le crate compilable, cf. Step 5)
- Test: `crates/wimux-protocol/src/lib.rs`

**Interfaces:**
- Produces: `PaneKind`, `LayoutNode::Leaf { pane_id, kind }`, `OpenWebPane`/`WebNavigate`/`WebBack`/`WebForward`.

- [ ] **Step 1 : Écrire le test de round-trip (échoue)**

Dans le module de tests de `crates/wimux-protocol/src/lib.rs` :

```rust
#[test]
fn aller_retour_leaf_web_et_messages_navigateur() {
    // Une feuille NAVIGATEUR transporte son URL.
    let tree = LayoutNode::Leaf {
        pane_id: 4,
        kind: PaneKind::Web {
            url: "http://localhost:5173/".into(),
        },
    };
    let bytes = postcard::to_allocvec(&tree).unwrap();
    match postcard::from_bytes::<LayoutNode>(&bytes).unwrap() {
        LayoutNode::Leaf { pane_id, kind } => {
            assert_eq!(pane_id, 4);
            assert_eq!(
                kind,
                PaneKind::Web {
                    url: "http://localhost:5173/".into()
                }
            );
        }
        _ => panic!("attendu une feuille"),
    }

    // Une feuille TERMINAL reste distinguable.
    let tree = LayoutNode::Leaf {
        pane_id: 1,
        kind: PaneKind::Terminal,
    };
    let bytes = postcard::to_allocvec(&tree).unwrap();
    match postcard::from_bytes::<LayoutNode>(&bytes).unwrap() {
        LayoutNode::Leaf { kind, .. } => assert_eq!(kind, PaneKind::Terminal),
        _ => panic!("attendu une feuille"),
    }

    let msg = ClientMessage::OpenWebPane {
        session: "s".into(),
        from_pane: Some(2),
        dir: SplitDir::LeftRight,
        url: "http://localhost:3000/".into(),
    };
    let bytes = postcard::to_allocvec(&msg).unwrap();
    match postcard::from_bytes::<ClientMessage>(&bytes).unwrap() {
        ClientMessage::OpenWebPane { from_pane, url, .. } => {
            assert_eq!(from_pane, Some(2));
            assert_eq!(url, "http://localhost:3000/");
        }
        _ => panic!("variante inattendue"),
    }

    let msg = ClientMessage::WebBack {
        session: "s".into(),
        pane: 4,
    };
    let bytes = postcard::to_allocvec(&msg).unwrap();
    assert!(matches!(
        postcard::from_bytes::<ClientMessage>(&bytes).unwrap(),
        ClientMessage::WebBack { pane: 4, .. }
    ));
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-protocol aller_retour_leaf_web_et_messages_navigateur`
Expected: FAIL — `PaneKind` n'existe pas et `Leaf` n'a pas de champ `kind` (erreur de compilation).

- [ ] **Step 3 : Ajouter `PaneKind` et le champ `kind`**

Juste avant `pub enum LayoutNode` :

```rust
/// Nature d'une feuille de disposition (B1) : terminal, ou navigateur portant son
/// URL courante. C'est ce qui dit au frontend quoi rendre pour cette feuille.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaneKind {
    Terminal,
    Web { url: String },
}
```

Puis, dans `enum LayoutNode`, la variante `Leaf` devient :

```rust
    Leaf {
        pane_id: u64,
        /// B1 : terminal ou navigateur (+ URL). **Ajout de champ assumé** : il
        /// change tous les encodages de `Leaf`, d'où rebuild + redémarrage daemon.
        kind: PaneKind,
    },
```

- [ ] **Step 4 : Ajouter les 4 messages EN FIN de `ClientMessage`**

Juste avant l'accolade fermante de `enum ClientMessage` :

```rust
    /// B1 : ouvre un volet NAVIGATEUR en découpant depuis `from_pane` (défaut :
    /// volet actif). Réponse : `PaneSpawned { pane_id }`.
    OpenWebPane {
        session: String,
        from_pane: Option<u64>,
        dir: SplitDir,
        url: String,
    },
    /// B1 : fait naviguer un volet navigateur vers `url` (empile l'historique).
    WebNavigate {
        session: String,
        pane: u64,
        url: String,
    },
    /// B1 : recule d'un cran dans la pile d'URL du volet.
    WebBack { session: String, pane: u64 },
    /// B1 : avance d'un cran dans la pile d'URL du volet.
    WebForward { session: String, pane: u64 },
```

- [ ] **Step 5 : Garder `wimux-server` compilable**

L'ajout du champ casse la construction de `Leaf` dans
`crates/wimux-server/src/window.rs` (fonction `node_to_layout`). Corriger **dès
maintenant** en émettant `Terminal` pour toutes les feuilles — la vraie nature sera
émise en Task 3 :

```rust
        Node::Leaf(id) => wimux_protocol::LayoutNode::Leaf {
            pane_id: *id,
            // B1 (intérim) : Task 3 émettra la vraie nature depuis la table des volets.
            kind: wimux_protocol::PaneKind::Terminal,
        },
```

Corriger de même toute construction/déconstruction de `LayoutNode::Leaf` dans les
tests (`rg "LayoutNode::Leaf" crates/`) : ajouter `kind: PaneKind::Terminal` aux
constructions, et `..` ou `kind: _` aux motifs qui ne s'y intéressent pas.

- [ ] **Step 6 : Lancer les tests**

Run: `cargo test -p wimux-protocol && cargo build -p wimux-server`
Expected: tests protocole PASS ; `wimux-server` compile.

- [ ] **Step 7 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-protocol -p wimux-server
cargo clippy -p wimux-protocol --all-targets -- -D warnings
git add crates/wimux-protocol/src/lib.rs crates/wimux-server/src/window.rs
git commit -m "feat(browser): protocole B1 — PaneKind sur Leaf + OpenWebPane/WebNavigate/WebBack/WebForward

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B1.2 — Serveur : l'état d'un volet navigateur

### Task 2 : module `webpane.rs`

**Files:**
- Create: `crates/wimux-server/src/webpane.rs`
- Modify: `crates/wimux-server/src/lib.rs`
- Test: `crates/wimux-server/src/webpane.rs`

**Interfaces:**
- Produces: `WebPane::new/url/navigate/back/forward`.

- [ ] **Step 1 : Écrire les tests (échouent)**

Créer `crates/wimux-server/src/webpane.rs` avec **seulement** le module de tests :

```rust
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
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server webpane`
Expected: FAIL — module non déclaré / `WebPane` inexistant.

- [ ] **Step 3 : Déclarer le module**

Dans `crates/wimux-server/src/lib.rs`, ajouter en respectant l'ordre alphabétique
(donc après `pub mod window;`) :

```rust
pub mod webpane;
```

- [ ] **Step 4 : Écrire le module**

En tête de `webpane.rs`, **avant** le module de tests :

```rust
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
        st.history.truncate(st.cursor + 1);
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
```

- [ ] **Step 5 : Lancer, fmt, clippy, commit**

Run: `cargo test -p wimux-server webpane`
Expected: PASS (3 tests).

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/src/webpane.rs crates/wimux-server/src/lib.rs
git commit -m "feat(browser): WebPane — URL courante et pile d'historique (etat pur)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B1.3 — Serveur : la fenêtre accueille deux natures de volet

### Task 3 : `PaneSlot` dans `window.rs`

**Files:**
- Modify: `crates/wimux-server/src/window.rs`
- Test: `crates/wimux-server/src/window.rs`

**Interfaces:**
- Consumes: `WebPane` (Task 2).
- Produces: `PaneSlot`, `Window::{web_pane, contains, active_term_pane, split_web}`, `pane()` restreint aux terminaux, `layout_tree` émettant le vrai `kind`, substitut de rendu TUI.

- [ ] **Step 1 : Écrire les tests (échouent)**

Dans le module de tests de `window.rs` :

```rust
    fn dummy_web() -> Arc<crate::webpane::WebPane> {
        Arc::new(crate::webpane::WebPane::new(
            9_001,
            "http://localhost:5173/".into(),
        ))
    }

    #[test]
    fn volet_web_est_une_feuille_mais_pas_un_volet_terminal() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        let web = dummy_web();
        let idw = web.id;
        win.split_web(id1, SplitDir::LeftRight, Arc::clone(&web));

        // Il est dans la fenêtre…
        assert!(win.contains(idw), "le volet web est présent");
        assert!(win.pane_ids().contains(&idw));
        // …mais ce n'est pas un volet TERMINAL.
        assert!(
            win.pane(idw).is_none(),
            "pane() ne doit renvoyer que des terminaux"
        );
        assert!(win.web_pane(idw).is_some());
        // La disposition l'annonce comme navigateur, avec son URL.
        let tree = win.layout_tree();
        let kinds = collect_kinds(&tree);
        assert!(
            kinds.contains(&wimux_protocol::PaneKind::Web {
                url: "http://localhost:5173/".into()
            }),
            "la feuille web doit porter son URL : {kinds:?}"
        );
        win.kill_all();
    }

    #[test]
    fn volet_web_survit_au_reap() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        let web = dummy_web();
        let idw = web.id;
        win.split_web(id1, SplitDir::TopBottom, web);

        // Tuer le volet terminal puis reaper : le web doit rester.
        win.pane(id1).unwrap().kill();
        // `reap_dead` retire les terminaux morts ; il ne doit PAS toucher au web.
        let _ = win.reap_dead();
        assert!(win.contains(idw), "un volet web n'est jamais reapé");
        win.kill_all();
    }

    #[test]
    fn active_term_pane_est_none_sur_un_volet_web() {
        let p1 = dummy_pane();
        let id1 = p1.id;
        let mut win = Window::new(p1);
        let web = dummy_web();
        let idw = web.id;
        // `split_web` rend le nouveau volet actif.
        win.split_web(id1, SplitDir::LeftRight, web);
        assert_eq!(win.active_pane_id(), idw);
        assert!(
            win.active_term_pane().is_none(),
            "le volet actif est un navigateur : pas de volet terminal actif"
        );
        win.set_active(id1);
        assert!(win.active_term_pane().is_some());
        win.kill_all();
    }

    /// Aplatit les `PaneKind` d'un arbre de disposition (ordre non garanti).
    fn collect_kinds(node: &wimux_protocol::LayoutNode) -> Vec<wimux_protocol::PaneKind> {
        match node {
            wimux_protocol::LayoutNode::Leaf { kind, .. } => vec![kind.clone()],
            wimux_protocol::LayoutNode::Split { a, b, .. } => {
                let mut v = collect_kinds(a);
                v.extend(collect_kinds(b));
                v
            }
        }
    }
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server volet_web_est_une_feuille`
Expected: FAIL — `split_web`/`contains`/`web_pane`/`active_term_pane` n'existent pas.

- [ ] **Step 3 : Introduire `PaneSlot` et basculer la table**

En tête de `window.rs`, après les `use`, ajouter :

```rust
use crate::webpane::WebPane;

/// Contenu d'une feuille de disposition (B1) : un terminal, ou un navigateur.
/// C'est une ÉNUMÉRATION (et non une table parallèle) pour que le compilateur
/// recense tous les sites d'appel lors de l'ajout d'une nature.
pub enum PaneSlot {
    Term(Arc<Pane>),
    Web(Arc<WebPane>),
}

impl PaneSlot {
    /// Le volet terminal, ou `None` si cette feuille est un navigateur.
    fn term(&self) -> Option<Arc<Pane>> {
        match self {
            PaneSlot::Term(p) => Some(Arc::clone(p)),
            PaneSlot::Web(_) => None,
        }
    }
}
```

Changer le champ de `Window` :

```rust
    panes: HashMap<PaneId, PaneSlot>,
```

Et dans `Window::new`, insérer un slot :

```rust
        panes.insert(id, PaneSlot::Term(pane));
```

- [ ] **Step 4 : Adapter les accesseurs**

Remplacer `active_pane`, `pane`, et ajouter les nouveaux :

```rust
    /// Volet TERMINAL actif, ou `None` si le volet actif est un navigateur.
    pub fn active_term_pane(&self) -> Option<Arc<Pane>> {
        self.panes.get(&self.active).and_then(|s| s.term())
    }

    /// Volet TERMINAL d'identifiant `id` (`None` si absent ou si c'est un
    /// navigateur) — sémantique volontairement inchangée pour les appelants.
    pub fn pane(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.get(&id).and_then(|s| s.term())
    }

    /// Volet NAVIGATEUR d'identifiant `id`, s'il en est un.
    pub fn web_pane(&self, id: PaneId) -> Option<Arc<WebPane>> {
        match self.panes.get(&id) {
            Some(PaneSlot::Web(w)) => Some(Arc::clone(w)),
            _ => None,
        }
    }

    /// La fenêtre contient-elle cette feuille, quelle que soit sa nature ?
    pub fn contains(&self, id: PaneId) -> bool {
        self.panes.contains_key(&id)
    }
```

- [ ] **Step 5 : Adapter les méthodes qui parcourent la table**

`kill_all` — ne tue que les terminaux (un navigateur n'a rien à tuer) :

```rust
    pub fn kill_all(&self) {
        for slot in self.panes.values() {
            if let PaneSlot::Term(p) = slot {
                p.kill();
            }
        }
    }
```

`pane_list` — étiqueter la nature :

```rust
    pub fn pane_list(&self) -> Vec<String> {
        let mut ids: Vec<PaneId> = self.panes.keys().copied().collect();
        ids.sort_unstable();
        ids.iter()
            .map(|id| {
                let active = if *id == self.active { " (actif)" } else { "" };
                match &self.panes[id] {
                    PaneSlot::Term(p) => {
                        let (c, r) = p.size();
                        format!("volet {id}: {c}x{r}{active}")
                    }
                    PaneSlot::Web(w) => format!("volet {id}: navigateur {}{active}", w.url()),
                }
            })
            .collect()
    }
```

`split_pane` — insérer un slot terminal :

```rust
        self.panes.insert(new_id, PaneSlot::Term(new_pane));
```

`close_pane` — ne tuer que si terminal :

```rust
        if let Some(slot) = self.panes.remove(&target) {
            if let PaneSlot::Term(p) = slot {
                p.kill();
            }
        }
```

`reap_dead` — **ne considérer que les terminaux** :

```rust
        let dead: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, s)| match s {
                // Un volet navigateur n'a pas de processus : il ne meurt jamais.
                PaneSlot::Web(_) => false,
                PaneSlot::Term(p) => !p.is_alive(),
            })
            .map(|(id, _)| *id)
            .collect();
```

`reflow` — ne redimensionner que les terminaux (le rectangle est enregistré pour
tout le monde, le TUI en a besoin pour dessiner le substitut) :

```rust
        // Zoom : seul le volet actif est visible, à pleine taille.
        if self.zoomed && self.panes.contains_key(&self.active) {
            if let Some(PaneSlot::Term(pane)) = self.panes.get(&self.active) {
                pane.resize(area.w, area.h);
            }
            self.rects.insert(self.active, area);
            return;
        }

        let mut rects = HashMap::new();
        let mut borders = Vec::new();
        layout(&self.root, area, &mut rects, &mut borders);
        for (&id, r) in &rects {
            if let Some(PaneSlot::Term(pane)) = self.panes.get(&id) {
                pane.resize(r.w, r.h);
            }
        }
        self.rects = rects;
        self.borders = borders;
```

- [ ] **Step 6 : Ajouter `split_web`**

À côté de `split_pane` :

```rust
    /// Découpe le volet `target` en y insérant un volet NAVIGATEUR, qui devient
    /// actif (même mécanique que `split_pane`, autre nature de feuille).
    pub fn split_web(&mut self, target: PaneId, dir: SplitDir, web: Arc<WebPane>) {
        self.zoomed = false;
        let new_id = web.id;
        let node_id = NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed);
        Self::replace_leaf(&mut self.root, target, |old| Node::Split {
            node_id,
            dir,
            ratio: 0.5,
            a: Box::new(old),
            b: Box::new(Node::Leaf(new_id)),
        });
        self.panes.insert(new_id, PaneSlot::Web(web));
        self.active = new_id;
    }
```

- [ ] **Step 7 : Rendu — substitut TUI et vraie nature dans la disposition**

Dans `render`, remplacer la boucle sur les volets :

```rust
        // Volets.
        let mut cursor = (0, 0);
        for (&id, r) in &self.rects {
            match self.panes.get(&id) {
                Some(PaneSlot::Term(pane)) => {
                    let (grid, (cc, cr)) = pane.snapshot();
                    into.blit(&grid, r.x, r.y);
                    if id == self.active {
                        cursor = (
                            r.x + cc.min(r.w.saturating_sub(1)),
                            r.y + cr.min(r.h.saturating_sub(1)),
                        );
                    }
                }
                // Un client TEXTE ne peut pas afficher une page : on dessine un
                // substitut lisible plutôt que de laisser un trou.
                Some(PaneSlot::Web(web)) => draw_web_placeholder(into, *r, &web.url()),
                None => {}
            }
        }
```

Et ajouter la fonction libre, à côté de `layout` :

```rust
/// Dessine le substitut d'un volet navigateur pour les clients texte : une
/// étiquette et l'URL, tronquées à la largeur disponible.
fn draw_web_placeholder(into: &mut Grid, r: Rect, url: &str) {
    let pen = Pen {
        fg: Color::Indexed(6),
        ..Pen::default()
    };
    if r.h == 0 || r.w == 0 {
        return;
    }
    let clip = |s: &str| -> String { s.chars().take(r.w as usize).collect() };
    into.set_str(r.x, r.y, &clip("[navigateur]"), pen);
    if r.h >= 2 {
        into.set_str(r.x, r.y + 1, &clip(url), pen);
    }
}
```

Enfin, `layout_tree` doit émettre la vraie nature. Remplacer la méthode et la
fonction libre `node_to_layout` (qui devient une méthode, pour accéder à la table) :

```rust
    /// Traduit l'arbre interne en `LayoutNode` sérialisable pour la GUI.
    pub fn layout_tree(&self) -> wimux_protocol::LayoutNode {
        self.node_to_layout(&self.root)
    }

    /// Traduit un `Node` interne, en renseignant la NATURE de chaque feuille.
    fn node_to_layout(&self, node: &Node) -> wimux_protocol::LayoutNode {
        match node {
            Node::Leaf(id) => wimux_protocol::LayoutNode::Leaf {
                pane_id: *id,
                kind: match self.panes.get(id) {
                    Some(PaneSlot::Web(w)) => wimux_protocol::PaneKind::Web { url: w.url() },
                    _ => wimux_protocol::PaneKind::Terminal,
                },
            },
            Node::Split {
                node_id,
                dir,
                ratio,
                a,
                b,
            } => wimux_protocol::LayoutNode::Split {
                node_id: *node_id,
                dir: match dir {
                    SplitDir::LeftRight => wimux_protocol::SplitDir::LeftRight,
                    SplitDir::TopBottom => wimux_protocol::SplitDir::TopBottom,
                },
                ratio: *ratio,
                a: Box::new(self.node_to_layout(a)),
                b: Box::new(self.node_to_layout(b)),
            },
        }
    }
```

Supprimer l'ancienne fonction libre `node_to_layout` et adapter les tests qui
l'appelaient directement (`rg "node_to_layout" crates/wimux-server`) : ils passent
par `win.layout_tree()`.

- [ ] **Step 8 : Réparer les appelants de `active_pane()` dans `session.rs`**

`Window::active_pane()` n'existe plus. Mettre à jour (le compilateur les liste) :

- `session.rs` `active_pane()` : `.map(|w| w.active_pane())` → `.and_then(|w| w.active_term_pane())`
- les quatre constructions de `WindowInfo` : `cwd: w.active_pane().cwd()` →
  `cwd: w.active_term_pane().and_then(|p| p.cwd())`
- `gui_input` : `win.and_then(|w| w.pane(pane_id).or_else(|| Some(w.active_pane())))`
  → `win.and_then(|w| w.pane(pane_id).or_else(|| w.active_term_pane()))`
  *(une frappe destinée à un volet navigateur est ainsi simplement ignorée)*
- `send_input` : `.map(|w| w.active_pane())` → `.and_then(|w| w.active_term_pane())`

- [ ] **Step 9 : Lancer les tests**

Run: `cargo test -p wimux-server`
Expected: PASS (3 nouveaux tests + suites existantes, dont `gui_mode`).

- [ ] **Step 10 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/src/window.rs crates/wimux-server/src/session.rs
git commit -m "feat(browser): PaneSlot — la fenetre accueille terminaux et navigateurs

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B1.4 — Serveur : ouverture et navigation

### Task 4 : méthodes `Session` + substitut de capture

**Files:**
- Modify: `crates/wimux-server/src/session.rs`
- Test: `crates/wimux-server/src/session.rs`

**Interfaces:**
- Consumes: `WebPane`, `Window::{split_web, web_pane, contains}`.
- Produces: `Session::{open_web_pane, web_navigate, web_back, web_forward}`, `capture_pane` renvoyant le substitut.

- [ ] **Step 1 : Écrire les tests (échouent)**

Dans le module de tests de `session.rs` :

```rust
    #[test]
    fn open_web_pane_ajoute_une_feuille_navigateur() {
        let s = Session::new("web".into(), 80, 24, "cmd.exe").unwrap();
        let rev0 = s.layout_rev();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://localhost:5173/".into())
            .expect("un id de volet");
        assert!(s.layout_rev() > rev0, "layout_rev doit être bumpé");

        // La disposition annonce la feuille comme navigateur, avec son URL.
        let (tree, _) = s.window_layout().unwrap();
        assert!(
            layout_contient_web(&tree, id, "http://localhost:5173/"),
            "la feuille {id} doit être un navigateur sur la bonne URL : {tree:?}"
        );
        s.kill();
    }

    #[test]
    fn navigation_web_met_a_jour_l_url_et_l_historique() {
        let s = Session::new("web2".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://a/".into())
            .unwrap();

        assert!(s.web_navigate(id, "http://b/".into()));
        let (tree, _) = s.window_layout().unwrap();
        assert!(layout_contient_web(&tree, id, "http://b/"));

        assert!(s.web_back(id));
        let (tree, _) = s.window_layout().unwrap();
        assert!(layout_contient_web(&tree, id, "http://a/"));

        assert!(s.web_forward(id));
        let (tree, _) = s.window_layout().unwrap();
        assert!(layout_contient_web(&tree, id, "http://b/"));

        // Un id inconnu ou un volet terminal ne sont pas navigables.
        assert!(!s.web_navigate(999_999, "http://x/".into()));
        s.kill();
    }

    #[test]
    fn capture_pane_dun_volet_web_renvoie_le_substitut() {
        let s = Session::new("web3".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://localhost:9/".into())
            .unwrap();
        let txt = s.capture_pane(id).expect("une capture");
        assert!(txt.contains("[navigateur]"), "substitut attendu : {txt}");
        assert!(txt.contains("http://localhost:9/"), "URL attendue : {txt}");
        s.kill();
    }

    #[test]
    fn le_volet_web_est_toujours_la_apres_une_re_attache() {
        // La persistance est la raison d'être du choix « volet possédé par le
        // serveur » : une nouvelle attache doit retrouver le volet ET son URL.
        let s = Session::new("web4".into(), 80, 24, "cmd.exe").unwrap();
        let id = s
            .open_web_pane(None, SplitDir::LeftRight, "http://persist/".into())
            .unwrap();
        s.web_navigate(id, "http://persist/2".into());

        // `gui_attach_window` est le cycle d'attache complet joué par la GUI.
        let (tree, _, _, _, _) = s.gui_attach_window().expect("une fenêtre active");
        assert!(
            layout_contient_web(&tree, id, "http://persist/2"),
            "après ré-attache, le volet et son URL courante sont retrouvés : {tree:?}"
        );
        s.kill();
    }

    /// La disposition contient-elle une feuille `id` de nature web sur `url` ?
    fn layout_contient_web(node: &LayoutNode, id: u64, url: &str) -> bool {
        match node {
            LayoutNode::Leaf { pane_id, kind } => {
                *pane_id == id
                    && matches!(kind, wimux_protocol::PaneKind::Web { url: u } if u == url)
            }
            LayoutNode::Split { a, b, .. } => {
                layout_contient_web(a, id, url) || layout_contient_web(b, id, url)
            }
        }
    }
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server open_web_pane_ajoute_une_feuille`
Expected: FAIL — `open_web_pane` n'existe pas.

- [ ] **Step 3 : Écrire les quatre méthodes**

Dans `impl Session`, à côté de `spawn_pane` :

```rust
    /// B1 : ouvre un volet NAVIGATEUR en découpant depuis `from_pane` (défaut :
    /// volet actif de la fenêtre active). Renvoie l'id du volet créé.
    ///
    /// Comme `spawn_pane`, on découpe la fenêtre où se trouve réellement
    /// `from_pane`, pour que le navigateur apparaisse à côté de son demandeur.
    pub fn open_web_pane(
        &self,
        from_pane: Option<u64>,
        dir: SplitDir,
        url: String,
    ) -> Option<u64> {
        let web = Arc::new(crate::webpane::WebPane::new(
            crate::pane::next_pane_id(),
            url,
        ));
        let new_id = web.id;
        {
            let mut inner = self.inner.lock().unwrap();
            let target_window = from_pane
                .and_then(|id| inner.windows.iter().position(|w| w.contains(id)))
                .unwrap_or(inner.active_window);
            let win = inner.windows.get_mut(target_window)?;
            let target = from_pane
                .filter(|id| win.contains(*id))
                .unwrap_or_else(|| win.active_pane_id());
            win.split_web(target, dir, Arc::clone(&web));
            let area = content_area(inner.cols, inner.rows);
            inner.windows[target_window].reflow(area);
        }
        self.bump_layout_rev();
        self.notifier.bump();
        Some(new_id)
    }

    /// B1 : fait naviguer un volet navigateur. `false` si l'id n'est pas un
    /// volet navigateur de cette session.
    pub fn web_navigate(&self, pane_id: u64, url: String) -> bool {
        let Some(web) = self.find_web(pane_id) else {
            return false;
        };
        web.navigate(url);
        self.notifier.bump();
        true
    }

    /// B1 : recule d'un cran. `false` si l'id n'est pas navigable OU si on est
    /// déjà en tête de pile.
    pub fn web_back(&self, pane_id: u64) -> bool {
        let Some(web) = self.find_web(pane_id) else {
            return false;
        };
        let moved = web.back();
        if moved {
            self.notifier.bump();
        }
        moved
    }

    /// B1 : avance d'un cran. Mêmes conventions que `web_back`.
    pub fn web_forward(&self, pane_id: u64) -> bool {
        let Some(web) = self.find_web(pane_id) else {
            return false;
        };
        let moved = web.forward();
        if moved {
            self.notifier.bump();
        }
        moved
    }

    /// Cherche un volet navigateur par id dans toutes les fenêtres.
    fn find_web(&self, pane_id: u64) -> Option<Arc<crate::webpane::WebPane>> {
        let inner = self.inner.lock().unwrap();
        inner.windows.iter().find_map(|w| w.web_pane(pane_id))
    }
```

- [ ] **Step 4 : Exposer l'allocateur d'id de volet**

`open_web_pane` a besoin d'un identifiant pris dans la **même** séquence que les
volets terminal (les ids doivent rester uniques toutes natures confondues). Dans
`crates/wimux-server/src/pane.rs`, ajouter à côté de `NEXT_PANE_ID` :

```rust
/// Alloue le prochain identifiant de volet. Partagé par les volets terminal et
/// les volets navigateur (B1) : les ids sont uniques toutes natures confondues.
pub fn next_pane_id() -> PaneId {
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}
```

Et faire utiliser ce helper par `Pane::spawn_command` à la place de l'appel direct :

```rust
        let id = next_pane_id();
```

- [ ] **Step 5 : Substitut dans `capture_pane`**

Remplacer `Session::capture_pane` :

```rust
    /// A1 : contenu visible du volet `pane_id` (n'importe quelle fenêtre), texte.
    /// B1 : pour un volet navigateur, renvoie le substitut textuel plutôt qu'une
    /// erreur — l'appelant obtient une réponse utile, pas un échec.
    pub fn capture_pane(&self, pane_id: u64) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        for win in &inner.windows {
            if let Some(p) = win.pane(pane_id) {
                return Some(p.capture_text());
            }
            if let Some(w) = win.web_pane(pane_id) {
                return Some(format!("[navigateur]\r\n{}", w.url()));
            }
        }
        None
    }
```

- [ ] **Step 6 : Lancer les tests**

Run: `cargo test -p wimux-server`
Expected: PASS.

- [ ] **Step 7 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings
git add crates/wimux-server/src/session.rs crates/wimux-server/src/pane.rs
git commit -m "feat(browser): Session::open_web_pane/web_navigate/web_back/web_forward + substitut de capture

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 5 : handlers daemon

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/src/daemon.rs`

**Interfaces:**
- Consumes: `Session::{open_web_pane, web_navigate, web_back, web_forward}`.

- [ ] **Step 1 : (câblage — vérification par compilation + non-régression)**

Ces quatre handlers sont du **branchement** : ils traduisent un message en appel de
méthode `Session` (déjà testée en Task 4) et renvoient la réponse. Il n'y a pas de
logique propre à tester isolément, et exercer un handler demanderait une connexion
sur le pipe. Validation : **compilation + suite existante verte**, puis la démo
bout-en-bout de la revue finale. Ajouter tout de même un test de non-régression au
niveau `Server`, qui garantit que le chemin serveur reste utilisable :

```rust
#[test]
fn server_ouvre_un_volet_navigateur() {
    let server = Server::new();
    let s = server.create_session(Some("wb".into()), 80, 24).unwrap();
    let id = s
        .open_web_pane(None, crate::window::SplitDir::LeftRight, "http://a/".into())
        .expect("un id");
    assert!(
        s.capture_pane(id).unwrap().contains("[navigateur]"),
        "le volet créé est bien un navigateur"
    );
    server.kill("wb");
}
```

- [ ] **Step 2 : Vérifier qu'il passe**

Run: `cargo test -p wimux-server server_ouvre_un_volet_navigateur`
Expected: PASS (les méthodes de Task 4 sont en place).

- [ ] **Step 3 : Ajouter les 4 handlers**

Dans le `match msg` de `handle_client`, après les bras M4. Chaque bras commence par
**résoudre la session** : ces messages arrivent de la GUI sur sa connexion
persistante, où la session attachée est déjà connue — la GUI enverra donc une
chaîne vide (le helper `resolve_gui_session` est ajouté au Step 4) :

```rust
            ClientMessage::OpenWebPane {
                session,
                from_pane,
                dir,
                url,
            } => {
                let session = resolve_gui_session(session, &gui_attach);
                let reply = match server.get(&session) {
                    Some(s) => match s.open_web_pane(from_pane, dir.into(), url) {
                        Some(pane_id) => ServerMessage::PaneSpawned { pane_id },
                        None => ServerMessage::Error("échec d'ouverture du volet web".into()),
                    },
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::WebNavigate { session, pane, url } => {
                let session = resolve_gui_session(session, &gui_attach);
                let reply = match server.get(&session) {
                    Some(s) => {
                        if s.web_navigate(pane, url) {
                            ServerMessage::Ok
                        } else {
                            ServerMessage::Error(format!("volet navigateur introuvable : {pane}"))
                        }
                    }
                    None => ServerMessage::Error(format!("session introuvable : {session}")),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
                push_layout_if_gui(&gui_attach, &conn, &gui_write)?;
            }
            ClientMessage::WebBack { session, pane } => {
                let session = resolve_gui_session(session, &gui_attach);
                if let Some(s) = server.get(&session) {
                    s.web_back(pane);
                }
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
                push_layout_if_gui(&gui_attach, &conn, &gui_write)?;
            }
            ClientMessage::WebForward { session, pane } => {
                let session = resolve_gui_session(session, &gui_attach);
                if let Some(s) = server.get(&session) {
                    s.web_forward(pane);
                }
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Ok)?;
                push_layout_if_gui(&gui_attach, &conn, &gui_write)?;
            }
```

- [ ] **Step 4 : Ajouter le résolveur de session et le pousseur de disposition**

Ajouter les deux fonctions libres, à côté de `reattach_active_window` :

```rust
/// Ces messages arrivent de la GUI sur sa connexion persistante, où la session
/// attachée est déjà connue : la GUI envoie donc une chaîne vide. On retombe
/// alors sur la session de l'attachement (et on laisse la valeur telle quelle
/// quand elle est fournie, ce que fait la CLI).
fn resolve_gui_session(session: String, gui_attach: &Option<GuiAttachment>) -> String {
    if session.is_empty() {
        gui_attach
            .as_ref()
            .map(|ga| ga.session.name())
            .unwrap_or(session)
    } else {
        session
    }
}
```

Après la navigation, la GUI attachée doit recevoir la nouvelle disposition (c'est
elle qui porte l'URL) :

```rust
/// Repousse la disposition de la fenêtre active à la connexion GUI courante, si
/// cette connexion EST une GUI attachée. Utilisé après une navigation web : le
/// `WindowLayout` transporte l'URL, la GUI n'a plus qu'à refléter le `src`.
fn push_layout_if_gui(
    gui_attach: &Option<GuiAttachment>,
    conn: &Arc<PipeConn>,
    gui_write: &Arc<Mutex<()>>,
) -> std::io::Result<()> {
    if let Some(ga) = gui_attach
        && let Some((tree, active)) = ga.session.window_layout()
    {
        let _g = gui_write.lock().unwrap();
        let mut wr: &PipeConn = conn;
        send(&mut wr, &ServerMessage::WindowLayout { tree, active })?;
    }
    Ok(())
}
```

- [ ] **Step 5 : Lancer les tests + qualité + rebuild**

Run: `cargo test -p wimux-server && cargo fmt -p wimux-server && cargo clippy -p wimux-server --all-targets -- -D warnings && cargo build --release`
Expected: tout vert. Puis redémarrer le daemon détaché :
`./target/release/wimux.exe kill-server` (il repartira au prochain appel).

- [ ] **Step 6 : Commit**

```bash
git add crates/wimux-server/src/daemon.rs
git commit -m "feat(browser): handlers OpenWebPane/WebNavigate/WebBack/WebForward + push de disposition

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B1.5 — GUI

### Task 6 : rendu du volet navigateur

**Files:**
- Modify: `wimux-gui/src/panes.ts`, `wimux-gui/src/main.ts`, `wimux-gui/src/styles.css`
- Modify: `wimux-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `LayoutNode.Leaf.kind` (protocole), commandes Tauri ci-dessous.
- Produces: commandes `open_web_pane`, `web_navigate`, `web_back`, `web_forward`.

- [ ] **Step 1 : Ajouter les 4 commandes Tauri**

Dans `wimux-gui/src-tauri/src/lib.rs`, à côté de `split_pane` (même motif :
connexion **persistante**, pour que le serveur puisse repousser le `WindowLayout`) :

```rust
#[tauri::command]
fn open_web_pane(url: String, dir: String, bridge: State<Bridge>) -> Result<(), String> {
    let dir = match dir.as_str() {
        "LeftRight" => SplitDir::LeftRight,
        "TopBottom" => SplitDir::TopBottom,
        other => return Err(format!("direction inconnue : {other}")),
    };
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(
            &mut w,
            &ClientMessage::OpenWebPane {
                session: String::new(),
                from_pane: None,
                dir,
                url,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn web_navigate(pane_id: u64, url: String, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(
            &mut w,
            &ClientMessage::WebNavigate {
                session: String::new(),
                pane: pane_id,
                url,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn web_back(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(
            &mut w,
            &ClientMessage::WebBack {
                session: String::new(),
                pane: pane_id,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn web_forward(pane_id: u64, bridge: State<Bridge>) -> Result<(), String> {
    if let Some(conn) = bridge.conn.lock().unwrap().as_ref() {
        let mut w: &PipeConn = conn;
        send(
            &mut w,
            &ClientMessage::WebForward {
                session: String::new(),
                pane: pane_id,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
```

Les enregistrer dans le `invoke_handler![...]` existant.

La chaîne `session: String::new()` est **voulue** : sur la connexion persistante le
serveur connaît déjà la session attachée, et le helper `resolve_gui_session` ajouté
en Task 5 retombe dessus quand le champ est vide. La CLI, elle, renseigne le champ.

- [ ] **Step 2 : Étendre le type `LayoutNode` et les callbacks (panes.ts)**

Dans `wimux-gui/src/panes.ts` :

```typescript
/// Nature d'une feuille (miroir de `PaneKind` serveur, serde externally-tagged).
export type PaneKind = "Terminal" | { Web: { url: string } };

export type LayoutNode =
  | { Leaf: { pane_id: number; kind: PaneKind } }
  | {
      Split: {
        node_id: number;
        dir: "LeftRight" | "TopBottom";
        ratio: number;
        a: LayoutNode;
        b: LayoutNode;
      };
    };
```

Et ajouter à `PaneCallbacks` :

```typescript
  onWebNavigate: (paneId: number, url: string) => void;
  onWebBack: (paneId: number) => void;
  onWebForward: (paneId: number) => void;
```

- [ ] **Step 3 : Rendre une feuille navigateur**

Toujours dans `panes.ts`, ajouter le champ de suivi des vues web à la classe :

```typescript
  private webViews = new Map<number, { el: HTMLElement; frame: HTMLIFrameElement; input: HTMLInputElement }>();
```

Ajouter la construction (à côté de `ensureView`) :

```typescript
  /// Construit (ou réutilise) le conteneur d'un volet NAVIGATEUR : barre de
  /// chrome (URL, précédent, suivant, recharger) + iframe.
  private ensureWebView(paneId: number, url: string) {
    const existing = this.webViews.get(paneId);
    if (existing) {
      // Le serveur est la source de vérité : on suit l'URL reçue.
      if (existing.frame.getAttribute("src") !== url) {
        existing.frame.setAttribute("src", url);
      }
      if (document.activeElement !== existing.input) existing.input.value = url;
      return existing;
    }
    const el = document.createElement("div");
    el.className = "pane pane-web";
    el.dataset.paneId = String(paneId);

    const bar = document.createElement("div");
    bar.className = "web-bar";
    const back = document.createElement("button");
    back.textContent = "◀";
    back.title = "Précédent";
    back.onclick = () => this.cb.onWebBack(paneId);
    const fwd = document.createElement("button");
    fwd.textContent = "▶";
    fwd.title = "Suivant";
    fwd.onclick = () => this.cb.onWebForward(paneId);
    const reload = document.createElement("button");
    reload.textContent = "⟳";
    reload.title = "Recharger";
    const input = document.createElement("input");
    input.className = "web-url";
    input.value = url;
    input.onkeydown = (ev) => {
      if (ev.key === "Enter") this.cb.onWebNavigate(paneId, input.value.trim());
    };
    bar.append(back, fwd, reload, input);

    const frame = document.createElement("iframe");
    frame.className = "web-frame";
    frame.setAttribute("src", url);
    // Recharger est purement client : pas d'aller-retour serveur.
    reload.onclick = () => {
      frame.setAttribute("src", frame.getAttribute("src") ?? url);
    };

    // Avertissement permanent : un refus d'affichage en cadre n'est PAS
    // détectable de façon fiable depuis la page hôte, donc on informe au lieu
    // de prétendre diagnostiquer.
    const hint = document.createElement("div");
    hint.className = "web-hint";
    hint.textContent = "Certains sites refusent l'affichage en cadre.";

    el.append(bar, frame, hint);
    el.addEventListener("mousedown", () => this.cb.onFocus(paneId));
    const view = { el, frame, input };
    this.webViews.set(paneId, view);
    return view;
  }
```

Adapter `buildNode` pour dispatcher sur la nature :

```typescript
  private buildNode(tree: LayoutNode): HTMLElement {
    if ("Leaf" in tree) {
      const { pane_id, kind } = tree.Leaf;
      if (kind !== "Terminal" && "Web" in kind) {
        return this.ensureWebView(pane_id, kind.Web.url).el;
      }
      return this.ensureView(pane_id).el;
    }
    // ... reste inchangé
```

Adapter `collectIds` (les feuilles web comptent aussi) — il lit déjà
`tree.Leaf.pane_id`, donc **aucun changement**. Adapter `renderLayout` pour
disposer les vues web disparues, à côté du nettoyage des terminaux :

```typescript
    for (const [id, v] of this.webViews) {
      if (!wanted.has(id)) {
        v.el.remove();
        this.webViews.delete(id);
      }
    }
```

Et dans `reset()`, ajouter `this.webViews.clear();`.

Enfin, la signature structurelle doit inclure la nature, sinon une navigation
(même arbre, URL différente) ne redéclencherait pas de mise à jour :

```typescript
  private computeSignature(tree: LayoutNode): string {
    if ("Leaf" in tree) {
      const k = tree.Leaf.kind;
      const kindSig = k === "Terminal" ? "T" : `W:${k.Web.url}`;
      return `L${tree.Leaf.pane_id}:${kindSig}`;
    }
    const s = tree.Split;
    return `S${s.node_id}:${s.dir}:(${this.computeSignature(s.a)},${this.computeSignature(s.b)})`;
  }
```

- [ ] **Step 4 : Câbler les callbacks (main.ts)**

Là où `PaneManager` est construit, ajouter aux callbacks :

```typescript
  onWebNavigate: (paneId, url) => {
    invoke("web_navigate", { paneId, url }).catch((e) => console.error("web_navigate:", e));
  },
  onWebBack: (paneId) => {
    invoke("web_back", { paneId }).catch((e) => console.error("web_back:", e));
  },
  onWebForward: (paneId) => {
    invoke("web_forward", { paneId }).catch((e) => console.error("web_forward:", e));
  },
```

Ajouter un bouton d'ouverture dans la barre d'un volet terminal (`panes.ts`,
`ensureView`, à côté des boutons de découpe) :

```typescript
    const bWeb = document.createElement("button");
    bWeb.textContent = "🌐";
    bWeb.title = "Ouvrir un navigateur à côté";
    bWeb.onclick = (ev) => {
      ev.stopPropagation();
      this.cb.onOpenWeb(paneId);
    };
```

avec le callback correspondant dans `PaneCallbacks` :

```typescript
  onOpenWeb: (paneId: number) => void;
```

et dans `main.ts` :

```typescript
  onOpenWeb: () => {
    const url = prompt("URL à ouvrir :", "http://localhost:5173/");
    if (!url) return;
    invoke("open_web_pane", { url, dir: "LeftRight" }).catch((e) =>
      console.error("open_web_pane:", e),
    );
  },
```

- [ ] **Step 5 : Styles (styles.css)**

```css
/* Volet navigateur (B1) : barre de chrome + iframe pleine hauteur. */
.pane-web { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
.web-bar { display: flex; gap: 4px; align-items: center; padding: 3px 4px; background: #2d2d2d; }
.web-bar button {
  background: transparent; border: 0; color: #ccc; cursor: pointer;
  font-size: 12px; padding: 2px 5px; border-radius: 3px;
}
.web-bar button:hover { background: #3a3a3a; }
.web-url {
  flex: 1; min-width: 0; background: #1e1e1e; border: 1px solid #3a3a3a;
  color: #ddd; border-radius: 3px; padding: 2px 6px; font-size: 12px;
}
.web-frame { flex: 1; min-height: 0; border: 0; background: #fff; }
.web-hint { font-size: 10px; color: #777; padding: 2px 6px; background: #2d2d2d; }
```

- [ ] **Step 6 : Builds**

Run: `cd wimux-gui && npm run build`
Puis : `cd wimux-gui/src-tauri && cargo clippy --all-targets -- -D warnings && cargo fmt -- --check`
Expected: tout vert.

- [ ] **Step 7 : Test manuel**

Rebuild release, redémarrer le daemon, lancer la GUI (`npm run tauri dev`), créer un
workspace, cliquer 🌐 sur la barre d'un volet terminal, saisir
`http://localhost:5173/` (ou toute URL locale servie). Vérifier : le volet
navigateur apparaît à côté du terminal ; la barre d'URL navigue à la touche Entrée ;
précédent/suivant parcourent la pile ; recharger recharge ; **fermer et rouvrir la
GUI conserve le volet et son URL**.

- [ ] **Step 8 : Commit**

```bash
git add wimux-gui/src/panes.ts wimux-gui/src/main.ts wimux-gui/src/styles.css wimux-gui/src-tauri/src/lib.rs
git commit -m "feat(browser): rendu du volet navigateur (iframe + barre de chrome) et cablage Tauri

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase B1.6 — CLI

### Task 7 : `wimux browser open`

**Files:**
- Modify: `crates/wimux-cli/src/main.rs`
- Test: `crates/wimux-cli/src/main.rs`

**Interfaces:**
- Consumes: `ClientMessage::OpenWebPane`, helpers A1 `connected()`, `default_session()`, `agent::json_escape`.

- [ ] **Step 1 : Écrire le test de parsing (échoue)**

Dans le module `batch_tests` (ou un nouveau `browser_tests`) de `main.rs` :

```rust
#[cfg(test)]
mod browser_tests {
    use super::browser::*;
    use wimux_protocol::SplitDir;

    #[test]
    fn parse_open_lit_url_dir_et_cible() {
        let a = parse_open(&[
            "--url".into(), "http://localhost:5173/".into(),
            "--dir".into(), "v".into(),
            "-t".into(), "sess".into(),
            "--from-pane".into(), "3".into(),
        ])
        .unwrap();
        assert_eq!(a.url, "http://localhost:5173/");
        assert!(matches!(a.dir, SplitDir::TopBottom));
        assert_eq!(a.session.as_deref(), Some("sess"));
        assert_eq!(a.from_pane, Some(3));
    }

    #[test]
    fn parse_open_exige_une_url_et_defaut_cote_a_cote() {
        assert!(parse_open(&[]).is_err(), "sans --url c'est une erreur");
        let a = parse_open(&["--url".into(), "http://a/".into()]).unwrap();
        assert!(matches!(a.dir, SplitDir::LeftRight), "défaut : côte à côte");
    }
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-cli parse_open_lit_url_dir_et_cible`
Expected: FAIL — module `browser` absent.

- [ ] **Step 3 : Écrire le module de parsing**

Dans `main.rs`, après le module `batch` :

```rust
mod browser {
    use std::io;
    use wimux_protocol::SplitDir;

    /// Arguments analysés de `wimux browser open`.
    pub struct OpenArgs {
        pub url: String,
        pub dir: SplitDir,
        pub session: Option<String>,
        pub from_pane: Option<u64>,
    }

    /// Analyse `wimux browser open --url <url> [--dir h|v] [-t <session>] [--from-pane <id>]`.
    pub fn parse_open(args: &[String]) -> io::Result<OpenArgs> {
        let mut url = None;
        let mut dir = SplitDir::LeftRight; // défaut : côte à côte
        let mut session = None;
        let mut from_pane = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--url" => {
                    url = args.get(i + 1).cloned();
                    i += 2;
                }
                "--dir" => {
                    dir = match args.get(i + 1).map(String::as_str) {
                        Some("v") | Some("vertical") => SplitDir::TopBottom,
                        _ => SplitDir::LeftRight,
                    };
                    i += 2;
                }
                "-t" | "--target" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                "--from-pane" | "-p" => {
                    from_pane = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                _ => i += 1,
            }
        }
        match url {
            Some(url) => Ok(OpenArgs { url, dir, session, from_pane }),
            None => Err(io::Error::other(
                "usage : wimux browser open --url <url> [--dir h|v] [-t <session>] [--from-pane <id>]",
            )),
        }
    }
}
```

- [ ] **Step 4 : Router et écrire la commande**

Dans le `match cmd` de `main()`, avant `Some(other)` :

```rust
        Some("browser") => cmd_browser(&args[1..]),
```

Et les fonctions, à côté de `cmd_batch` :

```rust
fn cmd_browser(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("open") => browser_open(&args[1..]),
        _ => Err(io::Error::other("usage : wimux browser open --url <url> …")),
    }
}

fn browser_open(args: &[String]) -> io::Result<()> {
    let a = browser::parse_open(args)?;
    let session = default_session(a.session)?;
    let from_pane = a
        .from_pane
        .or_else(|| std::env::var("WIMUX_PANE").ok().and_then(|s| s.parse().ok()));
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::OpenWebPane {
            session,
            from_pane,
            dir: a.dir,
            url: a.url,
        },
    )?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PaneSpawned { pane_id } => {
            println!("{{\"pane_id\":{pane_id}}}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}
```

- [ ] **Step 5 : Ligne d'aide**

Dans `print_help`, après la ligne `batch <sous-cmd>` :

```
             browser open --url <url>    Ouvre un volet navigateur\n    \
```

- [ ] **Step 6 : Documenter les deux limites au README**

La spec exige que le comportement honnête soit écrit, pas seulement suggéré par
l'UI. Ajouter au `README.md` (à la racine), près de la description de la GUI :

```markdown
## Volet navigateur

Un volet de la disposition peut être un navigateur : bouton 🌐 sur la barre d'un
volet terminal, ou `wimux browser open --url http://localhost:5173/`. Le volet est
possédé par le serveur : il **survit au redémarrage de la GUI**, avec son URL.

Deux limites, assumées :

- **Certains sites refusent l'affichage en cadre** (en-tête `X-Frame-Options` ou
  `CSP frame-ancestors`) et resteront blancs. Ce refus n'est pas détectable de
  façon fiable depuis l'application : on ne peut donc pas afficher un diagnostic
  précis, seulement un avertissement général. Le cas d'usage visé est la
  prévisualisation d'un **serveur de développement local**, qui fonctionne.
- **Précédent/suivant parcourent l'historique de wimux**, c'est-à-dire les URL
  posées via la barre d'adresse ou l'ouverture du volet — pas celui du site. Les
  navigations faites *à l'intérieur* de la page (clic sur un lien) ne nous sont pas
  visibles quand elle est d'une autre origine.
```

- [ ] **Step 7 : Lancer, qualité, commit**

Run: `cargo test -p wimux-cli && cargo clippy -p wimux-cli --all-targets -- -D warnings && cargo fmt -p wimux-cli`
Expected: tout vert.

```bash
git add crates/wimux-cli/src/main.rs README.md
git commit -m "feat(browser): CLI wimux browser open + aide + limites documentees au README

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Revue finale (toute la branche)

- [ ] **Step 1 : Suite complète + qualité**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cd wimux-gui/src-tauri && cargo clippy --all-targets -- -D warnings && cargo fmt -- --check
cd wimux-gui && npm run build
```
Expected: tout vert. (Rappel : `wimux-gui/src-tauri` n'est **pas** membre du
workspace — il se vérifie séparément.)

- [ ] **Step 2 : Rebuild release + redémarrage du daemon**

```bash
cargo build --release
./target/release/wimux.exe kill-server
```

- [ ] **Step 3 : Démo bout-en-bout**

Servir une page en local (n'importe quel serveur de dev), puis : ouvrir un volet
navigateur depuis la GUI **et** depuis la CLI (`wimux browser open --url …`) ;
naviguer, reculer, avancer, recharger ; **fermer et rouvrir la GUI** pour vérifier
la persistance ; s'attacher en TUI (`wimux attach`) pour voir le substitut
`[navigateur] <url>`.

- [ ] **Step 4 : Mémoire projet** — consigner B1 fait dans `wimux-etat-avancement`.

---

## Notes de conception (rappels)

- **`Window::pane()` reste « terminal uniquement »** : c'est ce qui permet aux ~10
  appelants existants de continuer à compiler et à se comporter correctement (un
  navigateur n'est pas un volet terminal). `contains()` sert quand la nature est
  indifférente.
- **Le serveur est la source de vérité de la navigation** ; seul *recharger* est
  purement client.
- **La signature de disposition (GUI) inclut l'URL** : sans ça, une navigation ne
  déclencherait aucune mise à jour, puisque l'arbre serait structurellement identique.
- **Les ids de volet sont uniques toutes natures confondues** (même compteur), sinon
  un volet web pourrait entrer en collision avec un terminal.
- **L'avertissement `X-Frame-Options` est permanent et non diagnostique** : le refus
  n'est pas détectable de façon fiable depuis la page hôte.
