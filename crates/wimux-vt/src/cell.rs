//! Modèle d'une cellule de terminal : caractère, couleurs, attributs, largeur.

use serde::{Deserialize, Serialize};

/// Couleur d'avant- ou d'arrière-plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Color {
    /// Couleur par défaut du terminal.
    #[default]
    Default,
    /// Couleur de la palette indexée (0-255).
    Indexed(u8),
    /// Couleur « vraie » 24 bits.
    Rgb(u8, u8, u8),
}

/// Attributs de style d'une cellule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// « Stylo » courant : couleurs et attributs appliqués aux caractères imprimés.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

/// Une cellule de la grille.
///
/// `width` vaut 1 (normal), 2 (caractère large type CJK/emoji, qui occupe deux
/// colonnes) ou 0 (colonne de continuation, juste après un caractère large :
/// elle ne doit pas être rendue séparément).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    pub pen: Pen,
    pub width: u8,
}

impl Cell {
    /// Cellule vide (espace, style par défaut).
    pub fn blank() -> Self {
        Cell {
            ch: ' ',
            pen: Pen::default(),
            width: 1,
        }
    }

    /// Cellule vide conservant un style d'arrière-plan (utile pour l'effacement
    /// avec couleur de fond active).
    pub fn blank_with(pen: Pen) -> Self {
        Cell {
            ch: ' ',
            pen,
            width: 1,
        }
    }

    pub fn new(ch: char, pen: Pen, width: u8) -> Self {
        Cell { ch, pen, width }
    }

    /// Colonne de continuation d'un caractère large (non rendue seule).
    pub fn continuation(pen: Pen) -> Self {
        Cell {
            ch: ' ',
            pen,
            width: 0,
        }
    }

    /// Vrai si cette cellule est une continuation de caractère large.
    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::blank()
    }
}
