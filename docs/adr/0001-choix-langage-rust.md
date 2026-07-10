# ADR-0001 — Langage : Rust

- **Statut** : accepté
- **Date** : 2026-07-10

## Contexte

wimux est un multiplexeur de terminal natif Windows (serveur détaché + client
TUI, ConPTY, IPC, parser VT). Le langage doit offrir un binaire autonome sans
runtime, un bon accès aux API Win32, de l'I/O asynchrone fiable, et un écosystème
terminal existant.

## Décision

**Rust.**

Justification :
- Écosystème terminal mûr et sous licence permissive (MIT/Apache) :
  `portable-pty`, `wezterm-term`, `vte`, `crossterm`, `ratatui`, `windows`.
- Binaire unique de quelques Mo, sans dépendance runtime — idéal pour une
  distribution winget/scoop.
- Précédents directs et probants : WezTerm, Zellij et psmux sont écrits en Rust.
- Sûreté mémoire dans un domaine (parsing d'octets non fiables, concurrence
  I/O) où les bugs sont autrement fréquents.

## Alternatives écartées

- **C# / .NET** : écosystème terminal pauvre (wrappers ConPTY épars), dépendance
  runtime ou publication AOT lourde.
- **C++** : réutilisation possible du code de Windows Terminal, mais extraction
  difficile et forte dépendance au SDK Windows.

## Conséquences

- Toolchain requise : Rust stable ≥ 1.85 (edition 2024). Développé sur 1.95.
- Workspace Cargo à 5 crates : `wimux-protocol`, `wimux-vt`, `wimux-server`,
  `wimux-client`, `wimux-cli`.
- La cible principale est `x86_64-pc-windows-msvc`.
