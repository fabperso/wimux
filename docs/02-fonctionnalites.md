# Cahier des charges fonctionnel — wimux

Inventaire des fonctionnalités des multiplexeurs de référence (tmux 3.6, GNU Screen 4.9, Zellij 0.44), classées par priorité pour la recréation sur Windows.

Légende priorité : **P0** = MVP indispensable · **P1** = v1.0 · **P2** = v2+ · **P3** = optionnel/niche

## 1. Gestion des sessions

| Fonctionnalité | Description | Priorité |
|---|---|---|
| `new-session` | Créer une session nommée ou anonyme | P0 |
| `attach-session` | Attacher un client à une session existante | P0 |
| `detach` (prefix + d) | Détacher le client, la session continue en arrière-plan | P0 |
| Persistance | Les processus survivent à la fermeture du terminal client | P0 |
| `new -A` | Attacher ou créer si absente | P0 |
| `list-sessions` / `kill-session` | Lister et détruire les sessions | P0 |
| `rename-session` | Renommer une session | P1 |
| Multi-clients | Plusieurs clients attachés à la même session, tailles différentes gérées | P1 |
| `switch-client` | Basculer un client entre sessions | P1 |
| Groupes de sessions | Sessions partageant les mêmes fenêtres | P3 |
| Résurrection après reboot | Sauvegarde/restauration du layout et des cwd (à la tmux-resurrect / Zellij) | P2 |

## 2. Fenêtres (onglets)

| Fonctionnalité | Description | Priorité |
|---|---|---|
| `new-window` (prefix + c) | Nouvel onglet dans la session | P0 |
| Navigation (n/p/l, index 0-9) | Fenêtre suivante/précédente/dernière, accès direct par numéro | P0 |
| `rename-window` | Renommer un onglet | P0 |
| `kill-window` | Fermer un onglet | P0 |
| `move-window` / renumérotation | Réordonner les onglets | P1 |
| `link-window` | Partager une fenêtre entre sessions | P3 |

## 3. Volets (panes)

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Split horizontal/vertical (prefix + % / ") | Division récursive illimitée (arbre de splits) | P0 |
| Navigation directionnelle | Sélection du volet actif par flèches/hjkl | P0 |
| `kill-pane` | Fermer un volet | P0 |
| Redimensionnement | Par incréments, ou taille absolue | P0 |
| Zoom (prefix + z) | Volet en plein écran, retour au layout précédent | P1 |
| Layouts prédéfinis | even-horizontal, even-vertical, main-horizontal, main-vertical, tiled + cycle | P1 |
| `swap-pane` / rotation | Échanger/faire tourner les volets | P1 |
| `break-pane` / `join-pane` | Extraire un volet en fenêtre, fusionner | P1 |
| `synchronize-panes` | Frappe simultanée dans tous les volets | P1 |
| Bordures + titres de volets | Affichage du titre, styles de bordures | P1 |
| Volets flottants (Zellij) | Volet en superposition, affichable/masquable | P2 |
| Volets empilés (Zellij) | Stack avec navigation type onglets | P3 |

## 4. Mode copie & scrollback

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Scrollback par volet | Historique configurable (history-limit) | P0 |
| Mode copie (prefix + [) | Navigation clavier dans l'historique | P0 |
| Keybindings vi + emacs | Deux jeux de touches configurables | P1 (vi d'abord) |
| Recherche avant/arrière | / et ? avec surbrillance et n/N | P1 |
| Sélection caractère/ligne/bloc | Sélection visuelle puis copie | P0 (caractère), P1 (bloc) |
| Buffers multiples | Historique de buffers nommés, `paste-buffer` | P1 |
| Clipboard Windows | Copie directe dans le presse-papiers système (API Win32) + OSC 52 | P0 |
| Molette souris = scroll | Entrée auto en mode copie au scroll | P1 |

## 5. Système de commandes & raccourcis

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Prefix key configurable | Ctrl+b par défaut, remappable | P0 |
| Invite de commande (prefix + :) | Exécuter toute commande, complétion, historique | P1 |
| `bind-key` / `unbind-key` | Remappage complet | P1 |
| Key tables | root, prefix, copy-mode + tables personnalisées | P1 (base), P2 (custom) |
| CLI complète | `wimux <commande>` pilotable depuis n'importe quel shell (comme `tmux send-keys ...`) | P0 |
| Aide découvrable (Zellij) | Raccourcis affichés dans la barre de statut | P2 |

## 6. Configuration

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Fichier de config | `%USERPROFILE%\.wimux.conf` + `%APPDATA%\wimux\wimux.conf` (format à définir : syntaxe tmux ou TOML/KDL) | P0 |
| Rechargement à chaud | `source-file` / commande reload | P1 |
| Scopes d'options | server → session → window → pane avec héritage | P1 |
| Hooks d'événements | after-new-session, client-attached, etc. | P2 |
| Variables de format `#{...}` | session_name, pane_current_path, conditionnelles `#{?a,b,c}` | P1 |
| Layouts déclaratifs (Zellij) | Fichiers de layout réutilisables (sessions projet prédéfinies) | P2 |

## 7. Barre de statut

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Barre basique | Nom de session + liste des fenêtres + heure | P0 |
| status-left / status-right | Segments personnalisables avec formats | P1 |
| Styles/couleurs | 256 couleurs + true color, attributs | P1 |
| Indicateurs d'état | fenêtre active (*), activité (#), zoom (Z), cloche (!) | P1 |
| Contenu dynamique `#(cmd)` | Sortie de commandes externes, intervalle de rafraîchissement | P2 |
| Clic souris sur la barre | Sélection de fenêtre au clic | P2 |

## 8. Scripting & contrôle externe

| Fonctionnalité | Description | Priorité |
|---|---|---|
| `send-keys` | Injecter des frappes dans un volet cible | P0 |
| `capture-pane` | Capturer le contenu d'un volet (avec ou sans couleurs) | P1 |
| `run-shell` / `if-shell` | Exécution shell (a)synchrone, conditionnelle | P1 |
| `wait-for` | Synchronisation de scripts | P2 |
| `display-popup` | Fenêtre popup éphémère | P2 |
| `display-menu` | Menus contextuels | P2 |
| Control mode (`-CC`) | Protocole texte pour intégration par d'autres outils (iTerm2-style) | P3 |

## 9. Fonctionnalités avancées

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Souris complète | Clic pour focus, drag des bordures, sélection, molette | P1 |
| monitor-activity / monitor-silence | Alertes d'activité/silence par fenêtre | P2 |
| Sessions imbriquées | Détection `$WIMUX`, prefix double | P2 |
| Système de plugins | À la TPM ou WASM (Zellij) | P3 |
| Logging par volet (Screen) | Enregistrement de session dans un fichier | P2 |
| Thème clair/sombre | Adaptation au thème du terminal hôte | P3 |
| Sessions collaboratives / client web (Zellij) | Multi-utilisateurs, accès navigateur | P3 |
| Port série (Screen) | Console série | P3 |

## 10. Spécifique Windows (différenciateurs)

| Fonctionnalité | Description | Priorité |
|---|---|---|
| Support PowerShell/cmd/pwsh natif | Chaque volet lance le shell Windows de son choix | P0 |
| ConPTY par volet | Émulation VT fidèle pour les applis console Windows | P0 |
| Fonctionne dans Windows Terminal, VS Code, ConEmu | Le client s'exécute dans tout terminal VT | P0 |
| Ctrl+C correct | `GenerateConsoleCtrlEvent` vers le bon groupe de processus | P0 |
| Nettoyage d'arbres de processus | Job Objects (kill-on-close) | P0 |
| Installation winget/Scoop/Chocolatey | Distribution standard Windows | P1 |
| Module PowerShell | Cmdlets `New-WimuxSession`, etc. (wrapper de la CLI) | P3 |
