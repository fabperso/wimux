# Design — wimux B1 : volet navigateur intégré

- **Date** : 2026-07-21
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : le **navigateur intégré**, dernier élément nommé de la feuille de route GUI d'origine
- **Prérequis** : GUI G1→G4 + série W + multi-agents M1→M4 + A1 faits et fusionnés dans `main`.

## Décomposition : B1 puis B2

Le navigateur intégré est **deux sous-systèmes indépendants**, décomposés en conséquence :

- **B1** *(ce document)* — le **volet navigateur** lui-même : visible, navigable à la
  main, composable avec les volets terminal. A de la valeur seul (prévisualiser un
  serveur de dev à côté de son terminal).
- **B2** — l'**automatisation par l'agent** : les verbes que Claude utilisera pour
  piloter et lire la page. N'a aucun sens sans B1.

## Ce que fait cmux — vérifié

Décision prise **après lecture de la source primaire** (`cmux.com/fr/docs/browser-automation`),
et non par extrapolation :

- cmux expose un groupe `cmux browser [surface:N] <sous-commande>` d'environ
  **40 sous-commandes** : navigation (`open`, `open-split`, `navigate`,
  `back/forward/reload`, `url`, `devtools`, `zoom`), attente
  (`wait --selector/--text/--function`), actions DOM (`click`, `type`, `fill`,
  `press`, `select`, `hover`, `check`, `scroll`), inspection
  (`snapshot --interactive --compact`, `screenshot --out`,
  `get title/url/text/html/value/attr`, `is visible/enabled`, `find`), JavaScript
  (`eval`, `addinitscript`), état (`cookies`, `storage`, `state save/load`), plus
  onglets/console/erreurs/dialogues/téléchargements.
- Les surfaces navigateur sont créées par `open`/`open-split` et ciblées par
  `surface:N`. L'agent lit la page via snapshot structuré, capture PNG ou getters ;
  **pas de références réutilisables** entre commandes (les sélecteurs sont réémis).
- La techno sous-jacente n'est pas documentée.

C'est le périmètre de **B2**. B1 ne vise que la surface manuelle.

## Le point dur, propre à wimux

Chez wimux, l'arbre de volets appartient au **serveur** : le daemon possède les
volets (ConPTY) et la GUI ne fait que rendre un `LayoutNode`. Un volet navigateur
est pourtant un widget **côté GUI**. Il fallait trancher où il vit.

L'élément décisif est venu de B2 : **la CLI parle au daemon, pas à la GUI**. Si le
navigateur n'existait que côté GUI, un futur `wimux browser click …` lancé depuis un
volet Claude n'aurait **aucun chemin** pour l'atteindre — exactement le problème de
connexions séparées rencontré en A1.5. D'où le choix d'un volet **first-class côté
serveur**.

## Décisions (validées lors du brainstorm)

| Sujet | Décision |
|---|---|
| Découpage | **B1 (volet) puis B2 (automatisation)**, chacun avec sa spec/plan/revue |
| Architecture | Volet **first-class dans l'arbre serveur** : le daemon possède id + URL + historique (sans processus), la GUI rend. Gains : le volet **survit au redémarrage de la GUI**, et B2 aura une identité à cibler |
| Rendu | **`<iframe>`** dans la webview existante |
| Périmètre manuel | Barre d'URL + **recharger** + **précédent/suivant** (sur notre pile d'URL). **Zoom et devtools écartés** |
| Table des volets | **Énumération** (`Term | Web`) dans la table existante de `Window`, pas de table parallèle |
| Verbe CLI | **Inclus dans B1** (`wimux browser open`) |

### Pourquoi l'iframe plutôt que la webview enfant Tauri

Tauri v2 sait créer des webviews enfants, mais **derrière un drapeau Cargo
`unstable`**, fonctionnalité explicitement inachevée. Les bugs rapportés tombent
sur notre motif exact : le contenu **cesse de se redimensionner** après plusieurs
redimensionnements de fenêtre ([#10131](https://github.com/tauri-apps/tauri/issues/10131)),
positionnement cassé ([#10420](https://github.com/tauri-apps/tauri/issues/10420)),
sites externes non affichés ([#10011](https://github.com/tauri-apps/tauri/issues/10011)).
Or nos volets redimensionnent en permanence (découpe, glissement de ratio, reflow).

L'`<iframe>` est un simple élément du DOM : il se positionne et se redimensionne
**parfaitement** dans l'arbre flex existant, sans aucune synchronisation de
coordonnées, et sans dépendance instable. Le cas d'usage nommé (prévisualiser un
serveur de dev **local**) fonctionne, et l'automatisation B2 restera possible en
**même origine**.

### Pourquoi zoom et devtools sont écartés

- **DevTools : non livrable.** Il n'existe aucun moyen d'ouvrir les devtools *d'un
  iframe* depuis la page hôte. Tauri sait ouvrir celles de sa webview principale —
  ce qui montrerait le DOM de wimux, avec l'iframe en boîte noire. Ce serait un
  bouton qui ment sur ce qu'il fait.
- **Zoom : dégradé.** Faisable en `transform: scale()`, mais c'est une mise à
  l'échelle visuelle **sans remise en page**, et le facteur d'échelle fausserait
  les coordonnées de clic de B2. Écarté au profit d'un périmètre honnête.

## Serveur : le modèle

- Nouveau `WebPane { id, url, history: Vec<String>, cursor: usize }` — état pur,
  **aucun processus**.
- `Window` héberge désormais deux natures de feuille. On remplace la valeur de la
  table existante par une **énumération `PaneSlot`** (`Term(Arc<Pane>)` |
  `Web(Arc<WebPane>)`) plutôt que d'ajouter une table parallèle : le compilateur
  recense alors **tous** les sites d'appel (`reflow`, `render`, `close_pane`,
  `reap_dead`, `pane_list`, `active_pane`…), au lieu de nous laisser en oublier un
  silencieusement. C'est le poste de coût principal de B1.
  *(Nommage : `PaneSlot` côté serveur, distinct de `PaneKind` côté protocole — le
  premier porte les objets, le second n'est qu'une étiquette sérialisée.)*
- **Volet actif de nature web** : `Session::active_pane()` renvoie aujourd'hui un
  `Arc<Pane>`. Il devient `active_term_pane() -> Option<Arc<Pane>>`, qui renvoie
  `None` si le volet actif est un navigateur. Tous ses appelants (routage d'entrée,
  `capture_pane`, mode copie, zoom, `active_pane_cwd`) traitent alors ce `None`
  comme un **no-op** — sauf `capture_pane`, qui renvoie le substitut textuel
  (`[navigateur] <url>`) plutôt qu'une erreur.
- Comportements :
  - un volet web n'est **jamais reapé** (il ne meurt pas) ;
  - `reflow` lui attribue son rectangle **sans** redimensionner de PTY ;
  - les frappes qui lui seraient routées côté TUI sont **ignorées** ;
  - `close_pane` le retire sans rien tuer.

## Protocole

- `LayoutNode::Leaf` gagne un **`kind`** : `PaneKind::Terminal` |
  `PaneKind::Web { url: String }`. C'est ce qui dit à la GUI quoi rendre, et ça
  transporte l'URL courante.
- Nouveaux `ClientMessage` (en fin d'enum) :
  - `OpenWebPane { session, from_pane: Option<u64>, dir: SplitDir, url: String }`
    → réutilise `ServerMessage::PaneSpawned { pane_id }`
  - `WebNavigate { session, pane: u64, url: String }`
  - `WebBack { session, pane: u64 }` / `WebForward { session, pane: u64 }`
  → réponse `Ok`/`Error` ; la mise à jour visible arrive par le `WindowLayout` poussé.

**Flux de navigation** : la GUI envoie l'action, le serveur met à jour URL et
historique, puis pousse un `WindowLayout` — la GUI reflète le `src`. Une seule
source de vérité, et la persistance vient gratuitement. **Exception** : *recharger*
se fait côté client (réassigner `src`), sans aller-retour serveur.

## Précédent / suivant : le comportement réel

La pile d'historique est celle des **URL que wimux a posées** (barre d'URL,
`OpenWebPane`, et plus tard B2). Les navigations effectuées *à l'intérieur* de la
page en cross-origin nous sont **invisibles** (`history` d'un iframe cross-origin
est inaccessible). Le « précédent » remonte donc l'historique de *notre* barre, pas
celui de la page. Ce comportement est documenté tel quel dans l'UI et le README —
il n'est pas présenté comme un historique de navigateur.

## GUI

`panes.ts` rend, pour une feuille `Web`, un conteneur `.pane-web` composé d'une
barre de chrome (champ URL éditable, boutons précédent/suivant/recharger) et d'un
`<iframe>`. Les boutons envoient les messages ci-dessus ; le `src` suit le
`WindowLayout` reçu. Le volet reste une feuille ordinaire : **découpe, fermeture,
focus, ratios et `layout_rev` fonctionnent sans modification**.

## TUI

Le client texte ne peut pas afficher de page : il dessine un **substitut** dans le
rectangle du volet — un cadre avec `[navigateur]` et l'URL. Nécessaire, puisque
`Window::render` compose tous les volets d'une fenêtre.

## CLI

`wimux browser open --url <url> [-t <session>] [--dir h|v] [--from-pane <id>]`
→ crée un volet navigateur et imprime `{"pane_id":N}`. Réutilise le message
`OpenWebPane` ; défauts `-t`/`--from-pane` pris dans `$WIMUX_SESSION`/`$WIMUX_PANE`
comme les verbes `wimux agent`.

## Limites assumées

- Les sites envoyant `X-Frame-Options: DENY` ou `CSP frame-ancestors` **refuseront
  de s'afficher**.
- **Ce refus n'est pas détectable de façon fiable** depuis la page hôte : l'échec
  ne produit pas d'événement exploitable. On affiche donc un **avertissement
  permanent et discret** sous la barre d'URL, plutôt qu'un faux message d'erreur
  qui prétendrait diagnostiquer.

## Tests

- **Protocole** : round-trip postcard de `LayoutNode::Leaf` avec ses deux `kind`,
  et des quatre nouveaux messages.
- **Serveur** : création d'un volet web (id renvoyé, présent dans l'arbre) ; pile
  d'historique (`navigate` ×2 → `back` → `forward` redonne les bonnes URL, et
  `back` en tête de pile est un no-op) ; **non-reaping** (le volet survit à un
  `reap`, contrairement à un volet terminal mort) ; fermeture ; **persistance** —
  après une ré-attache, le volet et son URL sont toujours là.
- **TUI** : le substitut `[navigateur] <url>` est rendu dans le rectangle.
- **GUI** : validation manuelle (procédure au README).
- **Non-régression** : suites TUI + GUI + M1→M4 + A1 vertes ; `fmt` + `clippy
  --all-targets -D warnings` (workspace **et** crate Tauri) ; `npm run build`.

## Découpage prévisionnel (pour le plan)

- **B1.1** — Protocole : `PaneKind` sur `Leaf` + les 4 messages.
- **B1.2** — Serveur : `WebPane` + bascule de la table de `Window` vers l'énumération
  (le gros du refactor), comportements (non-reap, reflow, close).
- **B1.3** — Serveur : navigation/historique + handlers + persistance.
- **B1.4** — TUI : substitut de rendu.
- **B1.5** — GUI : rendu `.pane-web` (iframe + barre de chrome) et câblage.
- **B1.6** — CLI : `wimux browser open` + aide.

## Hors-périmètre B1

- **Toute l'automatisation agent** (snapshot, click, type, screenshot, eval…) → **B2**.
- **Zoom** et **devtools** (écartés ci-dessus).
- **Webview enfant Tauri** — à reconsidérer si Tauri stabilise la fonctionnalité.
- Onglets multiples dans un même volet navigateur ; cookies/état persistés ;
  téléchargements ; gestion des dialogues.

## Risques

| Risque | Parade |
|---|---|
| `LayoutNode::Leaf` gagnant un champ change **tous** les encodages existants | Rebuild complet + redémarrage du daemon détaché (piège déjà consigné) ; aucun client tiers à ménager |
| Refactor de `window.rs` (table → énumération) touchant beaucoup de sites | C'est le choix assumé : l'énumération force le compilateur à tous les recenser ; suites TUI/GUI existantes en filet |
| Sites refusant l'affichage en cadre | Documenté ; avertissement permanent, pas de faux diagnostic |
| Historique trompeur (navigations internes invisibles) | Comportement documenté dans l'UI et le README ; on ne le présente pas comme un historique de navigateur |
| Volet web actif côté TUI (frappes, `capture-pane`) | Frappes ignorées ; `capture-pane` renvoie le substitut textuel, pas une erreur |
| Un volet web « actif » perturbant le routage d'entrée existant | Couvert par les suites TUI ; le volet actif reste un concept de fenêtre, seule la nature change |
