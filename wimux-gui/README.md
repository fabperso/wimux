# wimux-gui (fondation G1)

Interface graphique Tauri pour wimux. G1 : affiche une session (`dev`) dans un
xterm.js, frappe fonctionnelle, persistance via le serveur wimux.

## Développement
- Prérequis : un serveur wimux avec une session `dev` (`wimux new -s dev`).
- `npm install` puis `npm run tauri dev`.

## Vérification bout-en-bout (manuelle)

1. Construire le workspace Rust et lancer un serveur + une session `dev` :
   ```bash
   cargo build --release
   target/release/wimux.exe new -s dev
   ```
   (Laisser cette fenêtre TUI ouverte, ou se détacher avec `Ctrl-b d` — la
   session `dev` reste vivante côté serveur.)
2. Lancer la GUI :
   ```bash
   cd wimux-gui && npm run tauri dev
   ```
3. **Attendu :** la fenêtre GUI affiche le contenu de la session `dev`
   (snapshot), et taper des commandes dans la fenêtre GUI les exécute
   (l'output s'affiche). Tester `Get-Date` puis Entrée.
4. Vérifier la **persistance** : fermer la fenêtre GUI, la relancer → le
   contenu réapparaît (snapshot), la session a survécu.

## Vérification manuelle du rail (G2)

1. Construire le workspace Rust et lancer un serveur avec deux sessions détachées :
   ```bash
   cargo build --release
   target/release/wimux.exe new -s a --detach
   target/release/wimux.exe new -s b --detach
   ```
2. Lancer la GUI :
   ```bash
   cd wimux-gui && npm run tauri dev
   ```
3. **Attendu :**
   - le rail (colonne de gauche) liste `a` et `b` ;
   - cliquer sur une entrée bascule le terminal affiché sur cette session
     (le flux de la session précédente s'arrête proprement) ;
   - le bouton `+` crée une nouvelle session et bascule dessus ;
   - la croix `×` (visible au survol) ferme la session correspondante ;
   - un double-clic sur le nom d'une session permet de la renommer (Entrée
     valide, Échap annule) ;
   - la frappe clavier est toujours routée vers la session actuellement
     affichée dans le terminal.
4. Le rail se met à jour tout seul (sondage 1 s) si une session est
   créée/fermée depuis un autre terminal (ex. `wimux new -s c` dans une
   fenêtre TUI).

## Vérification manuelle des volets (G3b — rendu)

1. Construire le workspace et lancer un serveur avec une session découpée en TUI :
   ```bash
   cargo build --release
   target/release/wimux.exe new -s dev
   ```
   Dans la fenêtre TUI, découper : `Ctrl-b %` (gauche/droite) puis `Ctrl-b "`
   (haut/bas), puis se détacher `Ctrl-b d`.
2. Lancer la GUI : `cd wimux-gui && npm run tauri dev`.
3. **Attendu :** la session `dev` s'affiche avec UN xterm par volet, disposés
   selon l'arbre (proportions = ratios), **en couleur** dès l'attache, curseur au
   bon endroit. Taper dans chaque volet route l'entrée vers ce volet (chaque
   xterm porte son `pane_id`). Redimensionner la fenêtre reflow les volets.

## Vérification manuelle des volets (G3c — opérations)

1. GUI lancée sur une session (`npm run tauri dev`).
2. Survoler un volet : une barre apparaît en haut à droite (⬍ ⬌ ✕).
3. **Attendu :**
   - ⬍ découpe le volet en haut/bas, ⬌ en gauche/droite (nouveau volet créé,
     shell démarré, snapshot coloré) ;
   - ✕ ferme le volet ; l'espace est repris par le volet frère ;
   - cliquer dans un volet le focalise (bordure bleue `.pane.active`) et la
     frappe va à ce volet.

## Vérification manuelle G3 (récapitulatif complet)

Avec la GUI attachée à une session découpée :
- **Couleurs à l'attache** : le contenu coloré (ex. `ls` colorisé, prompt) apparaît
  en couleur immédiatement, curseur au bon endroit.
- **Découper** : ⬍ (haut/bas) et ⬌ (gauche/droite) créent des volets vivants.
- **Fermer** : ✕ retire le volet, le frère reprend la place.
- **Focus** : clic → bordure bleue, la frappe suit le volet cliqué.
- **Taper dans chaque volet** : chaque volet exécute indépendamment (`whoami`, etc.).
- **Glisser les bordures** : tirer un séparateur redimensionne en direct (borné
  10 %–90 %) ; relâcher fixe le ratio côté serveur (le TUI attaché voit le même
  ratio).

## Vérification manuelle G4 (indicateurs d'activité)

Prérequis : deux sessions (ex. `dev` et `build`), la GUI attachée à `dev`.

1. Dans la session **inactive** `build` (via un TUI attaché ailleurs, ou
   `wimux send-keys -t build ...`), produire de la sortie, p. ex. `ls` ou
   `Write-Output test`.
   - **Attendu :** dans le rail, une **pastille bleue discrète** apparaît à droite
     du nom `build` (activité non vue), en ~1 s (sondage `List`).
2. Dans `build`, provoquer un **BEL** en sortie, p. ex. `[Console]::Write([char]7)`.
   - **Attendu :** la pastille de `build` devient une **cloche 🔔** (prioritaire
     sur l'activité).
3. Cliquer sur `build` dans le rail pour la regarder.
   - **Attendu :** sa pastille **disparaît immédiatement** (effacement optimiste),
     et le sondage suivant la maintient éteinte tant que `build` est affichée.
4. La session **active** n'affiche jamais de pastille.
