# Plan de développement — wimux

Multiplexeur de terminal natif Windows (sessions persistantes, detach/reattach, panes), inspiré de tmux pour les concepts et de Zellij pour l'ergonomie.

## 1. Choix techniques

### Langage : Rust (recommandé)

| Critère | Rust | C# (.NET) | C++ |
|---|---|---|---|
| Écosystème terminal | Excellent (portable-pty, vte, termwiz, wezterm-term, crossterm, ratatui — tous MIT/Apache) | Faible (wrappers ConPTY épars) | Bon (source Windows Terminal, mais difficile à extraire) |
| Binaire | Unique, sans runtime, ~2-5 Mo | Nécessite .NET ou publie AOT lourd | Unique |
| Async I/O | tokio, mature | Bon | Manuel |
| Précédents directs | WezTerm, Zellij, psmux sont en Rust | — | Windows Terminal |

### Briques logicielles

| Rôle | Crate/API | Licence |
|---|---|---|
| Pseudo-console | `portable-pty` (wrapper ConPTY de WezTerm) ou appel direct via `windows` | MIT |
| API Win32 | `windows` (officiel Microsoft) | MIT |
| Émulation VT par volet | `wezterm-term` (complet) ou `vte` + grille maison (léger) — décision au jalon PoC | MIT / Apache-2.0 |
| IPC client↔serveur | Named Pipes (`\\.\pipe\wimux-<user>-<socket>`) via `tokio` / `interprocess` | MIT |
| Protocole RPC | `serde` + `bincode` (ou `postcard`), messages versionnés | MIT |
| I/O terminal client | `crossterm` (raw mode, alternate screen, événements clavier/souris) | MIT |
| Nettoyage processus | Job Objects (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) | — |
| Configuration | Format style tmux (parseur maison) ou KDL (`kdl` crate) — trancher en phase 1 | — |

**Prérequis OS** : Windows 10 1809+ (ConPTY). Le terminal hôte doit supporter `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (Windows Terminal, VS Code, ConEmu, Alacritty, WezTerm — OK partout aujourd'hui).

### Contraintes ConPTY à intégrer dès la conception
- I/O input/output sur **threads séparés** (deadlock sinon).
- Pas de requête d'état du buffer : le serveur maintient **sa propre grille VT par volet** (d'où l'émulateur VT côté serveur).
- Pas de SIGWINCH : resize propagé explicitement client → serveur → `ResizePseudoConsole`.
- Ctrl+C : `GenerateConsoleCtrlEvent` sur le groupe de processus du volet cible.
- Reflow au resize parfois incohérent (issue microsoft/terminal#15976) : prévoir des tests dédiés.

## 2. Architecture cible

```
┌──────────────────────────────┐        ┌─────────────────────────────────────┐
│  wimux.exe (client, TUI)     │        │  wimux-server.exe (détaché,         │
│  s'exécute dans n'importe    │  Named │  CREATE_NO_WINDOW, 1 par user)      │
│  quel terminal VT            │  Pipe  │                                     │
│  ├─ crossterm (raw mode)     │◄──────►│  ├─ SessionManager                  │
│  ├─ rendu diff (compositeur) │  RPC   │  │   └─ Session ─ Window ─ Pane     │
│  └─ input → événements RPC   │        │  ├─ Pane = ConPTY + parser VT       │
└──────────────────────────────┘        │  │   + grille + scrollback          │
        (N clients possibles)           │  ├─ Job Object par volet            │
                                        │  └─ persistance d'état (JSON)       │
                                        └─────────────────────────────────────┘
                                             │ ConPTY (pipes in/out)
                                        ┌────┴────────────────────────────────┐
                                        │ powershell.exe / cmd.exe / pwsh ... │
                                        └─────────────────────────────────────┘
```

Principes :
- **Le serveur est la source de vérité** : il parse le flux VT de chaque volet et maintient grille + scrollback. Le client ne fait que rendre.
- **Le client est jetable** : détacher = fermer le client ; rattacher = nouveau client qui reçoit un snapshot de grille puis les deltas.
- **Protocole versionné** dès le premier jour (champ version dans le handshake) pour permettre les mises à jour du serveur sans casser les clients.

### Organisation du code (workspace Cargo)

```
wimux/
├─ Cargo.toml            (workspace)
├─ crates/
│  ├─ wimux-protocol/    types RPC partagés (serde)
│  ├─ wimux-vt/          parser VT + grille + scrollback (isolé, très testé)
│  ├─ wimux-server/      démon : sessions, panes, ConPTY, IPC
│  ├─ wimux-client/      TUI : rendu, input, attach/detach
│  └─ wimux-cli/         binaire `wimux` : parse la ligne de commande,
│                        lance serveur si absent, délègue au client ou envoie une commande
└─ docs/
```

## 3. Phases de développement

### Phase 0 — Fondations (≈ 1 semaine)
- [ ] Init repo Git, workspace Cargo, CI GitHub Actions (build + tests Windows).
- [ ] Squelette des 5 crates, protocole RPC minimal (handshake, version).
- [ ] Décision documentée (ADR) : format de config, crate VT retenu.

**Jalon J0** : `cargo build` vert en CI sur windows-latest.

### Phase 1 — PoC ConPTY (≈ 1-2 semaines) ⚠️ phase de dé-risquage
- [ ] Spawn de PowerShell via ConPTY (portable-pty), passthrough brut stdin/stdout dans le terminal courant (pas encore de serveur).
- [ ] Resize propagé, Ctrl+C fonctionnel, fermeture propre (Job Object).
- [ ] Tester : vim (via git), applis TUI, `cls`, couleurs 256/true color, Unicode/emoji, cmd.exe et pwsh.
- [ ] Valider le crate VT : parser le flux capturé et re-rendre la grille à l'identique.

**Jalon J1** : un shell interactif complet fonctionne à travers wimux en mode « pane unique », y compris vim et le redimensionnement. **Go/no-go sur les choix de crates.**

### Phase 2 — Serveur + detach/reattach (≈ 2-3 semaines) — le cœur de la valeur
- [ ] `wimux-server` détaché (auto-lancé par la CLI s'il n'existe pas), Named Pipe par utilisateur.
- [ ] Protocole : NewSession, Attach, Detach, ListSessions, KillSession, Input, Resize, snapshot de grille + deltas.
- [ ] Grille VT côté serveur (wimux-vt) alimentée en continu, même sans client.
- [ ] Client : raw mode, alternate screen, rendu du snapshot puis application des deltas, `prefix + d` pour détacher.
- [ ] Survie du serveur à la fermeture du terminal client (fenêtre Windows Terminal fermée → session vivante).

**Jalon J2 (= proto démontrable)** : `wimux new -s dev` → lancer un script long → fermer la fenêtre → `wimux attach -t dev` retrouve la session vivante avec son historique. **C'est la fonctionnalité qui n'existait pas sur Windows.**

### Phase 3 — Fenêtres & volets (≈ 3 semaines)
- [ ] Modèle Session → Window → arbre de splits → Pane.
- [ ] Compositeur côté client : bordures, volet actif, rendu multi-panes avec diff minimal.
- [ ] Commandes : split h/v, navigation directionnelle, kill-pane, new/next/prev/rename/kill-window, resize-pane.
- [ ] Prefix key (Ctrl+b) + table de raccourcis par défaut.
- [ ] Barre de statut basique (session, liste fenêtres, heure).

**Jalon J3** : parité « usage quotidien » de base avec tmux (sessions + fenêtres + splits + statut).

### Phase 4 — Scrollback & mode copie (≈ 2-3 semaines)
- [ ] Scrollback borné par volet (history-limit), stockage compact des lignes.
- [ ] Mode copie : navigation vi, sélection, copie vers le **presse-papiers Windows** (API Win32) + buffers internes, paste.
- [ ] Recherche / et ? avec surbrillance.
- [ ] Molette souris → scroll ; OSC 52 pour les applis qui copient elles-mêmes.

**Jalon J4** : MVP complet (tout le P0 du cahier des charges) → **v0.1 publiable**.

### Phase 5 — Configuration & commandes (≈ 2-3 semaines)
- [ ] Fichier de config chargé au démarrage du serveur + commande reload.
- [ ] `bind-key`/`unbind-key`, options avec scopes (server/session/window/pane).
- [ ] Invite de commande `prefix + :` avec complétion et historique.
- [ ] CLI scriptable : `wimux send-keys -t dev:0.1 "npm test" Enter`, `wimux capture-pane`, `list-*` avec formats `#{...}`.

**Jalon J5** : wimux est scriptable et personnalisable → **v0.2**.

### Phase 6 — Confort & parité étendue (≈ 3-4 semaines)
- [ ] Souris complète (focus au clic, drag des bordures, sélection, clic barre de statut).
- [ ] Layouts prédéfinis + cycle, zoom, swap/rotate, break-pane/join-pane, synchronize-panes.
- [ ] Multi-clients sur une session (taille = min des clients, comme tmux).
- [ ] status-left/right personnalisables, styles/couleurs, indicateurs (Z, *, !).
- [ ] `run-shell`, `if-shell`, hooks de base.

**Jalon J6** : **v0.5** — parité confortable avec l'usage tmux courant.

### Phase 7 — Distribution & qualité (≈ 2 semaines, en parallèle de la 6)
- [ ] Binaire signé si possible, release GitHub automatisée.
- [ ] Manifests **winget**, **Scoop**, éventuellement Chocolatey.
- [ ] Doc utilisateur (site ou README riche), cheatsheet, guide de migration tmux → wimux.
- [ ] Suite de tests : unités (wimux-vt exhaustivement : séquences CSI/OSC/SGR, resize/reflow), intégration (spawn réel ConPTY en CI), tests de fuzz du parser.

**Jalon J7** : **v1.0** installable par `winget install wimux`.

### Phase 8 — Différenciateurs (v2, ≈ ouvert)
- Résurrection de session après reboot (layout + cwd + commandes, à la Zellij/tmux-resurrect).
- Layouts déclaratifs par projet (fichier `wimux.layout` versionnable).
- Volets flottants, raccourcis découvrables dans la barre.
- `display-popup` / `display-menu`, monitor-activity/silence.
- Module PowerShell (cmdlets), control mode pour intégrations.

## 4. Risques principaux et parades

| Risque | Impact | Parade |
|---|---|---|
| Bizarreries ConPTY (reflow au resize, deadlocks I/O, différences selon build Windows) | Élevé | Phase 1 dédiée au dé-risquage ; threads I/O séparés ; matrice de tests Win10/Win11 ; suivre OpenConsole |
| Émulation VT incomplète (applis TUI cassées) | Élevé | Réutiliser `wezterm-term` (éprouvé) plutôt que réécrire ; corpus de tests avec vim, htop-like, applis .NET |
| Performance du rendu (gros débit de sortie, ex. `type gros_fichier`) | Moyen | Coalescing des deltas, rendu diff, backpressure sur le pipe |
| Concurrents (psmux mûrit, Zellij Windows s'améliore) | Moyen | Se différencier : UX Windows-first, PowerShell, layouts projet ; ne pas courir après la compat tmux à 100 % |
| Sécurité IPC (autre utilisateur se connectant au pipe) | Moyen | DACL sur le Named Pipe restreint à l'utilisateur courant, vérification du SID à la connexion |
| Portée du projet (tmux = ~20 ans de fonctionnalités) | Élevé | Priorités P0-P3 strictes ; chaque phase livre un outil utilisable |

## 5. Estimation globale

- **MVP utilisable (J4, v0.1)** : ~9 à 12 semaines de travail effectif pour un développeur.
- **v1.0 distribuée** : ~5 à 6 mois.
- Le point de non-retour positif est le **jalon J2** (detach/reattach) : dès qu'il est atteint, l'outil a déjà de la valeur quotidienne.

## 6. Premiers pas concrets (prochaine session de travail)

1. `git init` + workspace Cargo + CI.
2. Prototype phase 1 : `portable-pty` + PowerShell + passthrough (~200 lignes) pour valider ConPTY sur votre machine.
3. ADR n°1 : crate VT (`wezterm-term` vs `vte`+grille maison) après essai sur le flux réel du prototype.
