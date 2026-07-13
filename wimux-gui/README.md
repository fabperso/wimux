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
