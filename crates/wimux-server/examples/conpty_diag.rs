//! Diagnostic ConPTY chronométré : isole précisément l'étape qui bloque.
//! Lance : `cargo run -p wimux-server --example conpty_diag`

use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn main() -> Result<()> {
    let t0 = Instant::now();
    macro_rules! step {
        ($($arg:tt)*) => {{
            eprintln!("[{:>7.1}ms] {}", t0.elapsed().as_secs_f64() * 1000.0, format!($($arg)*));
        }};
    }

    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty = native_pty_system();
    let pair = pty.openpty(size)?;
    step!("openpty OK");

    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "echo", "DIAG-OK"]);
    let mut child = pair.slave.spawn_command(cmd)?;
    step!("spawn OK");

    let mut reader = pair.master.try_clone_reader()?;
    step!("clone_reader OK");

    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    step!("take_writer OK");

    drop(pair.slave);
    step!("drop slave OK");

    // Le thread de lecture signale chaque étape ET répond aux requêtes DSR.
    let (tx, rx) = mpsc::channel::<String>();
    let writer_for_reader = Arc::clone(&writer);
    let reader_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    let _ = tx.send("reader: EOF (read 0)".into());
                    break;
                }
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    let _ = tx.send(format!("reader: +{n} octets (total {})", buf.len()));
                    // Répondre à ESC[6n (DSR) par ESC[1;1R (CPR).
                    if chunk[..n].windows(4).any(|w| w == b"\x1b[6n") {
                        let _ = tx.send("reader: DSR detecte -> reponse CPR".into());
                        if let Ok(mut w) = writer_for_reader.lock() {
                            let _ = w.write_all(b"\x1b[1;1R");
                            let _ = w.flush();
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(format!("reader: erreur {e}"));
                    break;
                }
            }
        }
        buf
    });

    // Drainer les messages du lecteur pendant qu'on attend l'enfant, via un
    // second thread pour ne pas bloquer.
    let logger = std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            eprintln!("        (reader) {msg}");
        }
    });

    step!("avant child.wait()");
    let status = child.wait()?;
    step!("child.wait() -> code {}", status.exit_code());

    step!("avant drop writer");
    drop(writer);
    step!("writer refs restantes attendues nulles");
    step!("avant drop master");
    drop(pair.master);
    step!("avant join reader");
    let buf = reader_thread.join().unwrap();
    let _ = logger.join();
    step!("join OK, {} octets", buf.len());

    let text = String::from_utf8_lossy(&buf);
    eprintln!("contient DIAG-OK ? {}", text.contains("DIAG-OK"));
    Ok(())
}
