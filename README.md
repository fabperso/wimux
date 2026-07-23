# wimux

Multiplexeur de terminal **natif Windows** : sessions persistantes, detach/reattach, fenêtres et volets — les concepts de tmux, l'ergonomie de Zellij, pensé pour PowerShell et Windows Terminal.

**Statut : phase 3 en cours — vrai multiplexeur** (juillet 2026). Sessions
persistantes (jalon J2), **découpes de volets, fenêtres multiples, navigation et
barre de statut**. `wimux new -s dev` → détacher (`Ctrl-b d`) ou fermer la fenêtre
→ `wimux attach dev` retrouve la session vivante. Le serveur compose la vue
(volets + bordures + statut) et interprète le préfixe `Ctrl-b`, comme tmux.

## Documents

1. [État des lieux](docs/01-etat-des-lieux.md) — ce qui existe (ou pas) sur Windows et le positionnement du projet.
2. [Cahier des charges fonctionnel](docs/02-fonctionnalites.md) — inventaire des fonctionnalités tmux/Screen/Zellij, priorisées P0 → P3.
3. [Plan de développement](docs/03-plan-developpement.md) — choix techniques (Rust, ConPTY, Named Pipes), architecture client/serveur, phases et jalons.
4. [Décisions d'architecture (ADR)](docs/adr/) — [0001 choix de Rust](docs/adr/0001-choix-langage-rust.md), [0002 leçons du PoC ConPTY](docs/adr/0002-conpty-lecons-du-poc.md), [0003 crate d'émulation VT](docs/adr/0003-crate-emulation-vt.md), [0004 IPC overlapped](docs/adr/0004-ipc-overlapped-io.md), [0005 format de configuration](docs/adr/0005-format-configuration.md).

## Essayer

```sh
cargo build --release
target/release/wimux new -s dev      # crée une session et s'y attache
#   … travailler dans le shell, puis Ctrl-b d pour se détacher …
target/release/wimux ls              # liste les sessions
target/release/wimux attach dev      # se rattache

# Scriptable (sans s'attacher) — le point fort côté automatisation :
target/release/wimux send-keys -t dev "npm test" Enter
target/release/wimux split-window -t dev -h    # découpe la session dev
target/release/wimux capture-pane -t dev       # récupère le contenu du volet
target/release/wimux list-panes -t dev         # liste les volets
```

## Configuration

wimux lit `%USERPROFILE%\.wimux.conf` au démarrage (syntaxe façon tmux). Voir
[l'exemple commenté](docs/wimux.conf.example) :

```text
set prefix C-a
set default-shell pwsh.exe
bind | split-window -h
bind - split-window -v
```

### Raccourcis (préfixe `Ctrl-b`)

| Touche | Action |
|--------|--------|
| `d` | Se détacher (la session survit) |
| `%` | Découper le volet (côte à côte) |
| `"` | Découper le volet (empilé) |
| `h` `j` `k` `l` | Aller au volet gauche/bas/haut/droite |
| `H` `J` `K` `L` | Redimensionner le volet (gauche/bas/haut/droite) |
| `z` | Zoom du volet actif (plein écran, `Z` dans la barre) |
| `o` | Volet suivant |
| `x` | Fermer le volet actif |
| `c` | Nouvelle fenêtre |
| `n` / `p` | Fenêtre suivante / précédente |
| `0`–`9` | Aller à la fenêtre N |
| `[` | Entrer en **mode copie** (défilement de l'historique) |
| `]` | Coller le dernier texte copié |
| `:` | Invite de commande (`split-window -h`, `new-window`, ...) |

### Souris

Activée par défaut (désactivable avec `set mouse off` dans la config) :

- **Molette** dans un volet → entre en mode copie et fait défiler l'historique.
- **Clic gauche** sur un volet → le rend actif.

### Mode copie (après `Ctrl-b [`)

Navigation façon vi dans le scrollback :

| Touche | Action |
|--------|--------|
| `j` / `k` | Descendre / monter d'une ligne |
| `Ctrl-u` / `Ctrl-d` | Demi-page haut / bas |
| `g` / `G` | Début / fin du scrollback |
| `h` / `l` / `0` / `$` | Déplacer le curseur dans la ligne |
| `w` / `b` | Mot suivant / précédent |
| `/` / `?` | Rechercher vers l'avant / l'arrière |
| `n` / `N` | Correspondance suivante / précédente |
| `Espace` | Démarrer la sélection |
| `y` ou `Entrée` | Copier (→ presse-papiers Windows) et quitter |
| `q` ou `Échap` | Quitter sans copier |

## Volet navigateur

Un volet de la disposition peut être un navigateur : bouton 🌐 sur la barre d'un
volet terminal, ou `wimux browser open --url http://localhost:5173/`. Le volet est
possédé par le serveur : il **survit au redémarrage de la GUI**, avec son URL.

Trois limites, assumées :

- **Certains sites refusent l'affichage en cadre** (en-tête `X-Frame-Options` ou
  `CSP frame-ancestors`) et resteront blancs. Ce refus n'est pas détectable de
  façon fiable depuis l'application : on ne peut donc pas afficher un diagnostic
  précis, seulement un avertissement général. Le cas d'usage visé est la
  prévisualisation d'un **serveur de développement local**, qui fonctionne.
- **Précédent/suivant parcourent l'historique de wimux**, c'est-à-dire les URL
  posées via la barre d'adresse ou l'ouverture du volet — pas celui du site. Les
  navigations faites *à l'intérieur* de la page (clic sur un lien) ne nous sont pas
  visibles quand elle est d'une autre origine : si vous cliquez un lien puis
  appuyez sur ◀, wimux ne connaît pas la page affichée par le lien — ◀ vous
  ramène à la **dernière URL connue de wimux** (celle d'avant le clic), pas à
  une page intermédiaire que le site aurait pu montrer.
- **Reparenter l'iframe recharge la page.** `PaneManager.renderLayout` reconstruit
  le DOM (`mount.replaceChildren(root)`) à chaque changement structurel de la
  disposition — découpe, fermeture, bascule d'onglet, ré-attache. Or déplacer un
  `<iframe>` existant vers un nouveau parent détruit son contexte de navigation :
  la page se recharge intégralement, avec perte de l'état applicatif et du
  défilement. Ce n'est pas corrigé (il faudrait déplacer les nœuds DOM existants
  au lieu de reconstruire l'arbre) — seulement documenté ici honnêtement.

## Navigateur pilotable (CDP)

En plus du volet iframe ci-dessus, `wimux browser` pilote un navigateur
Chromium (Edge/Chrome) via CDP, sans afficher de fenêtre par défaut
(**headless**) : c'est plus fiable pour l'automatisation clavier — certains
gestionnaires JS de page ne reçoivent pas toujours `Input.dispatchKeyEvent`
quand le processus tourne en mode « tête » (fenêtre visible) mais en
arrière-plan. Pour repasser en mode visible (la « vitrine », utile pour
observer ou démontrer un scénario), ajoute dans la config wimux :

```
set browser-headless off
```

Commandes de base :

```
wimux browser launch                       # démarre le moteur
wimux browser navigate --url <url>         # charge une page (vide les refs)
wimux browser url                          # URL courante
wimux browser snapshot                     # arbre d'accessibilité + [ref=eN]
wimux browser screenshot                   # capture PNG
wimux browser status                       # état du moteur
wimux browser close                        # arrête le moteur
```

### Actions (B2.2)

`snapshot` liste les éléments interactifs avec une référence `[ref=eN]`
(ex. `[ref=e3] textbox "Email"`). Ces refs servent de cible aux verbes
d'action ci-dessous via `--ref eN` ; elles sont **vidées à chaque
navigation** (une ref d'une page précédente est rejetée avec une erreur).

| Verbe | Usage |
|-------|-------|
| `click` | `wimux browser click --ref <eN>` |
| `type` | `wimux browser type --ref <eN> --text <texte>` |
| `press` | `wimux browser press <touche> [--ref <eN>]` |
| `scroll` | `wimux browser scroll --ref <eN>` \| `--dy <entier>` |
| `wait` | `wimux browser wait --text <s>` \| `--ms <n>` \| `--settle` |

Exemple minimal (connexion sur une page de login) :

```
wimux browser navigate --url https://example.com/login
wimux browser snapshot                 # repère [ref=e3] textbox "Email", [ref=e7] button "Se connecter"
wimux browser type --ref e3 --text "moi@example.com"
wimux browser press Tab
wimux browser wait --settle
wimux browser click --ref e7           # action sortante : à confirmer avec l'utilisateur
```

**Sécurité :** ces verbes exécutent mécaniquement ce qu'on leur demande, sans
jugement sur le contenu de la page. Ne jamais saisir d'identifiants, mots de
passe ou données financières via `type`. Toute action irréversible ou
sortante — un `click` sur un bouton de soumission, un `press Enter` qui
valide un formulaire — doit être confirmée avec l'utilisateur avant d'être
lancée, exactement comme pour toute autre action de ce type dans wimux.

### Scripting (B2.3)

Au-delà des actions mécaniques ci-dessus, ces trois verbes exécutent du
JavaScript arbitraire dans la page :

| Verbe | Usage |
|-------|-------|
| `eval` | `wimux browser eval "<js>"` — exécute une expression JS, attend les promesses, renvoie du JSON. Pour plusieurs instructions, utiliser une IIFE : `(()=>{ … })()`. |
| `select` | `wimux browser select --ref <eN> --value <v>` — choisit une option de `<select>` par valeur, sinon par texte visible. |
| `addscript` | `wimux browser addscript "<js>"` — enregistre un script exécuté au tout début de chaque futur chargement de page ; renvoie un identifiant de script. |

Exemple (naviguer, extraire une donnée via `eval`, puis choisir une option) :

```
wimux browser navigate --url https://example.com/data
wimux browser eval "(() => JSON.parse(document.querySelector('#payload').textContent).total)()"
wimux browser select --ref e5 --value "France"
```

**Sécurité :** `eval` et `addscript` exécutent du JavaScript arbitraire dans
la page — c'est un pouvoir strictement plus large que les actions mécaniques
ci-dessus. Ne jamais laisser le **contenu d'une page** (un texte lu dans un
snapshot, une réponse réseau, etc.) **dicter quel JavaScript évaluer** : un
site malveillant ou compromis pourrait ainsi injecter des instructions qui se
font passer pour celles de l'utilisateur (boucle d'injection de prompt). Le
JS évalué doit toujours venir d'une instruction explicite de l'opérateur, pas
du texte d'une page. Ne pas utiliser `eval`/`addscript` pour exfiltrer des
identifiants ou des données financières, ni pour déclencher une action
sortante (`fetch` en POST, soumission de formulaire, etc.) sans confirmation
préalable de l'utilisateur. Le résultat d'un `eval` doit lui-même être traité
comme une donnée non fiable, pas comme une instruction.

## Résumé du plan

- **Stack** : Rust · ConPTY (`portable-pty`) · émulation VT côté serveur (`wezterm-term`) · IPC par Named Pipes · client TUI (`crossterm`).
- **Architecture** : un serveur détaché par utilisateur (source de vérité, survit à la fermeture du terminal) + des clients légers qui s'attachent depuis n'importe quel terminal VT.
- **Jalon clé (J2)** : `wimux new -s dev` → fermer la fenêtre → `wimux attach -t dev` retrouve la session vivante.
- **MVP (v0.1)** : ~9-12 semaines · **v1.0** installable via winget : ~5-6 mois.

## Skill Claude (orchestration d'agents)

wimux fournit un skill dans `skills/wimux/` qui apprend à Claude à créer des
volets-agents et lire leur sortie via `wimux agent`. Pour l'activer, lie ou copie
`skills/wimux` dans le dossier des skills de ton client Claude (par ex.
`~/.claude/skills/wimux`), puis lance Claude **depuis un volet wimux**. Vérifie
avec `wimux agent whoami`.
