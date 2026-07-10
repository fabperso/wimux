# État des lieux — Multiplexeurs de terminal sur Windows (juillet 2026)

## Verdict sur l'hypothèse de départ

> « Il n'y a pas d'équivalent de tmux sur Windows »

**Réponse nuancée : c'était vrai jusqu'à fin 2025, mais ce n'est plus totalement exact en 2026.**
Le créneau n'est cependant **pas saturé** : les solutions natives sont très jeunes, peu adoptées, et aucune n'est devenue un standard. Il reste une vraie place pour un outil natif, soigné et bien intégré à l'écosystème Windows (PowerShell, Windows Terminal, winget).

## Panorama des solutions existantes

| Outil | Natif Windows | Detach/Reattach | Panes/Splits | Persistance de session | Maturité |
|---|---|---|---|---|---|
| **Windows Terminal** | Oui | Non | Oui | Non (fermeture = mort des processus) | Très mature, mais pas un multiplexeur |
| **ConEmu / Cmder** | Oui | Non | Oui | Profils seulement | Mature, pas de serveur de session |
| **WezTerm** | Oui | Oui (mux server) | Oui | Oui | Mature, mais c'est un émulateur complet, pas un multiplexeur qu'on lance dans n'importe quel terminal |
| **psmux** (Rust) | Oui | Oui | Oui | Oui | Jeune (2025+), ~83 commandes tmux, lit `.tmux.conf` |
| **tmux-windows** (port Win32) | Oui | Oui | Oui | Oui | Très confidentiel (quasi aucune adoption) |
| **Zellij 0.44+** | Oui (support Windows annoncé en mars 2026) | Oui | Oui | Oui | Support Windows tout récent |
| **tmux via WSL/Cygwin/MSYS2** | Non | Oui | Oui | Partielle | Ne multiplexe que des shells POSIX, pas PowerShell/cmd natifs |

## Ce qui manquait historiquement (et qui est aujourd'hui résolu côté OS)

1. **Pseudo-terminal** : résolu par **ConPTY** (`CreatePseudoConsole`, Windows 10 1809+).
2. **IPC de type socket Unix** : résolu par le support **AF_UNIX** (build 17063+) et les **Named Pipes** (depuis toujours).
3. **Durée de vie des processus liée à la console** : contournable avec un **serveur détaché** (`CREATE_NO_WINDOW`) et des **Job Objects**.

## Conséquence pour le projet « wimux »

La fenêtre d'opportunité : les concurrents natifs (psmux, tmux-windows) sont des clones de tmux avant tout ; aucun ne propose une expérience **pensée pour Windows** :
- intégration PowerShell de première classe (profils, complétion, cmdlets de pilotage) ;
- installation et mise à jour via winget/Scoop ;
- UX moderne à la Zellij (raccourcis découvrables, layouts déclaratifs) ;
- documentation et communauté francophones inexistantes sur ce créneau.

**Positionnement recommandé** : ne pas viser la compatibilité tmux à 100 % (psmux occupe déjà cette case), mais un multiplexeur natif Windows avec les concepts de tmux (sessions persistantes, detach/reattach, panes) et l'ergonomie de Zellij.

## Sources principales

- ConPTY : https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/
- AF_UNIX sur Windows : https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/
- psmux : https://github.com/psmux/psmux
- tmux-windows : https://github.com/bitcode/tmux-windows
- Zellij sur Windows : https://zellij.dev/news/remote-sessions-windows-cli/
- WezTerm multiplexing : https://wezterm.org/multiplexing.html
