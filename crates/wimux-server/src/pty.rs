//! Enveloppe ConPTY : lancer un processus enfant dans une pseudo-console et
//! échanger avec lui. C'est la brique de dé-risquage de la phase 1.
//!
//! Sur Windows, `portable_pty::native_pty_system()` s'appuie sur l'API ConPTY
//! (`CreatePseudoConsole`). Deux règles ConPTY sont respectées ici :
//!  1. **Lecture sur un thread dédié** — servir l'entrée et la sortie sur le
//!     même thread peut provoquer un interblocage.
//!  2. **Fermer le côté esclave** après le spawn, sinon le lecteur ne verra
//!     jamais de fin de flux (EOF).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Résultat d'une exécution capturée à travers ConPTY.
#[derive(Debug)]
pub struct Capture {
    /// Sortie brute (contient les séquences VT émises par ConPTY).
    pub output: String,
    /// Code de sortie du processus enfant.
    pub exit_code: u32,
}

impl Capture {
    /// Sortie « nettoyée » des séquences d'échappement ANSI/VT les plus
    /// courantes, pratique pour les assertions de test. Ce n'est **pas** un
    /// vrai parser VT (ça viendra dans `wimux-vt`) : juste de quoi retrouver le
    /// texte visible dans un PoC.
    pub fn visible_text(&self) -> String {
        strip_ansi(&self.output)
    }
}

/// Lance `cmd` dans une pseudo-console de taille `size`, écrit éventuellement
/// `input` sur son entrée, puis capture toute la sortie jusqu'à la fin du
/// processus.
pub fn run_capture(cmd: CommandBuilder, size: PtySize, input: Option<&[u8]>) -> Result<Capture> {
    let pty = native_pty_system();
    let pair = pty.openpty(size)?;

    // Démarrer l'enfant sur le côté esclave de la pseudo-console.
    let mut child = pair.slave.spawn_command(cmd)?;

    // Lecteur cloné depuis le maître, servi sur un thread dédié (règle ConPTY :
    // ne jamais servir entrée et sortie sur le même thread).
    let mut reader = pair.master.try_clone_reader()?;

    // L'écrivain (stdin) est partagé entre ce thread (pour l'entrée utilisateur)
    // et le thread de lecture (pour répondre aux requêtes du terminal).
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));

    if let Some(bytes) = input {
        let mut w = writer.lock().expect("writer mutex");
        w.write_all(bytes)?;
        w.flush()?;
    }

    // Fermer l'esclave : le côté esclave ne doit plus être détenu par le parent.
    drop(pair.slave);

    // Point clé découvert au dé-risquage : ConPTY émet `ESC[6n` (DSR) au
    // démarrage et **bloque l'exécution du processus enfant** tant qu'aucune
    // réponse `ESC[<row>;<col>R` (CPR) ne lui parvient. Un multiplexeur est un
    // terminal : il DOIT répondre à ces requêtes. On le fait ici depuis le
    // thread de lecture, au fil de l'eau.
    let writer_for_reader = Arc::clone(&writer);
    let reader_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break, // EOF (survient au drop du maître, ci-dessous)
                Ok(n) => {
                    let slice = &chunk[..n];
                    buf.extend_from_slice(slice);
                    answer_terminal_queries(slice, &writer_for_reader);
                }
                Err(_) => break,
            }
        }
        buf
    });

    // Laisser l'enfant s'exécuter et se terminer naturellement.
    let status = child.wait()?;

    // Démanteler la pseudo-console (ClosePseudoConsole) : c'est ce drop qui
    // fait tomber l'EOF côté lecteur et débloque le thread.
    drop(pair.master);

    let buf = reader_thread
        .join()
        .map_err(|_| anyhow::anyhow!("le thread de lecture ConPTY a paniqué"))?;

    Ok(Capture {
        output: String::from_utf8_lossy(&buf).into_owned(),
        exit_code: status.exit_code(),
    })
}

/// Répond aux requêtes du terminal présentes dans `data` en écrivant sur
/// `writer` (le stdin de l'enfant). Sans ces réponses, ConPTY et certaines
/// applications se bloquent.
///
/// Gère pour l'instant :
///  - `ESC[6n` (DSR — Device Status Report, position curseur) -> `ESC[1;1R` (CPR)
///  - `ESC[5n` (DSR — état de l'appareil) -> `ESC[0n` (OK)
///
/// À enrichir (phase 2+) : Device Attributes (`ESC[c`, `ESC[>c`) avec la vraie
/// position du curseur suivie par la grille VT plutôt qu'un `1;1` fixe.
fn answer_terminal_queries(data: &[u8], writer: &Arc<Mutex<Box<dyn Write + Send>>>) {
    let reply = query_reply(data);
    if !reply.is_empty()
        && let Ok(mut w) = writer.lock()
    {
        let _ = w.write_all(&reply);
        let _ = w.flush();
    }
}

/// Calcule la réponse à envoyer pour les requêtes du terminal contenues dans
/// `data`. Fonction pure, testable sans pseudo-console.
fn query_reply(data: &[u8]) -> Vec<u8> {
    let mut reply = Vec::new();
    if contains(data, b"\x1b[6n") {
        // CPR : position curseur (1;1 en l'absence de grille suivie).
        reply.extend_from_slice(b"\x1b[1;1R");
    }
    if contains(data, b"\x1b[5n") {
        // DSR : appareil OK.
        reply.extend_from_slice(b"\x1b[0n");
    }
    reply
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Retire les séquences d'échappement CSI (`ESC [ ... lettre`) et OSC
/// (`ESC ] ... BEL|ST`) les plus courantes. Suffisant pour un PoC.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    // CSI : consommer jusqu'à un octet final 0x40..=0x7e.
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1; // l'octet final
                }
                b']' => {
                    // OSC : consommer jusqu'à BEL (0x07) ou ST (ESC \).
                    i += 2;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                _ => {
                    // Autre séquence ESC courte : sauter ESC + 1 octet.
                    i += 2;
                }
            }
        } else {
            // Reconstituer le caractère UTF-8 à partir de l'octet courant.
            let ch_len = utf8_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            if let Ok(chunk) = std::str::from_utf8(&bytes[i..end]) {
                out.push_str(chunk);
            }
            i = end;
        }
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_retire_les_sequences_csi() {
        let s = "\x1b[31mrouge\x1b[0m et normal";
        assert_eq!(strip_ansi(s), "rouge et normal");
    }

    #[test]
    fn strip_ansi_retire_les_sequences_osc() {
        let s = "\x1b]0;titre\x07contenu";
        assert_eq!(strip_ansi(s), "contenu");
    }

    #[test]
    fn strip_ansi_preserve_utf8() {
        let s = "caf\u{e9} \u{1f680}"; // café 🚀
        assert_eq!(strip_ansi(s), "café 🚀");
    }

    #[test]
    fn dsr_curseur_donne_une_reponse_cpr() {
        assert_eq!(query_reply(b"\x1b[6n"), b"\x1b[1;1R");
    }

    #[test]
    fn dsr_curseur_detecte_dans_un_flux() {
        let flux = b"avant\x1b[6napres";
        assert_eq!(query_reply(flux), b"\x1b[1;1R");
    }

    #[test]
    fn dsr_etat_appareil_donne_ok() {
        assert_eq!(query_reply(b"\x1b[5n"), b"\x1b[0n");
    }

    #[test]
    fn flux_sans_requete_ne_repond_rien() {
        assert!(query_reply(b"du texte normal\r\n").is_empty());
    }

    #[test]
    fn contains_fonctionne() {
        assert!(contains(b"abcdef", b"cde"));
        assert!(!contains(b"abc", b"xyz"));
        assert!(!contains(b"ab", b"abcdef"));
    }
}
