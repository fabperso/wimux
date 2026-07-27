// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
//! Tests d'intégration ConPTY (phase 1, dé-risquage).
//!
//! Ils lancent de vrais processus Windows à travers une pseudo-console et
//! vérifient qu'on récupère bien leur sortie. C'est la preuve que la brique
//! fondamentale du multiplexeur fonctionne sur cette machine / en CI.

use portable_pty::{CommandBuilder, PtySize};
use wimux_server::pty::run_capture;

fn size() -> PtySize {
    PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[test]
fn cmd_echo_est_capture() {
    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "echo", "wimux-conpty-cmd-ok"]);

    let capture = run_capture(cmd, size(), None).expect("exécution ConPTY");

    assert!(
        capture.visible_text().contains("wimux-conpty-cmd-ok"),
        "sortie inattendue : {:?}",
        capture.output
    );
    assert_eq!(capture.exit_code, 0);
}

#[test]
fn powershell_natif_est_capture() {
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-Command",
        "Write-Output 'wimux-conpty-ps-ok'",
    ]);

    let capture = run_capture(cmd, size(), None).expect("exécution ConPTY");

    assert!(
        capture.visible_text().contains("wimux-conpty-ps-ok"),
        "sortie inattendue : {:?}",
        capture.output
    );
    assert_eq!(capture.exit_code, 0);
}

#[test]
fn unicode_et_emoji_traversent_conpty() {
    // Différenciateur clé : le rendu Unicode que psmux gère mal.
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-Command",
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Write-Output 'café €'",
    ]);

    let capture = run_capture(cmd, size(), None).expect("exécution ConPTY");

    assert!(
        capture.visible_text().contains("café"),
        "sortie inattendue : {:?}",
        capture.output
    );
}

#[test]
fn code_de_sortie_non_nul_remonte() {
    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "exit", "3"]);

    let capture = run_capture(cmd, size(), None).expect("exécution ConPTY");

    assert_eq!(capture.exit_code, 3);
}

#[test]
fn entree_ecrite_est_traitee() {
    // Un processus qui lit stdin puis le renvoie : prouve l'aller-retour.
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-Command",
        "$line = [Console]::In.ReadLine(); Write-Output \"recu:$line\"",
    ]);

    let capture =
        run_capture(cmd, size(), Some(b"ping123\r\n")).expect("exécution ConPTY avec entrée");

    assert!(
        capture.visible_text().contains("recu:ping123"),
        "sortie inattendue : {:?}",
        capture.output
    );
}
