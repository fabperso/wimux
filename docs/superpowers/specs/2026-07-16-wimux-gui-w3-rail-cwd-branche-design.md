# Design — wimux GUI W3 : rail enrichi (cwd + branche git)

- **Date** : 2026-07-16
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : l'épi **parité CMUX** (mémoire `wimux-parite-cmux`) — W1 (redimensionnement volets) + W2 (onglets) faits ; W3 (ce document) ; W4 (contrôles split visibles + icônes), W5 (menu contextuel) à suivre.
- **Prérequis** : G2 (rail + sondage `list-sessions` / `SessionInfo`), G4 (indicateurs de rail), M1/M3 (`SessionInfo` étendu). Le rail sonde déjà périodiquement la liste des sessions.

## Contexte

CMUX affiche sous chaque workspace son **répertoire courant vivant** (qui suit les `cd`) et sa **branche git**. Chez wimux, le rail n'affiche que le nom : les sessions normales spawnent sans `cwd` (toutes héritent du cwd du daemon) et le serveur ne suit pas le répertoire courant. W3 comble cet écart : suivre le cwd en direct via **OSC 7** et en dériver la branche.

## Objectif (W3)

Afficher, sous le nom de chaque session dans le rail, son **cwd courant** (suivant les `cd`) et sa **branche git**. Décision validée : source du cwd = **OSC 7 vivant** (la séquence que le shell émet à chaque prompt), branche dérivée du cwd via `.git/HEAD` (sans lancer `git`).

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Source du cwd | **OSC 7 vivant** (`ESC ]7;file://host/chemin BEL|ST`), parsé dans le flux de sortie du volet |
| Granularité | cwd d'une session = cwd du **volet actif de la fenêtre active** (ce que l'utilisateur regarde) |
| Émission OSC 7 | **injectée** au spawn pour PowerShell/pwsh (hook de prompt préservant le prompt existant) ; `cmd.exe` (pas d'OSC 7) → repli sur le cwd de spawn / `None` |
| Branche | lue dans `.git/HEAD` du cwd (`ref: refs/heads/<b>` → `<b>` ; sha détaché → sha court ; pas un repo → `None`), **sans** sous-processus `git` |
| Transport | `SessionInfo` gagne `cwd`/`branch` (le rail les reçoit via le sondage existant) |
| Périmètre | cwd + branche **uniquement**. Ports + texte de notif → différés (queue de série) |

## Modèle

- **Sniffer OSC 7 par volet** : dans le `reader_loop` du volet (`pane.rs`), les octets bruts de la pseudo-console sont déjà lus. On y ajoute un **extracteur OSC 7 à état** (une séquence peut être coupée entre deux lectures) qui, à chaque `ESC ]7;…BEL|ST`, décode l'URL `file://host/chemin` en chemin natif et met à jour `PaneState.cwd`. Le reste du pipeline (feed du `Terminal`, diffusion aux abonnés) est **inchangé** — c'est un sniffer passif, indépendant de `strip_ansi`.
- **`Pane`** : `PaneState` gagne `cwd: Option<String>` ; accesseur `Pane::cwd() -> Option<String>`.
- **`Session`** : `active_pane_cwd() -> Option<String>` = cwd du volet actif de la fenêtre active.
- **Branche** : fonction pure `git_branch(cwd: &Path) -> Option<String>` (lecture `.git/HEAD` ; gère `gitdir:` d'un worktree en repli `None` au premier jet). Calculée à la construction de `SessionInfo` (le sondage rail est déjà throttlé).

## Émission OSC 7 (le point de risque)

PowerShell **n'émet pas** l'OSC 7 par défaut. Au spawn d'une session normale (`NewSession`), si le shell est PowerShell/pwsh, on injecte un **hook de prompt** qui, à chaque prompt, émet `ESC ]7;file://<host>/<cwd-url-encodé> BEL` **puis** appelle le prompt existant (préservation). Deux mécanismes possibles (tranchés au plan, sécurisés par le débogage systématique) :
1. **Via args de spawn** : `powershell.exe -NoExit -Command "<snippet enveloppant $function:prompt>"` — propre mais sensible à l'ordre de chargement du profil.
2. **Via stdin après spawn** : écrire le snippet sur l'entrée du volet juste après le spawn (comme M2 pour les prompts d'agent) — insensible au profil, mais visible une fois et ajouté à l'historique.

**Repli** : si le shell n'est ni powershell ni pwsh (`cmd.exe`, autre), pas d'injection → `cwd`/`branch` valent le cwd de spawn (souvent `None`). Le rail masque simplement la 2e ligne quand `cwd == None`. **Aucune régression** : une session sans OSC 7 s'affiche comme avant (nom seul).

## Protocole (ajouts)

`SessionInfo` gagne **en fin de struct** (après `group`) :
- `cwd: Option<String>` — cwd courant du volet actif (chemin natif affichable), `None` si inconnu.
- `branch: Option<String>` — branche git du cwd, `None` si hors repo / inconnu.

(Struct sérialisée par postcard : champs ajoutés **en fin**, aucun réordonnancement.)

## Serveur

- `pane.rs` : `PaneState.cwd` + sniffer OSC 7 dans `reader_loop` (extracteur à état, décodage `file://` → chemin) + `Pane::cwd()`.
- `session.rs` : `Session::active_pane_cwd()`.
- Module `git.rs` (nouveau, petit) ou fonction utilitaire : `git_branch(cwd) -> Option<String>` (lecture `.git/HEAD`).
- `daemon.rs` : `server.list()` renseigne `cwd = session.active_pane_cwd()` et `branch = cwd.and_then(git_branch)` dans chaque `SessionInfo`.
- **Injection OSC 7** au spawn PowerShell dans le chemin `NewSession` (et pour les nouveaux onglets/volets qui spawnent le shell : `gui_new_window`/`gui_split`).

## Frontend

- `main.ts` : le type `SessionDto` gagne `cwd: string | null` + `branch: string | null` ; le rendu d'une entrée de rail passe sur **deux lignes** : nom (ligne 1) ; `cwd` (abrégé `~` pour le home, tronqué en ellipse) + branche (icône + nom) (ligne 2). Ligne 2 masquée si `cwd == null`.
- `styles.css` : styles de la 2e ligne (`.session .meta`, cwd muet, branche).
- Icônes : réutiliser des glyphes texte simples (pas de dépendance ; ex. un caractère dossier / `⎇` pour la branche) cohérents avec le rail existant.

## Tests

- **Parser OSC 7 (unitaire)** : `ESC ]7;file://host/C:/foo/bar BEL` → cwd `C:\foo\bar` ; terminateur `ST` (`ESC \`) géré ; séquence **coupée en deux lectures** reconstituée ; URL-décodage (`%20` → espace) ; octets non-OSC-7 ignorés (pas de faux positif).
- **`git_branch` (unitaire)** : dossier temp avec `.git/HEAD = "ref: refs/heads/feat/x"` → `Some("feat/x")` ; HEAD détaché (sha brut) → sha court ; sans `.git` → `None`. Nettoyage du temp.
- **Plomberie `SessionInfo` (intégration `gui_mode.rs`)** : après avoir alimenté le cwd d'un volet (injection directe d'une séquence OSC 7 dans le pipeline de sortie du volet, pour rester déterministe sans dépendre du shell), `list-sessions` renvoie le `cwd` attendu et la `branch` cohérente. (Éviter de dépendre de l'émission OSC 7 réelle de PowerShell dans les tests — non déterministe.)
- **Non-régression** : suites TUI + G1→G4 + M1→M3 + W2 vertes ; fmt + clippy `-D warnings` ; `npm run build` OK ; une session sans OSC 7 (`cmd.exe`) affiche le nom seul (pas de 2e ligne, pas d'erreur).

## Hors-périmètre W3 (rappel)

- **Ports écoutés** + **texte de notif agent** (les autres colonnes CMUX) → différés.
- **Contrôles de split toujours visibles + icônes d'onglet** → W4.
- **Menu contextuel** (renommer/couleur/position) → W5.
- Suivi du cwd via API Windows/PEB (rejeté au profit de l'OSC 7).

## Risques

| Risque | Parade |
|---|---|
| Injection OSC 7 propre dans PowerShell (sans écraser le prompt utilisateur / ordre du profil) | Enrober `$function:prompt` ; sécuriser le mécanisme (args vs stdin) au plan via débogage systématique ; **repli** cwd de spawn si échec |
| Séquence OSC 7 coupée entre deux lectures PTY | Extracteur **à état** (buffer partiel entre appels), testé sur découpe |
| Décodage `file://host/chemin` (URL-encodage, `/C:/` Windows, host distant) | Fonction de décodage dédiée + tests ; host non-local ignoré (repli `None`) |
| Coût de lecture `.git/HEAD` à chaque sondage rail | Lecture fichier only (pas de `git`), sondage déjà throttlé ; cache possible si besoin (différé) |
| `SessionInfo` étendu vs vieux daemon | Rebuild + redémarrage du daemon détaché (piège `wimux-daemon-restart-gotcha`) |
| Volet non-shell (agent, masqué) sans OSC 7 | `cwd = None` → rail nom seul ; aucun impact |
