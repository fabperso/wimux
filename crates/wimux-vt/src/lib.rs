//! Émulation de terminal (grille + scrollback) pour un volet.
//!
//! Chaque volet possède sa propre instance : le serveur y injecte en continu
//! le flux d'octets produit par ConPTY et maintient à jour une grille de
//! cellules, **même sans client attaché**. Un client qui (r)attache reçoit un
//! instantané de cette grille, puis les deltas.
//!
//! Phase 0 : uniquement le modèle de données (grille, cellule, redimension).
//! Le branchement d'un vrai parser VT (décision `wezterm-term` vs `vte` + grille
//! maison, cf. ADR-0002) intervient au jalon J1.

use serde::{Deserialize, Serialize};

/// Une cellule de la grille : un caractère et (à terme) ses attributs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    // TODO(J1) : couleurs avant/arrière-plan, gras, souligné, etc.
}

impl Cell {
    pub fn blank() -> Self {
        Cell { ch: ' ' }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::blank()
    }
}

/// Grille de cellules de taille fixe (le viewport visible d'un volet).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// Crée une grille vide de `rows` x `cols` (au moins 1x1).
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Grid {
            cols,
            rows,
            cells: vec![Cell::blank(); cols as usize * rows as usize],
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Cellule en (col, row), ou `None` hors limites.
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        self.index(col, row).map(|i| &self.cells[i])
    }

    /// Écrit une cellule en (col, row) si dans les limites.
    pub fn set(&mut self, col: u16, row: u16, cell: Cell) {
        if let Some(i) = self.index(col, row) {
            self.cells[i] = cell;
        }
    }

    /// Redimensionne la grille. Le contenu commun (coin haut-gauche) est
    /// préservé ; les nouvelles cellules sont vides. Le reflow fidèle
    /// (recomposition des lignes longues) viendra avec le parser réel.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut next = vec![Cell::blank(); cols as usize * rows as usize];
        let copy_cols = cols.min(self.cols);
        let copy_rows = rows.min(self.rows);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                let src = r as usize * self.cols as usize + c as usize;
                let dst = r as usize * cols as usize + c as usize;
                next[dst] = self.cells[src].clone();
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = next;
    }

    fn index(&self, col: u16, row: u16) -> Option<usize> {
        if col < self.cols && row < self.rows {
            Some(row as usize * self.cols as usize + col as usize)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nouvelle_grille_est_vide() {
        let g = Grid::new(3, 2);
        assert_eq!(g.cols(), 3);
        assert_eq!(g.rows(), 2);
        assert_eq!(g.cell(0, 0), Some(&Cell::blank()));
    }

    #[test]
    fn dimensions_minimales_forcees() {
        let g = Grid::new(0, 0);
        assert_eq!((g.cols(), g.rows()), (1, 1));
    }

    #[test]
    fn set_puis_get() {
        let mut g = Grid::new(4, 4);
        g.set(2, 1, Cell { ch: 'X' });
        assert_eq!(g.cell(2, 1), Some(&Cell { ch: 'X' }));
    }

    #[test]
    fn hors_limites_ignore() {
        let mut g = Grid::new(2, 2);
        g.set(5, 5, Cell { ch: 'Z' });
        assert_eq!(g.cell(5, 5), None);
    }

    #[test]
    fn resize_preserve_le_coin_haut_gauche() {
        let mut g = Grid::new(3, 3);
        g.set(0, 0, Cell { ch: 'A' });
        g.set(2, 2, Cell { ch: 'B' });
        g.resize(2, 2);
        assert_eq!(g.cell(0, 0), Some(&Cell { ch: 'A' }));
        // (2,2) est hors de la nouvelle grille 2x2.
        assert_eq!(g.cell(2, 2), None);
    }

    #[test]
    fn resize_agrandit_avec_cellules_vides() {
        let mut g = Grid::new(2, 2);
        g.set(0, 0, Cell { ch: 'A' });
        g.resize(4, 4);
        assert_eq!(g.cell(0, 0), Some(&Cell { ch: 'A' }));
        assert_eq!(g.cell(3, 3), Some(&Cell::blank()));
    }
}
