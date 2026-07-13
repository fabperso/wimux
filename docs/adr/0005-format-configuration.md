# ADR-0005 — Format du fichier de configuration

- **Statut** : accepté
- **Date** : 2026-07-13

## Contexte

wimux doit être configurable : au minimum la touche de préfixe, le shell par
défaut et les raccourcis. Le fichier est chargé au démarrage du serveur depuis
`%USERPROFILE%\.wimux.conf` (puis `%APPDATA%\wimux\wimux.conf`).

## Décision

**Syntaxe façon tmux, une directive par ligne** (et non TOML/KDL).

```text
# Préfixe et shell
set prefix C-a
set default-shell pwsh.exe

# Raccourcis personnalisés
bind | split-window -h
bind - split-window -v
```

Justification :
- **Familiarité** : le public visé (utilisateurs de tmux qui passent à Windows,
  cf. [[wimux-objectif-portfolio]]) connaît déjà cette syntaxe et sa mémoire
  musculaire (`set`, `bind`, `C-a`). Réduit la friction d'adoption.
- **Parsing trivial** : découpage par lignes puis par mots, aucune dépendance.
- Migration facile depuis un `.tmux.conf` existant pour les directives gérées.

## Conséquences

- Directives gérées à ce stade : `set prefix`, `set default-shell`,
  `bind <touche> <action>`. Actions reconnues : `split-window -h/-v`,
  `new-window`, `next-window`, `previous-window`, `next-pane`, `kill-pane`,
  `copy-mode`, `paste-buffer`, `detach-client`.
- Les directives inconnues sont **ignorées silencieusement** (tolérance pour
  reprendre un `.tmux.conf` partiellement compatible). Un mode strict avec
  avertissements pourra être ajouté.
- Le rechargement à chaud (`source-file`) et les options à portées
  (server/session/window/pane) restent à faire.
- Touches : `C-a`..`C-z`, `Space`, `Enter`, `Tab`, ou un caractère unique.
