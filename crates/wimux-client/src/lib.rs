//! Client TUI `wimux` : s'exécute dans n'importe quel terminal VT, se connecte
//! au serveur, affiche la grille reçue et transmet les entrées clavier. Le
//! client est « jetable » : le fermer (ou `Ctrl-b d`) revient à se détacher, la
//! session survit côté serveur.
//!
//! Architecture en threads autour d'une connexion partagée :
//! - **lecteur** : reçoit les `ServerMessage` et rend chaque frame ;
//! - **émetteur** : unique writer de la connexion, alimenté par un canal, ce qui
//!   sérialise toutes les écritures (frappes + redimensionnements) ;
//! - **entrée** : lit l'entrée VT brute et l'envoie (en interceptant le préfixe) ;
//! - **redimensionnement** : signale les changements de taille du terminal.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use crossterm::style::{
    Attribute, Color as CtColor, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute, queue};

use wimux_protocol::transport::PipeConn;
use wimux_protocol::{ClientMessage, Frame, ServerMessage, Version, recv, send};
use wimux_vt::{Cell, Color, Pen};

mod console;

/// Raison pour laquelle la boucle d'attachement se termine.
#[derive(Debug, Clone)]
pub enum ExitReason {
    /// L'utilisateur s'est détaché (Ctrl-b d) — la session reste vivante.
    Detached,
    /// Le shell du volet s'est terminé.
    PaneExited(u32),
    /// La connexion au serveur a été perdue.
    ServerGone,
}

/// Vérifie côté client que notre version de protocole est compatible avec celle
/// annoncée par le serveur.
pub fn is_server_compatible(server: Version) -> bool {
    wimux_protocol::PROTOCOL_VERSION.is_compatible_with(server)
}

/// Prend en charge une connexion déjà attachée à une session et pilote la boucle
/// TUI jusqu'au détachement, à la fin du volet, ou à la perte du serveur.
pub fn run(conn: PipeConn) -> io::Result<ExitReason> {
    let conn = Arc::new(conn);

    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, cursor::Hide)?;
    let _stdin_mode = console::RawStdinGuard::set();

    let (exit_tx, exit_rx) = channel::<ExitReason>();
    let (out_tx, out_rx) = channel::<ClientMessage>();

    // Thread émetteur : unique writer de la connexion.
    {
        let conn = Arc::clone(&conn);
        let exit_tx = exit_tx.clone();
        std::thread::spawn(move || {
            for msg in out_rx {
                let mut w: &PipeConn = &conn;
                if send(&mut w, &msg).is_err() {
                    let _ = exit_tx.send(ExitReason::ServerGone);
                    break;
                }
            }
        });
    }

    // Thread lecteur : reçoit les frames et les rend.
    {
        let conn = Arc::clone(&conn);
        let exit_tx = exit_tx.clone();
        std::thread::spawn(move || {
            let mut reader: &PipeConn = &conn;
            loop {
                match recv::<_, ServerMessage>(&mut reader) {
                    Ok(ServerMessage::Frame(frame)) => {
                        let _ = render(&frame);
                    }
                    Ok(ServerMessage::PaneExited { code }) => {
                        let _ = exit_tx.send(ExitReason::PaneExited(code));
                        break;
                    }
                    Ok(ServerMessage::Detached) => {
                        let _ = exit_tx.send(ExitReason::Detached);
                        break;
                    }
                    Ok(ServerMessage::SetClipboard(text)) => {
                        let _ = clipboard_win::set_clipboard_string(&text);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        let _ = exit_tx.send(ExitReason::ServerGone);
                        break;
                    }
                }
            }
        });
    }

    // Thread de redimensionnement : signale les changements de taille.
    {
        let out_tx = out_tx.clone();
        std::thread::spawn(move || {
            let mut last = crossterm::terminal::size().unwrap_or((80, 24));
            loop {
                std::thread::sleep(Duration::from_millis(200));
                if let Ok(size) = crossterm::terminal::size()
                    && size != last
                {
                    last = size;
                    if out_tx
                        .send(ClientMessage::Resize {
                            cols: size.0,
                            rows: size.1,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
    }

    // Thread d'entrée : transmet toute l'entrée brute au serveur (qui interprète
    // le préfixe Ctrl-b lui-même).
    {
        let out_tx = out_tx.clone();
        std::thread::spawn(move || input_loop(out_tx));
    }

    // Attendre la première raison de sortie.
    let reason = exit_rx.recv().unwrap_or(ExitReason::ServerGone);

    // Restauration du terminal.
    let mut out = io::stdout();
    let _ = execute!(out, cursor::Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();

    Ok(reason)
}

/// Boucle d'entrée : transmet l'entrée VT brute au serveur. Le préfixe `Ctrl-b`
/// et ses commandes sont décodés côté serveur.
fn input_loop(out_tx: Sender<ClientMessage>) {
    let mut stdin = io::stdin();
    let mut buf = [0u8; 1024];

    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if out_tx
            .send(ClientMessage::Input(buf[..n].to_vec()))
            .is_err()
        {
            break;
        }
    }
}

/// Rend une frame complète sur la sortie standard.
fn render(frame: &Frame) -> io::Result<()> {
    let mut out = io::stdout();
    queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;

    let mut last_pen: Option<Pen> = None;
    for row in 0..frame.rows {
        queue!(out, cursor::MoveTo(0, row))?;
        let mut col = 0u16;
        while col < frame.cols {
            let idx = row as usize * frame.cols as usize + col as usize;
            let cell = &frame.cells[idx];
            if cell.is_continuation() {
                col += 1;
                continue;
            }
            if last_pen != Some(cell.pen) {
                apply_pen(&mut out, cell.pen)?;
                last_pen = Some(cell.pen);
            }
            queue!(out, Print(cell.ch))?;
            col += (cell.width as u16).max(1);
        }
    }

    // Réinitialiser le style et positionner le curseur réel.
    apply_pen(&mut io::stdout(), Pen::default())?;
    queue!(
        out,
        cursor::MoveTo(frame.cursor_col, frame.cursor_row),
        cursor::Show
    )?;
    out.flush()
}

fn apply_pen(out: &mut impl Write, pen: Pen) -> io::Result<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;
    queue!(out, SetForegroundColor(to_ct(pen.fg, true)))?;
    queue!(out, SetBackgroundColor(to_ct(pen.bg, false)))?;
    if pen.attrs.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if pen.attrs.italic {
        queue!(out, SetAttribute(Attribute::Italic))?;
    }
    if pen.attrs.underline {
        queue!(out, SetAttribute(Attribute::Underlined))?;
    }
    if pen.attrs.reverse {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}

fn to_ct(color: Color, _foreground: bool) -> CtColor {
    match color {
        Color::Default => CtColor::Reset,
        Color::Indexed(n) => CtColor::AnsiValue(n),
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
    }
}

/// Rend une frame en texte (utilisé par les tests d'intégration).
pub fn frame_to_text(frame: &Frame) -> String {
    let mut lines = Vec::new();
    for row in 0..frame.rows {
        let mut line = String::new();
        for cell in cells_of_row(frame, row) {
            if !cell.is_continuation() {
                line.push(cell.ch);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

fn cells_of_row(frame: &Frame, row: u16) -> &[Cell] {
    let start = row as usize * frame.cols as usize;
    &frame.cells[start..start + frame.cols as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serveur_meme_majeure_compatible() {
        let server = Version {
            major: wimux_protocol::PROTOCOL_VERSION.major,
            minor: wimux_protocol::PROTOCOL_VERSION.minor + 5,
        };
        assert!(is_server_compatible(server));
    }
}
