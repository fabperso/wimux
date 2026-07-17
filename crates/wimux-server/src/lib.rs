//! Bibliothèque du démon `wimux-server`. Le binaire (`main.rs`) s'appuie dessus,
//! et les tests d'intégration l'exercent directement.

pub mod commands;
pub mod config;
pub mod daemon;
pub mod git;
pub mod pane;
pub mod pty;
pub mod session;
pub mod window;
pub mod worktree;
