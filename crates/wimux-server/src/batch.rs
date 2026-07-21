//! Opérations git/`gh` de la revue de lot (M4) : collecte des résultats d'un
//! agent (strictement NON MUTANTE — ni index ni arbre de travail touchés, un
//! agent peut encore tourner) et intégration du gagnant par Pull Request.

use std::path::Path;
use std::process::Command;

/// Chiffres d'un diff (fichiers suivis).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DiffStats {
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
}

/// Analyse une sortie `git diff --numstat` : une ligne par fichier,
/// `<ajouts>\t<suppressions>\t<chemin>`. Un binaire est rapporté `-\t-\t<chemin>`
/// (compté comme fichier changé, sans lignes). Fonction pure, testable sans git.
pub fn parse_numstat(out: &str) -> DiffStats {
    let mut s = DiffStats::default();
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let (add, del) = (parts.next().unwrap_or("-"), parts.next().unwrap_or("-"));
        if parts.next().is_none() {
            continue; // ligne malformée : ignorée
        }
        s.files_changed += 1;
        s.insertions += add.parse::<u32>().unwrap_or(0);
        s.deletions += del.parse::<u32>().unwrap_or(0);
    }
    s
}

/// Exécute `git -C <dir> <args>` et renvoie stdout (ou stderr en `Err`).
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git indisponible : {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Chiffres du diff des fichiers SUIVIS du worktree vs `base_sha` (commité + en
/// cours). Non mutant.
pub fn diff_stats(wt: &Path, base_sha: &str) -> Result<DiffStats, String> {
    let out = git(wt, &["diff", "--numstat", base_sha])?;
    Ok(parse_numstat(&out))
}

/// Fichiers NON suivis du worktree (hors ignorés). Non mutant.
pub fn untracked(wt: &Path) -> Vec<String> {
    match git(wt, &["ls-files", "--others", "--exclude-standard"]) {
        Ok(out) => out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// L'agent a-t-il au moins un commit au-delà de `base_sha` ?
pub fn has_commits(wt: &Path, base_sha: &str) -> bool {
    git(wt, &["rev-list", "--count", &format!("{base_sha}..HEAD")])
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Diff complet du travail de l'agent : les fichiers suivis vs `base_sha`, PUIS
/// le contenu de chaque fichier non suivi (via `diff --no-index` contre
/// `/dev/null`, accepté par git y compris sous Windows). Non mutant.
pub fn full_diff(wt: &Path, base_sha: &str) -> Result<String, String> {
    let mut text = git(wt, &["diff", base_sha])?;
    for file in untracked(wt) {
        // `diff --no-index` sort avec le code 1 quand ça diffère : c'est le cas
        // nominal ici, donc on lit stdout sans traiter le code comme une erreur.
        let out = Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["diff", "--no-index", "--", "/dev/null", &file])
            .output();
        if let Ok(out) = out {
            text.push_str(&String::from_utf8_lossy(&out.stdout));
        }
    }
    Ok(text)
}

/// Gardes préalables à l'intégration (M4) : un remote `origin` doit exister,
/// `gh` doit être installé ET authentifié. Aucun effet de bord.
pub fn gh_ready(wt: &Path) -> Result<(), String> {
    git(wt, &["remote", "get-url", "origin"])
        .map_err(|_| "aucun remote « origin » : impossible d'ouvrir une PR".to_string())?;
    let version = Command::new("gh").arg("--version").output();
    match version {
        Ok(o) if o.status.success() => {}
        _ => {
            return Err(
                "`gh` (GitHub CLI) est introuvable : installez-le pour ouvrir une PR".into(),
            );
        }
    }
    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|e| format!("`gh auth status` indisponible : {e}"))?;
    if !auth.status.success() {
        return Err("`gh` n'est pas authentifié : lancez `gh auth login`".into());
    }
    Ok(())
}

/// Commite le travail en cours du worktree (`git add -A` puis commit) sous
/// l'identité machine `wimux <wimux@localhost>`. Renvoie `Ok(false)` s'il n'y
/// avait rien à commiter. C'est le SEUL endroit de M4 qui mute le worktree.
pub fn commit_wip(wt: &Path, message: &str) -> Result<bool, String> {
    git(wt, &["add", "-A"])?;
    // `diff --cached --quiet` sort avec 1 s'il y a quelque chose d'indexé.
    let staged = Command::new("git")
        .arg("-C")
        .arg(wt)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .map_err(|e| format!("git indisponible : {e}"))?;
    if staged.success() {
        return Ok(false); // rien à commiter
    }
    git(
        wt,
        &[
            "-c",
            "user.name=wimux",
            "-c",
            "user.email=wimux@localhost",
            "commit",
            "-m",
            message,
        ],
    )?;
    Ok(true)
}

/// Pousse la branche de l'agent sur `origin`.
pub fn push_branch(wt: &Path, branch: &str) -> Result<(), String> {
    git(wt, &["push", "-u", "origin", branch])
        .map(|_| ())
        .map_err(|e| format!("échec du push de « {branch} » : {e}"))
}

/// Ouvre la Pull Request via `gh` depuis le worktree. Renvoie l'URL de la PR.
pub fn create_pr(
    wt: &Path,
    base_branch: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let out = Command::new("gh")
        .current_dir(wt)
        .args([
            "pr",
            "create",
            "--base",
            base_branch,
            "--head",
            branch,
            "--title",
            title,
            "--body",
            body,
        ])
        .output()
        .map_err(|e| format!("`gh pr create` indisponible : {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "échec de `gh pr create` : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // gh imprime l'URL de la PR sur stdout.
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        Err("`gh pr create` n'a renvoyé aucune URL".into())
    } else {
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numstat_somme_les_lignes() {
        let out = "12\t3\tsrc/a.rs\n0\t5\tsrc/b.rs\n";
        let s = parse_numstat(out);
        assert_eq!(s.files_changed, 2);
        assert_eq!(s.insertions, 12);
        assert_eq!(s.deletions, 8);
    }

    #[test]
    fn parse_numstat_ignore_les_binaires_mais_les_compte() {
        // Un binaire est rapporté "-\t-\tchemin" : compté comme fichier changé,
        // sans contribuer aux lignes.
        let out = "-\t-\tassets/logo.png\n4\t1\tsrc/a.rs\n";
        let s = parse_numstat(out);
        assert_eq!(s.files_changed, 2);
        assert_eq!(s.insertions, 4);
        assert_eq!(s.deletions, 1);
    }

    #[test]
    fn parse_numstat_vide_donne_zero() {
        let s = parse_numstat("");
        assert_eq!(s.files_changed, 0);
        assert_eq!(s.insertions, 0);
        assert_eq!(s.deletions, 0);
    }

    /// git est-il disponible ?
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn collecte_compte_commite_et_wip_et_non_suivi() {
        if !git_available() {
            eprintln!("git absent : test collecte ignoré");
            return;
        }
        let repo = std::env::temp_dir().join(format!("wimux-batch-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git_in(&repo, &["init"]));
        std::fs::write(repo.join("a.txt"), "ligne1\n").unwrap();
        assert!(git_in(&repo, &["add", "."]));
        assert!(git_in(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "init"
            ]
        ));
        let base_sha = crate::worktree::head_sha(&repo).unwrap();

        // (a) un commit au-delà de la base
        std::fs::write(repo.join("a.txt"), "ligne1\nligne2\n").unwrap();
        assert!(git_in(&repo, &["add", "."]));
        assert!(git_in(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "travail"
            ]
        ));
        // (b) du WIP non commité
        std::fs::write(repo.join("a.txt"), "ligne1\nligne2\nligne3\n").unwrap();
        // (c) un fichier non suivi
        std::fs::write(repo.join("nouveau.txt"), "contenu\n").unwrap();

        let stats = diff_stats(&repo, &base_sha).unwrap();
        assert_eq!(stats.files_changed, 1, "a.txt modifié (commité + WIP)");
        assert_eq!(stats.insertions, 2, "ligne2 (commitée) + ligne3 (WIP)");
        assert!(
            has_commits(&repo, &base_sha),
            "il y a un commit au-delà de la base"
        );
        assert_eq!(untracked(&repo), vec!["nouveau.txt".to_string()]);

        let diff = full_diff(&repo, &base_sha).unwrap();
        assert!(
            diff.contains("ligne3"),
            "le WIP doit apparaître dans le diff"
        );
        assert!(
            diff.contains("nouveau.txt"),
            "le non-suivi doit apparaître dans le diff"
        );

        // Non-mutation : le fichier non suivi l'est TOUJOURS après la collecte.
        assert_eq!(
            untracked(&repo),
            vec!["nouveau.txt".to_string()],
            "la collecte ne doit rien stager"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn gh_ready_echoue_sans_remote() {
        if !git_available() {
            eprintln!("git absent : test gh_ready ignoré");
            return;
        }
        // Repo git SANS remote : la garde doit refuser, quel que soit l'état de gh.
        let repo =
            std::env::temp_dir().join(format!("wimux-batch-noremote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git_in(&repo, &["init"]));
        let err = gh_ready(&repo).expect_err("sans remote origin, gh_ready doit échouer");
        assert!(
            err.contains("origin") || err.contains("gh"),
            "message explicite attendu, obtenu : {err}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn commit_wip_sans_changement_ne_commite_pas() {
        if !git_available() {
            eprintln!("git absent : test commit_wip ignoré");
            return;
        }
        let repo = std::env::temp_dir().join(format!("wimux-batch-nowip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git_in(&repo, &["init"]));
        std::fs::write(repo.join("a.txt"), "x\n").unwrap();
        assert!(git_in(&repo, &["add", "."]));
        assert!(git_in(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "init"
            ]
        ));

        // Arbre propre : commit_wip renvoie false (rien à commiter) sans erreur.
        assert!(
            !commit_wip(&repo, "wimux: test").unwrap(),
            "arbre propre : rien à commiter"
        );

        // Avec du WIP : commit_wip renvoie true et l'arbre redevient propre.
        std::fs::write(repo.join("a.txt"), "x\ny\n").unwrap();
        assert!(
            commit_wip(&repo, "wimux: test").unwrap(),
            "du WIP existe : commit_wip doit commiter"
        );
        assert_eq!(untracked(&repo).len(), 0);
        let stats = diff_stats(&repo, "HEAD").unwrap();
        assert_eq!(
            stats.files_changed, 0,
            "après commit_wip l'arbre est propre"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }
}
