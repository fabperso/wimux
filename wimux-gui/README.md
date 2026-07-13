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
