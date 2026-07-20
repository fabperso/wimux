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
