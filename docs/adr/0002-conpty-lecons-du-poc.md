# ADR-0002 — Leçons du PoC ConPTY (phase 1)

- **Statut** : accepté
- **Date** : 2026-07-10

## Contexte

La phase 1 du plan est une phase de **dé-risquage** : faire tourner un vrai
processus Windows (cmd, PowerShell) à travers une pseudo-console ConPTY via la
crate `portable-pty`, et en capturer la sortie de façon fiable. Le prototype
(`crates/wimux-server/src/pty.rs`, tests dans `tests/conpty.rs`, diagnostic dans
`examples/conpty_diag.rs`) a révélé deux comportements ConPTY non évidents qui
structurent l'architecture du multiplexeur.

## Découverte 1 — ConPTY bloque tant qu'on ne répond pas au DSR

Au démarrage, ConPTY émet la séquence `ESC[6n` (**DSR**, Device Status Report :
« quelle est la position du curseur ? ») et **bloque l'exécution du processus
enfant** tant qu'aucune réponse **CPR** `ESC[<row>;<col>R` ne lui parvient sur
l'entrée.

Symptôme observé : `child.wait()` ne revenait jamais (gel > 60 s) pour un simple
`cmd /c echo`. Le diagnostic chronométré a montré que le lecteur recevait
exactement 4 octets (`ESC[6n`) puis plus rien. Dès qu'on répond `ESC[1;1R`,
l'enfant s'exécute, produit sa sortie et se termine normalement (~90 ms).

**Conséquence architecturale : un multiplexeur EST un terminal.** Il doit
implémenter les réponses aux requêtes du terminal, au minimum :
- DSR position curseur `ESC[6n` -> CPR `ESC[<row>;<col>R` ;
- DSR état appareil `ESC[5n` -> `ESC[0n` ;
- (à venir) Device Attributes `ESC[c` / `ESC[>c`.

En phase 2+, la réponse CPR devra refléter la **vraie** position du curseur
suivie par la grille VT (`wimux-vt`), pas un `1;1` fixe. Ces réponses partagent
le canal d'entrée avec la frappe utilisateur.

## Découverte 2 — l'EOF de sortie vient du démantèlement de la pseudo-console

Le lecteur de sortie n'atteint `EOF` que lorsque la pseudo-console est fermée
(`ClosePseudoConsole`, déclenché par le `drop` du maître `portable-pty`). Il ne
suffit **pas** que le processus enfant se termine.

Ordre correct retenu dans `run_capture` :
1. `spawn` l'enfant sur l'esclave ;
2. cloner le lecteur ; prendre l'écrivain (partagé pour répondre au DSR) ;
3. `drop` de l'esclave ;
4. thread de lecture qui répond aux requêtes au fil de l'eau ;
5. `child.wait()` (terminaison naturelle) ;
6. `drop` du maître -> `EOF` -> le thread de lecture se termine ;
7. `join`.

**Piège écarté :** fermer l'entrée (stdin) pendant que l'enfant tourne déclenche
un événement de type Ctrl+C qui **tue** le processus (code de sortie
`0xC000013A` = `STATUS_CONTROL_C_EXIT`) avant qu'il ait produit sa sortie. Il ne
faut donc jamais fermer stdin pour « forcer » l'EOF ; c'est le `drop` du maître
qui s'en charge.

## Autres points confirmés par le PoC

- **PowerShell et cmd natifs** fonctionnent (pas de WSL). ✅
- **Unicode / accents** traversent correctement ConPTY. ✅
- **Codes de sortie** remontés fidèlement (`exit 3` -> 3). ✅
- **Entrée** écrite sur stdin bien reçue par l'enfant (aller-retour prouvé). ✅
- Règle ConPTY respectée : entrée et sortie servies sur des **threads séparés**.

## Décision connexe — crate d'émulation VT

Le PoC valide `portable-pty` (ConPTY) comme couche PTY. Pour l'émulation VT
complète (grille, scrollback, attributs, reflow), la décision `wezterm-term` vs
`vte` + grille maison est **reportée au début de la phase 2**, quand on aura
besoin de parser réellement le flux plutôt que de le relayer. Le PoC actuel se
contente d'un `strip_ansi` minimal pour ses assertions, ce qui suffit à valider
la chaîne.

## Statut du jalon J1

**Atteint.** Un shell interactif (cmd/PowerShell) s'exécute à travers ConPTY,
sortie capturée, entrée transmise, sans gel ni fuite. Go pour la phase 2.
