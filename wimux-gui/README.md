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

## Vérification manuelle M2 (agents)

Déclarer au moins un modèle dans `%USERPROFILE%\.wimux.conf` :

```text
agent-template echo   cmd.exe /c echo {prompt}
agent-template shell  cmd.exe
```

Rebuild + **redémarrer le démon détaché** (piège du serveur persistant), puis
lancer la GUI (`npm run tauri dev` dans `wimux-gui/`) et vérifier :

1. Cliquer **+ agent** : le dialogue s'ouvre, le menu liste `echo` et `shell`.
2. Modèle `echo`, prompt `bonjour`, cwd/nom vides → **Lancer**. La GUI bascule
   sur `echo-0` ; le glyphe passe ⚙ (*Working*, bleu) puis ✓ (*Done*, vert).
3. Modèle `shell` (interactif), prompt `echo salut` → **Lancer** : le prompt est
   injecté sur stdin (+ Entrée) ; la session vit puis (après `exit`) affiche ✓.
4. Un modèle absent / un cwd invalide affiche l'erreur dans le dialogue, sans
   créer de session.
5. Fermer un agent terminé via le `×` du rail.

## Vérification manuelle M3 (lots fan-out)

Prérequis : au moins un modèle configuré (cf. M2), un **repo git** local
(`git init` + un commit), et le serveur relancé après rebuild (piège du daemon
persistant). Racine des worktrees : `%LOCALAPPDATA%\wimux\worktrees` par défaut,
ou une directive `set agent-worktree-root <chemin>` dans `%USERPROFILE%\.wimux.conf`.

1. Cliquer **⇉ lot** : le dialogue s'ouvre (repo de base, modèle, prompt, nombre).
2. Renseigner le **repo de base** (chemin d'un dépôt git), modèle `echo`, prompt
   `bonjour`, nombre `2` → **Lancer**.
   - **Attendu :** le rail affiche un **en-tête de lot** (`batch0`) avec l'agrégat
     des statuts (`⚙2 ✓0 ✗0` puis `⚙0 ✓2 ✗0`), et **2 membres** en dessous
     (`echo-batch0-0`, `echo-batch0-1`), chacun avec son glyphe M2 (⚙ → ✓).
   - Sous `%LOCALAPPDATA%\wimux\worktrees`, deux dossiers `batch0-0` / `batch0-1`
     apparaissent (worktrees git ; `git -C <repo> worktree list` les montre).
3. Cliquer un membre : le terminal bascule sur cette session (agent dans son
   worktree).
4. Cliquer le **×** de l'en-tête de lot (visible au survol) : **fermer le lot**
   tue les 2 membres ; leurs dossiers de worktree et branches `wimux/batch0/*`
   disparaissent (`git -C <repo> worktree list` / `git branch` le confirment).
5. Un **repo non-git** (ou un chemin invalide) affiche l'erreur dans le dialogue,
   sans créer ni session ni worktree.

## Vérification manuelle W2 (onglets terminaux)

Prérequis : rebuild + redémarrage du daemon (changement de protocole), puis
`npm run tauri dev`. S'attacher à une session.

1. **État initial** : une session neuve affiche un seul onglet (libellé « 1 »),
   sans `×` (fermeture de la dernière fenêtre interdite), et un bouton `+`.
2. **Créer** : cliquer `+` → un 2e onglet apparaît (« 2 »), devient actif, et la
   zone de volets se réinitialise sur le shell de la nouvelle fenêtre.
3. **Basculer** : cliquer l'onglet « 1 » → le contenu revient à la 1re fenêtre
   (les volets et leur sortie suivent la bascule). L'onglet actif est surligné
   (bordure haute bleue).
4. **Renommer** : double-cliquer un onglet → champ d'édition inline ; taper
   « build » + Entrée → le libellé devient « build ». Vider le nom + Entrée →
   le libellé revient à la position.
5. **Fermer** : avec ≥ 2 onglets, survoler un onglet → un `×` apparaît ; cliquer
   → l'onglet disparaît. Quand il ne reste qu'un onglet, le `×` est masqué
   (le dernier onglet ne peut pas être fermé).
6. **Non-régression volets** : dans un onglet, découper un volet (W1/G3),
   redimensionner la bordure, fermer un volet — tout fonctionne comme avant, par
   onglet.

## Vérification manuelle W3 (rail enrichi : cwd + branche git)

Prérequis : rebuild + **redémarrage du daemon détaché** (changement de protocole
`SessionInfo`), shell par défaut PowerShell/pwsh, puis `cd wimux-gui && npm run tauri dev`.

1. **Session PowerShell** : créer/attacher une session (shell par défaut). Sous
   son nom dans le rail apparaît une **2e ligne** : le cwd abrégé (`~` pour le
   profil utilisateur) et, si le dossier est un dépôt git, la branche (`⎇ <nom>`).
2. **Suivi des `cd`** : dans le terminal, `cd` vers un **dépôt git** (ex. le repo
   `wimux`). En ~1 s (sondage `list-sessions`), la 2e ligne se met à jour : le
   cwd suit, et la branche affiche la branche courante (ex. `⎇ main`).
   Changer de branche (`git switch -c essai`) → la ligne reflète `⎇ essai`.
3. **Hors dépôt** : `cd C:\Windows` → le cwd s'affiche, **sans** branche.
4. **Repli cmd.exe** : lancer une session avec un shell non-PowerShell
   (`set default-shell cmd.exe` dans `%USERPROFILE%\.wimux.conf`, ou
   `WIMUX_SHELL=cmd.exe`). Cette session n'émet pas d'OSC 7 → le rail affiche le
   **nom seul** (pas de 2e ligne, aucune erreur). Aucune régression.
5. **Non-régression indicateurs** : les pastilles d'activité/cloche (G4) et les
   glyphes d'agent (M-series) restent affichés à droite du nom, sur la 1re ligne.
