# Design — Interface graphique wimux (fondation, façon CMUX)

- **Date** : 2026-07-13
- **Statut** : validé (design), en attente du plan d'implémentation

## Contexte et objectif

wimux est aujourd'hui un multiplexeur de terminal **natif Windows en mode texte
(TUI)**, façon tmux : serveur détaché (ConPTY, sessions persistantes, émulation
VT, IPC par Named Pipe) + client TUI. Cf. `docs/03-plan-developpement.md` et les
ADR 0001–0005.

L'objectif exprimé : se doter d'une **interface graphique** comparable à
**CMUX** (application native macOS 2026 : fenêtre autonome, onglets verticaux,
volets, navigateur intégré, indicateurs multi-agents). La cible est le jeu
**complet** de CMUX, mais construit **par couches**.

### Décomposition de la vision « tout CMUX »

Le CMUX complet est plusieurs sous-systèmes. On les traite en sous-projets
successifs, chacun avec son propre cycle spec → plan → implémentation :

1. **Fondation GUI** *(ce document)* : fenêtre, onglets verticaux = sessions,
   volets graphiques, indicateurs d'activité. Réutilise le serveur existant.
2. Navigateur web intégré (type de volet « web view »).
3. Orchestration multi-agents avancée (tableaux de bord, lancement d'agents).
4. Notifications au niveau de l'OS.

Ce document ne couvre que la **fondation (1)**.

## Décisions structurantes

| Sujet | Décision | Raison |
|---|---|---|
| Framework GUI | **Tauri** (cœur Rust + WebView2) | S'aligne sur notre serveur Rust ; binaires légers ; navigateur intégré natif plus tard |
| Rendu terminal | **xterm.js** (un par volet) | Bibliothèque de rendu terminal très mature |
| Disposition | **Onglets verticaux à gauche** (style CMUX) | Choix utilisateur (maquette A) |
| Style par défaut | **Sombre neutre + accent bleu** (façon VS Code) | Choix utilisateur (maquette 2) ; professionnel, familier |
| Moteur | **Le serveur wimux existant, inchangé dans son rôle** | Préserve la persistance des sessions — la force de wimux |
| Coexistence | GUI **et** TUI sont deux clients du même serveur | On ne casse rien de l'existant |

## Architecture

```
wimux-gui (app Tauri)
├─ Frontend web (TypeScript + xterm.js)
│   • rail d'onglets verticaux (= sessions)
│   • disposition des volets (HTML/CSS) + un xterm.js par volet
│   • indicateurs d'activité (pastilles)
│   ⇅ IPC Tauri (commands JS→Rust, events Rust→JS)
├─ Backend Rust (src-tauri)
│   • réutilise les crates `wimux-protocol` (transport Named Pipe + messages)
│   • se connecte au serveur wimux en « mode GUI »
│   • fait le pont : messages serveur ⇄ events/commands Tauri
│
└──► Named Pipe (mode GUI)
     wimux-server  (INCHANGÉ dans son rôle : ConPTY, sessions, arbre de
                    volets, persistance, émulation VT)
                    + NOUVEAU mode GUI dans le protocole
```

**Principe** : le serveur reste la **source de vérité** (sessions, arbre de
volets, état VT, persistance). La GUI en est un **miroir** : elle affiche l'état
et renvoie les actions. Fermer/rouvrir la fenêtre GUI retrouve les sessions
vivantes, exactement comme la TUI.

## Le mode « GUI » du protocole

Le mode TUI actuel (grille composée unique) n'est pas modifié. On **ajoute** un
mode où le serveur parle « par volet ».

### Client GUI → serveur
- `AttachGui { session }` — s'attacher en mode GUI (flux bruts par volet).
- `PaneInput { pane_id, bytes }` — frappes vers un volet précis.
- `PaneResize { pane_id, cols, rows }` — un volet a changé de taille dans la GUI.
- Commandes de structure (découper, fermer, nouvel onglet…) — réutilisent
  l'infrastructure `Command` existante, ciblées par volet/session.

### Serveur → client GUI
- `Structure { arbre sessions/fenêtres/volets }` — état complet à l'attache,
  puis deltas (volet créé/fermé/redimensionné, volet actif changé).
- `PaneSnapshot { pane_id, bytes }` — contenu courant du volet, reconstruit
  depuis la grille VT déjà maintenue, pour restaurer l'affichage xterm.js au
  (re)attachement.
- `PaneOutput { pane_id, bytes }` — flux brut du volet, en continu.
- `PaneActivity { pane_id, kind }` — activité / cloche / fin de process → nourrit
  les indicateurs.

### Impact serveur (ciblé)
- Le lecteur de chaque volet (qui lit déjà ConPTY et alimente l'émulateur VT)
  **pousse en plus** les octets bruts vers les clients GUI abonnés au volet.
- Les changements de structure émettent un événement vers les clients GUI.
- `PaneSnapshot` : générer une séquence d'octets rejouable à partir de la grille
  VT (au minimum l'écran visible ; le scrollback pourra suivre).

## Frontend

- **Rail vertical** à gauche : une entrée par session (nom + pastille
  d'activité), session active surlignée (barre bleue), bouton « + » pour créer.
- **Zone de volets** : la disposition (arbre de découpes du serveur) rendue en
  HTML/CSS (flexbox), un `xterm.js` par volet, volet actif mis en évidence.
- **Indicateurs** : pastille par session/volet — vert = actif, jaune = attend
  une saisie / activité récente, gris = inactif, rouge/gris = process terminé.
- **Thème** par défaut sombre + accent bleu ; thème changeable prévu plus tard.

## Jalons de construction (fondation)

- **G1 — Tuyauterie (dé-risquage)** : app Tauri, backend Rust connecté au
  serveur en mode GUI, **une session / un volet** dans un xterm.js, frappe
  fonctionnelle. Valide toute la chaîne.
- **G2 — Onglets verticaux** : rail des sessions, changement/création/fermeture.
- **G3 — Volets graphiques** : rendu de l'arbre de découpes, un xterm.js par
  volet, clic pour focus, redimensionnement.
- **G4 — Indicateurs** : pastilles d'activité/attention/fin par session et volet.
- **Polish** : thème bleu, polices, chrome de la fenêtre, raccourcis.

## Structure du code

- Nouveau dossier `wimux-gui/` :
  - `src-tauri/` — backend Rust (dépend de `wimux-protocol`), commandes/événements
    Tauri, pont vers le serveur.
  - `src/` — frontend TypeScript + xterm.js (build via Node/Vite).
- Le workspace Cargo existant gagne éventuellement le crate `src-tauri` ; le
  frontend a sa propre chaîne (npm/vite).

## Tests & CI

- **Serveur (mode GUI)** : tests d'intégration comme l'existant (attacher en
  mode GUI, vérifier `Structure`, `PaneOutput`, `PaneSnapshot`, `PaneInput`).
- **Pont Rust Tauri** : testable en se connectant à un serveur de test.
- **Frontend web** : validé **manuellement** (comme la TUI). Éventuel smoke test.
- **CI** : job séparé pour l'app Tauri (WebView2 + Node) ; la CI Rust existante
  (fmt/clippy/test) reste inchangée.

## Risques

| Risque | Parade |
|---|---|
| Restauration fidèle de l'affichage xterm.js au reattach | `PaneSnapshot` généré depuis la grille VT ; commencer par l'écran visible, itérer |
| Latence/volume du flux brut par volet | Coalescing des `PaneOutput` ; back-pressure ; c'est du local (Named Pipe) |
| Complexité CI Tauri (WebView2 + Node) | Job CI séparé, non bloquant pour la CI Rust |
| Périmètre « tout CMUX » trop large | Déjà décomposé en sous-projets ; cette spec ne couvre que la fondation |
| Double émulation VT (serveur + xterm.js) | Assumé : le serveur garde sa grille (TUI, capture-pane) ; la GUI délègue le rendu à xterm.js |

## Hors périmètre (rappel)

Navigateur intégré, orchestration multi-agents avancée, notifications OS : ce
sont les sous-projets 2 à 4, hors de cette fondation.
