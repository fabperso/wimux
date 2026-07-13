# wimux

Multiplexeur de terminal **natif Windows** : sessions persistantes, detach/reattach, fenêtres et volets — les concepts de tmux, l'ergonomie de Zellij, pensé pour PowerShell et Windows Terminal.

**Statut : phase 3 en cours — vrai multiplexeur** (juillet 2026). Sessions
persistantes (jalon J2), **découpes de volets, fenêtres multiples, navigation et
barre de statut**. `wimux new -s dev` → détacher (`Ctrl-b d`) ou fermer la fenêtre
→ `wimux attach dev` retrouve la session vivante. Le serveur compose la vue
(volets + bordures + statut) et interprète le préfixe `Ctrl-b`, comme tmux.

## Documents

1. [État des lieux](docs/01-etat-des-lieux.md) — ce qui existe (ou pas) sur Windows et le positionnement du projet.
2. [Cahier des charges fonctionnel](docs/02-fonctionnalites.md) — inventaire des fonctionnalités tmux/Screen/Zellij, priorisées P0 → P3.
3. [Plan de développement](docs/03-plan-developpement.md) — choix techniques (Rust, ConPTY, Named Pipes), architecture client/serveur, phases et jalons.
4. [Décisions d'architecture (ADR)](docs/adr/) — [0001 choix de Rust](docs/adr/0001-choix-langage-rust.md), [0002 leçons du PoC ConPTY](docs/adr/0002-conpty-lecons-du-poc.md), [0003 crate d'émulation VT](docs/adr/0003-crate-emulation-vt.md), [0004 IPC overlapped](docs/adr/0004-ipc-overlapped-io.md), [0005 format de configuration](docs/adr/0005-format-configuration.md).

## Essayer

```sh
cargo build --release
target/release/wimux new -s dev      # crée une session et s'y attache
#   … travailler dans le shell, puis Ctrl-b d pour se détacher …
target/release/wimux ls              # liste les sessions
target/release/wimux attach dev      # se rattache

# Scriptable (sans s'attacher) — le point fort côté automatisation :
target/release/wimux send-keys -t dev "npm test" Enter
```

## Configuration

wimux lit `%USERPROFILE%\.wimux.conf` au démarrage (syntaxe façon tmux). Voir
[l'exemple commenté](docs/wimux.conf.example) :

```text
set prefix C-a
set default-shell pwsh.exe
bind | split-window -h
bind - split-window -v
```

### Raccourcis (préfixe `Ctrl-b`)

| Touche | Action |
|--------|--------|
| `d` | Se détacher (la session survit) |
| `%` | Découper le volet (côte à côte) |
| `"` | Découper le volet (empilé) |
| `h` `j` `k` `l` | Aller au volet gauche/bas/haut/droite |
| `o` | Volet suivant |
| `x` | Fermer le volet actif |
| `c` | Nouvelle fenêtre |
| `n` / `p` | Fenêtre suivante / précédente |
| `0`–`9` | Aller à la fenêtre N |
| `[` | Entrer en **mode copie** (défilement de l'historique) |
| `]` | Coller le dernier texte copié |

### Mode copie (après `Ctrl-b [`)

Navigation façon vi dans le scrollback :

| Touche | Action |
|--------|--------|
| `j` / `k` | Descendre / monter d'une ligne |
| `Ctrl-u` / `Ctrl-d` | Demi-page haut / bas |
| `g` / `G` | Début / fin du scrollback |
| `h` / `l` / `0` / `$` | Déplacer le curseur dans la ligne |
| `w` / `b` | Mot suivant / précédent |
| `/` / `?` | Rechercher vers l'avant / l'arrière |
| `n` / `N` | Correspondance suivante / précédente |
| `Espace` | Démarrer la sélection |
| `y` ou `Entrée` | Copier (→ presse-papiers Windows) et quitter |
| `q` ou `Échap` | Quitter sans copier |

## Résumé du plan

- **Stack** : Rust · ConPTY (`portable-pty`) · émulation VT côté serveur (`wezterm-term`) · IPC par Named Pipes · client TUI (`crossterm`).
- **Architecture** : un serveur détaché par utilisateur (source de vérité, survit à la fermeture du terminal) + des clients légers qui s'attachent depuis n'importe quel terminal VT.
- **Jalon clé (J2)** : `wimux new -s dev` → fermer la fenêtre → `wimux attach -t dev` retrouve la session vivante.
- **MVP (v0.1)** : ~9-12 semaines · **v1.0** installable via winget : ~5-6 mois.
