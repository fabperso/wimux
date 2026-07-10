//! Grille de cellules de taille fixe (le viewport visible d'un volet).

use serde::{Deserialize, Serialize};

use crate::cell::{Cell, Pen};

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

    /// Toutes les cellules d'une ligne (vide si `row` hors limites).
    pub fn row(&self, row: u16) -> &[Cell] {
        match self.index(0, row) {
            Some(start) => &self.cells[start..start + self.cols as usize],
            None => &[],
        }
    }

    /// Remplit une plage de colonnes `[from, to)` d'une ligne avec `cell`.
    pub fn fill_row(&mut self, row: u16, from: u16, to: u16, cell: &Cell) {
        if row >= self.rows {
            return;
        }
        let to = to.min(self.cols);
        for col in from..to {
            if let Some(i) = self.index(col, row) {
                self.cells[i] = *cell;
            }
        }
    }

    /// Vide entièrement une ligne (avec le style `pen` pour l'arrière-plan).
    pub fn clear_row(&mut self, row: u16, pen: Pen) {
        self.fill_row(row, 0, self.cols, &Cell::blank_with(pen));
    }

    /// Vide toute la grille.
    pub fn clear(&mut self, pen: Pen) {
        for row in 0..self.rows {
            self.clear_row(row, pen);
        }
    }

    /// Fait défiler la grille d'une ligne vers le haut : la ligne 0 est retournée
    /// (pour l'archiver en scrollback), les lignes remontent, et la dernière
    /// ligne est vidée avec le style `pen`.
    pub fn scroll_up(&mut self, pen: Pen) -> Vec<Cell> {
        let cols = self.cols as usize;
        let first: Vec<Cell> = self.cells[..cols].to_vec();
        self.cells.copy_within(cols.., 0);
        let start = (self.rows as usize - 1) * cols;
        for cell in &mut self.cells[start..] {
            *cell = Cell::blank_with(pen);
        }
        first
    }

    /// Redimensionne la grille. Le contenu du coin haut-gauche est préservé ;
    /// les nouvelles cellules sont vides. Le reflow fidèle viendra plus tard.
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
                next[dst] = self.cells[src];
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

    /// Recopie la grille `src` dans cette grille, coin haut-gauche en `(x0, y0)`.
    /// Utilisé par le serveur pour composer plusieurs volets en une seule vue.
    pub fn blit(&mut self, src: &Grid, x0: u16, y0: u16) {
        for r in 0..src.rows {
            for c in 0..src.cols {
                let cell = src.cells[r as usize * src.cols as usize + c as usize];
                self.set(x0 + c, y0 + r, cell);
            }
        }
    }

    /// Écrit une chaîne à partir de `(x, y)`, sur une seule ligne, tronquée aux
    /// limites de la grille. Ne gère pas les caractères larges (usage : bordures
    /// et barre de statut).
    pub fn set_str(&mut self, x: u16, y: u16, text: &str, pen: Pen) {
        for (i, ch) in text.chars().enumerate() {
            let col = x.saturating_add(i as u16);
            if col >= self.cols {
                break;
            }
            self.set(col, y, Cell::new(ch, pen, 1));
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
        let cell = Cell::new('X', Pen::default(), 1);
        g.set(2, 1, cell);
        assert_eq!(g.cell(2, 1), Some(&cell));
    }

    #[test]
    fn hors_limites_ignore() {
        let mut g = Grid::new(2, 2);
        g.set(5, 5, Cell::new('Z', Pen::default(), 1));
        assert_eq!(g.cell(5, 5), None);
    }

    #[test]
    fn resize_preserve_le_coin_haut_gauche() {
        let mut g = Grid::new(3, 3);
        let a = Cell::new('A', Pen::default(), 1);
        g.set(0, 0, a);
        g.set(2, 2, Cell::new('B', Pen::default(), 1));
        g.resize(2, 2);
        assert_eq!(g.cell(0, 0), Some(&a));
        assert_eq!(g.cell(2, 2), None);
    }

    #[test]
    fn scroll_up_remonte_les_lignes() {
        let mut g = Grid::new(2, 3);
        g.set(0, 0, Cell::new('a', Pen::default(), 1));
        g.set(0, 1, Cell::new('b', Pen::default(), 1));
        g.set(0, 2, Cell::new('c', Pen::default(), 1));
        let sortie = g.scroll_up(Pen::default());
        assert_eq!(sortie[0].ch, 'a');
        assert_eq!(g.cell(0, 0).unwrap().ch, 'b');
        assert_eq!(g.cell(0, 1).unwrap().ch, 'c');
        assert_eq!(g.cell(0, 2).unwrap().ch, ' ');
    }

    #[test]
    fn clear_row_vide_une_ligne() {
        let mut g = Grid::new(3, 2);
        g.set(1, 0, Cell::new('x', Pen::default(), 1));
        g.clear_row(0, Pen::default());
        assert_eq!(g.cell(1, 0).unwrap().ch, ' ');
    }
}
