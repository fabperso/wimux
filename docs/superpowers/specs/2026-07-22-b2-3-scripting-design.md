# B2.3 — Scripting du navigateur (eval / select / addscript)

**Date :** 2026-07-22
**Sous-projet :** B2.3 (3ᵉ tranche de B2 « browser automation façon cmux »)
**Base :** B2.2 (actions par référence, fusionné dans `main` au commit `4f0857b`)

## Contexte et décomposition

B2 (pilotage du navigateur par Claude) est découpé en tranches :

- **B2.1 — FAIT** : moteur Chromium externe piloté par CDP (lecture seule) — launch/close/status/navigate/url/snapshot (arbre AX) / screenshot. **Headless par défaut** (clavier CDP fiable ; vitrine via `set browser-headless off`).
- **B2.2 — FAIT** : actions par **référence** — click/type/press/scroll/wait, ciblées par des jetons `[ref=eN]` du snapshot. **Zéro JS de page.**
- **B2.3 — CE DOCUMENT** : la famille **scripting** — `eval`, `select`, `addscript`. C'est le **renversement assumé de la posture « zéro JS de page »**.
- **Reste (B2.4+)** : état (cookies/storage), diagnostics (console/errors), multi-onglets (tabs), **et le skill de pilotage + durcissement sécurité**.

Le fourre-tout « B2.3 » d'origine (scripting + état + diagnostics + tabs) mélangeait quatre préoccupations indépendantes ; B2.3 est resserré sur le **scripting** — cohérent (tout sur `page.evaluate*`), à forte valeur (l'escape hatch), et il concentre la décision de sécurité centrale de tout B2. Le reste est reporté (état/diagnostics/tabs sont des chantiers distincts — le multi-onglets refond `Session` qui n'a qu'une page ; console/errors exige de bufferiser des événements CDP).

## Décision centrale : introduire l'exécution de JavaScript

B2.1 et B2.2 ont délibérément banni tout JS de page (lectures et actions 100 % CDP natives). B2.3 introduit `eval`/`addscript` : **Claude peut exécuter du JavaScript arbitraire** dans le navigateur d'automatisation. C'est un choix explicite (validé) : `eval` est la capacité la plus puissante du pilotage (lire/extraire n'importe quel état, interagir avec des widgets complexes, appeler des API de la page). Voir la section Sécurité pour le cadrage.

## Les verbes

Tous exposés en CLI sous `wimux browser <verbe> …`.

### `eval` — évaluer une expression JavaScript

```
wimux browser eval "<expression js>"
```

- Exécute l'expression dans le contexte de la page via `page.evaluate_expression` (CDP `Runtime.evaluate`), avec `await_promise = true` et `return_by_value = true`.
- **Attend les promesses** : `eval "(async () => { const r = await fetch('/api'); return r.status })()"` renvoie le statut résolu.
- **Renvoie le résultat sérialisé en JSON** (`EvaluationResult.value()` = `serde_json::Value`, réimprimé compact). `undefined` → `null`.
- **Multi-instructions** : l'appelant enveloppe dans une IIFE — `eval "(() => { const x = 1; return x + 2 })()"`.
- **Erreur** : une exception JS (ou un rejet de promesse) → `Error` avec le message JS.
- N'invalide **pas** les refs du dernier snapshot (aucune navigation) ; mais si l'`eval` mute le DOM, Claude re-snapshote pour agir sur les nouveaux éléments.

### `select` — choisir une option d'un menu déroulant

```
wimux browser select --ref eN --value "<valeur ou libellé>"
```

Le verbe promis puis différé de B2.2 (impossible en CDP natif : le menu d'un `<select>` natif est rendu par l'OS). Mécanique :

1. Résoudre `ref → backend_node_id` (table B2.2 ; erreur explicite `ref inconnue` si absente/périmée).
2. `DOM.resolveNode { backendNodeId }` → `RemoteObject.objectId`.
3. `Runtime.callFunctionOn { objectId, functionDeclaration, arguments: [value] }` avec un **helper JS figé (le nôtre, pas du contenu de page)** : pose `el.value = v` ; si aucune `<option>` n'a cette `value`, retombe sur une correspondance par **texte visible** (`el.options[i].text`) ; puis `el.dispatchEvent(new Event('change', { bubbles: true }))`. Renvoie un booléen « une option a-t-elle été sélectionnée ? ».
4. Erreur explicite si l'élément n'est pas un `<select>`, ou si aucune option ne correspond (ni par valeur ni par texte).

C'est un **helper JS interne appliqué à un nœud résolu** — la nuance actée en B2.2 : la frontière « zéro JS » devient « zéro JS *de page* ».

### `addscript` — injecter un script persistant

```
wimux browser addscript "<js>"
```

- `page.evaluate_on_new_document` (CDP `Page.addScriptToEvaluateOnNewDocument`) : le script s'exécute au **début de chaque futur** chargement de document, avant le JS de la page (utile pour instrumenter/patcher : stubber une API, poser un observateur, neutraliser un anti-bot bénin).
- Renvoie l'**identifiant CDP** du script (`ScriptIdentifier`, imprimé comme texte). Le retrait d'un script (`removeScriptToEvaluateOnNewDocument`) est **YAGNI** pour B2.3.

## Sécurité — le fil rouge amplifié

- **Le JS de `eval`/`select`/`addscript` est écrit par l'OPÉRATEUR (Claude / l'appelant CLI), pas par la page.** Ce n'est donc pas un vecteur d'injection *par la page* dans le moteur ; celui-ci reste **pur mécanisme**.
- **Risque nouveau et central — la boucle d'injection de prompt.** Claude lit du contenu de page **non fiable** via `snapshot` ; si ce contenu le manipule pour qu'il `eval` du JS malveillant (exfiltration de `document.cookie` / `localStorage`, requête sortante, soumission de formulaire), le moteur exécutera docilement. **Règle B2.3 : le contenu de page ne doit JAMAIS dicter quel JavaScript évaluer.** Le moteur ne peut pas l'imposer — c'est une contrainte sur l'opérateur, à **durcir dans le skill B2.4**.
- **Règles d'opération maintenues** : ne jamais saisir/exfiltrer d'identifiants ou de données financières ; confirmer les actions **irréversibles ou sortantes** qu'un `eval` pourrait déclencher (un `fetch` POST, un `form.submit()`).
- **Valve de défense en profondeur — reportée à B2.4** : une directive de config `browser-eval off` interdirait `eval`/`select`/`addscript` au niveau moteur (pour un déploiement qui veut un navigateur pilotable strictement sans JS). Hors périmètre B2.3 (documenté comme item B2.4).
- Le contenu **renvoyé** par `eval` (valeur JSON) et par `addscript` (identifiant) reste une donnée ; il est imprimé tel quel côté CLI. La sortie `eval` peut contenir du contenu de page — elle est donnée non fiable comme le snapshot (pas d'exécution, pas de pilotage de flot).

## Protocole (postcard — ajouts en FIN d'enum)

Nouveaux `ClientMessage`, ajoutés après les variantes B2.2 :

- `BrowserEval { js: String }`
- `BrowserSelect { ref_: String, value: String }`
- `BrowserAddScript { js: String }`

Réponses réutilisées de B2.1/B2.2 : `eval` et `addscript` → `BrowserText` (JSON du résultat / identifiant de script) ; `select` → `Ok` ; erreurs → `Error`. **Aucun nouveau `ServerMessage`.** Nouvelles variantes de `BrowserCommand` (interne serveur) et bras de `dispatch`.

## Moteur (implémentation)

- **`eval`** : `EvaluateParams::builder().expression(js).await_promise(true).return_by_value(true).build()` → `page.evaluate_expression` → `EvaluationResult` → `.value()` (`Option<&serde_json::Value>`) → `serde_json::to_string` (ou `"null"`). Erreur CDP/exception → `Error`.
- **`select`** : `DOM.resolveNode { backend_node_id }` → `objectId` ; `Runtime.callFunctionOn` avec le `functionDeclaration` figé + `arguments: [{ value: <valeur> }]` + `return_by_value` ; interpréter le booléen ; nettoyer l'`objectId` (best-effort). Réutilise `backend_id_for` (B2.2).
- **`addscript`** : `page.evaluate_on_new_document(js)` → `ScriptIdentifier` → texte.
- Ces trois verbes **introduisent `Runtime.evaluate`/`callFunctionOn`** dans le moteur — c'est précisément le périmètre de B2.3.

## Tests

- **Purs (sans navigateur)** : parse CLI des trois verbes (`eval` exige une expression ; `select` exige `--ref` et `--value` ; `addscript` exige un script).
- **Intégration (headless, page locale)** — conditionnés à la présence d'un navigateur :
  - `eval "1 + 2"` → `3` ; `eval "document.title"` → le titre de la page ;
  - `eval` d'une promesse : `eval "Promise.resolve(42)"` → `42` (prouve `await_promise`) ;
  - `eval` d'une expression fautive (`eval "definitivement.pas.defini"`) → `Error` ;
  - `select` sur un `<select>` à 3 options : par **valeur** puis par **texte visible**, vérifié en relisant `el.value` via `eval` ; option inexistante → `Error` ;
  - `addscript "window.__wimux = 1"` puis `navigate` puis `eval "window.__wimux"` → `1` (prouve l'exécution au chargement suivant).

## Ce qui n'est PAS dans B2.3 (rappels de périmètre)

- `cookies`, `storage`/`state`, `console`/`errors`, `tabs`, `frame`/`dialog`/`download` → tranches B2.x ultérieures.
- Le skill de pilotage + le durcissement sécurité (dont la valve `browser-eval off`) → B2.4.
- Le retrait d'un script d'`addscript` (`removeScriptToEvaluateOnNewDocument`) → YAGNI, non planifié.
