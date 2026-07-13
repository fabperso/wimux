# Design — wimux GUI G3 : volets graphiques (arbre de découpes rendu)

- **Date** : 2026-07-13
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : `docs/superpowers/specs/2026-07-13-wimux-gui-foundation-design.md`
- **Prérequis** : G1 (mode GUI du protocole) et G2 (rail de sessions) faits et fusionnés dans `main`.

## Objectif

Rendre, dans `wimux-gui`, l'**arbre de découpes de la fenêtre active** d'une session
avec **un xterm.js par volet** (au lieu de la grille composite texte-seul de G1/G2).
L'utilisateur peut découper, fermer, focaliser et redimensionner les volets à la
souris. C'est le passage d'« un terminal par session » à un vrai multiplexeur
graphique par-dessus l'arbre `Window` déjà présent côté serveur.

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Opérations volets | Rendu + découper (H/V) + fermer + naviguer + **glisser les bordures** |
| Niveau fenêtres | **Fenêtre active seulement** ; multi-fenêtres différé (hors G3) |
| Déclenchement | **Souris intégrale** : boutons au survol + clic-focus + glisser-bordure ; pas de touche de préfixe |
| Autorité de disposition | **Approche A** : le serveur possède l'arbre (`dir`+`ratio`), la GUI possède les pixels (rendu + tailles) |
| Commandes | **Commandes de volet explicites** (`SplitPane`/`ClosePane`/`FocusPane`/`SetSplitRatio`), pas de simulation de touches `Ctrl-b` |
| Fidélité instantané | **Fidèle** : couleurs (SGR depuis le `Pen`) + position du curseur — résout la dette « texte-seul » traînée depuis G1 |

## Architecture (Approche A)

Le serveur reste **la source de vérité de la topologie** : il conserve l'arbre
binaire `Window` existant (`Node::Leaf(PaneId)` / `Node::Split { dir, ratio, a, b }`,
cf. `crates/wimux-server/src/window.rs`), le même que le TUI utilise — donc une
session vue en TUI et en GUI partage le **même** arbre.

La GUI est **la source de vérité du rendu et des entrées** : elle reçoit la
structure de l'arbre, construit des conteneurs flex imbriqués, place un xterm.js
par feuille, dimensionne chacun depuis les ratios × la taille de la fenêtre, et
renvoie au serveur la taille réelle (cellules) de chaque volet.

Flux de données :
1. `AttachGui { session }` → le serveur attache la **fenêtre active** : il répond
   `WindowLayout { tree, active }` + un `PaneSnapshot { pane_id, bytes }` **fidèle
   par volet**, puis diffuse le `PaneOutput { pane_id, bytes }` de **tous** les
   volets via un canal fusionné.
2. La GUI rend l'arbre, écrit chaque snapshot dans le xterm correspondant, et
   pour chaque volet mesure sa taille et envoie `PaneResize { pane_id, cols, rows }`.
3. Frappe : le volet focus envoie `PaneInput { pane_id, bytes }` (chaque xterm
   porte son propre `pane_id`, donc pas de notion de « volet actif » pour router
   l'entrée).
4. Opérations : `SplitPane`/`ClosePane`/`FocusPane`/`SetSplitRatio` mutent la
   fenêtre active ; le serveur repousse `WindowLayout` (et, au split, un snapshot
   pour le nouveau volet + son abonnement au canal fusionné).

**Approches écartées.** B (la GUI possède tout l'arbre, le serveur n'est qu'une
ferme de PTY) diverge du modèle TUI et duplique la logique d'arbre. C (image
composite colorée dans un seul xterm) n'est pas « un xterm par volet » : pas de
scrollback / redimensionnement / glisser propres par volet.

## Protocole (ajouts minimes, réutilisation maximale)

Nouveau type partagé dans `wimux-protocol` :

```
enum SplitDir { LeftRight, TopBottom }

enum LayoutNode {
    Leaf { pane_id: u64 },
    Split { node_id: u32, dir: SplitDir, ratio: f32,
            a: Box<LayoutNode>, b: Box<LayoutNode> },
}
```

Nouveaux `ClientMessage` :
- `SplitPane { pane_id: u64, dir: SplitDir }` — découpe le volet désigné ; le
  nouveau volet devient actif.
- `ClosePane { pane_id: u64 }` — ferme le volet désigné.
- `FocusPane { pane_id: u64 }` — désigne le volet actif (bordure/cohérence TUI).
- `SetSplitRatio { node_id: u32, ratio: f32 }` — fixe le ratio d'un nœud de
  découpe interne (glisser-bordure). `ratio` est borné `[0.1, 0.9]` côté serveur.

Nouveau `ServerMessage` :
- `WindowLayout { tree: LayoutNode, active: u64 }` — la disposition de la fenêtre
  active. Envoyé à l'attache et après chaque changement de topologie ou de ratio.

Réutilisés (sémantique étendue) :
- `AttachGui { session }` → répond `WindowLayout` + un `PaneSnapshot` par volet +
  diffuse le `PaneOutput` de tous les volets. **Sémantique de bascule inchangée**
  (l'attache précédente est arrêtée proprement, cf. G2).
- `PaneInput { pane_id, bytes }` — inchangé (routé vers ce volet).
- `PaneResize { pane_id, cols, rows }` — désormais **honoré** : redimensionne le
  PTY du volet.
- `PaneSnapshot { pane_id, bytes }` — désormais **fidèle** (voir plus bas).
- `PaneOutput { pane_id, bytes }`, `PaneExited { pane_id }` — par volet.

**`node_id` stable.** Chaque `Node::Split` porte un identifiant `u32` attribué à
sa création (compteur par fenêtre). Il survit aux re-sérialisations, ce qui rend
`SetSplitRatio` non ambigu même si l'arbre a changé ailleurs entre l'envoi du
layout et le glisser.

## Serveur

1. **`window.rs`**
   - Ajouter `node_id: u32` à `Node::Split` ; un compteur (par `Window`) attribue
     les ids au moment du `split`.
   - `layout_tree(&self) -> LayoutNode` : traduit l'arbre interne en `LayoutNode`
     sérialisable ; expose aussi le volet actif.
   - Généraliser les opérations à un volet **désigné** (les versions actuelles
     agissent sur le volet actif) : `split_pane(&mut self, id, dir, new_pane)`,
     `close_pane(&mut self, id) -> bool`. `set_active(id)` existe déjà.
   - `set_ratio(&mut self, node_id, ratio)` : trouve le nœud par id et fixe son
     ratio (borné).
2. **Instantané fidèle** : `grid_to_ansi(grid, cursor) -> Vec<u8>` émettant
   `\x1b[2J\x1b[H`, puis pour chaque ligne des séquences SGR groupées par run de
   `Pen` identique (fg/bg indexés ou défaut, gras/italique/souligné/inverse),
   `\x1b[0m` en fin, puis positionnement du curseur `\x1b[<r>;<c>H`. Remplace le
   rendu texte-seul pour les snapshots GUI. Le reset SGR entre runs évite les
   fuites d'attributs.
3. **Attache GUI multi-volets** : un **canal fusionné** par attachement. À
   l'attache, pour la fenêtre active, chaque volet est abonné de façon à pousser
   sa sortie — taguée `pane_id` — dans un unique `mpsc::Sender` détenu par
   l'attachement ; le démon retransmet chaque `(pane_id, bytes)` en `PaneOutput`.
   Le `GuiAttachment` de G2 (drapeau `AtomicBool` + `JoinHandle`, `Drop`
   stoppe+join) est étendu pour couvrir N volets dynamiques : au `SplitPane` on
   abonne le nouveau volet au même canal et on lui envoie son snapshot ; au
   `ClosePane` (ou volet mort) on retire l'abonnement. La bascule de session
   arrête proprement tout l'attachement (garantie G2 préservée).
4. **Câblage des commandes** : `SplitPane`/`ClosePane`/`FocusPane`/`SetSplitRatio`
   mutent la fenêtre active de la session attachée, puis le démon repousse
   `WindowLayout`. `PaneResize` → `pane.resize(cols, rows)`.

**Autorité de taille.** Quand une fenêtre est attachée en GUI, ses tailles de
volets sont pilotées par les `PaneResize` de la GUI (pixels → cellules), pas par
le `reflow` du TUI. Le cas TUI+GUI simultanés sur la **même** fenêtre avec des
tailles divergentes n'est pas supporté en G3 (la GUI est prioritaire) — voir
hors-périmètre.

## Frontend / pont Tauri

- **Moteur d'arbre** (`src/main.ts`, éventuellement scindé en un module
  `panes.ts`) : `renderLayout(tree)` construit récursivement des conteneurs flex
  imbriqués — `flex-direction: row` pour `LeftRight`, `column` pour `TopBottom` —
  avec `flex-grow` proportionnel aux ratios. Chaque feuille héberge un xterm.js
  issu d'une `Map<pane_id, PaneView>`. Au reçu d'un `WindowLayout` : **diff** —
  créer les xterms des volets ajoutés, disposer ceux retirés, et **réutiliser**
  (reparenter le nœud DOM) les xterms des volets persistants (ne jamais recréer un
  xterm qui existe déjà).
- **Par volet** (`PaneView`) : `Terminal` + `FitAddon` ; un `ResizeObserver` sur
  le conteneur du volet → `fit()` → `PaneResize { pane_id, cols, rows }` ;
  `onData` → `PaneInput { pane_id, bytes }` ; routage des events
  `pane-output` / `pane-snapshot` par `pane_id` vers le bon `term.write`.
- **Opérations** : barre au survol de chaque volet (boutons découper-H,
  découper-V, fermer) → `SplitPane` / `ClosePane` ; clic sur un volet →
  `FocusPane` + focus DOM + bordure active ; **glisser une bordure** → calcul du
  ratio depuis la position du pointeur dans le conteneur du split → `SetSplitRatio`
  (throttlé) ; le serveur ré-émet `WindowLayout` (mise à jour optimiste possible
  côté GUI en attendant l'écho).
- **Pont Tauri** (`src-tauri/src/lib.rs`) : nouvelles commandes `split_pane`,
  `close_pane`, `focus_pane`, `set_split_ratio` (sur la connexion persistante) ;
  le thread lecteur émet aussi un event `window-layout` en plus de
  `pane-snapshot` / `pane-output` / `pane-error`.

## Jalons de construction

- **G3a — serveur & protocole** : `SplitDir`/`LayoutNode` + les 4 commandes +
  `WindowLayout` ; `layout_tree`, `node_id` stable, `split_pane`/`close_pane`/
  `set_ratio` ; attache GUI multi-volets (canal fusionné) ; instantané fidèle
  (`grid_to_ansi`) ; `PaneResize` honoré. Testé par intégration serveur.
- **G3b — rendu frontend** : moteur d'arbre (un xterm/volet, flex imbriqué depuis
  le layout), routage des flux par `pane_id`, entrée + `PaneResize` par volet,
  snapshots colorés. Livrable : les découpes existantes (créées via TUI)
  s'affichent et s'utilisent en direct dans la GUI.
- **G3c — opérations & glisser** : boutons découper/fermer au survol, clic-focus +
  bordure active, glisser les bordures (`SetSplitRatio`). Livrable : gestion
  complète des volets à la souris depuis la GUI.

## Tests

- **Serveur (intégration, `tests/gui_mode.rs`)** :
  - Attache : `AttachGui` renvoie un `WindowLayout` cohérent + un `PaneSnapshot`
    par volet ; sur une session à volet unique, l'arbre est une feuille.
  - `SplitPane` : l'arbre gagne une feuille (le `WindowLayout` repoussé la
    reflète) et le nouveau volet diffuse son `PaneOutput`.
  - `ClosePane` : l'arbre perd la feuille ; le volet fermé cesse de diffuser.
  - `SetSplitRatio` : le ratio du nœud visé change dans le `WindowLayout` suivant ;
    valeur bornée `[0.1, 0.9]`.
  - Instantané fidèle : pour une grille colorée connue, `grid_to_ansi` produit une
    sortie qui, re-parsée par `wimux-vt`, restitue les mêmes couleurs et la même
    position de curseur (test dédié, possiblement dans `wimux-vt` ou `wimux-server`).
  - `PaneResize` : après envoi, la taille du PTY/volet correspond.
- **Frontend** : validé **manuellement** (comme G1/G2), procédure documentée dans
  `wimux-gui/README.md` — découper H/V, fermer, focus au clic, glisser les
  bordures, taper dans chaque volet, couleurs présentes dès l'attache.
- **Non-régression** : suites existantes (TUI + G1 + G2) vertes ; fmt + clippy
  `-D warnings` propres ; `npm run build` (frontend) OK.

## Hors-périmètre (rappel)

Multiples fenêtres par session + sélecteur de fenêtres (différé, éventuel G3d ou
plus tard) ; pastilles d'activité live des onglets/volets inactifs (**G4**) ;
attache multi-sessions « chaud » (**G4**) ; TUI et GUI attachés simultanément à la
**même** fenêtre avec des tailles divergentes (non supporté ; la GUI est
prioritaire) ; navigateur intégré ; multi-agents.

## Risques

| Risque | Parade |
|---|---|
| Cycle de vie de l'attache multi-volets (abonnement dynamique au split, nettoyage au close) | Canal fusionné unique par attachement ; `GuiAttachment` (drapeau+join de G2) étendu à N volets ; bascule arrête tout |
| Conflit d'autorité de taille (reflow serveur vs `PaneResize` GUI) | Fenêtre attachée en GUI = pilotée par la GUI ; TUI+GUI simultanés hors-périmètre |
| Exactitude de l'instantané SGR (runs, 256 couleurs, défaut, reset) | Test VT dédié : re-parser la sortie de `grid_to_ansi` et comparer à la grille source |
| Identification du nœud au glisser après une mutation concurrente | `node_id` stable stocké dans le `Node::Split`, pas un index de traversée |
| Diff DOM qui recrée des xterms (perte de scrollback/état) | Réutiliser (reparenter) les `PaneView` persistants ; ne créer/disposer que les volets ajoutés/retirés |
| Régression TUI (opérations volets généralisées à un id) | `split_pane(id)`/`close_pane(id)` conservent le comportement des versions « actif » quand `id == active` ; suites TUI vertes |
