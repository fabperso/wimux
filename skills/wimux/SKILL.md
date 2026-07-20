---
name: wimux-orchestration
description: Use when running inside a wimux pane and you need to spawn sub-agents in their own terminals and read their output. Provides `wimux agent` commands to create panes running a task, list them, read their logs, capture their screen, send input, and close them.
---

# Orchestrer des agents avec wimux

Tu tournes dans un **volet wimux**. Tu peux créer d'autres volets (« agents »),
chacun lançant une tâche dans son propre terminal, puis lire leur sortie.

## Contexte
- `wimux agent whoami` → `{"session","pane","pipe"}` : ta session et ton volet.
- Les variables d'env `WIMUX_SESSION` / `WIMUX_PANE` sont déjà posées ; les
  commandes `wimux agent` les prennent par défaut (`-t`/`-p` pour surcharger).

## Boucle type
1. **Lancer un agent pour une tâche** (mode print pour un journal = transcript propre) :
   `wimux agent spawn --dir v -- claude -p "<décris la tâche>"`
   → imprime `{"pane_id":N}`. Garde N.
2. **Surveiller** : `wimux agent list` → pour chaque volet `running`/`exit_code`.
3. **Lire la sortie** : `wimux agent logs -p N --tail 50` (ou `--follow`).
4. **Photographier** un agent en TUI plein écran : `wimux agent capture -p N`.
5. **Répondre à une invite** : `wimux agent send -p N "oui" Enter`.
6. **Fermer** : `wimux agent kill -p N`.

## Bonnes pratiques
- Préfère les sous-agents **non-interactifs / print** (`claude -p ...`) : leur
  journal est un transcript linéaire lisible. Pour un agent TUI qui se redessine,
  utilise `capture` plutôt que `logs`.
- Un agent est **terminé** quand `wimux agent list` montre `running:false` et un
  `exit_code`. Le journal ne grossit plus.
- `--dir v` empile (haut/bas), `--dir h` (défaut) place côte à côte.

## Référence
Voir `references/commands.md` pour tous les drapeaux.
