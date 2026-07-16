# Design — wimux multi-agents M2 : création + lanceur + glyphe de statut

- **Date** : 2026-07-16
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : la fonctionnalité **multi-agents** de wimux (façon CMUX)
- **Prérequis** : GUI G1→G4 + **M1** (couche serveur de statut d'agent) faits et fusionnés dans `main`.

## Contexte : décomposition multi-agents

- **M1** *(fait)* — couche serveur de statut d'agent (`agent`/`agent_status` sur `SessionInfo`, non-reap).
- **M2** *(ce document)* — **création de sessions agent + lanceur GUI + glyphe de statut dans le rail** : ce qui rend M1 démontrable et utilisable.
- **M3** — orchestration fan-out (N agents sur une tâche).
- **M4** — revue & agrégation des résultats.

## Objectif (M2)

Permettre de **lancer un agent** depuis la GUI (choisir un template configuré,
saisir une tâche/prompt, un répertoire de travail) et **voir son statut** (calculé
par M1) via un glyphe dans le rail. Le serveur crée une session dont le processus
racine est l'agent, la marque `agent` (M1), et gère la livraison du prompt.

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Livraison du prompt | Si un argument du template contient `{prompt}` → **substitution en argument** ; sinon → prompt envoyé sur **stdin** après le spawn (+ Entrée) |
| Répertoire de travail | **Champ éditable** dans le dialogue (défaut = cwd du daemon) ; passé au spawn (`CommandBuilder::cwd`) |
| Où vivent les templates | **Config serveur** (`wimux.conf`, façon tmux) ; la **substitution se fait côté serveur** ; le frontend récupère juste la liste des templates |
| Nommage auto | `<template>-<n>` (ex. `claude-0`) si aucun nom fourni |
| Correctif M1 inclus | `Pane::kill` **ferme le `MasterPty`** pour débloquer le `reader_loop` parqué d'un agent terminé (prérequis d'une fermeture propre) |

## Config — templates d'agents

Nouvelle directive (façon tmux, une par ligne) :

```
agent-template <nom> <programme> [args...]
```

- `<programme>` = un seul jeton (exécutable) ; `[args...]` = arguments séparés
  (piège M1 : `portable-pty` n'accepte pas une chaîne multi-jetons comme
  programme).
- Un argument peut être le jeton `{prompt}` (substitué par le texte saisi).

Exemples :
```
agent-template claude   claude -p {prompt}   # one-shot : {prompt} → argument
agent-template claude-i claude               # interactif : pas de {prompt} → prompt sur stdin
```

`Config` gagne `agent_templates: Vec<AgentTemplate>` (parsé dans `apply`).
`AgentTemplate { name: String, program: String, args: Vec<String> }`.

## Protocole (ajouts)

- `ClientMessage::ListAgentTemplates` → `ServerMessage::AgentTemplates(Vec<AgentTemplate>)`
  (pour peupler le menu du lanceur). `AgentTemplate` est sérialisable dans
  `wimux-protocol` (`{ name, program, args }`).
- `ClientMessage::CreateAgentSession { name: Option<String>, template: String,
  prompt: String, cwd: Option<String> }` → le serveur :
  1. résout le template par son nom (sinon `Error`) ;
  2. substitue `{prompt}` dans les args ; note s'il reste du prompt à envoyer sur
     stdin (aucun arg ne contenait `{prompt}`) ;
  3. crée une session dont le **volet racine** exécute `program` + args résolus,
     dans `cwd` (défaut = cwd du daemon), et la **marque agent** (`mark_agent`) ;
  4. si prompt-stdin, l'envoie au volet racine (+ `\r`) après le spawn ;
  5. répond `ServerMessage::SessionCreated { name }` (ou `Error`).

`AgentStatus`/`SessionInfo.agent`/`agent_status` existent déjà (M1) — pas de
changement protocole côté statut ; seul le pont Tauri (`SessionDto`) doit
exposer `agent_status` au frontend.

## Serveur

1. **Création de volet paramétrée.** Le spawn de volet (`Pane::spawn`) est étendu
   pour accepter, en plus de `cols`/`rows`, un **programme**, des **args** et un
   **cwd** optionnels (aujourd'hui il prend un `shell: &str` unique). Réutilise
   `CommandBuilder::new(program).args(args)` + `.cwd(cwd)` de `portable-pty`. Le
   chemin shell existant devient un cas particulier (programme = shell, pas
   d'args). `Session` gagne une création agent (`Session::new_agent(...)` ou
   équivalent) qui construit le volet racine ainsi et appelle `mark_agent`.
2. **Livraison stdin.** Si aucun `{prompt}` n'a été substitué, le serveur envoie
   `prompt` + `\r` au volet racine après le spawn (le PTY tamponne l'entrée ;
   l'agent la lit quand il est prêt).
3. **Correctif M1 (`Pane::kill`).** Après `child.kill()`, **dropper/fermer le
   `MasterPty`** du volet pour provoquer l'EOF du `reader_loop` (ConPTY n'EOF pas
   sur sortie propre) et libérer le thread lecteur parqué + son `Arc<Pane>`. Sans
   ça, tuer un agent terminé fuit un thread + un handle (noté en M1).
4. **Nommage.** Si `name` absent : `<template>-<n>` où `n` est le plus petit
   entier rendant le nom libre (comme la génération de nom auto existante, mais
   préfixée par le template).
5. **`Server::list`** est inchangé (M1 renseigne déjà `agent`/`agent_status`).

## Frontend / pont Tauri

- **Pont** : commandes Tauri `list_agent_templates() -> Vec<AgentTemplateDto>`
  (connexion jetable, comme `list_sessions`) et `create_agent(template, prompt,
  cwd, name) -> String` (connexion jetable → `CreateAgentSession`). `SessionDto`
  gagne `agent: bool` + `agent_status: Option<String>` (mappé depuis
  `SessionInfo`).
- **Lanceur** : un bouton « + agent » (près du `+` de session dans le rail) ouvre
  un **dialogue modal** : menu déroulant *template* (peuplé via
  `list_agent_templates`), zone de texte *prompt*, champ *répertoire* (défaut =
  vide → cwd du daemon), champ *nom* optionnel, boutons **Lancer** / **Annuler**.
  Au lancement : `create_agent(...)` puis bascule sur la nouvelle session.
- **Glyphe de statut** dans le rail, pour chaque session `agent` (remplace la
  pastille G4 activité/cloche) : ⚙ *Working* (bleu), ○ *Idle* (gris), ❗
  *Attention* (orange), ✓ *Done* (vert), ✗ *Error* (rouge). Les sessions
  non-agent gardent les pastilles activité/cloche de G4. Le sondage 1 s existant
  rafraîchit le glyphe.

## Tests

- **Serveur (intégration `gui_mode.rs`)** :
  - `ListAgentTemplates` renvoie les templates chargés depuis une config de test
    (on peut piloter `Config` via un point d'entrée de test, ou une config par
    défaut avec un template).
  - `CreateAgentSession` avec un template one-shot **déterministe** (programme =
    `cmd.exe`, args = `["/c", "echo", "{prompt}"]`) : la session apparaît dans
    `List` avec `agent = true`, puis se termine (`agent_status = Done`).
  - Variante stdin (template sans `{prompt}`, ex. programme = `cmd.exe` ; on
    vérifie que le prompt+`\r` est bien injecté — p. ex. `exit 0` fait sortir cmd).
  - **Correctif `Pane::kill`** (test **lib** dans `pane.rs`, preuve honnête du
    déblocage) : spawn un volet (`cmd.exe`), envoyer `exit 0\r\n`, sonder jusqu'à
    ce que `exit_code()` soit `Some` (le processus est mort, mais avant le
    correctif le `reader_loop` reste parqué sur `read()` et retient un
    `Arc<Pane>`) ; appeler `pane.kill()` (qui ferme le `MasterPty`) ; sonder
    `Arc::strong_count(&pane)` jusqu'à ce qu'il retombe à 1 (le thread lecteur a
    reçu l'EOF, s'est terminé et a relâché son `Arc`) dans un délai borné. Sans le
    correctif, le compteur resterait ≥ 2 (thread parqué). C'est la preuve
    vérifiable que le master fermé débloque le lecteur.
- **Frontend** : validé **manuellement** (README) — ouvrir le dialogue, lancer un
  agent (one-shot et interactif), voir le glyphe passer *Working* → *Done*, tuer
  un agent.
- **Non-régression** : suites TUI + G1/G2/G3/G4 + M1 vertes ; fmt + clippy
  `-D warnings` propres ; `npm run build` OK.

## Hors-périmètre M2 (rappel)

Orchestration fan-out (N agents sur une tâche) → **M3** ; revue/agrégation des
résultats → **M4** ; sélecteur de répertoire natif (champ texte en M2) ; édition
des templates depuis la GUI (fichier `wimux.conf` en M2) ; multi-volets par agent
(un agent = un volet racine) ; « fermer tous les terminés » en un clic (peut
venir plus tard).

## Risques

| Risque | Parade |
|---|---|
| Timing du prompt-stdin (agent pas encore prêt à lire) | Le PTY tamponne stdin ; envoi juste après le spawn ; si un agent particulier rate, un petit délai configurable pourra être ajouté (hors M2 par défaut) |
| Substitution `{prompt}` avec espaces/guillemets | Le prompt est **un seul argument** (pas de re-split) : `{prompt}` est remplacé par la valeur entière dans l'arg qui le contient |
| `cwd` invalide / programme introuvable | Le spawn échoue → `Error` remonté au frontend (affiché dans le dialogue) ; pas de session créée |
| Correctif `Pane::kill` (fermeture du master) casse le chemin normal | Le drop du `MasterPty` ne concerne que la fin de vie du volet ; couvert par la non-régression (detach/reattach, G1-G4) + un test dédié |
| `SessionInfo`/`SessionDto` étendus | Rebuild conjoint serveur + CLI + GUI ; cf. piège du daemon persistant (redémarrage requis) |
