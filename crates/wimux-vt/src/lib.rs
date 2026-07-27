// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
//! Émulation de terminal (grille + scrollback) pour un volet.
//!
//! Chaque volet possède sa propre instance de [`Terminal`] : le serveur y injecte
//! en continu le flux d'octets produit par ConPTY et maintient à jour une grille
//! de cellules, **même sans client attaché**. Un client qui (r)attache reçoit un
//! instantané de la grille, puis les deltas.
//!
//! - [`cell`] : modèle de cellule (caractère, couleurs, attributs, largeur).
//! - [`grid`] : grille de taille fixe (viewport visible).
//! - [`emulator`] : moteur VT branché sur `vte`, qui met à jour la grille.

pub mod cell;
pub mod emulator;
pub mod grid;

pub use cell::{Attrs, Cell, Color, Pen};
pub use emulator::Terminal;
pub use grid::Grid;
