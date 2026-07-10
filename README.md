# wimux

Multiplexeur de terminal **natif Windows** : sessions persistantes, detach/reattach, fenêtres et volets — les concepts de tmux, l'ergonomie de Zellij, pensé pour PowerShell et Windows Terminal.

**Statut : phase 1 terminée** (juillet 2026) — le PoC ConPTY fonctionne : cmd et
PowerShell natifs s'exécutent dans une pseudo-console, sortie capturée et entrée
transmise, sans gel. Voir [ADR-0002](docs/adr/0002-conpty-lecons-du-poc.md).

## Documents

1. [État des lieux](docs/01-etat-des-lieux.md) — ce qui existe (ou pas) sur Windows et le positionnement du projet.
2. [Cahier des charges fonctionnel](docs/02-fonctionnalites.md) — inventaire des fonctionnalités tmux/Screen/Zellij, priorisées P0 → P3.
3. [Plan de développement](docs/03-plan-developpement.md) — choix techniques (Rust, ConPTY, Named Pipes), architecture client/serveur, phases et jalons.
4. [Décisions d'architecture (ADR)](docs/adr/) — [0001 choix de Rust](docs/adr/0001-choix-langage-rust.md), [0002 leçons du PoC ConPTY](docs/adr/0002-conpty-lecons-du-poc.md).

## Résumé du plan

- **Stack** : Rust · ConPTY (`portable-pty`) · émulation VT côté serveur (`wezterm-term`) · IPC par Named Pipes · client TUI (`crossterm`).
- **Architecture** : un serveur détaché par utilisateur (source de vérité, survit à la fermeture du terminal) + des clients légers qui s'attachent depuis n'importe quel terminal VT.
- **Jalon clé (J2)** : `wimux new -s dev` → fermer la fenêtre → `wimux attach -t dev` retrouve la session vivante.
- **MVP (v0.1)** : ~9-12 semaines · **v1.0** installable via winget : ~5-6 mois.
