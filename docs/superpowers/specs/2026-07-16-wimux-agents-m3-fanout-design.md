# Design — wimux multi-agents M3 : orchestration fan-out

- **Date** : 2026-07-16
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : la fonctionnalité **multi-agents** de wimux (façon CMUX)
- **Prérequis** : GUI G1→G4 + **M1** (statut d'agent) + **M2** (création + lanceur) faits et fusionnés dans `main`.

## Contexte : décomposition multi-agents

- **M1** *(fait)* — statut d'agent (`agent_status` sur `SessionInfo`, non-reap).
- **M2** *(fait)* — création de session agent + lanceur GUI + glyphe de statut.
- **M3** *(ce document)* — **orchestration fan-out** : lancer la même tâche sur N agents en parallèle, chacun isolé dans un worktree git, + tableau de bord agrégé.
- **M4** — revue & agrégation des résultats (comparer les diffs, choisir/fusionner le gagnant).

## Objectif (M3)

Lancer un **lot** de N agents (même template) sur la même tâche, **chacun dans son
propre worktree git** d'un repo de base (isolation : les agents ne se marchent pas
dessus), et voir leur **statut agrégé** regroupé dans le rail. Réutilise M2 (spawn
avec `cwd`) et M1 (statut par agent).

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Isolation | **Un worktree git par agent** (`git worktree add`), lancé dedans ; les agents modifient des copies indépendantes |
| Composition du lot | **Même template ×N** (N tentatives indépendantes du même agent sur la tâche) |
| Orchestration | **Côté serveur** : un seul message crée les N worktrees + N sessions (les worktrees doivent être créés/suivis/nettoyés de façon coordonnée) |
| Tableau de bord | **Regroupement dans le rail** : en-tête de lot + agrégat des statuts + membres, pas de panneau séparé |
| Nettoyage | **Automatique** : tuer un agent de lot (ou fermer le lot) retire son worktree (`git worktree remove --force`) + sa branche |

## Modèle : le lot (batch)

Un **lot** est un groupe de N sessions agent partageant un identifiant **`group`**.
Chaque session tourne dans son worktree git isolé. La création est atomique côté
serveur (un `CreateAgentBatch`). Chaque session est nommée
`<template>-<group>-<i>` (i = 0..N). Le statut de chaque agent est déjà calculé par
M1 (`agent_status`) ; le rail agrège par `group`.

## Worktrees git

- **Repo de base** : fourni au lancement, doit être un dépôt git (sinon `Error`).
- **Création** : pour chaque agent `i` : `git -C <base> worktree add <root>/<group>-<i> -b wimux/<group>/<i> HEAD`.
  L'agent est spawné avec `cwd = <root>/<group>-<i>` (réutilise le `cwd` de M2).
- **Emplacement** : une **racine de worktrees configurable** (`agent-worktree-root`,
  défaut sous un dossier applicatif/temp, p. ex. `%LOCALAPPDATA%\wimux\worktrees`),
  pour ne pas polluer le repo de base. Chaque worktree dans un sous-dossier
  `<group>-<i>`.
- **Branches** : `wimux/<group>/<i>` (créée par `worktree add -b`), pour que M4
  puisse comparer/diffuser les résultats.
- **Nettoyage** (automatique) : quand une session à worktree est tuée
  (`kill_session`) : `git -C <base> worktree remove --force <path>` puis
  `git -C <base> branch -D wimux/<group>/<i>`. « Fermer le lot » = tuer les N
  membres (chacun nettoie son worktree). Comme les sessions agent ne se font
  **pas** reaper (M1), aucun nettoyage surprise : il n'a lieu qu'à la fermeture
  explicite.
- **Prérequis/échecs** : `git` absent ou base non-git → `Error` clair, **aucun**
  worktree/agent créé. Échec au i-ème worktree → nettoyage des worktrees déjà
  créés (0..i) puis `Error` (création atomique).
- **Robustesse** : un worktree orphelin (serveur tué brutalement) est toléré ;
  `git worktree prune` best-effort au prochain nettoyage. `group` unique (compteur
  par démon) → pas de collision de branches.

## Protocole (ajouts)

- `ClientMessage::CreateAgentBatch { template: String, prompt: String, base_repo: String, count: u32 }`
  → `ServerMessage::BatchCreated { group: String, sessions: Vec<String> }` (ou `Error`).
- `SessionInfo` gagne `group: Option<String>` (pour regrouper dans le rail). Le
  statut par agent reste `agent_status` (M1).

Réutilisés : `ClientMessage::Kill { name }` (tue une session, nettoie son
worktree) ; `List` (renseigne `group`/`agent_status`).

## Serveur

- **`Session`** gagne `group: Option<String>` et `worktree: Option<Worktree>` où
  `Worktree { base_repo: PathBuf, path: PathBuf, branch: String }`. Setters
  internes posés à la création de lot.
- **Nouveau module `worktree.rs`** : `add(base, path, branch) -> Result<()>`
  (`git -C base worktree add path -b branch HEAD`), `remove(base, path, branch)`
  (`git -C base worktree remove --force path` + `git -C base branch -D branch`),
  et une vérif « base est un repo git » (`git -C base rev-parse --is-inside-work-tree`).
  S'appuie sur `std::process::Command` (git doit être dans le PATH).
- **`Server::create_agent_batch(template, prompt, base_repo, count)`** : vérifie
  la base (git), résout le template (sinon `Err`), génère un `group` (compteur
  `AtomicU64`), boucle `i` : crée le worktree, `Session::new_agent(...)` avec
  `cwd = worktree`, pose `group`/`worktree`, substitue `{prompt}`/stdin (comme M2),
  insère sous le nom `<template>-<group>-<i>`. Échec partiel → nettoie les
  worktrees créés + retire les sessions insérées, renvoie `Err`.
- **`Session::kill`** : après avoir tué les volets, si `worktree` est présent,
  appelle `worktree::remove(...)`. (Les sessions agent ne se font pas reaper, donc
  le nettoyage n'a lieu qu'au `kill` explicite.)
- **`Server::list`** renseigne `group = s.group()`.

## Frontend

- **Dialogue fan-out** : un bouton « ⇉ lot » (près de « + agent ») ouvre un modal :
  champ **repo de base**, menu **template** (peuplé via `list_agent_templates`),
  zone **prompt/tâche**, champ **nombre N** (entier ≥ 1), boutons Lancer/Annuler.
  Lancer → commande Tauri `create_batch(template, prompt, baseRepo, count)` →
  `CreateAgentBatch`. Erreur (base non-git, git absent…) affichée dans le modal.
- **Regroupement dans le rail** : les `SessionDto` d'un même `group` sont rendus
  sous un **en-tête de lot** (nom du groupe) affichant l'**agrégat** des statuts
  (compteurs, p. ex. `⚙2 ✓1 ✗0`), avec les membres listés en dessous (glyphe M2
  chacun, clic = focus). Un bouton **« fermer le lot »** sur l'en-tête tue les N
  membres (boucle `kill_session`, chacun nettoie son worktree). Les sessions
  **sans** `group` restent affichées comme avant (pastilles G4 / glyphe agent M2).
- Le pont Tauri : `SessionDto` gagne `group: Option<String>` ; commande
  `create_batch(...)`.

## Tests

- **Serveur (intégration `gui_mode.rs`)** :
  - `create_batch` sur un **repo git temporaire** (le test l'initialise :
    `git init` + un commit dans un dossier temp) avec un template déterministe
    (`cmd.exe /c echo {prompt}`) et `count = 2` : `BatchCreated` renvoie un `group`
    et 2 noms ; `List` montre 2 sessions même `group`, `agent = true` ; les 2
    dossiers de worktree **existent** ; après `Kill` des 2, les dossiers de
    worktree **n'existent plus** (nettoyage) et les branches sont supprimées.
  - Base non-git → `Error`, aucun worktree créé.
  - (Le test skippe proprement si `git` n'est pas dans le PATH — assertion
    conditionnelle documentée.)
- **`worktree.rs` (unitaire/intégration)** : `add` crée un worktree + branche sur
  un repo temp ; `remove` les supprime ; vérif « est un repo git ».
- **Frontend** : validé **manuellement** (README) — lancer un lot, voir
  l'en-tête + l'agrégat évoluer, fermer le lot.
- **Non-régression** : suites TUI + G1/G2/G3/G4 + M1 + M2 vertes ; fmt + clippy
  `-D warnings` ; `npm run build` OK.

## Hors-périmètre M3 (rappel)

Revue/comparaison des résultats — diffs des branches `wimux/<group>/<i>`, choisir
la meilleure tentative, **fusionner** le worktree gagnant dans le repo de base →
**M4**. Lot **multi-templates** (M3 = même template ×N). Édition de
`agent-worktree-root` depuis la GUI (config fichier en M3). Sélecteur de repo
natif (champ texte en M3). Reprise/relance d'un agent échoué du lot.

## Risques

| Risque | Parade |
|---|---|
| `git` absent du PATH / base non-git | Vérif préalable (`rev-parse --is-inside-work-tree`) → `Error` clair au dialogue ; tests conditionnels |
| Nettoyage d'un worktree dont l'agent tourne encore | `git worktree remove --force` (tue-le d'abord via `Session::kill` des volets) |
| Collisions de branches entre lots | `group` unique (compteur par démon) → branches `wimux/<group>/<i>` distinctes |
| Worktree orphelin si le serveur meurt | Toléré ; `git worktree prune` best-effort ; le nettoyage nominal reste au `kill` |
| Échec partiel de création (i-ème worktree KO) | Création atomique : rollback des worktrees/sessions déjà créés, `Err` |
| `SessionInfo`/`SessionDto` étendus | Rebuild conjoint serveur + CLI + GUI ; cf. piège du daemon persistant |
| Écriture de config (`agent-worktree-root`) via PowerShell | Écrire `.wimux.conf` **sans BOM** (le BOM avale la 1re directive) — cf. piège consigné |
