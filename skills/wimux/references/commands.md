# Référence `wimux agent`

Toutes les commandes prennent `-t <session>` (défaut `$WIMUX_SESSION`) et, quand
un volet est ciblé, `-p <pane>` (défaut `$WIMUX_PANE`).

## spawn
`wimux agent spawn [--dir h|v] [--cwd DIR] [-t SESSION] [--from-pane ID] -- <commande...>`
Découpe la fenêtre active (à partir de `--from-pane`, défaut volet courant) et
lance la commande dans le nouveau volet (journalisé). Sortie : `{"pane_id":N}`.

## list
`wimux agent list [-t SESSION]`
Sortie JSON : `[{"pane_id","running","exit_code","cwd","log_path"}, ...]`.

## logs
`wimux agent logs -p PANE [-t SESSION] [--tail N] [--follow] [--raw]`
Lit le journal du volet (dé-ANSI par défaut ; `--raw` pour les octets bruts ;
`--follow` pour suivre).

## capture
`wimux agent capture -p PANE [-t SESSION]`
Contenu visible (photo) du volet — utile pour un agent TUI.

## send
`wimux agent send -p PANE [-t SESSION] <touches...>`
Envoie des frappes. Jetons : `Enter`, `Tab`, `Space`, `Escape`, `C-<x>` ; le reste
littéral.

## kill
`wimux agent kill -p PANE [-t SESSION]`
Ferme le volet.

## whoami
`wimux agent whoami`
`{"session","pane","pipe"}` — le contexte courant.

## Lots d'agents (`wimux batch`)

### create
`wimux batch create --repo <chemin> --template <nom> --prompt "…" [--count N]`
Lance N agents (défaut 2), chacun dans un worktree git isolé du dépôt.
Sortie : `{"group":"…","sessions":[…]}`.

### list
`wimux batch list` → `[{"group","base_repo","base_branch","sessions":[…]}]`.

### review
`wimux batch review -g <group>`
→ `[{"session","index","branch","status","files_changed","insertions","deletions","untracked","has_commits"}]`.

### diff
`wimux batch diff -g <group> -i <index>` (ou `-s <session>`)
Diff complet : fichiers suivis vs la base + contenu des fichiers non suivis.

### pr
`wimux batch pr -g <group> -i <index> [--title "…"] [--body "…"]`
Commite le travail en cours du gagnant, pousse sa branche, ouvre la PR (via `gh`),
renvoie `{"url":"…"}`, puis supprime les agents perdants. Refuse proprement si
`gh` est absent/non authentifié, s'il n'y a pas de remote `origin`, ou si l'agent
n'a rien produit.
