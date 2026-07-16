# Design — wimux GUI W2 : onglets terminaux par workspace

- **Date** : 2026-07-16
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : l'épi **parité CMUX** (cf. mémoire `wimux-parite-cmux`) — W1 (redimensionnement des volets) fait ; W2 (ce document) ; W3 (rail enrichi), W4 (contrôles de split visibles), W5 (menu contextuel) à suivre.
- **Prérequis** : G2 (cycle d'attache GUI `GuiAttachment` qui arrête proprement le flux précédent à la bascule de session) + G3 (volets graphiques : arbre `LayoutNode`, canal fusionné multi-volets, `WindowLayout`) faits et fusionnés.

## Contexte

CMUX affiche, par **workspace**, une **barre d'onglets** de terminaux au-dessus de la zone de travail (cf. capture de référence). Chez wimux, une **session** contient déjà plusieurs **fenêtres** façon tmux (`Session.inner.windows: Vec<Window>`, `active_window: usize`), chacune avec son propre arbre de volets — mais la GUI n'affiche que la fenêtre active et n'expose aucun moyen d'en lister / créer / changer / fermer. W2 comble exactement cet écart.

## Objectif (W2)

Exposer dans la GUI les **fenêtres d'une session comme des onglets** : une barre d'onglets horizontale au-dessus de la zone de volets, permettant de **créer / basculer / fermer / renommer** une fenêtre. Chaque onglet réutilise l'arbre de volets G3 (dont le redimensionnement W1). Bascule d'onglet = changer la fenêtre active de la session côté serveur, qui réémet la disposition + réabonne les flux.

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Modèle | **Onglet = fenêtre (window) tmux existante** de la session (réutilise `Vec<Window>`/`active_window`) |
| Emplacement | **Barre d'onglets horizontale** au-dessus de la zone de volets, pour la session GUI-attachée |
| Opérations W2 | **Créer (`+`) / basculer (clic) / fermer (`×`) / renommer (double-clic)** |
| Libellé d'onglet | **Nom de fenêtre** si défini, sinon la position 1..N (calculée par la GUI) |
| Fermeture de la dernière fenêtre | **Interdite** (une session garde toujours ≥ 1 fenêtre ; le `×` du dernier onglet est masqué / no-op) |
| Différés | Pastille d'activité par onglet (→ réutilisera G4), réordonner par glisser (→ W5), libellé « titre vivant » du programme via OSC (→ ultérieur) |

## Modèle

- Une **fenêtre** = un onglet. La session GUI-attachée porte `windows: Vec<Window>` + `active_window`. Le `pane_id` est déjà **global** (`NEXT_PANE_ID`) → pas de collision d'ids entre fenêtres.
- **Nom de fenêtre** : `Window` gagne `name: Option<String>`. La GUI affiche `name` s'il existe, sinon `String(index + 1)`. `RenameWindow` fixe le nom.
- Les opérations d'onglet, comme les opérations de volet G3 (`SplitPane`/`FocusPane`), s'appliquent à **la session de l'attache GUI courante** — pas de paramètre `session` (le serveur connaît la session via le `GuiAttachment`).

## Protocole (ajouts)

Nouveaux `ClientMessage` (ajoutés **en fin d'enum**, compat postcard par index) :
- `NewWindow` — crée une fenêtre (shell par défaut de la session) dans la session attachée et la rend active.
- `SelectWindow { index: u32 }` — rend active la fenêtre `index`.
- `CloseWindow { index: u32 }` — ferme la fenêtre `index` (tue ses volets) ; **no-op s'il ne reste qu'une fenêtre**.
- `RenameWindow { index: u32, name: String }` — nomme la fenêtre `index`.

Nouveau `ServerMessage` (en fin d'enum) :
- `WindowList { windows: Vec<WindowInfo>, active: u32 }` où `WindowInfo { name: Option<String> }`.
  Émis **à l'attache** (état initial) et **après chaque opération** de fenêtre (création/fermeture/renommage/bascule).

Réutilisés : `WindowLayout { tree, active }` (disposition de la fenêtre active), `PaneSnapshot`/`PaneOutput` (contenu des volets).

## Serveur

- **`Window`** gagne `name: Option<String>` (défaut `None`) + accès/`set_name`.
- **`Session`** (agit sur la session attachée) gagne :
  - `gui_new_window(cols, rows) -> (WindowList, WindowLayout)` : pousse une `Window` neuve (un volet racine = shell par défaut, comme à la création de session), fixe `active_window` sur elle.
  - `gui_select_window(index) -> Option<(WindowList, WindowLayout)>` : borne l'index, fixe `active_window`.
  - `gui_close_window(index) -> Option<WindowList>` : refuse si `windows.len() == 1` ; sinon tue les volets de la fenêtre, la retire, réajuste `active_window` (borne comme `gui_close` le fait déjà).
  - `gui_rename_window(index, name)` : pose le nom.
  - `window_list() -> (Vec<WindowInfo>, u32)` : projette `windows`/`active_window`.
- **Ré-abonnement des flux à la bascule** (point technique central) : `SelectWindow`/`NewWindow` changent la fenêtre active → le serveur **rejoue le chemin d'attache GUI pour la nouvelle fenêtre** : arrêt propre du flux fusionné précédent, abonnement aux volets de la nouvelle fenêtre, envoi de `WindowLayout` + d'un `PaneSnapshot` frais par volet, reprise de `PaneOutput`. C'est exactement le cycle `GuiAttachment` que G2 fait déjà à la **bascule de session** — ici transposé à la bascule de **fenêtre** dans une même session (mutualiser la logique).
- **Câblage `handle_client`** : les 4 nouveaux messages → mutation via les méthodes `Session` ci-dessus, puis envoi de `WindowList` (+ `WindowLayout` et re-snapshots quand la fenêtre active change), sérialisés par le verrou d'écriture GUI `gui_write` (G3).

## Frontend

- **Pont Tauri** : commandes `new_window()`, `select_window(index)`, `close_window(index)`, `rename_window(index, name)` (sur la connexion persistante) ; événement `window-list` de charge utile `(WindowInfo[], active)`.
- **Barre d'onglets** (nouveau, au-dessus de `#terminal`) : rendue depuis `window-list`. Un onglet par fenêtre (libellé = `name ?? (index+1)`), l'actif surligné + `×` (masqué si une seule fenêtre) ; un bouton `+` en fin de barre. Clic = `select_window` ; `×` = `close_window` ; double-clic = édition inline → `rename_window` ; `+` = `new_window`.
- **Bascule** : à réception d'un `window-list` dont l'`active` change (ou d'un `window-layout` d'une autre fenêtre), le `PaneManager` fait un `reset()` puis rend la nouvelle disposition ; les `pane-snapshot` frais peignent le contenu (réutilise l'existant — les `pane_id` étant globaux, `renderLayout` dispose les volets absents et crée les nouveaux).
- Les sessions **sans** interaction d'onglet restent identiques (une session neuve = une fenêtre = un onglet).

## Tests

- **Serveur (intégration `gui_mode.rs`)**, sur une session GUI-attachée avec un shell déterministe :
  - À l'attache : `WindowList` contient 1 fenêtre, `active = 0`.
  - `NewWindow` → `WindowList` a 2 fenêtres, `active = 1` ; un `WindowLayout` de la nouvelle fenêtre suit (arbre à une feuille, `pane_id` distinct de la 1re fenêtre).
  - `SelectWindow { index: 0 }` → `WindowList` `active = 0` ; `WindowLayout` de la fenêtre 0.
  - `RenameWindow { index: 0, name: "build" }` → `WindowList` reflète `name = Some("build")`.
  - `CloseWindow { index: 1 }` → `WindowList` a 1 fenêtre ; un second `CloseWindow` sur l'unique fenêtre est **no-op** (toujours 1 fenêtre).
- **`Window` (unitaire)** : `set_name`/`name` ; fermeture bornée.
- **Frontend** : validé **manuellement** (README) — créer un onglet, basculer, renommer (double-clic), fermer ; vérifier que le contenu du terminal suit la bascule et que le dernier onglet n'a pas de `×`.
- **Non-régression** : suites TUI + G1→G4 + M1→M3 vertes ; fmt + clippy `-D warnings` ; `npm run build` OK.

## Hors-périmètre W2 (rappel)

- **Pastille d'activité par onglet** (sortie non vue / cloche d'une fenêtre non-active) → réutilisera la logique G4 au niveau fenêtre, plus tard.
- **Réordonner les onglets** par glisser → recoupe W5 (position/organisation).
- **Libellé « titre vivant »** (nom d'onglet auto depuis le programme courant via OSC 0/2) → ultérieur ; W2 = nom de fenêtre explicite ou position.
- **cwd / branche git au rail** → W3. **Contrôles de split toujours visibles** → W4. **Menu contextuel** → W5.

## Risques

| Risque | Parade |
|---|---|
| Ré-abonnement des flux à la bascule de fenêtre (contenu figé / fuite du flux précédent) | Mutualiser le cycle `GuiAttachment` de G2 (déjà éprouvé à la bascule de session) ; re-snapshot systématique |
| Fermeture de la dernière fenêtre (session sans fenêtre) | Interdite au serveur (`windows.len() == 1` → no-op) ; `×` masqué côté GUI |
| Écritures GUI concurrentes (WindowList vs WindowLayout vs PaneOutput) | Verrou `gui_write` (G3) sérialise tous les envois GUI |
| `SessionInfo`/messages étendus vs vieux daemon | Rebuild + redémarrage du daemon détaché (piège consigné `wimux-daemon-restart-gotcha`) |
| Collision de `pane_id` entre fenêtres | Aucune : `pane_id` global via `NEXT_PANE_ID` |
