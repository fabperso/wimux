//! Gestion des worktrees git pour l'orchestration fan-out (M3). Chaque agent d'un
//! lot tourne dans un worktree git isolé d'un repo de base, sur une branche
//! dédiée. Les commandes git passent par `std::process::Command` (git doit être
//! dans le PATH).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Worktree git porté par une session agent d'un lot (M3). Cloné pour être posé
/// sur la session (setter `set_worktree`) et rejoué au nettoyage (`kill`).
#[derive(Debug, Clone)]
pub struct Worktree {
    pub base_repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
}

/// `base` est-il un dépôt git ? (`git -C base rev-parse --is-inside-work-tree`
/// doit réussir ET écrire `true`). Renvoie `false` si git est absent ou si le
/// répertoire n'est pas un repo.
pub fn is_git_repo(base: &Path) -> bool {
    match Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
    {
        Ok(out) => out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// Crée un worktree : `git -C base worktree add path -b branch HEAD`. Crée le
/// dossier **parent** de `path` au préalable (git worktree add exige que le parent
/// existe, mais pas `path` lui-même). En cas d'échec, renvoie le `stderr` de git.
pub fn add(base: &Path, path: &Path, branch: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("création du dossier parent du worktree : {e}"))?;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(base)
        .arg("worktree")
        .arg("add")
        .arg(path)
        .arg("-b")
        .arg(branch)
        .arg("HEAD")
        .output()
        .map_err(|e| format!("git worktree add : {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Retire un worktree et sa branche (best-effort) : `git -C base worktree remove
/// --force path` puis `git -C base branch -D branch`. Ne panique jamais ; log les
/// erreurs sur stderr (un worktree orphelin après crash serveur reste toléré).
pub fn remove(base: &Path, path: &Path, branch: &str) {
    match Command::new("git")
        .arg("-C")
        .arg(base)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(path)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "wimux: échec `git worktree remove {}` : {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("wimux: `git worktree remove` indisponible : {e}"),
    }
    match Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["branch", "-D", branch])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "wimux: échec `git branch -D {branch}` : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("wimux: `git branch -D` indisponible : {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git est-il disponible ? (garde de test — l'env CI/dev l'a normalement.)
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Exécute `git <args>` dans `dir`, renvoie true si succès.
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
    fn add_puis_remove_worktree() {
        if !git_available() {
            eprintln!("git absent : test add_puis_remove_worktree ignoré");
            return;
        }
        // Repo de base temporaire (init + commit vide, config locale au commit).
        let base = std::env::temp_dir().join(format!("wimux-wt-base-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert!(git_in(&base, &["init"]), "git init a échoué");
        assert!(
            git_in(
                &base,
                &[
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "init",
                ],
            ),
            "commit initial a échoué"
        );
        assert!(is_git_repo(&base), "la base devrait être un repo git");

        let tree = std::env::temp_dir().join(format!("wimux-wt-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        add(&base, &tree, "wimux/test/0").expect("add worktree devrait réussir");
        assert!(
            tree.exists(),
            "le dossier worktree devrait exister après add"
        );

        remove(&base, &tree, "wimux/test/0");
        assert!(
            !tree.exists(),
            "le dossier worktree devrait avoir disparu après remove"
        );

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&tree);
    }

    #[test]
    fn is_git_repo_faux_hors_repo() {
        let dir = std::env::temp_dir().join(format!("wimux-wt-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !is_git_repo(&dir),
            "un dossier vide hors repo ne doit pas être vu comme un repo git"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
