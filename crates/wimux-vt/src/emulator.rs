// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
//! Moteur d'émulation VT : consomme le flux d'octets d'un volet et maintient une
//! grille à jour. S'appuie sur `vte` (machine à états d'Alacritty) pour le
//! parsing ; toute la sémantique « écran » (curseur, impression, effacements,
//! SGR, défilement) est implémentée ici.
//!
//! Portée phase 2 : impression avec caractères larges, `CR`/`LF`/`BS`/`TAB`,
//! déplacements de curseur (CUP/CUU/CUD/CUF/CUB/CHA/VPA), effacements (ED/EL),
//! attributs SGR courants, réponses aux requêtes (DSR/CPR, DA). Le reste
//! (scrollback navigable, écran alterné, reflow) s'ajoute aux phases suivantes.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

use crate::cell::{Cell, Color, Pen};
use crate::grid::Grid;

/// Nombre maximal de lignes conservées en scrollback (historique hors écran).
const HISTORY_LIMIT: usize = 5000;

/// Terminal virtuel d'un volet : parser + écran.
pub struct Terminal {
    parser: Parser,
    screen: Screen,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        Terminal {
            parser: Parser::new(),
            screen: Screen::new(cols, rows),
        }
    }

    /// Fait avancer l'émulation avec un fragment du flux de sortie du volet.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }

    /// Grille visible courante.
    pub fn grid(&self) -> &Grid {
        &self.screen.grid
    }

    /// Position du curseur (colonne, ligne), 0-based.
    pub fn cursor(&self) -> (u16, u16) {
        (self.screen.cx, self.screen.cy)
    }

    /// Récupère (et vide) les octets à renvoyer vers l'entrée du volet en
    /// réponse aux requêtes du terminal (CPR, DA...). À écrire sur le PTY.
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.screen.responses)
    }

    /// Récupère (et remet à faux) le drapeau « cloche » : `true` si au moins un
    /// BEL (`0x07`) a été reçu comme contrôle depuis le dernier appel. Miroir de
    /// `take_responses`.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.screen.bell_pending)
    }

    /// Lignes de scrollback (les plus anciennes en tête).
    pub fn history(&self) -> &VecDeque<Vec<Cell>> {
        &self.screen.history
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.screen.resize(cols, rows);
    }
}

struct Screen {
    grid: Grid,
    cols: u16,
    rows: u16,
    cx: u16,
    cy: u16,
    pen: Pen,
    /// Enroulement différé : le curseur « colle » à la marge droite jusqu'au
    /// prochain caractère imprimable (comportement xterm).
    wrap_next: bool,
    history: VecDeque<Vec<Cell>>,
    responses: Vec<u8>,
    /// Un BEL (`0x07`) a été reçu comme contrôle C0 depuis le dernier `take_bell`.
    bell_pending: bool,
}

impl Screen {
    fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Screen {
            grid: Grid::new(cols, rows),
            cols,
            rows,
            cx: 0,
            cy: 0,
            pen: Pen::default(),
            wrap_next: false,
            history: VecDeque::new(),
            responses: Vec::new(),
            bell_pending: false,
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        self.grid.resize(cols, rows);
        self.cols = cols;
        self.rows = rows;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.wrap_next = false;
    }

    fn line_feed(&mut self) {
        if self.cy + 1 >= self.rows {
            let scrolled = self.grid.scroll_up(self.pen);
            self.history.push_back(scrolled);
            while self.history.len() > HISTORY_LIMIT {
                self.history.pop_front();
            }
        } else {
            self.cy += 1;
        }
    }

    fn put_char(&mut self, c: char) {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if w == 0 {
            // Caractère combinant / de largeur nulle : ignoré pour l'instant
            // (le rattacher au glyphe précédent viendra plus tard).
            return;
        }
        let w = w as u16;

        if self.wrap_next {
            self.cx = 0;
            self.line_feed();
            self.wrap_next = false;
        }

        // Si le caractère large ne tient pas en fin de ligne, on enroule.
        if self.cx + w > self.cols {
            self.cx = 0;
            self.line_feed();
        }

        self.grid
            .set(self.cx, self.cy, Cell::new(c, self.pen, w as u8));
        if w == 2 {
            self.grid
                .set(self.cx + 1, self.cy, Cell::continuation(self.pen));
        }

        self.cx += w;
        if self.cx >= self.cols {
            // Coller à la marge droite ; l'enroulement se fera au prochain char.
            self.cx = self.cols - 1;
            self.wrap_next = true;
        }
    }

    fn move_to(&mut self, col: u16, row: u16) {
        self.cx = col.min(self.cols - 1);
        self.cy = row.min(self.rows - 1);
        self.wrap_next = false;
    }

    /// ED — effacement d'écran. mode: 0 curseur→fin, 1 début→curseur, 2/3 tout.
    fn erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                self.grid
                    .fill_row(self.cy, self.cx, self.cols, &Cell::blank_with(self.pen));
                for row in (self.cy + 1)..self.rows {
                    self.grid.clear_row(row, self.pen);
                }
            }
            1 => {
                for row in 0..self.cy {
                    self.grid.clear_row(row, self.pen);
                }
                self.grid
                    .fill_row(self.cy, 0, self.cx + 1, &Cell::blank_with(self.pen));
            }
            _ => self.grid.clear(self.pen),
        }
    }

    /// EL — effacement de ligne. mode: 0 curseur→fin, 1 début→curseur, 2 tout.
    fn erase_line(&mut self, mode: u16) {
        match mode {
            0 => self
                .grid
                .fill_row(self.cy, self.cx, self.cols, &Cell::blank_with(self.pen)),
            1 => self
                .grid
                .fill_row(self.cy, 0, self.cx + 1, &Cell::blank_with(self.pen)),
            _ => self.grid.clear_row(self.cy, self.pen),
        }
    }

    fn apply_sgr(&mut self, ps: &[u16]) {
        let mut i = 0;
        if ps.is_empty() {
            self.pen = Pen::default();
            return;
        }
        while i < ps.len() {
            match ps[i] {
                0 => self.pen = Pen::default(),
                1 => self.pen.attrs.bold = true,
                3 => self.pen.attrs.italic = true,
                4 => self.pen.attrs.underline = true,
                7 => self.pen.attrs.reverse = true,
                22 => self.pen.attrs.bold = false,
                23 => self.pen.attrs.italic = false,
                24 => self.pen.attrs.underline = false,
                27 => self.pen.attrs.reverse = false,
                30..=37 => self.pen.fg = Color::Indexed((ps[i] - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((ps[i] - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((ps[i] - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((ps[i] - 100 + 8) as u8),
                38 => {
                    if let Some((color, consumed)) = parse_extended_color(&ps[i..]) {
                        self.pen.fg = color;
                        i += consumed;
                        continue;
                    }
                }
                48 => {
                    if let Some((color, consumed)) = parse_extended_color(&ps[i..]) {
                        self.pen.bg = color;
                        i += consumed;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Analyse `38;5;n` (indexé) ou `38;2;r;g;b` (RGB) à partir de `ps` (qui commence
/// par 38 ou 48). Retourne la couleur et le nombre d'éléments consommés.
fn parse_extended_color(ps: &[u16]) -> Option<(Color, usize)> {
    match ps.get(1) {
        Some(5) => ps.get(2).map(|&n| (Color::Indexed(n as u8), 3)),
        Some(2) => {
            let r = *ps.get(2)? as u8;
            let g = *ps.get(3)? as u8;
            let b = *ps.get(4)? as u8;
            Some((Color::Rgb(r, g, b), 5))
        }
        _ => None,
    }
}

/// Convertit les paramètres d'une séquence CSI en valeurs primaires (une par
/// paramètre), en substituant `default` aux valeurs absentes/nulles.
fn primary_params(params: &Params, default: u16) -> Vec<u16> {
    params
        .iter()
        .map(|sub| {
            let v = sub.first().copied().unwrap_or(0);
            if v == 0 { default } else { v }
        })
        .collect()
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0a..=0x0c => {
                // LF / VT / FF : descendre d'une ligne.
                self.line_feed();
                self.wrap_next = false;
            }
            0x0d => {
                self.cx = 0;
                self.wrap_next = false;
            }
            0x08 => {
                self.cx = self.cx.saturating_sub(1);
                self.wrap_next = false;
            }
            0x09 => {
                let next = ((self.cx / 8) + 1) * 8;
                self.cx = next.min(self.cols - 1);
                self.wrap_next = false;
            }
            0x07 => self.bell_pending = true,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // On ignore les séquences privées (préfixe '?') non gérées.
        let private = intermediates.first() == Some(&b'?');
        match action {
            'H' | 'f' if !private => {
                let ps = primary_params(params, 1);
                let row = ps.first().copied().unwrap_or(1);
                let col = ps.get(1).copied().unwrap_or(1);
                self.move_to(col.saturating_sub(1), row.saturating_sub(1));
            }
            'A' => {
                let n = primary_params(params, 1).first().copied().unwrap_or(1);
                self.cy = self.cy.saturating_sub(n);
                self.wrap_next = false;
            }
            'B' => {
                let n = primary_params(params, 1).first().copied().unwrap_or(1);
                self.cy = (self.cy + n).min(self.rows - 1);
                self.wrap_next = false;
            }
            'C' => {
                let n = primary_params(params, 1).first().copied().unwrap_or(1);
                self.cx = (self.cx + n).min(self.cols - 1);
                self.wrap_next = false;
            }
            'D' => {
                let n = primary_params(params, 1).first().copied().unwrap_or(1);
                self.cx = self.cx.saturating_sub(n);
                self.wrap_next = false;
            }
            'G' => {
                let n = primary_params(params, 1).first().copied().unwrap_or(1);
                self.cx = n.saturating_sub(1).min(self.cols - 1);
                self.wrap_next = false;
            }
            'd' => {
                let n = primary_params(params, 1).first().copied().unwrap_or(1);
                self.cy = n.saturating_sub(1).min(self.rows - 1);
                self.wrap_next = false;
            }
            'J' if !private => {
                let mode = params
                    .iter()
                    .next()
                    .and_then(|s| s.first())
                    .copied()
                    .unwrap_or(0);
                self.erase_display(mode);
            }
            'K' if !private => {
                let mode = params
                    .iter()
                    .next()
                    .and_then(|s| s.first())
                    .copied()
                    .unwrap_or(0);
                self.erase_line(mode);
            }
            'm' => {
                let ps: Vec<u16> = params
                    .iter()
                    .map(|s| s.first().copied().unwrap_or(0))
                    .collect();
                self.apply_sgr(&ps);
            }
            'n' if !private => {
                // DSR : 6 -> position curseur (CPR), 5 -> état appareil.
                let req = params
                    .iter()
                    .next()
                    .and_then(|s| s.first())
                    .copied()
                    .unwrap_or(0);
                match req {
                    6 => {
                        let reply = format!("\x1b[{};{}R", self.cy + 1, self.cx + 1);
                        self.responses.extend_from_slice(reply.as_bytes());
                    }
                    5 => self.responses.extend_from_slice(b"\x1b[0n"),
                    _ => {}
                }
            }
            'c' if !private => {
                // Primary Device Attributes : on se déclare « VT100 avancé ».
                self.responses.extend_from_slice(b"\x1b[?1;2c");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_row(t: &Terminal, row: u16) -> String {
        t.grid()
            .row(row)
            .iter()
            .filter(|c| !c.is_continuation())
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn impression_simple() {
        let mut t = Terminal::new(20, 5);
        t.advance(b"Bonjour");
        assert_eq!(text_row(&t, 0), "Bonjour");
        assert_eq!(t.cursor(), (7, 0));
    }

    #[test]
    fn retour_chariot_et_saut_de_ligne() {
        let mut t = Terminal::new(20, 5);
        t.advance(b"abc\r\ndef");
        assert_eq!(text_row(&t, 0), "abc");
        assert_eq!(text_row(&t, 1), "def");
    }

    #[test]
    fn cup_positionne_le_curseur() {
        let mut t = Terminal::new(20, 5);
        t.advance(b"\x1b[3;5HX");
        // ligne 3, colonne 5 (1-based) -> (4, 2) 0-based, puis X imprimé.
        assert_eq!(t.grid().cell(4, 2).unwrap().ch, 'X');
    }

    #[test]
    fn effacement_ecran_total() {
        let mut t = Terminal::new(10, 3);
        t.advance(b"plein\x1b[2J");
        assert_eq!(text_row(&t, 0), "");
    }

    #[test]
    fn caractere_large_occupe_deux_colonnes() {
        let mut t = Terminal::new(10, 2);
        t.advance("世".as_bytes());
        let c0 = t.grid().cell(0, 0).unwrap();
        assert_eq!(c0.ch, '世');
        assert_eq!(c0.width, 2);
        assert!(t.grid().cell(1, 0).unwrap().is_continuation());
        assert_eq!(t.cursor(), (2, 0));
    }

    #[test]
    fn sgr_applique_une_couleur() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"\x1b[31mR");
        assert_eq!(t.grid().cell(0, 0).unwrap().pen.fg, Color::Indexed(1));
    }

    #[test]
    fn sgr_truecolor() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"\x1b[38;2;10;20;30mX");
        assert_eq!(t.grid().cell(0, 0).unwrap().pen.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn dsr_cursor_genere_une_reponse_cpr() {
        let mut t = Terminal::new(20, 5);
        t.advance(b"\x1b[2;3H\x1b[6n");
        let resp = t.take_responses();
        assert_eq!(resp, b"\x1b[2;3R");
    }

    #[test]
    fn defilement_quand_on_depasse_le_bas() {
        let mut t = Terminal::new(5, 2);
        t.advance(b"un\r\ndeux\r\ntrois");
        // 2 lignes visibles : "deux" et "trois" ; "un" est parti en scrollback.
        assert_eq!(text_row(&t, 0), "deux");
        assert_eq!(text_row(&t, 1), "trois");
        assert_eq!(t.history().len(), 1);
        assert_eq!(t.history()[0][0].ch, 'u');
    }

    #[test]
    fn enroulement_en_fin_de_ligne() {
        let mut t = Terminal::new(3, 3);
        t.advance(b"abcd");
        assert_eq!(text_row(&t, 0), "abc");
        assert_eq!(text_row(&t, 1), "d");
    }

    #[test]
    fn bel_nu_leve_la_cloche() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"\x07");
        assert!(t.take_bell(), "un BEL nu doit lever la cloche");
        assert!(!t.take_bell(), "la cloche est consommée au premier take");
    }

    #[test]
    fn bel_terminateur_osc_ne_leve_pas_la_cloche() {
        let mut t = Terminal::new(10, 2);
        // ESC ] 0 ; titre BEL : le BEL termine l'OSC, il ne passe pas par execute.
        t.advance(b"\x1b]0;titre\x07");
        assert!(
            !t.take_bell(),
            "un BEL terminateur d'OSC ne doit PAS lever la cloche"
        );
    }

    #[test]
    fn sans_bel_pas_de_cloche() {
        let mut t = Terminal::new(10, 2);
        t.advance(b"abc");
        assert!(!t.take_bell());
    }
}
