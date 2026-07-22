# B2.2 — Actions de pilotage du navigateur (click/type/press/scroll/wait)

**Date :** 2026-07-22
**Sous-projet :** B2.2 (2ᵉ tranche de B2 « browser automation façon cmux »)
**Base :** B2.1 (moteur navigateur CDP lecture seule, fusionné dans `main` au commit `34b2965`)

## Contexte et décomposition

B2 (pilotage du navigateur par Claude, façon `cmux browser`) est découpé en :

- **B2.1 — FAIT** : moteur Chromium externe piloté par CDP (lecture seule) — `launch`/`close`/`status`/`navigate`/`url`/`snapshot` (arbre d'accessibilité) / `screenshot`. Pont daemon-synchrone → thread tokio, profil isolé par lancement, garde URL http(s), **zéro JS de page** (lectures CDP natives).
- **B2.2 — CE DOCUMENT** : les **actions** — `click`, `type`, `press`, `scroll`, `wait`. Toutes CDP natives, **zéro JS**.
- **B2.3 — à venir** : puissance/état — `select` (nécessite un helper JS), `eval`/`addscript`, `cookies`/`storage`/`state`, `tabs`/`console`/`errors`.
- **B2.4 — à venir** : skill de pilotage + durcissement sécurité (clarifie l'usage des deux « browser » : `open` volet iframe de B1 vs le moteur pilotable).

`select` a été **explicitement différé à B2.3** : le menu déroulant d'un `<select>` natif est rendu par l'OS (hors DOM/AX), donc inatteignable en CDP natif ; la seule voie fiable (Puppeteer) passe par un bout de JS, ce qui appartient à B2.3.

## Décision centrale : ciblage par **référence** issue du snapshot

Le `snapshot` de B2.1 renvoie un arbre d'accessibilité textuel **sans identifiants d'éléments**. Pour qu'une action désigne un élément, on enrichit le snapshot de **jetons de référence stables**, plutôt que d'exiger des sélecteurs CSS (modèle cmux) que Claude devrait deviner à partir d'un arbre AX qui n'en donne aucun.

Modèle retenu (agent-first, aligné sur les outils navigateur de Claude) :

```
[ref=e1] button "Continuer" [focusable]
[ref=e2] textbox "Email"
[ref=e3] link "Aide"
```

- Chaque nœud **effectivement affiché** (non décoratif) reçoit un jeton séquentiel `eN` (N croissant par snapshot).
- Le serveur tient une table `refs: HashMap<String, i64>` (ref → `backend_node_id` CDP), **reconstruite à chaque `snapshot`** et **vidée à chaque `navigate`** (après navigation le DOM ciblé n'existe plus).
- Agir sur une ref absente ou obsolète → **erreur explicite** `ref inconnue (e5) — refais un snapshot`. Jamais de clic à l'aveugle.

### Résolution ref → action (100 % CDP natif, aucun `Runtime.evaluate`)

`backend_node_id` sert de clé à :

- `DOM.getBoxModel` (ou `DOM.getContentQuads`) → coordonnées → centre de l'élément ;
- `DOM.focus` → mise au focus ;
- `DOM.scrollIntoViewIfNeeded` → amener dans le viewport.

Puis les événements natifs `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` / `Input.insertText`. Rien ne passe par du JavaScript.

## Impacts sur les types de B2.1

- **`AxSnapshotNode`** gagne un champ `backend_node_id: Option<i64>`, capté dans `map_ax_node` depuis `AxNode.backend_dom_node_id` (champ CDP `Option<BackendNodeId>`). Champ **ajouté en fin de struct**.
- **`render_ax_tree`** change de signature : il renvoie désormais `(String, Vec<(String, i64)>)` — le texte **et** la table des refs affichées (ref → backend id), au lieu d'un simple `String`. Reste **pur et testable sans navigateur**. Changement de signature délibéré (met à jour l'unique appelant `dispatch::Snapshot` et les tests de rendu).
  - Le préfixe `[ref=eN] ` est émis **uniquement pour les nœuds affichés** : ainsi « ce que Claude voit = ce que Claude peut cibler ». Les nœuds décoratifs élagués ne consomment pas de ref.
  - La numérotation suit l'ordre d'affichage (parcours préfixe), pour que `e1, e2, …` se lisent de haut en bas.
- **`Session`** gagne `refs: HashMap<String, i64>` : renseignée quand un `Snapshot` est produit, vidée dans `Navigate`.

## Les verbes

Tous exposés en CLI sous `wimux browser <verbe> …`. Réponse `Ok` (→ `ServerMessage::Ok`) sauf `wait --text` qui renvoie un texte (→ `BrowserText`) ; erreur → `Error` avec raison.

| Verbe | Signature CLI | Mécanique CDP native |
|---|---|---|
| `click` | `click --ref e5` | `scrollIntoViewIfNeeded` → `getBoxModel` → centre → `dispatchMouseEvent` (mousePressed + mouseReleased, bouton gauche) |
| `type` | `type --ref e2 --text "…"` | `DOM.focus` → `dispatchKeyEvent` Ctrl+A (sélectionne tout) → `Input.insertText` (remplace la sélection ; gère l'unicode) |
| `press` | `press <touche> [--ref e2]` | (`DOM.focus` si `--ref`) → `dispatchKeyEvent` keyDown + keyUp ; **table de touches** nommées |
| `scroll` | `scroll --ref e9` **ou** `scroll --dy <n>` | `--ref` → `scrollIntoViewIfNeeded` ; `--dy` → `dispatchMouseEvent` type `mouseWheel` avec `deltaY` (positif = vers le bas) |
| `wait` | `wait --text "…"` \| `wait --ms <n>` \| `wait --settle` | voir ci-dessous |

### Détails

- **`type`** applique une sémantique **remplacer** (vide puis saisit) : `focus` → Ctrl+A → `insertText`. `insertText` remplace la sélection courante, donc Ctrl+A + insertText = tout remplacer, sans suppression explicite.
- **`press`** — table de correspondance nom → paramètres CDP (`key`, `code`, `windowsVirtualKeyCode`, `text` le cas échéant) pour au moins : `Enter`, `Tab`, `Escape`, `Backspace`, `Delete`, `ArrowUp`/`ArrowDown`/`ArrowLeft`/`ArrowRight`, `Home`, `End`, `PageUp`, `PageDown`. Touche inconnue → erreur explicite listant les touches gérées. Sans `--ref`, la touche va à l'élément actuellement au focus.
- **`scroll`** — exactement un mode requis (`--ref` **xor** `--dy`) ; fournir les deux ou aucun → erreur d'usage.
- **`wait`** — exactement un mode requis parmi :
  - `--text "<s>"` : **re-poll du snapshot AX** (mécanique B2.1, aucun JS) à intervalle court jusqu'à ce qu'un nom de nœud **contienne** `<s>` ; **timeout** (défaut 10 s) → erreur. Succès → renvoie le texte trouvé.
  - `--ms <n>` : délai fixe de `n` millisecondes (borne supérieure raisonnable, ex. 60 000).
  - `--settle` : attend la stabilisation du chargement (événement `load`/navigation), avec le **même timeout de 30 s** que `navigate`.

## Protocole (postcard — ajouts en FIN d'enum)

Nouveaux `ClientMessage`, ajoutés après les variantes B2.1 :

- `BrowserClick { ref_: String }`
- `BrowserType { ref_: String, text: String }`
- `BrowserPress { key: String, ref_: Option<String> }`
- `BrowserScroll { ref_: Option<String>, dy: Option<i64> }`
- `BrowserWait { text: Option<String>, ms: Option<u64>, settle: bool }`

Aucun nouveau `ServerMessage` : les réponses réutilisent `Ok` / `BrowserText` / `Error` de B2.1. Nouvelles variantes de `BrowserCommand` (interne serveur) et bras de `dispatch` correspondants.

## Sécurité (fil rouge B2)

- Le **moteur reste pur mécanisme** : il exécute l'action demandée sans jugement. La **politique** vit ailleurs :
  - **Mes règles d'opération + le skill B2.4** : ne jamais saisir d'identifiants / données financières via `type` ; confirmer les actions **irréversibles ou sortantes** (un `click` de soumission, un `press Enter` qui valide un formulaire) avant de les exécuter.
- **Nouveauté vs B2.1** : ces verbes **écrivent** dans la page (frappe, clic, soumission). C'est précisément la raison d'être du durcissement prévu en B2.4 ; B2.2 le documente sans l'implémenter.
- Le **contenu de page reste donnée non fiable** : les noms AX servant à construire les refs et le texte de `wait --text` sont déjà neutralisés côté serveur (F2 de B2.1, `nettoyer` : caractères de contrôle → espace). Aucune donnée de page ne pilote le flot de contrôle.
- **Toujours zéro JS de page** en B2.2 : aucune de ces actions n'introduit `Runtime.evaluate`/`callFunctionOn` (c'est `select`/`eval` de B2.3).

## Tests

- **Purs (sans navigateur)** :
  - rendu du snapshot avec préfixes `[ref=eN]` + table ref→backend id renvoyée (dont : nœuds décoratifs non numérotés, numérotation en ordre d'affichage) ;
  - parsing des flags CLI de chaque verbe (xor `--ref`/`--dy` pour scroll ; xor des modes de wait ; touche inconnue) ;
  - table de touches (nom connu → params ; inconnu → erreur).
- **Intégration (page HTML locale, comme B2.1)** — conditionnés à la présence d'un navigateur :
  - `type` dans un `<input>` puis relire sa valeur via un `snapshot` (ou l'attribut) ;
  - `click` sur un bouton qui modifie un texte observable, vérifié par snapshot ;
  - `press Enter` soumet un formulaire (navigation observable) ;
  - `scroll --ref` amène un élément en bas de page dans le viewport (box model dans le viewport) ;
  - `wait --text` réussit sur un contenu injecté après un court délai, et **timeoute** proprement si absent.

## Ce qui n'est PAS dans B2.2 (rappels de périmètre)

- `select`, `eval`, `addscript`, `cookies`/`storage`/`state`, `tabs`/`console`/`errors`, `frame`/`dialog`/`download` → B2.3.
- Le skill de pilotage et le durcissement sécurité → B2.4.
- Les requêtes `find`/`get`/`is` de cmux : jugées redondantes avec le snapshot (qui liste déjà rôles, noms et états avec refs) ; non planifiées, à rediscuter si un besoin réel émerge.
