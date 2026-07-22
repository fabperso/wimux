# Design — wimux B2.1 : moteur navigateur pilotable (lecture seule)

- **Date** : 2026-07-22
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : **B2 — automatisation du navigateur par Claude** (parité `cmux browser`)
- **Prérequis** : B1 (volet navigateur iframe) fait et fusionné dans `main` ; A1 (CLI `wimux agent`/`batch` + skill) fait.

## Contexte : décomposition de B2

Piloter un navigateur pour Claude, façon `cmux browser` (~40 sous-commandes), est **trop
gros pour une seule spec**. Découpage (chaque morceau = sa spec/plan/revue) :

- **B2.1** *(ce document)* — **moteur + lecture seule** : le daemon possède un Chromium
  visible, pilotable ; CLI `wimux browser launch/close/status/navigate/url/snapshot/screenshot`.
  Aucune action mutante, aucun JS de page.
- **B2.2** — **actions & synchronisation** : `click`, `type`, `fill`, `select`, `check`,
  `scroll`, et surtout `wait`. Ciblage par sélecteur CSS + `find` par rôle/texte.
- **B2.3** — **puissance & état** : `eval` (JS arbitraire), cookies, storage, `console`/
  `errors`, onglets.
- **B2.4** — **skill + garde-fous** : le skill qui enseigne la boucle à Claude, et le
  durcissement de sécurité (contenu de page non fiable, refus d'identifiants et d'actions
  financières, confirmation des actions irréversibles).

## Pourquoi un Chromium externe, et pas le volet iframe de B1

Vérifié en amont : `cmux browser` pilote un **vrai** navigateur (exécution de JS dans la
page, lecture du DOM/arbre d'accessibilité, clic par sélecteur, cookies). L'`<iframe>` de
B1 ne peut **pas** faire ça : la même barrière cross-origin qui empêche une page d'atteindre
son parent (qu'on a d'ailleurs renforcée en B1 avec `sandbox`) empêche le parent de piloter
une page tierce. Un vrai navigateur navigue vers un site comme **document de premier niveau**
— il *est* la page — donc le JS injecté s'y exécute sans barrière.

Décision (validée) : un **Chromium externe piloté par CDP** via `chromiumoxide` (l'équivalent
Rust de Puppeteer : toute l'automatisation existe déjà, mûre, Windows OK), **avec une fenêtre
visible** (« vitrine ») pour voir le navigateur travailler. Le volet iframe de B1 reste, lui,
pour prévisualiser un serveur de dev local. Ce sont **deux facilités distinctes** pour deux
usages distincts.

L'alternative — webview enfant Tauri — a été écartée : drapeau `unstable` + bugs de
redimensionnement (déjà la raison du rejet en B1), `eval` sans valeur de retour, et **aucune**
API d'automatisation haut niveau (il faudrait tout réécrire à la main).

## Décisions (validées lors du brainstorm)

| Sujet | Décision |
|---|---|
| Moteur | **Chromium externe piloté par CDP** (`chromiumoxide`), fenêtre **visible** |
| Binaire cible | **Chrome si présent, sinon `msedge.exe`** (toujours là sur Win11), sinon erreur claire |
| Format de `snapshot` | **Arbre d'accessibilité** (CDP `Accessibility.getFullAXTree`), rôle + nom + état |
| Lancement | **Paresseux** : `navigate`/`launch` lancent le navigateur ; les lectures (`url`/`snapshot`/`screenshot`) exigent un navigateur vivant, erreur claire sinon |
| JS de page | **Aucune exécution** en B2.1 (lectures CDP natives uniquement) — surface d'attaque minimale |
| Emplacement | Module `browser.rs` dans `wimux-server` (à côté de `batch.rs`, `webpane.rs`) ; ajoute `chromiumoxide` + `tokio` |
| Cycle de vie | Un seul navigateur par daemon, meurt avec lui, **ne survit pas** à un redémarrage du daemon (processus éphémère) |

## Le moteur et le pont sync↔async

`chromiumoxide` exige **tokio** ; le serveur wimux est **100 % threads `std`**, sans async.
Le pont :

- Un module `browser.rs` expose un `BrowserEngine`. Au **premier** appel, il démarre un
  **thread dédié** qui construit un `tokio::runtime::Runtime` et y `block_on` une boucle qui :
  1. lance le navigateur (`Browser::launch(BrowserConfig::builder().with_head()… )`), 
  2. fait tourner le `Handler` (obligatoire — sans lui le navigateur ne répond pas), en
     parallèle de la boucle de commandes,
  3. détient l'**unique** `Page`.
- La partie **synchrone** envoie des `BrowserCommand` sur un canal (`std::sync::mpsc`) et
  **bloque** sur la réponse (canal retour) : `engine.exec(cmd) -> Result<Reply, String>`.
  Le daemon reste donc entièrement synchrone ; seul le thread moteur connaît tokio.
- **Découverte du binaire** : `chromiumoxide` accepte un chemin d'exécutable explicite
  (`BrowserConfig::builder().chrome_executable(path)`). On résout Chrome (emplacements
  d'installation standard / `where chrome`), sinon `msedge.exe`, sinon `Err` explicite. La
  résolution est une **fonction pure testable** prenant une liste de chemins candidats et
  renvoyant le premier existant.
- **Cycle de vie** : lancement paresseux ; `close` droppe le `Browser` (ferme Chrome) et
  arrête le thread moteur ; l'arrêt du daemon fait de même. Un `launch` quand c'est déjà
  lancé est un no-op ; les lectures sans navigateur renvoient `Err`.

## Protocole (ajouts — additifs, en fin d'enum)

Nouveaux `ClientMessage` : `BrowserLaunch`, `BrowserClose`, `BrowserStatus`,
`BrowserNavigate { url: String }`, `BrowserUrl`, `BrowserSnapshot`, `BrowserScreenshot`.

Nouveaux `ServerMessage` : `BrowserStatus { running: bool, url: Option<String> }`,
`BrowserText(String)` (réutilisé pour `url`/`navigate`/`snapshot`),
`BrowserShot { path: String }`. `Ok`/`Error` pour `launch`/`close`.

Ces commandes ne ciblent pas de session (le navigateur est unique au daemon), contrairement
aux volets.

## CLI `wimux browser` (lecture seule)

- `wimux browser launch` → lance le navigateur (no-op s'il tourne déjà).
- `wimux browser close` → le ferme.
- `wimux browser status` → `{"running":bool,"url":"…"|null}`.
- `wimux browser navigate --url <url>` → lance au besoin, va à `url`, attend le chargement,
  imprime l'URL finale.
- `wimux browser url` → l'URL courante.
- `wimux browser snapshot` → l'arbre d'accessibilité (texte indenté).
- `wimux browser screenshot` → capture PNG écrite sous
  `%LOCALAPPDATA%\wimux\screenshots\<horodatage>.png`, imprime le chemin (façon `cmux --out`,
  pratique pour que Claude la lise avec l'outil de lecture d'image).

## `snapshot` : l'arbre d'accessibilité

Via CDP `Accessibility.getFullAXTree`, transformé en arbre **compact et lisible** : un nœud
par ligne, indenté selon la profondeur, `rôle "nom" [états]` (ex. `button "Continuer"`,
`textbox "Email" [focusable]`, `link "Accueil"`). Les nœuds décoratifs / ignorés (`role:
none`, sans nom ni enfant utile) sont élagués. C'est ce que Claude lit pour comprendre la
page. **Pas de refs réutilisables** (comme cmux) : le ciblage précis pour agir (sélecteur CSS)
arrive en B2.2. La transformation `AXTree brut → texte` est une **fonction pure**, testée sur
un échantillon CDP figé.

## Sécurité (le fil rouge commence ici)

- `navigate` n'accepte que les schémas **`http`/`https`** (refus de `file:`, `javascript:`,
  `data:`) — même esprit que la garde d'URL de B1.
- **Aucune exécution de JavaScript de page** en B2.1 : pas de `Runtime.evaluate`. On ne fait
  que lire via des méthodes CDP natives (AX tree, screenshot, URL, title). La surface
  d'attaque est minimale.
- Le contenu de `snapshot`/`screenshot` est de la **donnée non fiable** que Claude lit,
  **jamais** des instructions. Ce principe sera écrit explicitement dans le skill de B2.4 ;
  la posture commence dès maintenant (rien dans B2.1 ne laisse le contenu de page déclencher
  une action, puisqu'il n'y a pas d'action).

## Erreurs

| Cas | Comportement |
|---|---|
| Ni Chrome ni Edge trouvés | `Error` clair (« aucun navigateur Chrome/Edge trouvé ») |
| Échec de lancement du navigateur | `Error` avec le motif |
| Lecture (`url`/`snapshot`/`screenshot`) sans navigateur lancé | `Error` (« aucun navigateur : lance-le ou navigue d'abord ») |
| `navigate` vers un schéma non http(s) | `Error` (« URL refusée : http(s) seulement ») |
| Échec/timeout de navigation | `Error` avec le motif |
| Échec d'écriture de la capture | `Error` (chemin/permission) |

## Tests

- **Découverte du binaire** (fonction pure) : parmi une liste de chemins candidats, renvoie
  le premier existant ; `None` si aucun.
- **Transformation AX tree** (fonction pure) : un échantillon `getFullAXTree` figé produit
  l'arbre texte attendu (rôles/noms/élagage).
- **Garde d'URL** (fonction pure) : `http(s)` acceptés, le reste refusé (dont la casse).
- **Intégration, conditionnelle** (comme les tests git de M3/M4) : ignorés proprement si
  aucun binaire navigateur n'est présent ; sinon `launch` → `navigate` vers une **page locale
  servie par le test** (aucun accès réseau externe) → `url`/`snapshot`/`screenshot` renvoient
  du contenu cohérent → `close`. Le pont sync↔async est ainsi exercé de bout en bout.
- **Non-régression** : suites TUI + GUI + M1→M4 + A1 + B1 vertes ; `fmt` + `clippy
  --all-targets -D warnings` (workspace **et** crate Tauri) ; `npm run build`.

## Découpage prévisionnel (pour le plan)

- **B2.1.1** — Dépendances + `BrowserEngine` : pont sync↔async (thread + runtime tokio +
  canaux), découverte du binaire, lancement/fermeture paresseux. Protocole `launch`/`close`/
  `status`.
- **B2.1.2** — `navigate` (garde d'URL, attente de chargement) + `url`.
- **B2.1.3** — `snapshot` (AX tree → texte).
- **B2.1.4** — `screenshot` (PNG → fichier).
- **B2.1.5** — CLI `wimux browser` (les 7 verbes) + aide.

## Hors périmètre B2.1

- Toutes les **actions** (click/type/fill/select/check/scroll/press) et `wait`/`find` → **B2.2**.
- `eval`, cookies, storage, `console`/`errors`, onglets multiples → **B2.3**.
- Le **skill** + garde-fous lourds (identifiants, actions financières, injection) → **B2.4**.
- Multi-surfaces (plusieurs pages/onglets adressables `surface:N`).
- Intégration de la fenêtre navigateur dans la disposition wimux (elle reste une fenêtre OS
  séparée).

## Risques

| Risque | Parade |
|---|---|
| `chromiumoxide` + `tokio` alourdissent le crate serveur (aujourd'hui : `portable-pty`) | Poids assumé (choix validé) ; contenu dans un module `browser.rs` + un thread moteur isolé |
| Cohabitation sync (daemon) ↔ async (tokio) | Pont par canaux : le daemon reste synchrone, seul le thread moteur connaît tokio ; frontière étroite et testée de bout en bout |
| Chrome/Edge absent ou version exotique | Découverte Chrome→Edge→erreur ; tests d'intégration conditionnels |
| Daemon détaché lançant une fenêtre GUI visible | Lancer un processus GUI depuis un process détaché est supporté sous Windows ; la fenêtre apparaît sur le bureau utilisateur |
| Le `Handler` non pompé fige le navigateur | Il tourne dans le thread moteur en parallèle de la boucle de commandes (contrainte `chromiumoxide` respectée) |
| Fuite du processus Chrome si le daemon meurt brutalement | `Browser` droppé ferme Chrome au `close`/arrêt propre ; un orphelin après crash est toléré (l'utilisateur peut le fermer) |
| Changement de protocole vs daemon persistant | Ajouts **en fin d'enum** ; rebuild release + redémarrage du daemon (piège consigné) |
