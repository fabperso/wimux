# ADR-0003 — Crate d'émulation VT : `vte` + grille maison

- **Statut** : accepté
- **Date** : 2026-07-10

## Contexte

À partir de la phase 2, le serveur doit **parser** le flux d'octets produit par
ConPTY pour maintenir une grille de cellules par volet (et non plus seulement le
relayer). Il faut donc un moteur d'émulation VT : machine à états qui interprète
les séquences d'échappement et met à jour la grille (curseur, impression,
effacements, attributs SGR, scrollback).

L'ADR-0002 envisageait `wezterm-term` comme parade au risque « émulation VT
incomplète ». Vérification faite sur crates.io :
- **`wezterm-term`** n'est **pas** publié comme crate autonome maintenue ; on ne
  trouve que des forks (`tattoy-wezterm-term`, `shadow-terminal`). L'utiliser
  imposerait de vendorer depuis le dépôt git de WezTerm — dépendance fragile.
- **`vte` 0.15** : le parser d'Alacritty, publié et activement maintenu. Ne fait
  QUE la machine à états (trait `Perform`) ; la grille reste à notre charge.
- **`termwiz` 0.23** : écosystème WezTerm publié, plus complet (modèle `Surface`)
  mais lourd et taillé pour WezTerm.

## Décision

**`vte` 0.15 pour le parsing + une grille maison dans `wimux-vt`, avec
`unicode-width` pour la largeur des caractères.**

Justification :
- `vte` fournit la partie difficile et éprouvée (la machine à états VT d'Alacritty),
  ce qui écarte le risque de mal réimplémenter le parsing.
- On garde le contrôle total du modèle de grille et du scrollback, adaptés aux
  besoins d'un multiplexeur (snapshots, deltas, par volet). Bon pour la
  maîtrise du code et pour la démonstration technique (objectif portfolio).
- Dépendance propre et maintenue, sans fork ni vendoring.
- **`unicode-width`** traite correctement les caractères larges (CJK) et
  combinants — précisément la faiblesse de rendu reprochée à psmux. C'est un
  différenciateur assumé : la fiabilité du rendu.

## Conséquences

- `wimux-vt` expose un moteur (`Emulator`/`Terminal`) : `advance(&[u8])` fait
  avancer le parser `vte`, un `Perform` maison met à jour la `Grid`.
- Portée VT en phase 2 (volet unique, détach/reattach) : impression de texte,
  `CR`/`LF`/`BS`/`TAB`, déplacements de curseur (CUP/CUU/CUD/CUF/CUB), effacements
  (ED/EL), attributs SGR de base (couleurs, gras...), gestion des caractères
  larges. Le reste (scrollback riche, écran alterné, modes, reflow fidèle) est
  ajouté au fil des phases.
- Réévaluation possible vers `termwiz` si notre moteur montre des lacunes de
  fidélité difficiles à combler — décision documentée le cas échéant.
