# Design — wimux M4 : revue de lot et intégration par Pull Request

- **Date** : 2026-07-21
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : la fonctionnalité **multi-agents** de wimux
- **Prérequis** : M1 (statut) + M2 (création) + M3 (fan-out worktrees) + **A1** (CLI `wimux agent` + skill) faits et fusionnés dans `main`.

## Contexte : décomposition multi-agents

- **M1** *(fait)* — statut d'agent.
- **M2** *(fait)* — création de session agent + lanceur GUI.
- **M3** *(fait)* — orchestration fan-out : N agents, un worktree git isolé chacun,
  branche `wimux/<group>/<i>`, nettoyage automatique au `kill`.
- **A1** *(fait)* — pilotage par Claude via la CLI `wimux agent` + skill.
- **M4** *(ce document)* — **revue des résultats du lot et intégration du gagnant
  par Pull Request**, arbitrée par Claude.

## Ce que fait (et ne fait pas) CMUX — vérifié

Décision de conception prise **après vérification** de la doc CMUX, et non par
extrapolation :

- La doc cmux ne contient **aucune** page sur le fan-out parallèle d'une même
  tâche, les runs/attempts, un diff viewer, une évaluation automatique ou une
  intégration merge/PR. Le multi-agent de cmux s'arrête à de la **visibilité** :
  un `git worktree` par onglet, onglets affichant **branche + numéro/statut de
  PR**, anneaux de notification. La comparaison, la sélection et l'intégration
  sont faites **par l'humain, hors de cmux**.
- Le **« crown evaluator »** (évaluateur désignant le meilleur run, surchargeable)
  n'apparaît que dans des sources **secondaires** et est attribué à **Manaflow**,
  produit distinct du terminal cmux. À traiter comme une piste, pas une référence.

**Conséquences** : M3 a déjà emmené wimux **au-delà** de cmux ; M4 est du terrain
**original**. Deux idées sont néanmoins empruntées : l'**intégration par PR**
(cmux rend visible branche + statut de PR par worktree — signe que le flux réel
passe par la PR) et la **justification du choix** du gagnant (idée « crown »),
intégrée sans étape séparée : elle vit dans le corps de la PR écrit par Claude.

## Objectif (M4)

Permettre à **Claude** (via le skill A1) de : créer un lot de N agents, lire un
**résumé** par agent, demander le **diff complet** de ceux qui l'intéressent,
désigner un gagnant et **ouvrir une PR** avec son travail — les perdants étant
nettoyés, le gagnant restant vivant pour traiter les retours de revue.

## Décisions (validées lors du brainstorm)

| Sujet | Décision |
|---|---|
| Consommateur | **CLI d'abord**, Claude arbitre (prolonge A1). Pas de panneau GUI en M4 |
| Travail non commité | **Tout compte** : le diff = tout ce qui diffère de la base (commité + en cours + non suivi) ; wimux **commite le WIP** du gagnant avant intégration |
| Intégration | **Par Pull Request** (`gh`) : commit du WIP → push de la branche → `gh pr create`. **Pivot** assumé vs un merge local |
| Après la PR | **Garder le gagnant** (session + worktree vivants sur sa branche poussée, pour itérer sur la revue), **nettoyer les perdants** (worktrees + branches locales, jamais poussées) |
| Titre/corps de PR | **Claude les fournit** (`--title`/`--body`) ; **repli mécanique** si absents ; wimux ajoute **toujours** un pied de page de provenance |
| Création de lot | **Nouveau verbe CLI** — `CreateAgentBatch` existe au protocole mais n'était exposé qu'en GUI (manque de M3 qui bloquerait la boucle de Claude) |

## Simplification apportée par le pivot PR

La voie PR ne touche **jamais** au répertoire de travail du repo de base : on ne
modifie que le worktree du gagnant (commit), le remote (push) et GitHub (PR). Le
garde-fou « refuser si la base est sale », nécessaire à un merge local, **n'existe
pas** ici.

## Surface CLI `wimux batch`

- `wimux batch create --repo <path> --template <t> --prompt "…" --count N`
  → `{"group":"batch0","sessions":[…]}` *(comble le manque CLI de M3)*
- `wimux batch list [--json]` → lots en cours + membres
- `wimux batch review -g <group>` → par agent : session, index, branche, statut,
  fichiers changés, `+/-` lignes, présence de commits, nombre de non-suivis
- `wimux batch diff -g <group> -i <n>` (ou `-s <session>`) → diff complet
- `wimux batch pr -g <group> -i <n> [--title "…"] [--body "…"]` → commit WIP +
  push + PR + nettoyage des perdants

**Deux niveaux volontaires** (`review` puis `diff`) pour ménager le contexte de
Claude : le résumé d'abord, le diff complet seulement pour les agents retenus.

## Serveur : ce que « le diff » calcule

- `Worktree` gagne **`base_sha`** et **`base_branch`**, capturés à la création du
  lot (`git -C <base> rev-parse HEAD` et `rev-parse --abbrev-ref HEAD`).
  `base_sha` est la référence de comparaison **stable** même si la base avance ;
  `base_branch` est la cible de la future PR.
- Avant tout calcul : `git -C <wt> add -A -N` (*intent-to-add*) — idempotent et
  non destructif, il rend les **fichiers non suivis** visibles à `git diff` sans
  rien commiter.
- Résumé : `git -C <wt> diff --stat <base_sha>` (+ comptage des non-suivis).
- Diff complet : `git -C <wt> diff <base_sha>`.
- Présence de commits : `git -C <wt> rev-list --count <base_sha>..HEAD`.

## Serveur : l'intégration par PR

1. **Gardes** (échec ⇒ `Error` clair, **aucun effet de bord**) : `gh` présent et
   authentifié (`gh auth status`) ; remote `origin` existant ; l'agent a
   **réellement produit** quelque chose (diff non vide).
2. **Commit du WIP** dans le worktree gagnant : `git add -A` puis commit **s'il y
   a lieu**, avec l'identité `wimux <wimux@localhost>` (passée en `-c`) : marque
   honnêtement un commit machine ; les commits faits par l'agent lui-même gardent
   leur identité d'origine.
3. **Push** : `git -C <wt> push -u origin <branche>`.
4. **PR** : `gh pr create --base <base_branch> --head <branche> --title … --body …`.
   - **Titre/corps fournis par Claude** (`--title`/`--body`) — c'est là que va sa
     justification du choix du gagnant.
   - **Repli mécanique** si absents : titre = prompt du lot tronqué ; corps =
     agent + lot + `--stat`.
   - **Pied de page toujours ajouté** par wimux : lot, nom de session de l'agent,
     branche, `--stat` du diff (provenance tracée quoi qu'il arrive).
5. **Nettoyage** : kill des sessions **perdantes** uniquement (chacune retire son
   worktree + sa branche locale via M3). Le gagnant reste vivant.

**Rationale du partage des rôles** : wimux n'a aucune compréhension sémantique du
travail (il n'a que le prompt brut, un nom d'agent et un `--stat`) ; Claude vient
de lire les N diffs et de trancher. wimux fait la **plomberie**, l'intelligence
vit dans l'agent et le skill — même principe qu'en A1.

## Protocole (ajouts — additifs, en fin d'enum)

- `ClientMessage::ListBatches` → `ServerMessage::Batches(Vec<BatchInfo>)`
- `ClientMessage::ReviewBatch { group: String }` → `ServerMessage::BatchReview(Vec<AgentResult>)`
- `ClientMessage::DiffAgent { session: String }` → `ServerMessage::AgentDiff(String)`
- `ClientMessage::OpenPr { session: String, title: Option<String>, body: Option<String> }`
  → `ServerMessage::PrOpened { url: String }` ou `Error`

```rust
pub struct BatchInfo {
    pub group: String,
    pub sessions: Vec<String>,
    pub base_repo: String,
    pub base_branch: String,
}

pub struct AgentResult {
    pub session: String,
    pub index: u32,
    pub branch: String,
    pub status: Option<AgentStatus>, // réutilise M1
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
    pub untracked: u32,
    pub has_commits: bool,
}
```

La **création de lot** réutilise `CreateAgentBatch` (M3) — seule la CLI est
nouvelle. Un agent est désigné par **son nom de session** (unique) ; la CLI
résout `-g <group> -i <n>` vers ce nom.

`AgentResult.index` est le rang `i` de l'agent dans son lot. M3 nomme les
sessions `<template>-<group>-<i>` : l'index est donc **dérivé du suffixe du nom de
session** au moment de construire la réponse (l'ordre de `BatchInfo.sessions` le
reflète également). Aucun stockage supplémentaire n'est nécessaire.

## Correctif inclus : branche d'un worktree

`git.rs::git_branch` renvoie `None` quand `.git` est un **fichier** — précisément
le cas d'un worktree. Conséquence actuelle : les sessions d'un lot n'affichent
**aucune branche** dans le rail. Correctif : suivre le `gitdir: <path>` pointé par
le fichier `.git` et lire le `HEAD` de ce répertoire. Petit, et au cœur de M4
(qui manipule ces branches).

## Skill

Nouvelle section « revue de lot » dans `skills/wimux/SKILL.md` : la boucle
`create → review → diff → pr`, avec deux consignes explicites — passer par
`review` **avant** `diff` (économie de contexte), et **toujours fournir**
`--title`/`--body` (la justification du choix y vit).

## Erreurs

| Cas | Comportement |
|---|---|
| `gh` absent ou non authentifié | `Error` explicite, rien de fait |
| Pas de remote `origin` | `Error` explicite, rien de fait |
| Groupe / session inconnus | `Error` explicite |
| Agent sans production (diff vide) | `Error` (« cet agent n'a rien produit »), pas de PR |
| Push OK mais `gh pr create` échoue | `Error` **explicite sur l'état réel** : la branche est poussée, la PR reste à ouvrir (pas de nettoyage des perdants dans ce cas) |
| Repo de base non-git | `Error` (déjà garanti par M3 à la création) |

## Tests

- **Protocole** : round-trip postcard des nouveaux messages + `BatchInfo` / `AgentResult`.
- **Serveur (repo git temporaire)** : lot de 2 agents — l'un **commite**, l'autre
  laisse du **WIP + un fichier non suivi** ; `review` renseigne correctement
  `files_changed`/`insertions`/`untracked`/`has_commits` pour les deux ; `diff`
  contient le travail des deux formes ; `pr` **refuse proprement** sans `gh` ou
  sans remote ; le nettoyage ne retire que les perdants (worktrees disparus) et
  **préserve** le gagnant.
- **Conditionnels** : comme en M3, les tests git sont ignorés proprement si `git`
  est absent. **Aucune PR réelle n'est créée en test** — les chemins `gh`/réseau
  sont testés par leurs gardes (absence/refus), pas par un appel réel.
- **`git_branch`** : nouveau test — un `.git` fichier `gitdir: …` renvoie bien la
  branche du répertoire pointé (remplace l'ancien test qui attendait `None`).
- **Non-régression** : suites TUI + GUI + M1/M2/M3 + A1 vertes ; `fmt` + `clippy
  -D warnings` ; `npm run build` OK.

## Découpage prévisionnel (pour le plan)

- **M4.1** — Protocole : `BatchInfo`/`AgentResult` + 4 messages ; `Worktree`
  gagne `base_sha`/`base_branch` (capturés à la création du lot).
- **M4.2** — Serveur : collecte (`review` + `diff`) via git.
- **M4.3** — Serveur : intégration PR (gardes, commit WIP, push, `gh pr create`,
  nettoyage des perdants).
- **M4.4** — CLI : namespace `wimux batch` (create/list/review/diff/pr) + JSON.
- **M4.5** — Skill (section revue de lot) + correctif `git_branch` worktree.

## Hors-périmètre M4

- **Panneau GUI de revue** (diff viewer côte à côte) — M4 est CLI-first ; une
  consommation GUI viendra si le besoin s'en fait sentir.
- **Évaluation automatique** par un modèle côté serveur (« crown evaluator ») :
  c'est Claude qui arbitre, wimux ne score pas.
- **Merge local** comme alternative à la PR (écarté au brainstorm).
- Lot **multi-templates** (M3 = même template ×N) ; reprise/relance d'un agent
  échoué ; suivi du statut de la PR dans le rail (idée cmux, à part).

## Risques

| Risque | Parade |
|---|---|
| `gh` absent / non authentifié / pas de remote | Gardes explicites en amont, `Error` clair, aucun effet de bord |
| Effet de bord partiel (push OK, PR KO) | État réel annoncé dans l'erreur ; pas de nettoyage dans ce cas — rien n'est perdu |
| `git add -A -N` mute l'index de l'agent | *intent-to-add* est non destructif et idempotent ; aucun fichier n'est commité par la collecte |
| Commit auto sous une mauvaise identité | Identité `wimux <wimux@localhost>` passée en `-c` : n'écrase pas la config du repo et distingue les commits machine |
| Nettoyage des perdants trop agressif | Leurs branches ne sont **jamais poussées** ; seul du travail explicitement non retenu est supprimé, et le gagnant est préservé |
| Diff énorme saturant le contexte de Claude | Surface à deux niveaux (`review` puis `diff`) + consigne du skill |
| Changement de protocole vs daemon persistant | Ajouts **en fin d'enum** ; rebuild release + redémarrage du daemon (piège consigné) |
