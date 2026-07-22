# B2.2 — Actions de pilotage du navigateur — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ajouter au moteur navigateur CDP de B2.1 cinq verbes d'action — `click`, `type`, `press`, `scroll`, `wait` — ciblés par des **références** issues du snapshot, tout en CDP natif (zéro JS de page).

**Architecture:** Le snapshot d'accessibilité est enrichi de jetons `[ref=eN]` ; le serveur garde une table `ref → backend_node_id` (reconstruite à chaque snapshot, vidée à la navigation). Chaque action résout la ref en `backend_node_id` puis pilote la page via des commandes CDP natives `DOM.*` (géométrie/focus/scroll) et `Input.*` (souris/clavier/texte). Aucun `Runtime.evaluate`.

**Tech Stack:** Rust ; `chromiumoxide` 0.9.1 (CDP) ; le pont sync→tokio et le `BrowserEngine` de B2.1 ; postcard/serde pour le protocole ; CLI `wimux browser`.

## Global Constraints

- **Protocole postcard positionnel** : toute nouvelle variante de `ClientMessage` et toute nouvelle variante de `BrowserCommand` s'ajoutent **en FIN d'enum**. Idem : le nouveau champ `backend_node_id` s'ajoute **en FIN** de `struct AxSnapshotNode`. Ne jamais insérer au milieu.
- **Zéro JS de page** : aucune action n'utilise `Runtime.evaluate`/`callFunctionOn`. Uniquement `DOM.*` et `Input.*`. (`select`/`eval` sont B2.3.)
- **Contenu de page = donnée non fiable** : les noms AX (qui portent les refs) et le texte de `wait --text` passent déjà par `nettoyer` (caractères de contrôle → espace, acquis B2.1). Ne jamais laisser un contenu de page piloter le flot de contrôle.
- **Moteur = pur mécanisme** : le moteur exécute l'action sans juger. La politique de sécurité (pas d'identifiants via `type`, confirmer les soumissions) vit dans les règles d'opération de Claude et le skill B2.4, pas ici.
- **Nommage** : le champ « référence » s'appelle `ref_` partout (le mot `ref` est réservé en Rust). Jeton de ref = `eN` (e + entier ≥ 1, ordre d'affichage).
- **Table de touches gérées** (verbe `press`) : `Enter, Tab, Escape, Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown`. Touche inconnue → erreur listant les touches gérées.
- **Timeouts** : `wait --text` = 10 s ; `wait --settle` = 30 s (comme `navigate`). Poll de `wait --text` = 250 ms.
- **Réponses réutilisées de B2.1** : actions → `BrowserReply::Ok` (→ `ServerMessage::Ok`) ; `wait --text` → `BrowserReply::Text` (→ `ServerMessage::BrowserText`) ; erreurs → `Error`. **Aucun nouveau `ServerMessage`.**

### Référence API CDP vérifiée (chromiumoxide_cdp 0.9.1)

Imports :
- DOM : `chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, GetBoxModelParams, FocusParams, ScrollIntoViewIfNeededParams}`
- Input : `chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType, MouseButton, DispatchKeyEventParams, DispatchKeyEventType, InsertTextParams}`

Faits vérifiés :
- `BackendNodeId::new(i64)` ; `BackendNodeId::inner(&self) -> &i64`.
- `AxNode.backend_dom_node_id: Option<dom::BackendNodeId>`.
- `GetBoxModelParams { node_id: Option<NodeId>, backend_node_id: Option<BackendNodeId>, object_id: Option<...> }` (3 champs, tous `Option`). Réponse : `resp.result.model.content` de type `Quad`, avec `Quad::inner(&self) -> &Vec<f64>` = `[x1,y1,x2,y2,x3,y3,x4,y4]`.
- `FocusParams { node_id, backend_node_id, object_id }` (mêmes 3 champs `Option`).
- `ScrollIntoViewIfNeededParams { node_id, backend_node_id, object_id, rect }` (4 champs `Option`).
- `DispatchMouseEventParams::builder()` → méthodes `.r#type(DispatchMouseEventType)`, `.x(f64)`, `.y(f64)`, `.button(MouseButton)`, `.click_count(i64)`, `.delta_x(f64)`, `.delta_y(f64)`. `.build() -> Result<_, String>`. Types : `DispatchMouseEventType::{MousePressed, MouseReleased, MouseWheel}`, `MouseButton::Left`.
- `DispatchKeyEventParams::builder()` → `.r#type(DispatchKeyEventType)`, `.key(impl Into<String>)`, `.code(impl Into<String>)`, `.windows_virtual_key_code(i64)`, `.text(impl Into<String>)`, `.modifiers(i64)`. `.build() -> Result<_, String>`. Types : `DispatchKeyEventType::{KeyDown, KeyUp}`. Masque de modifieurs CDP : Alt=1, **Ctrl=2**, Meta=4, Shift=8.
- `InsertTextParams::new(impl Into<String>)`.
- `page.execute(params).await` → `Result<CommandResponse<Returns>, _>` ; le résultat utile est `resp.result` (ex. `.model`, `.nodes`). Pour les commandes `Input.*` le retour est ignoré.

---

## File Structure

- **Modifier** `crates/wimux-server/src/browser.rs` (fichier moteur B2.1) : champ `backend_node_id` sur `AxSnapshotNode` ; nouvelle signature de `render_ax_tree` ; `Session.refs` ; extraction de `snapshot_nodes` ; nouvelles variantes `BrowserCommand` + bras `dispatch` ; helpers de résolution/entrée CDP ; table de touches ; tests purs et d'intégration.
- **Modifier** `crates/wimux-protocol/src/lib.rs` : 5 variantes `ClientMessage` en fin d'enum.
- **Modifier** `crates/wimux-server/src/daemon.rs` : 5 bras de handler (mappent `ClientMessage::Browser*` → `BrowserCommand::*` via `browser_reply`).
- **Modifier** `crates/wimux-cli/src/main.rs` : routage `cmd_browser` + parseurs purs (`parse_ref`/`parse_type`/`parse_press`/`parse_scroll`/`parse_wait`) + `browser_wait` + ligne d'aide.

Le fichier `browser.rs` grandit (~+300 lignes). On garde un seul fichier : la machinerie (types AX, engine, dispatch, helpers) est cohésive et le `match` de `dispatch` doit rester au même endroit ; pas de découpage unilatéral (cohérent avec B2.1).

---

## Task 1 : Références dans le snapshot (rendu pur + cycle de vie)

Fonde le ciblage : le snapshot affiche `[ref=eN]` et le serveur mémorise `ref → backend_node_id`, table vidée à la navigation.

**Files:**
- Modify: `crates/wimux-server/src/browser.rs` (struct `AxSnapshotNode` ~45-51 ; `render_ax_tree` ~89-103 ; `render_node` ~119-170 ; `map_ax_node` ~439-482 ; `struct Session` ~250-254 ; `launch_session` ~319-323 ; bras `Snapshot` ~398-411 ; bras `Navigate` ~388-390 ; tests de rendu ~660-770)
- Test: mêmes tests, dans `mod tests`

**Interfaces:**
- Produces :
  - `AxSnapshotNode` gagne `pub backend_node_id: Option<i64>` (fin de struct).
  - `pub fn render_ax_tree(nodes: &[AxSnapshotNode]) -> (String, Vec<(String, i64)>)` — texte + paires `(ref, backend_id)` des nœuds affichés adossés à un nœud DOM.
  - `struct Session` gagne `refs: HashMap<String, i64>`.
  - `async fn snapshot_nodes(page: &chromiumoxide::Page) -> Result<Vec<AxSnapshotNode>, String>` (factorisation de la lecture AX, réutilisée par `wait`).

- [ ] **Step 1 : Écrire les tests de rendu (échouent : nouvelle signature + refs)**

Remplacer le corps des tests de rendu existants et en ajouter. Dans `mod tests` :

```rust
#[test]
fn render_numerote_les_noeuds_affiches_avec_backend_id() {
    // bouton (backend 100) + lien (backend 200), sous une racine décorative.
    let nodes = vec![
        AxSnapshotNode { node_id: "1".into(), role: "none".into(), name: None, states: vec![], child_ids: vec!["2".into(), "3".into()], backend_node_id: None },
        AxSnapshotNode { node_id: "2".into(), role: "button".into(), name: Some("Continuer".into()), states: vec!["focusable".into()], child_ids: vec![], backend_node_id: Some(100) },
        AxSnapshotNode { node_id: "3".into(), role: "link".into(), name: Some("Aide".into()), states: vec![], child_ids: vec![], backend_node_id: Some(200) },
    ];
    let (texte, refs) = render_ax_tree(&nodes);
    assert!(texte.contains("[ref=e1] button \"Continuer\" [focusable]"), "texte : {texte}");
    assert!(texte.contains("[ref=e2] link \"Aide\""), "texte : {texte}");
    // Numérotation en ordre d'affichage ; racine décorative non numérotée.
    assert_eq!(refs, vec![("e1".to_string(), 100), ("e2".to_string(), 200)]);
}

#[test]
fn render_noeud_affiche_sans_backend_id_na_pas_de_ref() {
    let nodes = vec![
        AxSnapshotNode { node_id: "1".into(), role: "heading".into(), name: Some("Titre".into()), states: vec![], child_ids: vec![], backend_node_id: None },
    ];
    let (texte, refs) = render_ax_tree(&nodes);
    assert_eq!(texte, "heading \"Titre\"");
    assert!(refs.is_empty());
}
```

Adapter aussi les tests de rendu B2.1 existants qui appellent `render_ax_tree` : ils reçoivent désormais un tuple. Pour chacun, remplacer `let texte = render_ax_tree(&nodes);` par `let (texte, _) = render_ax_tree(&nodes);` et ajouter `backend_node_id: None` à chaque littéral `AxSnapshotNode` (le champ est nouveau). Concernés (par leur nom) : `render_ax_tree_indente_role_nom_etats_et_elague`, `render_promeut_enfants_dun_noeud_decoratif`, `render_termine_sur_cycle` (ou noms équivalents présents), `render_arbre_tres_profond_ne_deborde_pas`, `render_chaine_decorative_profonde_ne_deborde_pas`, `render_neutralise_caracteres_de_controle`.

- [ ] **Step 2 : Lancer les tests → échec de compilation (signature/champ)**

Run: `cargo test -p wimux-server browser 2>&1 | head -30`
Expected: erreurs de compilation (`render_ax_tree` renvoie un tuple ; `backend_node_id` manquant ; champ inconnu).

- [ ] **Step 3 : Ajouter le champ, changer le rendu, factoriser la lecture AX**

Dans `struct AxSnapshotNode`, ajouter en fin :

```rust
    pub child_ids: Vec<String>,
    /// Identifiant DOM backend (CDP) pour cibler ce nœud dans les actions (B2.2).
    /// `None` si le nœud AX n'est pas adossé à un nœud DOM (non ciblable).
    pub backend_node_id: Option<i64>,
}
```

Dans `map_ax_node`, ajouter au littéral `AxSnapshotNode` (fin) :

```rust
        child_ids: n
            .child_ids
            .iter()
            .flatten()
            .map(|c| c.inner().clone())
            .collect(),
        backend_node_id: n.backend_dom_node_id.as_ref().map(|b| *b.inner()),
    }
```

Remplacer `render_ax_tree` :

```rust
pub fn render_ax_tree(nodes: &[AxSnapshotNode]) -> (String, Vec<(String, i64)>) {
    use std::collections::{HashMap, HashSet};
    if nodes.is_empty() {
        return (String::new(), Vec::new());
    }
    let index: HashMap<&str, &AxSnapshotNode> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let mut out = String::new();
    let mut visites: HashSet<&str> = HashSet::new();
    let mut compteur: usize = 0;
    let mut refs: Vec<(String, i64)> = Vec::new();
    render_node(
        &nodes[0], &index, 0, 0, &mut out, &mut visites, &mut compteur, &mut refs,
    );
    (out.trim_end().to_string(), refs)
}
```

Modifier `render_node` : ajouter les deux paramètres et émettre le préfixe de ref pour un nœud affiché doté d'un `backend_node_id`. Signature et bloc d'impression :

```rust
#[allow(clippy::too_many_arguments)]
fn render_node<'a>(
    node: &'a AxSnapshotNode,
    index: &std::collections::HashMap<&'a str, &'a AxSnapshotNode>,
    depth: usize,
    recursion: usize,
    out: &mut String,
    visites: &mut std::collections::HashSet<&'a str>,
    compteur: &mut usize,
    refs: &mut Vec<(String, i64)>,
) {
    if !visites.insert(node.node_id.as_str()) {
        return;
    }
    if recursion >= PROFONDEUR_MAX {
        return;
    }
    if !node.est_decoratif() {
        for _ in 0..depth {
            out.push_str("  ");
        }
        // Ref : uniquement pour un nœud AFFICHÉ adossé à un vrai nœud DOM.
        // « Ce que Claude voit numéroté = ce qu'il peut cibler. »
        if let Some(bid) = node.backend_node_id {
            *compteur += 1;
            let r = format!("e{compteur}");
            out.push_str(&format!("[ref={r}] "));
            refs.push((r, bid));
        }
        out.push_str(&nettoyer(&node.role));
        if let Some(name) = &node.name {
            out.push_str(&format!(" \"{}\"", nettoyer(name)));
        }
        if !node.states.is_empty() {
            let etats: Vec<String> = node.states.iter().map(|s| nettoyer(s)).collect();
            out.push_str(&format!(" [{}]", etats.join(", ")));
        }
        out.push('\n');
    }
    let child_depth = if node.est_decoratif() { depth } else { depth + 1 };
    for cid in &node.child_ids {
        if let Some(child) = index.get(cid.as_str()) {
            render_node(
                child, index, child_depth, recursion + 1, out, visites, compteur, refs,
            );
        }
    }
}
```

Ajouter la factorisation de la lecture AX (près de `map_ax_node`) :

```rust
/// Lit l'arbre d'accessibilité complet et le mappe vers nos `AxSnapshotNode`.
async fn snapshot_nodes(page: &chromiumoxide::Page) -> Result<Vec<AxSnapshotNode>, String> {
    use chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeParams;
    let resp = page
        .execute(GetFullAxTreeParams::default())
        .await
        .map_err(|e| format!("arbre d'accessibilité : {e}"))?;
    Ok(resp.result.nodes.iter().map(map_ax_node).collect())
}
```

- [ ] **Step 4 : Câbler `Session.refs`, le bras `Snapshot` et l'effacement dans `Navigate`**

Dans `struct Session`, ajouter le champ :

```rust
struct Session {
    browser: Browser,
    page: chromiumoxide::Page,
    _handler: tokio::task::JoinHandle<()>,
    /// Table `ref (eN) -> backend_node_id`, reconstruite à chaque `Snapshot`,
    /// vidée à chaque `Navigate` (les refs pointent le DOM de l'ancienne page).
    refs: std::collections::HashMap<String, i64>,
}
```

Dans `launch_session`, initialiser au retour :

```rust
    Ok(Session {
        browser,
        page,
        _handler: handler_task,
        refs: std::collections::HashMap::new(),
    })
```

Remplacer le bras `BrowserCommand::Snapshot` (utiliser `snapshot_nodes`, stocker les refs — `as_mut`) :

```rust
        BrowserCommand::Snapshot => {
            let s = sess
                .as_mut()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            let nodes = snapshot_nodes(&s.page).await?;
            let (texte, refs) = render_ax_tree(&nodes);
            s.refs = refs.into_iter().collect();
            Ok(BrowserReply::Text(texte))
        }
```

Dans le bras `BrowserCommand::Navigate`, après la navigation réussie (juste avant de lire `finale`), vider les refs de l'ancienne page :

```rust
            // Les refs du snapshot précédent ne valent plus rien après navigation.
            if let Some(s) = sess.as_mut() {
                s.refs.clear();
            }
            let finale = page.url().await.ok().flatten().unwrap_or_default();
```

Note : `page` est un emprunt de `sess` juste au-dessus ; le libérer avant `sess.as_mut()`. Concrètement, remplacer la fin du bras par un bloc qui relit l'URL après avoir vidé les refs — récupérer l'URL via `sess.as_ref().unwrap().page` :

```rust
            })
            .await
            .map_err(|_| "navigation : délai dépassé (30 s)".to_string())??;
            let s = sess.as_mut().unwrap();
            s.refs.clear();
            let finale = s.page.url().await.ok().flatten().unwrap_or_default();
            Ok(BrowserReply::Text(finale))
        }
```

- [ ] **Step 5 : Lancer les tests purs → succès**

Run: `cargo test -p wimux-server browser 2>&1 | tail -20`
Expected: PASS (dont `render_numerote_les_noeuds_affiches_avec_backend_id`, `render_noeud_affiche_sans_backend_id_na_pas_de_ref`, et les tests B2.1 adaptés). `cargo fmt --all` puis `cargo clippy -p wimux-server --all-targets` propres.

- [ ] **Step 6 : Test d'intégration — le snapshot montre des refs (si navigateur présent)**

Ajouter dans `mod tests` :

```rust
#[test]
fn snapshot_expose_des_refs_pour_les_elements() {
    if !navigateur_dispo() {
        eprintln!("aucun navigateur : test snapshot_refs ignoré");
        return;
    }
    let (url, _srv) =
        servir_page_locale("<!doctype html><title>T</title><button>Continuer</button>");
    let engine = BrowserEngine::new();
    engine.exec(BrowserCommand::Navigate(url)).unwrap();
    match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => {
            assert!(t.contains("[ref=e"), "snapshot sans ref : {t}");
            assert!(t.contains("button \"Continuer\""), "snapshot : {t}");
        }
        _ => panic!("Text attendu"),
    }
    let _ = engine.exec(BrowserCommand::Close);
}
```

Run: `cargo test -p wimux-server browser 2>&1 | tail -20`
Expected: PASS (ou « ignoré » sans navigateur).

- [ ] **Step 7 : Commit**

```bash
git add crates/wimux-server/src/browser.rs
git commit -m "feat(browser): refs [ref=eN] dans le snapshot + table ref->backend id (B2.2 base)"
```

---

## Task 2 : `click` (résolution ref→élément + clic souris natif)

Introduit la machinerie de résolution partagée (`backend_id_for`, `element_center`, `scroll_into_view`, `mouse_click_at`) et la première action, de bout en bout.

**Files:**
- Modify: `crates/wimux-server/src/browser.rs` (enum `BrowserCommand` ~129-138 ; `dispatch` ~327 ; helpers)
- Modify: `crates/wimux-protocol/src/lib.rs` (fin de `ClientMessage` ~482)
- Modify: `crates/wimux-server/src/daemon.rs` (après le bras `BrowserScreenshot` ~1383)
- Modify: `crates/wimux-cli/src/main.rs` (`cmd_browser` ~1170 ; `mod browser`)
- Test: `mod tests` de `browser.rs`

**Interfaces:**
- Consumes : `Session.refs`, `render_ax_tree` (Task 1).
- Produces :
  - `BrowserCommand::Click { ref_: String }`.
  - `fn backend_id_for(sess: &Session, r: &str) -> Result<i64, String>`.
  - `async fn scroll_into_view(page, backend: i64) -> Result<(), String>`.
  - `async fn element_center(page, backend: i64) -> Result<(f64, f64), String>`.
  - `async fn mouse_click_at(page, x: f64, y: f64) -> Result<(), String>`.
  - `ClientMessage::BrowserClick { ref_: String }` ; CLI `wimux browser click --ref eN`.

- [ ] **Step 1 : Test d'intégration `click` (échoue : verbe absent)**

```rust
#[test]
fn click_declenche_le_gestionnaire_de_la_page() {
    if !navigateur_dispo() {
        eprintln!("aucun navigateur : test click ignoré");
        return;
    }
    // Un clic sur le bouton change le texte d'un paragraphe -> visible au snapshot.
    let (url, _srv) = servir_page_locale(
        "<!doctype html><title>T</title>\
         <button onclick=\"document.getElementById('r').textContent='clické'\">Go</button>\
         <p id=r>vide</p>",
    );
    let engine = BrowserEngine::new();
    engine.exec(BrowserCommand::Navigate(url)).unwrap();
    let snap = match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => t,
        _ => panic!("Text"),
    };
    let r = ref_pour(&snap, "Go").expect("ref du bouton Go");
    assert!(matches!(
        engine.exec(BrowserCommand::Click { ref_: r }).unwrap(),
        BrowserReply::Ok
    ));
    match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => assert!(t.contains("clické"), "après clic : {t}"),
        _ => panic!("Text"),
    }
    let _ = engine.exec(BrowserCommand::Close);
}
```

Ajouter le helper de test (dans `mod tests`) qui extrait `eN` de la ligne contenant `besoin` :

```rust
/// Extrait le jeton `eN` de la première ligne du snapshot contenant `besoin`.
fn ref_pour(snapshot: &str, besoin: &str) -> Option<String> {
    let ligne = snapshot.lines().find(|l| l.contains(besoin))?;
    let deb = ligne.find("[ref=")? + 5;
    let fin = ligne[deb..].find(']')? + deb;
    Some(ligne[deb..fin].to_string())
}
```

- [ ] **Step 2 : Lancer → échec de compilation (`BrowserCommand::Click` inconnu)**

Run: `cargo test -p wimux-server browser 2>&1 | head -20`
Expected: erreur `no variant ... Click`.

- [ ] **Step 3 : Ajouter la variante, les helpers et le bras `dispatch`**

Dans `enum BrowserCommand`, en fin :

```rust
    Snapshot,
    Screenshot,
    /// B2.2 : clic gauche sur l'élément désigné par une ref de snapshot.
    Click { ref_: String },
}
```

Ajouter les helpers (près de `dispatch`) :

```rust
/// Traduit une ref de snapshot en `backend_node_id` mémorisé. Erreur explicite
/// si la ref est inconnue (jamais vue ou périmée depuis la dernière navigation).
fn backend_id_for(sess: &Session, r: &str) -> Result<i64, String> {
    sess.refs
        .get(r)
        .copied()
        .ok_or_else(|| format!("ref inconnue ({r}) — refais un snapshot"))
}

/// Amène l'élément dans le viewport (préalable à un clic fiable).
async fn scroll_into_view(page: &chromiumoxide::Page, backend: i64) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ScrollIntoViewIfNeededParams};
    page.execute(ScrollIntoViewIfNeededParams {
        node_id: None,
        backend_node_id: Some(BackendNodeId::new(backend)),
        object_id: None,
        rect: None,
    })
    .await
    .map_err(|e| format!("scrollIntoView : {e}"))?;
    Ok(())
}

/// Centre géométrique de l'élément (moyenne des 4 coins du quad `content`).
async fn element_center(page: &chromiumoxide::Page, backend: i64) -> Result<(f64, f64), String> {
    use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, GetBoxModelParams};
    let resp = page
        .execute(GetBoxModelParams {
            node_id: None,
            backend_node_id: Some(BackendNodeId::new(backend)),
            object_id: None,
        })
        .await
        .map_err(|e| format!("box model : {e}"))?;
    let q = resp.result.model.content.inner();
    if q.len() < 8 {
        return Err("élément sans géométrie visible".into());
    }
    let cx = (q[0] + q[2] + q[4] + q[6]) / 4.0;
    let cy = (q[1] + q[3] + q[5] + q[7]) / 4.0;
    Ok((cx, cy))
}

/// Clic gauche natif (press + release) aux coordonnées viewport données.
async fn mouse_click_at(page: &chromiumoxide::Page, x: f64, y: f64) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
    };
    let down = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MousePressed)
        .x(x)
        .y(y)
        .button(MouseButton::Left)
        .click_count(1)
        .build()?;
    page.execute(down)
        .await
        .map_err(|e| format!("clic (down) : {e}"))?;
    let up = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseReleased)
        .x(x)
        .y(y)
        .button(MouseButton::Left)
        .click_count(1)
        .build()?;
    page.execute(up)
        .await
        .map_err(|e| format!("clic (up) : {e}"))?;
    Ok(())
}
```

Ajouter le bras `dispatch` (avant le `}` final du `match`) :

```rust
        BrowserCommand::Click { ref_ } => {
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            let bid = backend_id_for(s, &ref_)?;
            scroll_into_view(&s.page, bid).await?;
            let (x, y) = element_center(&s.page, bid).await?;
            mouse_click_at(&s.page, x, y).await?;
            Ok(BrowserReply::Ok)
        }
```

- [ ] **Step 4 : Protocole + daemon + CLI**

`crates/wimux-protocol/src/lib.rs`, en fin de `ClientMessage` :

```rust
    /// B2.1 : capture PNG écrite sur disque, renvoie le chemin (erreur si non lancé).
    BrowserScreenshot,
    /// B2.2 : clic gauche sur l'élément désigné par une ref de snapshot.
    BrowserClick { ref_: String },
}
```

`crates/wimux-server/src/daemon.rs`, après le bras `BrowserScreenshot` :

```rust
            ClientMessage::BrowserClick { ref_ } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Click { ref_ }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

`crates/wimux-cli/src/main.rs` : dans `mod browser`, ajouter un lecteur générique de flag et `parse_ref` :

```rust
    /// Valeur suivant `--<nom>` dans `args`, si présente.
    pub fn flag(args: &[String], nom: &str) -> Option<String> {
        args.iter().position(|a| a == nom).and_then(|i| args.get(i + 1).cloned())
    }

    /// `--ref <eN>` obligatoire.
    pub fn parse_ref(args: &[String]) -> io::Result<String> {
        flag(args, "--ref").ok_or_else(|| io::Error::other("usage : wimux browser click --ref <eN>"))
    }
```

Dans `cmd_browser`, ajouter le routage `click` et compléter le message d'usage :

```rust
        Some("screenshot") => browser_screenshot(),
        Some("click") => {
            browser_simple(ClientMessage::BrowserClick { ref_: browser::parse_ref(&args[1..])? })
        }
        _ => Err(io::Error::other(
            "usage : wimux browser <open|launch|close|status|navigate|url|snapshot|screenshot|click|type|press|scroll|wait> …",
        )),
```

- [ ] **Step 5 : Compiler tout, lancer le test → succès**

Run: `cargo test -p wimux-server browser 2>&1 | tail -20 && cargo build -p wimux-cli 2>&1 | tail -3`
Expected: PASS (ou « ignoré »), CLI compile. `cargo fmt --all` ; `cargo clippy --workspace --all-targets` propre.

- [ ] **Step 6 : Commit**

```bash
git add crates/wimux-server/src/browser.rs crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs crates/wimux-cli/src/main.rs
git commit -m "feat(browser): verbe click (ref -> box model -> clic souris natif)"
```

---

## Task 3 : `type` (focus + tout sélectionner + insérer)

**Files:**
- Modify: `crates/wimux-server/src/browser.rs` (enum `BrowserCommand` ; `dispatch` ; helpers `focus_backend`, `dispatch_key`, `select_all`, `insert_text`)
- Modify: `crates/wimux-protocol/src/lib.rs` (fin `ClientMessage`)
- Modify: `crates/wimux-server/src/daemon.rs` (bras handler)
- Modify: `crates/wimux-cli/src/main.rs` (`cmd_browser` ; `parse_type` + struct `TypeArgs`)
- Test: `mod tests`

**Interfaces:**
- Consumes : `backend_id_for` (Task 2).
- Produces :
  - `BrowserCommand::Type { ref_: String, text: String }`.
  - `async fn focus_backend(page, backend: i64) -> Result<(), String>`.
  - `async fn dispatch_key(page, kind: DispatchKeyEventType, key: &str, code: &str, vk: i64, text: Option<&str>, modifiers: Option<i64>) -> Result<(), String>` (bas niveau, réutilisé par `press`).
  - `TypeArgs { ref_: String, text: String }` + `parse_type`.
  - CLI `wimux browser type --ref eN --text "…"`.

- [ ] **Step 1 : Test d'intégration `type` + test pur `parse_type` (échouent)**

`browser.rs` `mod tests` :

```rust
#[test]
fn type_ecrit_dans_un_champ() {
    if !navigateur_dispo() {
        eprintln!("aucun navigateur : test type ignoré");
        return;
    }
    // Un miroir reflète la valeur saisie dans un <p> -> visible au snapshot.
    let (url, _srv) = servir_page_locale(
        "<!doctype html><title>T</title>\
         <input aria-label=Nom oninput=\"document.getElementById('m').textContent=this.value\">\
         <p id=m>vide</p>",
    );
    let engine = BrowserEngine::new();
    engine.exec(BrowserCommand::Navigate(url)).unwrap();
    let snap = match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => t,
        _ => panic!("Text"),
    };
    let r = ref_pour(&snap, "Nom").expect("ref du champ");
    assert!(matches!(
        engine.exec(BrowserCommand::Type { ref_: r, text: "Fabrice".into() }).unwrap(),
        BrowserReply::Ok
    ));
    match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => assert!(t.contains("Fabrice"), "après type : {t}"),
        _ => panic!("Text"),
    }
    let _ = engine.exec(BrowserCommand::Close);
}
```

`main.rs` (module de tests CLI `browser_tests`, où résident déjà les tests de parsing) :

```rust
#[test]
fn parse_type_lit_ref_et_texte() {
    let a = parse_type(&["--ref".into(), "e2".into(), "--text".into(), "Bonjour".into()]).unwrap();
    assert_eq!(a.ref_, "e2");
    assert_eq!(a.text, "Bonjour");
    assert!(parse_type(&["--ref".into(), "e2".into()]).is_err()); // --text manquant
}
```

- [ ] **Step 2 : Lancer → échec (compilation)**

Run: `cargo test -p wimux-server browser 2>&1 | head -10 ; cargo test -p wimux-cli browser 2>&1 | head -10`
Expected: erreurs (`Type` inconnu ; `parse_type` absent).

- [ ] **Step 3 : Helpers d'entrée clavier + variante + bras `dispatch`**

`browser.rs`, ajouter les helpers :

```rust
/// Met le focus clavier sur l'élément.
async fn focus_backend(page: &chromiumoxide::Page, backend: i64) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, FocusParams};
    page.execute(FocusParams {
        node_id: None,
        backend_node_id: Some(BackendNodeId::new(backend)),
        object_id: None,
    })
    .await
    .map_err(|e| format!("focus : {e}"))?;
    Ok(())
}

/// Un événement clavier bas niveau (KeyDown ou KeyUp). `text` = caractère émis
/// (ex. "\r" pour Enter) ; `modifiers` = masque CDP (Ctrl=2…).
async fn dispatch_key(
    page: &chromiumoxide::Page,
    kind: chromiumoxide::cdp::browser_protocol::input::DispatchKeyEventType,
    key: &str,
    code: &str,
    vk: i64,
    text: Option<&str>,
    modifiers: Option<i64>,
) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::input::DispatchKeyEventParams;
    let mut b = DispatchKeyEventParams::builder()
        .r#type(kind)
        .key(key)
        .code(code)
        .windows_virtual_key_code(vk);
    if let Some(t) = text {
        b = b.text(t);
    }
    if let Some(m) = modifiers {
        b = b.modifiers(m);
    }
    let p = b.build()?;
    page.execute(p).await.map_err(|e| format!("touche : {e}"))?;
    Ok(())
}

/// Ctrl+A sur l'élément focalisé (sélectionne tout le contenu éditable).
async fn select_all(page: &chromiumoxide::Page) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::input::DispatchKeyEventType;
    dispatch_key(page, DispatchKeyEventType::KeyDown, "a", "KeyA", 65, None, Some(2)).await?;
    dispatch_key(page, DispatchKeyEventType::KeyUp, "a", "KeyA", 65, None, Some(2)).await?;
    Ok(())
}

/// Insère du texte (remplace la sélection courante ; gère l'unicode). CDP natif.
async fn insert_text(page: &chromiumoxide::Page, text: &str) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
    page.execute(InsertTextParams::new(text.to_string()))
        .await
        .map_err(|e| format!("insertText : {e}"))?;
    Ok(())
}
```

`enum BrowserCommand`, en fin :

```rust
    Click { ref_: String },
    /// B2.2 : vide le champ (Ctrl+A) puis saisit `text`.
    Type { ref_: String, text: String },
}
```

Bras `dispatch` :

```rust
        BrowserCommand::Type { ref_, text } => {
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            let bid = backend_id_for(s, &ref_)?;
            focus_backend(&s.page, bid).await?;
            select_all(&s.page).await?;
            insert_text(&s.page, &text).await?;
            Ok(BrowserReply::Ok)
        }
```

- [ ] **Step 4 : Protocole + daemon + CLI**

`lib.rs`, fin `ClientMessage` :

```rust
    BrowserClick { ref_: String },
    /// B2.2 : vide le champ puis saisit du texte.
    BrowserType { ref_: String, text: String },
}
```

`daemon.rs`, après le bras `BrowserClick` :

```rust
            ClientMessage::BrowserType { ref_, text } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Type { ref_, text }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

`main.rs` `mod browser`, ajouter la struct + parseur :

```rust
    #[derive(Debug, PartialEq)]
    pub struct TypeArgs {
        pub ref_: String,
        pub text: String,
    }

    /// `--ref <eN> --text <texte>` (les deux obligatoires ; texte vide autorisé).
    pub fn parse_type(args: &[String]) -> io::Result<TypeArgs> {
        let ref_ = flag(args, "--ref")
            .ok_or_else(|| io::Error::other("usage : wimux browser type --ref <eN> --text <texte>"))?;
        let text = flag(args, "--text")
            .ok_or_else(|| io::Error::other("usage : wimux browser type --ref <eN> --text <texte>"))?;
        Ok(TypeArgs { ref_, text })
    }
```

`cmd_browser`, routage :

```rust
        Some("type") => {
            let a = browser::parse_type(&args[1..])?;
            browser_simple(ClientMessage::BrowserType { ref_: a.ref_, text: a.text })
        }
```

- [ ] **Step 5 : Tests → succès**

Run: `cargo test -p wimux-server browser 2>&1 | tail -10 ; cargo test -p wimux-cli browser 2>&1 | tail -10`
Expected: PASS. `cargo fmt --all` ; `cargo clippy --workspace --all-targets` propre.

- [ ] **Step 6 : Commit**

```bash
git add -A
git commit -m "feat(browser): verbe type (focus + Ctrl+A + insertText, CDP natif)"
```

---

## Task 4 : `press` (table de touches nommées)

**Files:**
- Modify: `crates/wimux-server/src/browser.rs` (fonction pure `touche_cdp` ; enum ; `dispatch`)
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs`
- Modify: `crates/wimux-cli/src/main.rs` (`parse_press` + `PressArgs`)
- Test: `mod tests`

**Interfaces:**
- Consumes : `dispatch_key`, `focus_backend`, `backend_id_for` (Tasks 2-3).
- Produces :
  - `fn touche_cdp(nom: &str) -> Option<(&'static str, &'static str, i64)>` (nom → (key, code, windows_vk)).
  - `BrowserCommand::Press { key: String, ref_: Option<String> }`.
  - `PressArgs { key: String, ref_: Option<String> }` + `parse_press`.
  - CLI `wimux browser press <touche> [--ref eN]`.

- [ ] **Step 1 : Tests (pur `touche_cdp` + pur `parse_press` + intégration Enter)**

`browser.rs` `mod tests` :

```rust
#[test]
fn touche_cdp_connait_les_touches_usuelles() {
    assert_eq!(touche_cdp("Enter"), Some(("Enter", "Enter", 13)));
    assert_eq!(touche_cdp("ArrowDown"), Some(("ArrowDown", "ArrowDown", 40)));
    assert_eq!(touche_cdp("PageUp"), Some(("PageUp", "PageUp", 33)));
    assert_eq!(touche_cdp("Grokk"), None);
}

#[test]
fn press_enter_soumet_un_formulaire() {
    if !navigateur_dispo() {
        eprintln!("aucun navigateur : test press ignoré");
        return;
    }
    // Enter dans le champ soumet le form -> navigation vers /page2 (même contenu).
    let (url, _srv) = servir_page_locale(
        "<!doctype html><title>T</title>\
         <form action=\"page2\"><input aria-label=Q></form>",
    );
    let engine = BrowserEngine::new();
    engine.exec(BrowserCommand::Navigate(url)).unwrap();
    let snap = match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => t,
        _ => panic!("Text"),
    };
    let r = ref_pour(&snap, "Q").expect("ref du champ");
    engine
        .exec(BrowserCommand::Press { key: "Enter".into(), ref_: Some(r) })
        .unwrap();
    // Laisser la navigation se faire, puis vérifier l'URL.
    engine.exec(BrowserCommand::Wait { text: None, ms: Some(400), settle: false }).unwrap();
    match engine.exec(BrowserCommand::Url).unwrap() {
        BrowserReply::Text(u) => assert!(u.contains("page2"), "url après Enter : {u}"),
        _ => panic!("Text"),
    }
    let _ = engine.exec(BrowserCommand::Close);
}
```

> Note : ce test utilise `BrowserCommand::Wait { ms }`, livré en Task 6. Le test compile mais échouera tant que `Wait` n'existe pas. **Ordonnancement** : si l'implémenteur exécute les tasks dans l'ordre, remplacer temporairement cette ligne d'attente par `std::thread::sleep(std::time::Duration::from_millis(400));` puis rétablir `Wait` en Task 6. (Le reviewer verra la version finale.)

`main.rs` `browser_tests` :

```rust
#[test]
fn parse_press_touche_positionnelle_et_ref_option() {
    let a = parse_press(&["Enter".into()]).unwrap();
    assert_eq!(a.key, "Enter");
    assert_eq!(a.ref_, None);
    let b = parse_press(&["Tab".into(), "--ref".into(), "e3".into()]).unwrap();
    assert_eq!(b.key, "Tab");
    assert_eq!(b.ref_, Some("e3".into()));
    assert!(parse_press(&[]).is_err()); // touche manquante
}
```

- [ ] **Step 2 : Lancer → échec (compilation)**

Run: `cargo test -p wimux-server browser 2>&1 | head -10 ; cargo test -p wimux-cli browser 2>&1 | head -10`
Expected: erreurs (`touche_cdp`, `Press`, `parse_press` absents).

- [ ] **Step 3 : `touche_cdp` + variante + bras `dispatch`**

`browser.rs`, fonction pure :

```rust
/// Nom de touche → (key, code, windows_virtual_key_code) pour CDP. `None` si
/// non gérée. Ensemble volontairement restreint (navigation/édition usuelles).
fn touche_cdp(nom: &str) -> Option<(&'static str, &'static str, i64)> {
    Some(match nom {
        "Enter" => ("Enter", "Enter", 13),
        "Tab" => ("Tab", "Tab", 9),
        "Escape" => ("Escape", "Escape", 27),
        "Backspace" => ("Backspace", "Backspace", 8),
        "Delete" => ("Delete", "Delete", 46),
        "ArrowUp" => ("ArrowUp", "ArrowUp", 38),
        "ArrowDown" => ("ArrowDown", "ArrowDown", 40),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37),
        "ArrowRight" => ("ArrowRight", "ArrowRight", 39),
        "Home" => ("Home", "Home", 36),
        "End" => ("End", "End", 35),
        "PageUp" => ("PageUp", "PageUp", 33),
        "PageDown" => ("PageDown", "PageDown", 34),
        _ => return None,
    })
}
```

`enum BrowserCommand`, en fin :

```rust
    Type { ref_: String, text: String },
    /// B2.2 : appuie une touche nommée (optionnellement après focus sur une ref).
    Press { key: String, ref_: Option<String> },
}
```

Bras `dispatch` :

```rust
        BrowserCommand::Press { key, ref_ } => {
            use chromiumoxide::cdp::browser_protocol::input::DispatchKeyEventType;
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            if let Some(r) = &ref_ {
                let bid = backend_id_for(s, r)?;
                focus_backend(&s.page, bid).await?;
            }
            let (k, code, vk) = touche_cdp(&key).ok_or_else(|| {
                format!(
                    "touche inconnue : {key} (gérées : Enter, Tab, Escape, Backspace, \
                     Delete, ArrowUp/Down/Left/Right, Home, End, PageUp, PageDown)"
                )
            })?;
            // Enter émet aussi le caractère de retour ; les touches d'édition non.
            let text = if key == "Enter" { Some("\r") } else { None };
            dispatch_key(&s.page, DispatchKeyEventType::KeyDown, k, code, vk, text, None).await?;
            dispatch_key(&s.page, DispatchKeyEventType::KeyUp, k, code, vk, text, None).await?;
            Ok(BrowserReply::Ok)
        }
```

- [ ] **Step 4 : Protocole + daemon + CLI**

`lib.rs`, fin `ClientMessage` :

```rust
    BrowserType { ref_: String, text: String },
    /// B2.2 : appuie une touche nommée (focus optionnel sur une ref).
    BrowserPress { key: String, ref_: Option<String> },
}
```

`daemon.rs`, après `BrowserType` :

```rust
            ClientMessage::BrowserPress { key, ref_ } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Press { key, ref_ }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

`main.rs` `mod browser` :

```rust
    #[derive(Debug, PartialEq)]
    pub struct PressArgs {
        pub key: String,
        pub ref_: Option<String>,
    }

    /// `<touche> [--ref <eN>]` : la touche est le premier argument non-flag.
    pub fn parse_press(args: &[String]) -> io::Result<PressArgs> {
        let key = args
            .iter()
            .find(|a| !a.starts_with("--"))
            .cloned()
            .ok_or_else(|| io::Error::other("usage : wimux browser press <touche> [--ref <eN>]"))?;
        Ok(PressArgs { key, ref_: flag(args, "--ref") })
    }
```

`cmd_browser` :

```rust
        Some("press") => {
            let a = browser::parse_press(&args[1..])?;
            browser_simple(ClientMessage::BrowserPress { key: a.key, ref_: a.ref_ })
        }
```

- [ ] **Step 5 : Tests → succès**

Run: `cargo test -p wimux-server browser 2>&1 | tail -10 ; cargo test -p wimux-cli browser 2>&1 | tail -10`
Expected: PASS (le test `press_enter_soumet_un_formulaire` avec `thread::sleep` provisoire tant que Task 6 n'est pas faite). fmt + clippy propres.

- [ ] **Step 6 : Commit**

```bash
git add -A
git commit -m "feat(browser): verbe press (table de touches nommees, dispatchKeyEvent)"
```

---

## Task 5 : `scroll` (vers une ref, ou molette d'un delta)

**Files:**
- Modify: `crates/wimux-server/src/browser.rs` (enum ; `dispatch` ; helper `mouse_wheel`)
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs`
- Modify: `crates/wimux-cli/src/main.rs` (`parse_scroll` + `ScrollArgs`)
- Test: `mod tests`

**Interfaces:**
- Consumes : `scroll_into_view`, `backend_id_for` (Task 2).
- Produces :
  - `BrowserCommand::Scroll { ref_: Option<String>, dy: Option<i64> }`.
  - `async fn mouse_wheel(page, dy: f64) -> Result<(), String>`.
  - `ScrollArgs { ref_: Option<String>, dy: Option<i64> }` + `parse_scroll` (xor validé).
  - CLI `wimux browser scroll --ref eN` **ou** `--dy <n>`.

- [ ] **Step 1 : Tests (pur `parse_scroll` xor + intégration `--ref`)**

`main.rs` `browser_tests` :

```rust
#[test]
fn parse_scroll_exige_ref_xor_dy() {
    assert_eq!(parse_scroll(&["--ref".into(), "e5".into()]).unwrap().ref_, Some("e5".into()));
    assert_eq!(parse_scroll(&["--dy".into(), "300".into()]).unwrap().dy, Some(300));
    assert!(parse_scroll(&[]).is_err()); // aucun
    assert!(parse_scroll(&["--ref".into(), "e5".into(), "--dy".into(), "9".into()]).is_err()); // les deux
    assert!(parse_scroll(&["--dy".into(), "abc".into()]).is_err()); // dy non entier
}
```

`browser.rs` `mod tests` :

```rust
#[test]
fn scroll_vers_une_ref_reussit() {
    if !navigateur_dispo() {
        eprintln!("aucun navigateur : test scroll ignoré");
        return;
    }
    // Élément tout en bas d'une page haute.
    let (url, _srv) = servir_page_locale(
        "<!doctype html><title>T</title><div style=height:3000px></div>\
         <button>EnBas</button>",
    );
    let engine = BrowserEngine::new();
    engine.exec(BrowserCommand::Navigate(url)).unwrap();
    let snap = match engine.exec(BrowserCommand::Snapshot).unwrap() {
        BrowserReply::Text(t) => t,
        _ => panic!("Text"),
    };
    let r = ref_pour(&snap, "EnBas").expect("ref du bouton bas");
    assert!(matches!(
        engine.exec(BrowserCommand::Scroll { ref_: Some(r), dy: None }).unwrap(),
        BrowserReply::Ok
    ));
    // Delta molette : ne doit pas échouer non plus.
    assert!(matches!(
        engine.exec(BrowserCommand::Scroll { ref_: None, dy: Some(-500) }).unwrap(),
        BrowserReply::Ok
    ));
    let _ = engine.exec(BrowserCommand::Close);
}
```

- [ ] **Step 2 : Lancer → échec (compilation)**

Run: `cargo test -p wimux-server browser 2>&1 | head -10 ; cargo test -p wimux-cli browser 2>&1 | head -10`
Expected: erreurs (`Scroll`, `parse_scroll` absents).

- [ ] **Step 3 : Helper molette + variante + bras `dispatch`**

`browser.rs` :

```rust
/// Molette verticale au point (0,0) du viewport (défilement du document).
async fn mouse_wheel(page: &chromiumoxide::Page, dy: f64) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType};
    let p = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseWheel)
        .x(0.0)
        .y(0.0)
        .delta_x(0.0)
        .delta_y(dy)
        .build()?;
    page.execute(p).await.map_err(|e| format!("molette : {e}"))?;
    Ok(())
}
```

`enum BrowserCommand`, en fin :

```rust
    Press { key: String, ref_: Option<String> },
    /// B2.2 : défile vers une ref (dans le viewport) OU d'un delta molette (px).
    Scroll { ref_: Option<String>, dy: Option<i64> },
}
```

Bras `dispatch` :

```rust
        BrowserCommand::Scroll { ref_, dy } => {
            let s = sess
                .as_ref()
                .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
            match (ref_, dy) {
                (Some(r), None) => {
                    let bid = backend_id_for(s, &r)?;
                    scroll_into_view(&s.page, bid).await?;
                }
                (None, Some(d)) => {
                    mouse_wheel(&s.page, d as f64).await?;
                }
                _ => return Err("scroll : fournis --ref OU --dy (exactement un)".into()),
            }
            Ok(BrowserReply::Ok)
        }
```

- [ ] **Step 4 : Protocole + daemon + CLI**

`lib.rs`, fin `ClientMessage` :

```rust
    BrowserPress { key: String, ref_: Option<String> },
    /// B2.2 : défile vers une ref ou d'un delta molette.
    BrowserScroll { ref_: Option<String>, dy: Option<i64> },
}
```

`daemon.rs`, après `BrowserPress` :

```rust
            ClientMessage::BrowserScroll { ref_, dy } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Scroll { ref_, dy }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

`main.rs` `mod browser` :

```rust
    #[derive(Debug, PartialEq)]
    pub struct ScrollArgs {
        pub ref_: Option<String>,
        pub dy: Option<i64>,
    }

    /// `--ref <eN>` XOR `--dy <entier>` (exactement un).
    pub fn parse_scroll(args: &[String]) -> io::Result<ScrollArgs> {
        let ref_ = flag(args, "--ref");
        let dy = match flag(args, "--dy") {
            Some(v) => Some(
                v.parse::<i64>()
                    .map_err(|_| io::Error::other("--dy attend un entier"))?,
            ),
            None => None,
        };
        match (&ref_, dy) {
            (Some(_), None) | (None, Some(_)) => Ok(ScrollArgs { ref_, dy }),
            _ => Err(io::Error::other(
                "usage : wimux browser scroll --ref <eN> | --dy <entier>",
            )),
        }
    }
```

`cmd_browser` :

```rust
        Some("scroll") => {
            let a = browser::parse_scroll(&args[1..])?;
            browser_simple(ClientMessage::BrowserScroll { ref_: a.ref_, dy: a.dy })
        }
```

- [ ] **Step 5 : Tests → succès**

Run: `cargo test -p wimux-server browser 2>&1 | tail -10 ; cargo test -p wimux-cli browser 2>&1 | tail -10`
Expected: PASS. fmt + clippy propres.

- [ ] **Step 6 : Commit**

```bash
git add -A
git commit -m "feat(browser): verbe scroll (vers une ref ou delta molette)"
```

---

## Task 6 : `wait` (texte apparaît / délai fixe / stabilisation)

**Files:**
- Modify: `crates/wimux-server/src/browser.rs` (enum ; `dispatch`)
- Modify: `crates/wimux-protocol/src/lib.rs`
- Modify: `crates/wimux-server/src/daemon.rs`
- Modify: `crates/wimux-cli/src/main.rs` (`parse_wait` + `WaitArgs` ; `browser_wait`)
- Test: `mod tests`

**Interfaces:**
- Consumes : `snapshot_nodes`, `render_ax_tree` (Task 1).
- Produces :
  - `BrowserCommand::Wait { text: Option<String>, ms: Option<u64>, settle: bool }`.
  - `WaitArgs { text: Option<String>, ms: Option<u64>, settle: bool }` + `parse_wait` (exactement un mode).
  - CLI `wimux browser wait --text "…" | --ms <n> | --settle` ; helper `browser_wait`.

- [ ] **Step 1 : Tests (pur `parse_wait` + intégration `--text`)**

`main.rs` `browser_tests` :

```rust
#[test]
fn parse_wait_exactement_un_mode() {
    assert_eq!(parse_wait(&["--text".into(), "Merci".into()]).unwrap().text, Some("Merci".into()));
    assert_eq!(parse_wait(&["--ms".into(), "500".into()]).unwrap().ms, Some(500));
    assert!(parse_wait(&["--settle".into()]).unwrap().settle);
    assert!(parse_wait(&[]).is_err()); // aucun
    assert!(parse_wait(&["--text".into(), "x".into(), "--settle".into()]).is_err()); // deux
    assert!(parse_wait(&["--ms".into(), "abc".into()]).is_err()); // ms non entier
}
```

`browser.rs` `mod tests` :

```rust
#[test]
fn wait_text_attend_un_contenu_differe() {
    if !navigateur_dispo() {
        eprintln!("aucun navigateur : test wait ignoré");
        return;
    }
    let (url, _srv) = servir_page_locale(
        "<!doctype html><title>T</title><p id=r>patiente</p>\
         <script>setTimeout(()=>{document.getElementById('r').textContent='PRÊT'},300)</script>",
    );
    let engine = BrowserEngine::new();
    engine.exec(BrowserCommand::Navigate(url)).unwrap();
    // Apparaît sous ~300ms : doit réussir avant le timeout de 10s.
    match engine
        .exec(BrowserCommand::Wait { text: Some("PRÊT".into()), ms: None, settle: false })
        .unwrap()
    {
        BrowserReply::Text(t) => assert!(t.contains("PRÊT"), "wait a renvoyé : {t}"),
        _ => panic!("Text attendu"),
    }
    // Un texte jamais présent doit timeouter (erreur).
    let err = engine
        .exec(BrowserCommand::Wait { text: Some("JAMAIS".into()), ms: None, settle: false })
        .unwrap_err();
    assert!(err.contains("timeout") || err.contains("non trouvé"), "err : {err}");
    let _ = engine.exec(BrowserCommand::Close);
}
```

> Astuce test : pour ne pas attendre 10 s sur le cas « JAMAIS », l'implémenteur peut réduire ce timeout via une page où le texte apparaît, ou accepter l'attente de 10 s (le test reste correct, juste lent). Garder le timeout de prod à 10 s.

- [ ] **Step 2 : Lancer → échec (compilation)**

Run: `cargo test -p wimux-server browser 2>&1 | head -10 ; cargo test -p wimux-cli browser 2>&1 | head -10`
Expected: erreurs (`Wait`, `parse_wait` absents).

- [ ] **Step 3 : Variante + bras `dispatch`**

`enum BrowserCommand`, en fin :

```rust
    Scroll { ref_: Option<String>, dy: Option<i64> },
    /// B2.2 : attend qu'un texte apparaisse, un délai fixe, ou la stabilisation.
    Wait { text: Option<String>, ms: Option<u64>, settle: bool },
}
```

Bras `dispatch` :

```rust
        BrowserCommand::Wait { text, ms, settle } => {
            use std::time::Duration;
            if let Some(t) = text {
                let s = sess
                    .as_ref()
                    .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
                let texte = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let nodes = snapshot_nodes(&s.page).await?;
                        let (texte, _) = render_ax_tree(&nodes);
                        if texte.contains(t.as_str()) {
                            return Ok::<String, String>(texte);
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                })
                .await
                .map_err(|_| format!("wait --text : « {t} » non trouvé (timeout 10 s)"))??;
                Ok(BrowserReply::Text(texte))
            } else if let Some(n) = ms {
                tokio::time::sleep(Duration::from_millis(n)).await;
                Ok(BrowserReply::Ok)
            } else if settle {
                let s = sess
                    .as_ref()
                    .ok_or_else(|| "aucun navigateur : lance-le ou navigue d'abord".to_string())?;
                tokio::time::timeout(Duration::from_secs(30), s.page.wait_for_navigation())
                    .await
                    .map_err(|_| "wait --settle : timeout 30 s".to_string())?
                    .map_err(|e| format!("wait --settle : {e}"))?;
                Ok(BrowserReply::Ok)
            } else {
                Err("wait : fournis --text, --ms ou --settle".into())
            }
        }
```

Puis rétablir, dans `press_enter_soumet_un_formulaire` (Task 4), la ligne d'attente en `BrowserCommand::Wait { text: None, ms: Some(400), settle: false }` si un `thread::sleep` provisoire y avait été mis.

- [ ] **Step 4 : Protocole + daemon + CLI**

`lib.rs`, fin `ClientMessage` :

```rust
    BrowserScroll { ref_: Option<String>, dy: Option<i64> },
    /// B2.2 : attend un texte, un délai, ou la stabilisation du chargement.
    BrowserWait { text: Option<String>, ms: Option<u64>, settle: bool },
}
```

`daemon.rs`, après `BrowserScroll` :

```rust
            ClientMessage::BrowserWait { text, ms, settle } => {
                let reply = browser_reply(
                    server
                        .browser
                        .exec(crate::browser::BrowserCommand::Wait { text, ms, settle }),
                );
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

`main.rs` `mod browser` :

```rust
    #[derive(Debug, PartialEq)]
    pub struct WaitArgs {
        pub text: Option<String>,
        pub ms: Option<u64>,
        pub settle: bool,
    }

    /// Exactement un mode : `--text <s>` | `--ms <n>` | `--settle`.
    pub fn parse_wait(args: &[String]) -> io::Result<WaitArgs> {
        let text = flag(args, "--text");
        let ms = match flag(args, "--ms") {
            Some(v) => Some(
                v.parse::<u64>()
                    .map_err(|_| io::Error::other("--ms attend un entier"))?,
            ),
            None => None,
        };
        let settle = args.iter().any(|a| a == "--settle");
        let n = text.is_some() as u8 + ms.is_some() as u8 + settle as u8;
        if n != 1 {
            return Err(io::Error::other(
                "usage : wimux browser wait --text <s> | --ms <n> | --settle",
            ));
        }
        Ok(WaitArgs { text, ms, settle })
    }
```

`cmd_browser` :

```rust
        Some("wait") => {
            let a = browser::parse_wait(&args[1..])?;
            browser_wait(ClientMessage::BrowserWait { text: a.text, ms: a.ms, settle: a.settle })
        }
```

Ajouter le helper `browser_wait` (près de `browser_text`) :

```rust
/// Attente : `Ok` (délai/settle) ou un texte (mode --text).
fn browser_wait(msg: ClientMessage) -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &msg)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Ok => Ok(()),
        ServerMessage::BrowserText(t) => {
            println!("{t}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}
```

- [ ] **Step 5 : Tests → succès**

Run: `cargo test -p wimux-server browser 2>&1 | tail -15 ; cargo test -p wimux-cli browser 2>&1 | tail -10`
Expected: PASS. fmt + clippy propres.

- [ ] **Step 6 : Commit**

```bash
git add -A
git commit -m "feat(browser): verbe wait (texte apparait / delai / stabilisation)"
```

---

## Task 7 : Aide CLI, README et revue de câblage

Finalise la surface utilisateur et documente les deux « browser » + le fil rouge sécurité.

**Files:**
- Modify: `crates/wimux-cli/src/main.rs` (ligne d'aide `browser <sous-cmd>` ~1356)
- Modify: `crates/wimux-gui/README.md` (ou le README où B2.1 documente le navigateur pilotable) — section actions.

**Interfaces:** aucune nouvelle ; vérifie la cohérence de bout en bout.

- [ ] **Step 1 : Mettre à jour la ligne d'aide de `browser`**

Dans le texte d'aide global (`cmd_help`/constante d'usage), remplacer la ligne `browser` par :

```
             browser <sous-cmd>  Navigateur : open (volet B1) | launch/close/status/navigate/url/snapshot/screenshot (moteur pilotable) | click/type/press/scroll/wait (actions B2.2)\n    \
```

- [ ] **Step 2 : Documenter les verbes d'action dans le README**

Ajouter une sous-section « Actions (B2.2) » près de la doc B2.1 du moteur pilotable, décrivant : le modèle de refs (`snapshot` → `[ref=eN]` → agir sur `--ref eN` ; refs vidées à la navigation), chaque verbe avec un exemple, et **le rappel sécurité** : ne jamais saisir d'identifiants/données financières via `type` ; confirmer les actions irréversibles/sortantes (soumission via `click`/`press Enter`). Exemple minimal :

```
wimux browser navigate --url https://example.com/login
wimux browser snapshot                 # repère [ref=e3] textbox "Email", [ref=e7] button "Se connecter"
wimux browser type --ref e3 --text "moi@example.com"
wimux browser press Tab
wimux browser wait --settle
wimux browser click --ref e7           # action sortante : à confirmer avec l'utilisateur
```

- [ ] **Step 3 : Revue de câblage — grep de cohérence**

Vérifier que les 5 verbes sont routés partout :

Run:
```bash
grep -c "BrowserClick\|BrowserType\|BrowserPress\|BrowserScroll\|BrowserWait" crates/wimux-protocol/src/lib.rs crates/wimux-server/src/daemon.rs crates/wimux-cli/src/main.rs
```
Expected: chaque fichier référence les 5 (protocole : 5 définitions ; daemon : 5 bras ; CLI : 5 routages). Vérifier visuellement l'ordre d'ajout en FIN d'enum dans `lib.rs` et `BrowserCommand`.

- [ ] **Step 4 : Portes qualité complètes**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets 2>&1 | grep -E "^warning|^error" || echo "clippy clean"
cargo test --workspace 2>&1 | grep -E "test result:"
```
Expected: fmt clean, clippy clean, tous les tests verts (les tests d'intégration navigateur s'exécutent si Chrome/Edge présent, sinon « ignoré »).

- [ ] **Step 5 : Test manuel de bout en bout (daemon réel)**

Rebâtir le daemon release et le redémarrer (piège daemon persistant), puis dérouler un scénario réel sur une page de formulaire locale :

```bash
# 1) reconstruire + redémarrer le daemon (cf. mémoire « piège daemon »)
# 2) python -m http.server sur un dossier avec un form, puis :
wimux browser navigate --url http://localhost:8000/form.html
wimux browser snapshot           # relever une ref de champ et une ref de bouton
wimux browser type --ref e2 --text "Bonjour"
wimux browser press Tab
wimux browser scroll --ref e5
wimux browser wait --ms 200
wimux browser click --ref e5
wimux browser snapshot           # vérifier l'effet
wimux browser close
```
Attendu : chaque commande répond sans erreur ; le snapshot final reflète les actions.

- [ ] **Step 6 : Commit**

```bash
git add -A
git commit -m "docs(browser): aide CLI + README des actions B2.2 (refs, verbes, securite)"
```

---

## Self-Review (rempli)

**Couverture de la spec :**
- Ciblage par ref (`[ref=eN]`, table serveur, vidée à la navigation, erreur sur ref obsolète) → Task 1 (+ `backend_id_for` Task 2).
- `click`/`type`/`press`/`scroll`/`wait` → Tasks 2/3/4/5/6.
- `select` différé à B2.3 → hors périmètre (documenté).
- Protocole en fin d'enum, réponses réutilisées → chaque task (Steps 4) + Global Constraints.
- Zéro JS de page → aucun `Runtime.*` dans le plan ; uniquement `DOM.*`/`Input.*`.
- Sécurité (moteur mécanisme, politique en B2.4, contenu non fiable neutralisé) → Global Constraints + Task 7 README.
- Tests purs (rendu+refs, parseurs, table de touches) et d'intégration (click/type/press/scroll/wait) → présents.

**Placeholders :** aucun « TBD/TODO » ; tout le code est fourni. Les deux notes d'ordonnancement (test `press` utilisant `Wait` de Task 6 ; timeout lent du cas « JAMAIS ») sont des indications d'exécution explicites, pas des trous.

**Cohérence des types :** `ref_` partout ; `BrowserCommand`/`ClientMessage` à variantes identiques ; helpers (`backend_id_for`, `focus_backend`, `dispatch_key`, `scroll_into_view`, `element_center`, `snapshot_nodes`) définis avant leurs consommateurs ; signatures CDP conformes à la référence vérifiée.

---

## Execution Handoff
(à présenter après sauvegarde)
