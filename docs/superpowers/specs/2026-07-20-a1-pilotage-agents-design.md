# Design — wimux A1 : pilotage d'agents par Claude (CLI `wimux agent` + skill)

- **Date** : 2026-07-20
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : parité CMUX — **intégration agent** (façon `cmux-cli` + skill Claude)
- **Prérequis** : GUI G1→G4 + série W (W1→W6 + polish) + agents M1/M2/M3 faits et fusionnés dans `main`.

## Objectif

Permettre à un **Claude orchestrateur, exécuté dans un volet wimux**, de créer par
lui-même des volets « agents » (chacun lançant une tâche, typiquement
`claude -p "<tâche>"`), de les **surveiller** et de **lire la sortie** de chaque
terminal — le tout via une nouvelle CLI `wimux agent` qui parle au daemon en
messages typés. C'est l'équivalent wimux du couple `cmux-cli` + skill de CMUX.

Point d'appui : wimux a **déjà** le daemon + protocole + une CLI scriptable
(`send-keys`, `capture-pane`, `split-window`, `list-panes`) et — contrairement à
ce que documente CMUX — sait **déjà lire la sortie** d'un volet (`capture-pane`).
A1 comble l'écart : une surface CLI orientée agent (adressable par pane-id, JSON),
un journal par volet, l'injection du contexte, et un **skill** qui l'enseigne à Claude.

## Décisions (validées lors du brainstorm)

| Sujet | Décision |
|---|---|
| Topologie | Claude tourne **dans** un volet wimux → contexte fourni par variables d'env (`WIMUX_SESSION`/`WIMUX_PANE`/`WIMUX_PIPE`) |
| Lecture de sortie | **Journal par volet** (fichier append-only) **+ photo** `capture-pane` (grille VT visible) |
| Granularité d'un agent | Un agent = **un volet (split)** de la session courante → ciblage **par pane-id** |
| Stratégie API | **Messages protocole typés dédiés** + namespace CLI `wimux agent …` + sortie **JSON** |
| GUI (live) | **À câbler** (A1.5) : un volet créé via CLI passe par une **connexion séparée** du daemon → aucun `WindowLayout` n'est poussé vers la GUI attachée. Solution : un **compteur de révision de layout** par session (champ additif sur `SessionInfo`), bumpé aux créations/fermetures de volet ; le `refresh()` frontend le compare pour la session active et re-déclenche `attach_gui` (réutilise le réattachement existant) → le volet-agent apparaît en direct |

## Composants

Quatre plans de modification :

- **Protocole** (`wimux-protocol`) : nouveaux messages typés, ajoutés **en fin
  d'enum** (règle de compatibilité postcard : indexation par position).
- **Serveur** (`wimux-server`) : injection d'env de contexte à chaque volet ;
  spawn d'un volet-commande renvoyant son pane-id ; capture par pane-id ;
  journalisation des volets agents ; nouveaux handlers.
- **CLI** (`wimux-cli`) : namespace `wimux agent …`, sortie JSON, détection de
  contexte via env.
- **Skill** (dépôt) : `skills/wimux/SKILL.md` (+ `references/commands.md`).

## Protocole (ajouts — tous additifs, en fin d'enum)

Nouveaux `ClientMessage` :

- `SpawnPane { session: String, from_pane: Option<u64>, dir: SplitDir, cwd: Option<String>, program: String, args: Vec<String> }`
  → découpe la fenêtre active de `session` à partir de `from_pane` (défaut : volet
  actif) et lance `program`/`args` dans le nouveau volet (journalisé).
  Réponse : `ServerMessage::PaneSpawned { pane_id: u64 }` ou `Error`.
- `CapturePane { session: String, pane: u64 }`
  → photo de la grille VT visible du volet `pane` (l'historique passe par le
  journal ; le paramètre `lines`/scrollback est abandonné — YAGNI).
  Réponse : `ServerMessage::PaneCapture(String)` ou `Error`.
- `ListPanes { session: String }`
  → Réponse : `ServerMessage::PaneList(Vec<PaneInfo>)` ou `Error`.
- `SendKeysPane { session: String, pane: u64, keys: Vec<u8> }`
  → frappes vers un volet précis. Réponse : `Ok`/`Error`.
- `KillPane { session: String, pane: u64 }`
  → ferme un volet agent (nommé `KillPane` pour ne pas entrer en collision avec la
  variante GUI existante `ClosePane { pane_id }`). Réponse : `Ok`/`Error`.

Nouveaux `ServerMessage` : `PaneSpawned { pane_id: u64 }`, `PaneCapture(String)`,
`PaneList(Vec<PaneInfo>)` (les autres réutilisent `Ok`/`Error`).

Nouvelle struct :

```rust
pub struct PaneInfo {
    pub pane_id: u64,
    pub cwd: Option<String>,     // dernier OSC 7 capté (W3)
    pub running: bool,           // l'enfant est-il vivant ?
    pub exit_code: Option<i32>,  // code de sortie si terminé
    pub log_path: Option<String>,// chemin du journal si volet journalisé
}
```

**La lecture du journal ne passe PAS par le protocole** : c'est un fichier local
(même machine, même utilisateur). La CLI le lit / le `tail` directement à partir de
`PaneInfo.log_path`, ce qui offre `--follow` sans logique serveur supplémentaire.

## Serveur

### Injection d'env de contexte (tous les volets)

`Pane::spawn_command` injecte dans la `CommandBuilder` :

- `WIMUX_SESSION` = nom de la session,
- `WIMUX_PANE` = id du volet (= le pane-id alloué),
- `WIMUX_PIPE` = nom du pipe utilisateur (`user_pipe_name()`).

Le pane-id étant alloué **dans** `spawn_command` (`NEXT_PANE_ID.fetch_add`), on
l'alloue **avant** de construire la `CommandBuilder` afin de pouvoir poser
`WIMUX_PANE`. Le nom de session est passé depuis `Session`/`Window` (les volets ne
le connaissent pas aujourd'hui) via un petit contexte de spawn
`PaneSpawnCtx { session: String, log: bool }`. Conséquence recherchée : **le volet
de l'orchestrateur lui-même** connaît son contexte (il a été spawné par wimux) →
la CLI `wimux agent` en déduit les défauts `-t`/`-p`.

### Journal (volets agents seulement)

Le `reader_loop` tee les octets bruts lus du PTY dans
`%LOCALAPPDATA%\wimux\logs\<session>\<pane_id>.log` (même racine applicative que
l'`agent-worktree-root` de M3, sous `%LOCALAPPDATA%\wimux`). Fichier **brut**
(fidèle aux octets VT). Seuls les volets créés via
`SpawnPane` sont journalisés (`PaneSpawnCtx.log = true`) — borne l'usage disque et
évite de journaliser les shells interactifs. Ajout à `PaneState` d'un
`log: Option<Mutex<BufWriter<File>>>` ouvert au spawn ; échec d'ouverture =
non-fatal (journal désactivé pour ce volet, `log_path = None`).

La **dé-ANSI** est faite **à la lecture, côté CLI** (voir `strip_ansi`, rendu
partagé — extrait de `pty.rs` vers un util réutilisable). Le fichier reste brut ;
`wimux agent logs` renvoie du texte dé-ANSI par défaut, `--raw` pour l'octet brut.

### Spawn / capture / inventaire

- `Session::spawn_pane(from_pane: Option<u64>, dir, cwd, program, args) -> u64` :
  généralise `split` — découpe à partir de `from_pane` (défaut actif) mais lance
  une **commande** au lieu du shell, et **renvoie l'id** du nouveau volet.
- `Session::capture_pane(pane_id) -> Option<String>` : rend la grille VT du volet
  ciblé en texte (généralise `capture_active_pane` avec ciblage).
- `Session::pane_infos() -> Vec<PaneInfo>` : parcourt les fenêtres/l'arbre et
  expose id, cwd, running (enfant vivant), exit_code, log_path.
- `Session::send_keys_pane(pane_id, &[u8])` et `Session::close_pane(pane_id)` :
  variantes ciblées des primitives existantes.

### Handlers

`handle_client` gagne les branches `SpawnPane`/`CapturePane`/`ListPanes`/
`SendKeysPane`/`ClosePane` (résolution de session, puis appel des méthodes
ci-dessus, réponses typées / `Error` clair si session ou pane inconnu).

**Rebuild release + redémarrage du daemon détaché obligatoire** après ce
changement de protocole (piège du daemon persistant consigné).

## CLI `wimux agent`

Nouveau sous-arbre de commandes sous `wimux agent <verbe>`. Défaut `-t` =
`$WIMUX_SESSION` ; défaut `--from-pane`/`-p` = `$WIMUX_PANE` (contexte injecté).

- `wimux agent spawn [--dir h|v] [--cwd DIR] [-t SESSION] [--from-pane ID] -- <commande...>`
  → imprime le pane-id créé (`{"pane_id":N}`).
- `wimux agent list [-t SESSION] [--json]`
  → volets : `pane_id`, `cwd`, `running`, `exit_code`, `log_path`.
- `wimux agent logs [-t SESSION] -p PANE [--tail N] [--follow] [--raw]`
  → lit le fichier journal (dé-ANSI par défaut). `--follow` = tail du fichier.
- `wimux agent capture [-t SESSION] -p PANE [--lines N]`
  → photo instantanée du volet (utile pour un agent en TUI plein écran).
- `wimux agent send [-t SESSION] -p PANE <keys...>`
  → frappes (jetons `Enter`/`Tab`/`Space`/`Escape`/`C-<x>` déjà gérés par
  `translate_keys`).
- `wimux agent kill [-t SESSION] -p PANE`
  → ferme le volet.
- `wimux agent whoami [--json]`
  → contexte courant (`WIMUX_SESSION`/`WIMUX_PANE`/`WIMUX_PIPE`).

Sortie JSON systématique pour `list`/`spawn`/`whoami` (parsing fiable par le
skill) ; `logs`/`capture` restent du texte. Le parsing d'args gère le séparateur
`--` (tout ce qui suit = programme + args de l'agent).

## Skill

`skills/wimux/SKILL.md` (frontmatter `name`/`description`) + `references/commands.md`.
Il enseigne à Claude :

1. **Détecter son contexte** : `wimux agent whoami` (ou lire `$WIMUX_SESSION`/`$WIMUX_PANE`).
2. **Lancer un sous-agent** en mode flux/print (journal = transcript propre) :
   `wimux agent spawn --dir v -- claude -p "<tâche>"` → récupérer le `pane_id`.
3. **Surveiller** : `wimux agent list` → `running` / `exit_code`.
4. **Lire la sortie** : `wimux agent logs -p <id> --tail N` (ou `--follow`).
5. **Photographier** un agent TUI : `wimux agent capture -p <id>`.
6. **Répondre à une invite** : `wimux agent send -p <id> "oui" Enter`.
7. **Nettoyer** : `wimux agent kill -p <id>`.

Bonne pratique clé documentée : lancer les sous-agents en mode **non-interactif /
print** pour que le journal soit un transcript propre ; réserver `capture` aux
agents TUI qui se redessinent. Installation : le skill est shippé dans le dépôt ;
le README explique comment le lier au dossier skills de Claude. Packaging
distribuable (`npx skills add …`, `agents/openai.yaml`) = évolution future.

## Flux (spawn → lecture)

1. Claude (volet P, session S) exécute
   `wimux agent spawn --dir v -- claude -p "tâche"`.
2. La CLI lit `WIMUX_SESSION=S`, `WIMUX_PANE=P` → envoie
   `SpawnPane { session: S, from_pane: Some(P), dir: TopBottom, cwd: None, program: "claude", args: ["-p","tâche"] }`.
3. Le daemon découpe la fenêtre, spawn le volet Q (env `WIMUX_SESSION=S`/
   `WIMUX_PANE=Q` + journal `%LOCALAPPDATA%\wimux\logs\S\Q.log`), répond `PaneSpawned { Q }`.
4. La CLI imprime `{"pane_id":Q}` ; la GUI (si attachée) affiche Q en direct.
5. Claude lit ensuite `wimux agent logs -p Q --tail 40` / `wimux agent list`
   (running → exit_code) jusqu'à complétion.

## Erreurs

- Session ou pane inconnu → `ServerMessage::Error` clair.
- Programme introuvable au spawn → `Error` (remontée de l'échec `spawn_command`).
- Ouverture du journal échouée → **non-fatal** : le volet vit, `log_path = None`.
- `capture`/`logs` sur volet terminé → dernière grille en mémoire / journal figé
  (toujours lisibles).

## Tests

- **Protocole** (`wimux-protocol`) : round-trip postcard des nouveaux
  `ClientMessage`/`ServerMessage` et de `PaneInfo`.
- **Serveur** (intégration) :
  - `spawn_pane` renvoie un id ; `capture_pane(id)` rend le contenu ; `pane_infos`
    reflète running → exit_code après sortie de l'enfant.
  - **Env injecté** : spawn d'un volet exécutant un `cmd.exe /c echo %WIMUX_SESSION%`
    (ou équivalent PowerShell) ; la capture/le journal contient le nom de session.
  - **Journal** : spawn d'un volet qui écrit une ligne connue ; le fichier journal
    la contient (après dé-ANSI).
  - `SendKeysPane`/`ClosePane` ciblent le bon volet.
- **CLI** (unitaire) : parsing des args (`--` séparateur, `-t`, `--from-pane`,
  `--dir`, `--tail`) ; défauts pris depuis l'env.
- **`strip_ansi`** partagé : tests existants conservés après extraction.
- **Skill** : validé manuellement (README) — un vrai Claude dans un volet lance un
  sous-agent, lit son journal, le tue.
- **Non-régression** : suites TUI + G1→G4 + W + M1/M2/M3 vertes ; `cargo fmt` +
  `clippy -D warnings` ; `npm run build` OK.

## Découpage prévisionnel (pour le plan)

- **A1.1** — Protocole : nouveaux messages + `PaneInfo` (+ tests round-trip).
- **A1.2** — Serveur : contexte env + `spawn_pane` + `capture_pane(id)` +
  `pane_infos` + journalisation + handlers (+ tests). Rebuild + restart daemon.
- **A1.3** — CLI : namespace `wimux agent` + JSON + lecture/`tail` du journal
  (+ tests d'args).
- **A1.4** — Skill : `SKILL.md` + `references/` + section README d'installation.
- **A1.5** — GUI live : compteur de révision de layout sur `SessionInfo` (bumpé
  aux créations/fermetures de volet) + re-attach frontend dans `refresh()` quand
  il change pour la session active (les volets-agents apparaissent en direct).

## Hors-périmètre A1 (rappel)

- Packaging distribuable du skill (`npx skills add`, `agents/openai.yaml`).
- Suivi/`--follow` du journal côté serveur (on lit le fichier côté CLI).
- Lancer un agent comme **session** dédiée via `wimux agent` (A1 = volet ;
  M2/M3 couvrent déjà la voie session).
- Notifications/`set-status`/`set-progress` orientées agent (OSC 9/777 de W6
  existent déjà ; verbes CLI dédiés = évolution future).
- Politique de rotation/purge des journaux (nettoyage best-effort au kill).

## Risques

| Risque | Parade |
|---|---|
| Journal d'un agent **TUI** illisible (redessine en place) | Documenter le mode print/flux pour les sous-agents ; `capture` en complément pour les TUI |
| `WIMUX_PANE` posé avant l'alloc de l'id | Réordonner `spawn_command` : allouer l'id d'abord, puis construire la `CommandBuilder` |
| Fuite/gonflement des journaux | Journaliser **uniquement** les volets `SpawnPane` ; purge best-effort au kill ; hors-périmètre = rotation |
| Séquence VT coupée entre deux chunks à la dé-ANSI | Dé-ANSI sur le **contenu entier** lu (one-shot) ; `--follow` best-effort, `--raw` disponible |
| Nom de session à threader jusqu'au spawn de volet | `PaneSpawnCtx { session, log }` passé depuis `Session`/`Window` |
| Volet créé en CLI **non reflété** dans la GUI attachée (connexion séparée, pas de push `WindowLayout`) | Compteur de révision de layout sur `SessionInfo` + re-attach frontend au changement (A1.5) |
| Changement de protocole vs daemon persistant | Ajouts **en fin d'enum** ; rebuild release + redémarrage du daemon (piège consigné) |
| Chemin journal non-UTF-8 / `%LOCALAPPDATA%` introuvable | Ouverture tolérante (non-fatal) → `log_path = None` ; création best-effort de `%LOCALAPPDATA%\wimux\logs` |
