//! Détermination de la branche git d'un répertoire, par **lecture de fichier
//! uniquement** (sans lancer `git`). On remonte les dossiers parents depuis le
//! cwd jusqu'à trouver un dossier contenant un `.git` répertoire (car
//! `.git/HEAD` n'existe qu'à la RACINE d'un dépôt), puis on lit `HEAD`.

use std::path::Path;

/// Branche git du dépôt contenant `cwd`, ou `None` (hors dépôt / HEAD illisible /
/// `.git` de worktree — repli `None` au premier jet).
pub fn git_branch(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let git = d.join(".git");
        if git.is_dir() {
            return read_head_branch(&git);
        }
        if git.is_file() {
            // `.git` fichier = worktree lié : il contient `gitdir: <chemin>`.
            // On suit ce chemin pour lire le HEAD du worktree (M4).
            return read_gitdir_pointer(&git).and_then(|d| read_head_branch(&d));
        }
        dir = d.parent();
    }
    None
}

/// Lit `<git_dir>/HEAD` : `ref: refs/heads/<b>` → `<b>` ; sha 40 hex → sha court.
fn read_head_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(rest) = head.strip_prefix("ref: refs/heads/") {
        if rest.is_empty() {
            return None;
        }
        return Some(rest.to_string());
    }
    if head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

/// Lit un `.git` FICHIER de worktree lié (`gitdir: <chemin>`) et renvoie le
/// répertoire git pointé. `None` si le format est inattendu.
fn read_gitdir_pointer(git_file: &Path) -> Option<std::path::PathBuf> {
    let content = std::fs::read_to_string(git_file).ok()?;
    let path = content.trim().strip_prefix("gitdir:")?.trim();
    if path.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Dossier temporaire unique auto-nettoyé (Drop).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!(
                "wimux-git-test-{}-{label}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Écrit `<root>/.git/HEAD` avec `content`.
    fn write_head(root: &Path, content: &str) {
        let git = root.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(git.join("HEAD"), content).unwrap();
    }

    #[test]
    fn branche_depuis_head_symbolique() {
        let t = TempDir::new("sym");
        write_head(t.path(), "ref: refs/heads/feat/x\n");
        assert_eq!(git_branch(t.path()).as_deref(), Some("feat/x"));
    }

    #[test]
    fn branche_depuis_sous_dossier_remonte() {
        let t = TempDir::new("sub");
        write_head(t.path(), "ref: refs/heads/feat/x\n");
        let sub = t.path().join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        // Le sous-dossier n'a pas de `.git` : on doit remonter jusqu'à la racine.
        assert_eq!(git_branch(&sub).as_deref(), Some("feat/x"));
    }

    #[test]
    fn head_detache_donne_sha_court() {
        let t = TempDir::new("det");
        write_head(t.path(), "0123456789abcdef0123456789abcdef01234567\n");
        assert_eq!(git_branch(t.path()).as_deref(), Some("0123456"));
    }

    #[test]
    fn sans_git_donne_none() {
        let t = TempDir::new("nogit");
        assert_eq!(git_branch(t.path()), None);
    }

    #[test]
    fn git_fichier_worktree_suit_gitdir() {
        let t = TempDir::new("wt");
        // Répertoire git du worktree, avec son propre HEAD.
        let gitdir = t.path().join("wtgit");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/wimux/batch0/1\n").unwrap();
        // Le worktree : un `.git` FICHIER qui pointe vers ce répertoire.
        let wt = t.path().join("arbre");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", gitdir.to_string_lossy()),
        )
        .unwrap();

        assert_eq!(git_branch(&wt).as_deref(), Some("wimux/batch0/1"));
    }

    #[test]
    fn git_fichier_worktree_illisible_donne_none() {
        let t = TempDir::new("wtko");
        let wt = t.path().join("arbre");
        fs::create_dir_all(&wt).unwrap();
        // `gitdir:` pointant nulle part : repli `None`, sans panique.
        fs::write(wt.join(".git"), "gitdir: C:/inexistant/xyz\n").unwrap();
        assert_eq!(git_branch(&wt), None);
    }
}
