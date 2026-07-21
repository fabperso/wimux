# M4 — Revue de lot et intégration par Pull Request — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permettre à Claude de créer un lot de N agents, lire un résumé puis le diff de chacun, désigner un gagnant et ouvrir une Pull Request avec son travail, en nettoyant les perdants.

**Architecture:** Un nouveau module serveur `batch.rs` encapsule toutes les opérations git/`gh` de collecte et d'intégration (non mutantes pour la collecte) ; le daemon expose 4 messages typés ; une CLI `wimux batch` les consomme en JSON ; le skill enseigne la boucle à Claude.

**Tech Stack:** Rust (`wimux-protocol` postcard/serde, `wimux-server`, `wimux-cli`), git et `gh` via `std::process::Command`.

## Global Constraints

- **Compat postcard** : toute nouvelle variante d'enum / nouveau champ de struct s'ajoute **EN FIN** (indexation par position). Jamais au milieu.
- **Daemon persistant** : après tout changement de `wimux-protocol`/`wimux-server`, **rebuild release + redémarrer le daemon détaché**, sinon échec silencieux.
- **Collecte strictement non mutante** : aucune commande de la collecte (`review`/`diff`) ne doit modifier l'index ou l'arbre de travail de l'agent. Pas de `git add` hors du chemin PR.
- **La voie PR ne touche jamais au répertoire de travail du repo de base** : on ne modifie que le worktree du gagnant (commit), le remote (push) et GitHub (PR).
- **Aucune PR réelle créée en test** : les chemins `gh`/réseau sont testés par leurs gardes (absence/refus), jamais par un appel réel.
- **Tests git conditionnels** : ignorés proprement si `git` est absent (motif M3).
- **Identité des commits automatiques** : `wimux <wimux@localhost>`, passée en `-c` (n'écrase pas la config du repo).
- **Qualité** : `cargo test --workspace`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings` verts et pristine.
- **Langue** : commentaires et messages en français, cohérents avec le code existant.

---

## File Structure

**Créés :**
- `crates/wimux-server/src/batch.rs` — **toutes** les opérations git/`gh` de M4 : collecte (numstat, non-suivis, diff complet, présence de commits) et intégration (gardes, commit WIP, push, création de PR). Fonctions libres et pures-de-session (prennent des chemins/chaînes), testables sur un repo temporaire sans démarrer de session.

**Modifiés :**
- `crates/wimux-protocol/src/lib.rs` — `BatchInfo`, `AgentResult` + 4 messages (en fin d'enum).
- `crates/wimux-server/src/worktree.rs` — `Worktree` gagne `base_sha`/`base_branch` ; ajout de `head_sha()`/`current_branch()`.
- `crates/wimux-server/src/daemon.rs` — capture `base_sha`/`base_branch` à la création du lot ; méthodes `Server::list_batches/review_batch/diff_agent/open_pr` ; 4 handlers.
- `crates/wimux-server/src/git.rs` — correctif : suivre `gitdir:` quand `.git` est un fichier (branche d'un worktree).
- `crates/wimux-server/src/main.rs` (ou `lib.rs`) — déclarer `mod batch;`.
- `crates/wimux-cli/src/main.rs` — namespace `wimux batch` (create/list/review/diff/pr).
- `skills/wimux/SKILL.md` + `skills/wimux/references/commands.md` — section « revue de lot ».

**Interfaces clés (verrouillées ici) :**

```rust
// wimux-protocol
pub struct BatchInfo {
    pub group: String,
    pub sessions: Vec<String>,
    pub base_repo: String,
    pub base_branch: String,
}
pub struct AgentResult {
    pub session: String,
    pub index: u32,
    pub branch: String,
    pub status: Option<AgentStatus>,
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
    pub untracked: u32,
    pub has_commits: bool,
}
// ClientMessage (en fin) :
//   ListBatches
//   ReviewBatch { group: String }
//   DiffAgent { session: String }
//   OpenPr { session: String, title: Option<String>, body: Option<String> }
// ServerMessage (en fin) :
//   Batches(Vec<BatchInfo>)
//   BatchReview(Vec<AgentResult>)
//   AgentDiff(String)
//   PrOpened { url: String }

// wimux-server / worktree.rs
pub struct Worktree {
    pub base_repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base_sha: String,     // NOUVEAU
    pub base_branch: String,  // NOUVEAU
}
pub fn head_sha(base: &Path) -> Option<String>;
pub fn current_branch(base: &Path) -> Option<String>;

// wimux-server / batch.rs
pub struct DiffStats { pub files_changed: u32, pub insertions: u32, pub deletions: u32 }
pub fn diff_stats(wt: &Path, base_sha: &str) -> Result<DiffStats, String>;
pub fn untracked(wt: &Path) -> Vec<String>;
pub fn full_diff(wt: &Path, base_sha: &str) -> Result<String, String>;
pub fn has_commits(wt: &Path, base_sha: &str) -> bool;
pub fn gh_ready(wt: &Path) -> Result<(), String>;
pub fn commit_wip(wt: &Path, message: &str) -> Result<bool, String>;
pub fn push_branch(wt: &Path, branch: &str) -> Result<(), String>;
pub fn create_pr(wt: &Path, base_branch: &str, branch: &str, title: &str, body: &str) -> Result<String, String>;
pub fn parse_numstat(out: &str) -> DiffStats;   // pur, testable sans git
```

---

## Phase M4.1 — Protocole & base de comparaison

### Task 1 : Messages, `BatchInfo`, `AgentResult`

**Files:**
- Modify: `crates/wimux-protocol/src/lib.rs`
- Test: `crates/wimux-protocol/src/lib.rs` (module `#[cfg(test)]` existant)

**Interfaces:**
- Produces: `BatchInfo`, `AgentResult`, `ListBatches`/`ReviewBatch`/`DiffAgent`/`OpenPr`, `Batches`/`BatchReview`/`AgentDiff`/`PrOpened`.

- [ ] **Step 1 : Écrire le test de round-trip (échoue)**

Ajouter dans le module de tests de `crates/wimux-protocol/src/lib.rs` :

```rust
#[test]
fn aller_retour_review_batch_et_open_pr() {
    let msg = ClientMessage::OpenPr {
        session: "claude-batch0-1".into(),
        title: Some("fix: gérer le payload vide".into()),
        body: None,
    };
    let bytes = postcard::to_allocvec(&msg).unwrap();
    match postcard::from_bytes::<ClientMessage>(&bytes).unwrap() {
        ClientMessage::OpenPr { session, title, body } => {
            assert_eq!(session, "claude-batch0-1");
            assert_eq!(title.as_deref(), Some("fix: gérer le payload vide"));
            assert_eq!(body, None);
        }
        _ => panic!("variante inattendue"),
    }

    let res = AgentResult {
        session: "claude-batch0-1".into(),
        index: 1,
        branch: "wimux/batch0/1".into(),
        status: Some(AgentStatus::Done),
        files_changed: 3,
        insertions: 42,
        deletions: 7,
        untracked: 2,
        has_commits: true,
    };
    let reply = ServerMessage::BatchReview(vec![res.clone()]);
    let bytes = postcard::to_allocvec(&reply).unwrap();
    match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
        ServerMessage::BatchReview(v) => assert_eq!(v[0], res),
        _ => panic!("variante inattendue"),
    }

    let info = BatchInfo {
        group: "batch0".into(),
        sessions: vec!["claude-batch0-0".into(), "claude-batch0-1".into()],
        base_repo: "C:\\repo".into(),
        base_branch: "main".into(),
    };
    let bytes = postcard::to_allocvec(&ServerMessage::Batches(vec![info.clone()])).unwrap();
    match postcard::from_bytes::<ServerMessage>(&bytes).unwrap() {
        ServerMessage::Batches(v) => assert_eq!(v[0], info),
        _ => panic!("variante inattendue"),
    }
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-protocol aller_retour_review_batch_et_open_pr`
Expected: FAIL — `BatchInfo`/`AgentResult`/`OpenPr` n'existent pas (erreur de compilation).

- [ ] **Step 3 : Ajouter les structs**

Après la déclaration de `PaneInfo` dans `lib.rs` :

```rust
/// Résumé d'un lot d'agents (M4). Renvoyé par `ListBatches`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchInfo {
    /// Identifiant de lot (`batch<N>`), partagé par les sessions membres.
    pub group: String,
    /// Noms de session des membres, dans l'ordre des index.
    pub sessions: Vec<String>,
    /// Dépôt de base du lot (chemin natif).
    pub base_repo: String,
    /// Branche du dépôt de base au lancement — cible des futures PR.
    pub base_branch: String,
}

/// Résultat produit par un agent d'un lot (M4). Renvoyé par `ReviewBatch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResult {
    pub session: String,
    /// Rang de l'agent dans son lot (dérivé du suffixe du nom de session).
    pub index: u32,
    pub branch: String,
    /// Statut d'agent (M1), `None` si indisponible.
    pub status: Option<AgentStatus>,
    /// Fichiers suivis modifiés vs la base (commité + en cours).
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
    /// Nombre de fichiers NON suivis (comptés à part : aucun double comptage).
    pub untracked: u32,
    /// L'agent a-t-il au moins un commit au-delà de la base ?
    pub has_commits: bool,
}
```

- [ ] **Step 4 : Ajouter les variantes `ClientMessage` EN FIN de l'enum**

Juste avant l'accolade fermante de `enum ClientMessage` :

```rust
    /// M4 : lister les lots d'agents en cours.
    ListBatches,
    /// M4 : résumé par agent des résultats d'un lot.
    ReviewBatch { group: String },
    /// M4 : diff complet du travail d'un agent.
    DiffAgent { session: String },
    /// M4 : intégrer le travail d'un agent par Pull Request (commit du WIP,
    /// push, `gh pr create`), puis nettoyer les perdants du lot.
    OpenPr {
        session: String,
        title: Option<String>,
        body: Option<String>,
    },
```

- [ ] **Step 5 : Ajouter les variantes `ServerMessage` EN FIN de l'enum**

```rust
    /// M4 : réponse à `ListBatches`.
    Batches(Vec<BatchInfo>),
    /// M4 : réponse à `ReviewBatch`.
    BatchReview(Vec<AgentResult>),
    /// M4 : réponse à `DiffAgent`.
    AgentDiff(String),
    /// M4 : réponse à `OpenPr` — URL de la Pull Request créée.
    PrOpened { url: String },
```

- [ ] **Step 6 : Lancer les tests**

Run: `cargo test -p wimux-protocol`
Expected: PASS (nouveau test + tous les existants).

- [ ] **Step 7 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-protocol && cargo clippy -p wimux-protocol -- -D warnings
git add crates/wimux-protocol/src/lib.rs
git commit -m "feat(batch): protocole M4 — BatchInfo, AgentResult, ListBatches/ReviewBatch/DiffAgent/OpenPr

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 2 : `Worktree` gagne `base_sha` / `base_branch`

**Files:**
- Modify: `crates/wimux-server/src/worktree.rs` (struct + 2 fonctions)
- Modify: `crates/wimux-server/src/daemon.rs` (capture à la création du lot)
- Test: `crates/wimux-server/src/worktree.rs`

**Interfaces:**
- Produces: `Worktree { base_repo, path, branch, base_sha, base_branch }`, `worktree::head_sha`, `worktree::current_branch`.
- Consumes: rien de nouveau.

- [ ] **Step 1 : Écrire le test (échoue)**

Ajouter dans le module de tests de `worktree.rs` :

```rust
#[test]
fn head_sha_et_current_branch_sur_repo_temporaire() {
    if !git_available() {
        eprintln!("git absent : test head_sha_et_current_branch ignoré");
        return;
    }
    let base = std::env::temp_dir().join(format!("wimux-wt-head-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    assert!(git_in(&base, &["init"]));
    assert!(git_in(
        &base,
        &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--allow-empty", "-m", "init"],
    ));

    let sha = head_sha(&base).expect("HEAD doit exister après un commit");
    assert_eq!(sha.len(), 40, "un sha complet fait 40 caractères : {sha}");
    assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));

    let branch = current_branch(&base).expect("une branche courante doit exister");
    assert!(!branch.is_empty() && branch != "HEAD", "branche : {branch}");

    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server head_sha_et_current_branch`
Expected: FAIL — `head_sha`/`current_branch` n'existent pas.

- [ ] **Step 3 : Étendre `Worktree` et ajouter les deux fonctions**

Dans `worktree.rs`, remplacer la struct :

```rust
#[derive(Debug, Clone)]
pub struct Worktree {
    pub base_repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// Commit du dépôt de base au lancement du lot (M4) : référence de
    /// comparaison STABLE même si la base avance ensuite.
    pub base_sha: String,
    /// Branche du dépôt de base au lancement du lot (M4) : cible des PR.
    pub base_branch: String,
}
```

Et ajouter, après `is_git_repo` :

```rust
/// Sha complet du `HEAD` du dépôt `base` (M4). `None` si git échoue ou si le
/// dépôt n'a encore aucun commit.
pub fn head_sha(base: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Branche courante du dépôt `base` (M4). `None` si git échoue ; renvoie `HEAD`
/// tel quel si le dépôt est en HEAD détaché (l'appelant décidera).
pub fn current_branch(base: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b.is_empty() { None } else { Some(b) }
}
```

- [ ] **Step 4 : Capturer `base_sha`/`base_branch` à la création du lot**

Dans `daemon.rs`, méthode `create_agent_batch`, **juste après** la vérification
`worktree::is_git_repo(&base)` (étape « (1) ») et avant la résolution du template,
ajouter :

```rust
        // (1 bis) Référence de comparaison du lot (M4) : sha + branche de la base
        // au lancement. Sans HEAD (dépôt sans commit), le lot n'a pas de base.
        let base_sha = worktree::head_sha(&base).ok_or_else(|| {
            format!("le dépôt de base n'a pas de HEAD (aucun commit ?) : {base_repo}")
        })?;
        let base_branch = worktree::current_branch(&base).ok_or_else(|| {
            format!("branche courante du dépôt de base introuvable : {base_repo}")
        })?;
```

Puis, dans la construction du `Worktree` (boucle `i`), ajouter les deux champs :

```rust
                    session.set_worktree(Worktree {
                        base_repo: base.clone(),
                        path: path.clone(),
                        branch: branch.clone(),
                        base_sha: base_sha.clone(),
                        base_branch: base_branch.clone(),
                    });
```

- [ ] **Step 5 : Corriger les autres constructions de `Worktree`**

Chercher toute autre construction littérale (`rg "Worktree \{" crates/wimux-server`)
— notamment dans les tests — et compléter avec `base_sha` / `base_branch`
(valeurs de test : `"0".repeat(40)` et `"main".into()`).

- [ ] **Step 6 : Lancer les tests**

Run: `cargo test -p wimux-server`
Expected: PASS (nouveau test + suites existantes, dont `gui_mode`).

- [ ] **Step 7 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings
git add crates/wimux-server/src/worktree.rs crates/wimux-server/src/daemon.rs
git commit -m "feat(batch): Worktree porte base_sha/base_branch (reference de comparaison du lot)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase M4.2 — Serveur : collecte (non mutante)

### Task 3 : Module `batch.rs` — collecte git

**Files:**
- Create: `crates/wimux-server/src/batch.rs`
- Modify: `crates/wimux-server/src/main.rs` (ajouter `mod batch;`)
- Test: `crates/wimux-server/src/batch.rs`

**Interfaces:**
- Produces: `DiffStats`, `parse_numstat`, `diff_stats`, `untracked`, `full_diff`, `has_commits`.
- Consumes: rien (fonctions libres sur des chemins).

- [ ] **Step 1 : Écrire les tests (échouent)**

Créer `crates/wimux-server/src/batch.rs` avec **uniquement** le module de tests
pour commencer :

```rust
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
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server parse_numstat`
Expected: FAIL — le module n'est pas déclaré / `parse_numstat` n'existe pas.

- [ ] **Step 3 : Déclarer le module**

Dans `crates/wimux-server/src/main.rs`, ajouter parmi les autres `mod` :

```rust
mod batch;
```

(Si les modules sont déclarés dans un `lib.rs`, l'ajouter là — suivre le motif
existant de `mod worktree;`.)

- [ ] **Step 4 : Écrire la collecte**

En tête de `batch.rs`, **avant** le module de tests :

```rust
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
```

- [ ] **Step 5 : Lancer les tests**

Run: `cargo test -p wimux-server parse_numstat`
Expected: PASS (3 tests).

- [ ] **Step 6 : Ajouter un test d'intégration sur repo temporaire**

Ajouter dans le module de tests de `batch.rs` :

```rust
    /// git est-il disponible ?
    fn git_available() -> bool {
        Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git").arg("-C").arg(dir).args(args).output().map(|o| o.status.success()).unwrap_or(false)
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
        assert!(git_in(&repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "init"]));
        let base_sha = crate::worktree::head_sha(&repo).unwrap();

        // (a) un commit au-delà de la base
        std::fs::write(repo.join("a.txt"), "ligne1\nligne2\n").unwrap();
        assert!(git_in(&repo, &["add", "."]));
        assert!(git_in(&repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "travail"]));
        // (b) du WIP non commité
        std::fs::write(repo.join("a.txt"), "ligne1\nligne2\nligne3\n").unwrap();
        // (c) un fichier non suivi
        std::fs::write(repo.join("nouveau.txt"), "contenu\n").unwrap();

        let stats = diff_stats(&repo, &base_sha).unwrap();
        assert_eq!(stats.files_changed, 1, "a.txt modifié (commité + WIP)");
        assert_eq!(stats.insertions, 2, "ligne2 (commitée) + ligne3 (WIP)");
        assert!(has_commits(&repo, &base_sha), "il y a un commit au-delà de la base");
        assert_eq!(untracked(&repo), vec!["nouveau.txt".to_string()]);

        let diff = full_diff(&repo, &base_sha).unwrap();
        assert!(diff.contains("ligne3"), "le WIP doit apparaître dans le diff");
        assert!(diff.contains("nouveau.txt"), "le non-suivi doit apparaître dans le diff");

        // Non-mutation : le fichier non suivi l'est TOUJOURS après la collecte.
        assert_eq!(untracked(&repo), vec!["nouveau.txt".to_string()], "la collecte ne doit rien stager");

        let _ = std::fs::remove_dir_all(&repo);
    }
```

- [ ] **Step 7 : Lancer, fmt, clippy, commit**

Run: `cargo test -p wimux-server batch`
Expected: PASS.

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings
git add crates/wimux-server/src/batch.rs crates/wimux-server/src/main.rs
git commit -m "feat(batch): module batch.rs — collecte non mutante (numstat, non-suivis, diff complet)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 4 : `Server::list_batches` / `review_batch` / `diff_agent` + handlers

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/src/daemon.rs`

**Interfaces:**
- Consumes: `batch::{diff_stats, untracked, has_commits, full_diff}`, `Session::{worktree, group, name, agent_status}`, `BatchInfo`, `AgentResult`.
- Produces: `Server::list_batches()`, `Server::review_batch(group)`, `Server::diff_agent(session)`.

- [ ] **Step 1 : Écrire le test (échoue)**

Dans le module de tests de `daemon.rs` :

```rust
#[test]
fn list_batches_vide_sans_lot() {
    let server = Server::new();
    assert!(server.list_batches().is_empty(), "aucun lot au démarrage");
}

#[test]
fn review_batch_groupe_inconnu_est_erreur() {
    let server = Server::new();
    assert!(server.review_batch("batch-inexistant").is_err());
}

#[test]
fn diff_agent_session_inconnue_est_erreur() {
    let server = Server::new();
    assert!(server.diff_agent("session-inexistante").is_err());
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server list_batches_vide_sans_lot`
Expected: FAIL — méthodes absentes.

- [ ] **Step 3 : Ajouter un helper d'index et les trois méthodes**

Dans `impl Server` de `daemon.rs` :

```rust
    /// Rang d'un agent dans son lot, dérivé du nom de session M3
    /// `<template>-<group>-<i>` : on lit le suffixe après le dernier `-`.
    fn agent_index(name: &str) -> u32 {
        name.rsplit('-').next().and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// M4 : lots en cours, construits depuis les sessions portant un `group`.
    fn list_batches(&self) -> Vec<BatchInfo> {
        let sessions = {
            let s = self.sessions.lock().unwrap();
            s.values().cloned().collect::<Vec<_>>()
        };
        let mut by_group: std::collections::BTreeMap<String, Vec<Arc<Session>>> =
            std::collections::BTreeMap::new();
        for s in sessions {
            if let Some(g) = s.group() {
                by_group.entry(g).or_default().push(s);
            }
        }
        by_group
            .into_iter()
            .map(|(group, mut members)| {
                members.sort_by_key(|s| Self::agent_index(&s.name()));
                // base_repo / base_branch : pris sur le premier membre (identiques
                // pour tout le lot, posés à la création).
                let wt = members.first().and_then(|s| s.worktree());
                BatchInfo {
                    group,
                    sessions: members.iter().map(|s| s.name()).collect(),
                    base_repo: wt
                        .as_ref()
                        .map(|w| w.base_repo.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    base_branch: wt.map(|w| w.base_branch).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// M4 : résumé par agent des résultats d'un lot.
    fn review_batch(&self, group: &str) -> Result<Vec<AgentResult>, String> {
        let members = {
            let s = self.sessions.lock().unwrap();
            let mut v: Vec<Arc<Session>> = s
                .values()
                .filter(|s| s.group().as_deref() == Some(group))
                .cloned()
                .collect();
            v.sort_by_key(|s| Self::agent_index(&s.name()));
            v
        };
        if members.is_empty() {
            return Err(format!("lot introuvable : {group}"));
        }
        let idle = std::time::Duration::from_secs(self.config.agent_idle_seconds);
        let mut out = Vec::new();
        for s in members {
            let name = s.name();
            let Some(wt) = s.worktree() else {
                return Err(format!("la session « {name} » n'a pas de worktree"));
            };
            let stats = crate::batch::diff_stats(&wt.path, &wt.base_sha)
                .map_err(|e| format!("diff de « {name} » : {e}"))?;
            out.push(AgentResult {
                session: name.clone(),
                index: Self::agent_index(&name),
                branch: wt.branch.clone(),
                status: s.agent_status(idle),
                files_changed: stats.files_changed,
                insertions: stats.insertions,
                deletions: stats.deletions,
                untracked: crate::batch::untracked(&wt.path).len() as u32,
                has_commits: crate::batch::has_commits(&wt.path, &wt.base_sha),
            });
        }
        Ok(out)
    }

    /// M4 : diff complet du travail d'un agent.
    fn diff_agent(&self, session: &str) -> Result<String, String> {
        let s = self
            .get(session)
            .ok_or_else(|| format!("session introuvable : {session}"))?;
        let wt = s
            .worktree()
            .ok_or_else(|| format!("la session « {session} » n'a pas de worktree"))?;
        crate::batch::full_diff(&wt.path, &wt.base_sha)
    }
```

Ajouter les imports nécessaires en tête de `daemon.rs` : `AgentResult`, `BatchInfo`
à la liste `use wimux_protocol::{…}`.

- [ ] **Step 4 : Ajouter les handlers**

Dans le `match msg` de `handle_client`, après les bras A1 :

```rust
            ClientMessage::ListBatches => {
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &ServerMessage::Batches(server.list_batches()))?;
            }
            ClientMessage::ReviewBatch { group } => {
                let reply = match server.review_batch(&group) {
                    Ok(v) => ServerMessage::BatchReview(v),
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
            ClientMessage::DiffAgent { session } => {
                let reply = match server.diff_agent(&session) {
                    Ok(text) => ServerMessage::AgentDiff(text),
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

**Note** : `OpenPr` est traité en Task 6. Pour que le `match` reste exhaustif dès
maintenant, ajouter un bras temporaire qui sera REMPLACÉ en Task 6 :

```rust
            // M4 : handler réel en Task 6.
            ClientMessage::OpenPr { .. } => {
                let mut wr: &PipeConn = &conn;
                send(
                    &mut wr,
                    &ServerMessage::Error("OpenPr : non implémenté (Task 6)".into()),
                )?;
            }
```

- [ ] **Step 5 : Lancer les tests**

Run: `cargo test -p wimux-server`
Expected: PASS.

- [ ] **Step 6 : fmt + clippy + commit**

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings
git add crates/wimux-server/src/daemon.rs
git commit -m "feat(batch): Server::list_batches/review_batch/diff_agent + handlers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase M4.3 — Serveur : intégration par PR

### Task 5 : `batch.rs` — gardes, commit WIP, push, création de PR

**Files:**
- Modify: `crates/wimux-server/src/batch.rs`
- Test: `crates/wimux-server/src/batch.rs`

**Interfaces:**
- Produces: `gh_ready`, `commit_wip`, `push_branch`, `create_pr`.
- Consumes: `git()` (helper interne de Task 3).

- [ ] **Step 1 : Écrire le test des gardes (échoue)**

Dans le module de tests de `batch.rs` :

```rust
    #[test]
    fn gh_ready_echoue_sans_remote() {
        if !git_available() {
            eprintln!("git absent : test gh_ready ignoré");
            return;
        }
        // Repo git SANS remote : la garde doit refuser, quel que soit l'état de gh.
        let repo = std::env::temp_dir().join(format!("wimux-batch-noremote-{}", std::process::id()));
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
        assert!(git_in(&repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "init"]));

        // Arbre propre : commit_wip renvoie false (rien à commiter) sans erreur.
        assert_eq!(commit_wip(&repo, "wimux: test").unwrap(), false);

        // Avec du WIP : commit_wip renvoie true et l'arbre redevient propre.
        std::fs::write(repo.join("a.txt"), "x\ny\n").unwrap();
        assert_eq!(commit_wip(&repo, "wimux: test").unwrap(), true);
        assert_eq!(untracked(&repo).len(), 0);
        let stats = diff_stats(&repo, "HEAD").unwrap();
        assert_eq!(stats.files_changed, 0, "après commit_wip l'arbre est propre");

        let _ = std::fs::remove_dir_all(&repo);
    }
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server gh_ready_echoue_sans_remote`
Expected: FAIL — `gh_ready`/`commit_wip` n'existent pas.

- [ ] **Step 3 : Écrire les quatre fonctions**

Ajouter dans `batch.rs`, après `full_diff` :

```rust
/// Gardes préalables à l'intégration (M4) : un remote `origin` doit exister,
/// `gh` doit être installé ET authentifié. Aucun effet de bord.
pub fn gh_ready(wt: &Path) -> Result<(), String> {
    git(wt, &["remote", "get-url", "origin"])
        .map_err(|_| "aucun remote « origin » : impossible d'ouvrir une PR".to_string())?;
    let version = Command::new("gh").arg("--version").output();
    match version {
        Ok(o) if o.status.success() => {}
        _ => return Err("`gh` (GitHub CLI) est introuvable : installez-le pour ouvrir une PR".into()),
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
            "pr", "create", "--base", base_branch, "--head", branch, "--title", title, "--body",
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
```

Le helper `git()` de Task 3 accepte déjà des options globales avant la
sous-commande (`-c user.name=…`), puisqu'il les passe telles quelles après `-C <dir>`.

- [ ] **Step 4 : Lancer, fmt, clippy, commit**

Run: `cargo test -p wimux-server batch`
Expected: PASS (tests de collecte + les 2 nouveaux).

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings
git add crates/wimux-server/src/batch.rs
git commit -m "feat(batch): gardes gh, commit du WIP, push et creation de PR

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 6 : `Server::open_pr` — orchestration + nettoyage des perdants

**Files:**
- Modify: `crates/wimux-server/src/daemon.rs`
- Test: `crates/wimux-server/src/daemon.rs`

**Interfaces:**
- Consumes: `batch::{gh_ready, commit_wip, push_branch, create_pr, diff_stats, untracked}`, `Server::{get, kill, review_batch}`.
- Produces: `Server::open_pr(session, title, body) -> Result<String, String>`.

- [ ] **Step 1 : Écrire le test (échoue)**

Dans le module de tests de `daemon.rs` :

```rust
#[test]
fn open_pr_session_inconnue_est_erreur() {
    let server = Server::new();
    let err = server
        .open_pr("session-inexistante", None, None)
        .expect_err("session inconnue doit échouer");
    assert!(err.contains("introuvable"), "message : {err}");
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server open_pr_session_inconnue_est_erreur`
Expected: FAIL — `open_pr` n'existe pas.

- [ ] **Step 3 : Écrire `open_pr`**

Dans `impl Server` de `daemon.rs` :

```rust
    /// M4 : intègre le travail d'un agent par Pull Request, puis nettoie les
    /// PERDANTS du lot (le gagnant reste vivant pour itérer sur la revue).
    ///
    /// Ordre volontaire : toutes les gardes AVANT le moindre effet de bord.
    fn open_pr(
        &self,
        session: &str,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<String, String> {
        // (1) Résolution + gardes, sans effet de bord.
        let s = self
            .get(session)
            .ok_or_else(|| format!("session introuvable : {session}"))?;
        let wt = s
            .worktree()
            .ok_or_else(|| format!("la session « {session} » n'a pas de worktree"))?;
        let group = s
            .group()
            .ok_or_else(|| format!("la session « {session} » n'appartient à aucun lot"))?;

        let stats = crate::batch::diff_stats(&wt.path, &wt.base_sha)?;
        let untracked_n = crate::batch::untracked(&wt.path).len();
        if stats.files_changed == 0 && untracked_n == 0 {
            return Err(format!("l'agent « {session} » n'a rien produit : pas de PR"));
        }
        crate::batch::gh_ready(&wt.path)?;

        // (2) Effets de bord : commit du WIP, push, PR.
        crate::batch::commit_wip(
            &wt.path,
            &format!("wimux: travail de l'agent {session} (lot {group})"),
        )?;
        crate::batch::push_branch(&wt.path, &wt.branch)?;

        let title = title.unwrap_or_else(|| format!("wimux: résultat de l'agent {session}"));
        let footer = format!(
            "\n\n---\nOuvert par wimux — lot `{group}`, agent `{session}`, branche `{}`.\n\
             {} fichier(s) suivi(s) modifié(s), +{} / -{}, {} non suivi(s).",
            wt.branch, stats.files_changed, stats.insertions, stats.deletions, untracked_n
        );
        let body = format!("{}{footer}", body.unwrap_or_default());

        // Le push a réussi : si la PR échoue, on le dit SANS nettoyer quoi que ce soit.
        let url = crate::batch::create_pr(&wt.path, &wt.base_branch, &wt.branch, &title, &body)
            .map_err(|e| {
                format!(
                    "{e}\n(la branche « {} » EST poussée sur origin ; la PR reste à ouvrir, \
                     rien n'a été nettoyé)",
                    wt.branch
                )
            })?;

        // (3) Nettoyage des PERDANTS uniquement.
        let losers: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .values()
                .filter(|o| o.group().as_deref() == Some(group.as_str()) && o.name() != session)
                .map(|o| o.name())
                .collect()
        };
        for name in losers {
            self.kill(&name);
        }
        Ok(url)
    }
```

- [ ] **Step 4 : Remplacer le bras `OpenPr` temporaire de Task 4**

Dans `handle_client`, **supprimer** le bras temporaire (« non implémenté (Task 6) »)
et le remplacer par :

```rust
            ClientMessage::OpenPr { session, title, body } => {
                let reply = match server.open_pr(&session, title, body) {
                    Ok(url) => ServerMessage::PrOpened { url },
                    Err(e) => ServerMessage::Error(e),
                };
                let mut wr: &PipeConn = &conn;
                send(&mut wr, &reply)?;
            }
```

- [ ] **Step 5 : Lancer les tests + qualité + rebuild**

Run: `cargo test -p wimux-server && cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings && cargo build --release`
Expected: tout vert. Puis redémarrer le daemon détaché (piège du daemon persistant) :
`./target/release/wimux.exe kill-server` (il repartira au prochain appel).

- [ ] **Step 6 : Commit**

```bash
git add crates/wimux-server/src/daemon.rs
git commit -m "feat(batch): Server::open_pr — gardes, commit WIP, push, PR, nettoyage des perdants

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase M4.4 — CLI `wimux batch`

### Task 7 : Parsing + `create` / `list`

**Files:**
- Modify: `crates/wimux-cli/src/main.rs`
- Test: `crates/wimux-cli/src/main.rs`

**Interfaces:**
- Produces: `batch::parse_create`, `batch::CreateArgs`, `cmd_batch`, `batch_create`, `batch_list`.
- Consumes: helpers A1 existants `connected()`, `agent::json_escape`, `send`/`recv`.

- [ ] **Step 1 : Écrire le test de parsing (échoue)**

Ajouter en bas de `main.rs` :

```rust
#[cfg(test)]
mod batch_tests {
    use super::batch::*;

    #[test]
    fn parse_create_lit_tous_les_champs() {
        let a = parse_create(&[
            "--repo".into(), "C:\\repo".into(),
            "--template".into(), "claude".into(),
            "--prompt".into(), "corrige le parser".into(),
            "--count".into(), "3".into(),
        ])
        .unwrap();
        assert_eq!(a.repo, "C:\\repo");
        assert_eq!(a.template, "claude");
        assert_eq!(a.prompt, "corrige le parser");
        assert_eq!(a.count, 3);
    }

    #[test]
    fn parse_create_exige_repo_template_prompt() {
        assert!(parse_create(&["--repo".into(), "C:\\repo".into()]).is_err());
    }

    #[test]
    fn parse_create_count_defaut_est_deux() {
        let a = parse_create(&[
            "--repo".into(), "r".into(),
            "--template".into(), "t".into(),
            "--prompt".into(), "p".into(),
        ])
        .unwrap();
        assert_eq!(a.count, 2, "count par défaut = 2");
    }
}
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-cli parse_create_lit_tous_les_champs`
Expected: FAIL — module `batch` absent côté CLI.

- [ ] **Step 3 : Écrire le module de parsing**

Dans `main.rs`, après le module `agent` :

```rust
mod batch {
    use std::io;

    /// Arguments analysés de `wimux batch create`.
    pub struct CreateArgs {
        pub repo: String,
        pub template: String,
        pub prompt: String,
        pub count: u32,
    }

    /// Analyse `wimux batch create --repo <p> --template <t> --prompt "…" [--count N]`.
    pub fn parse_create(args: &[String]) -> io::Result<CreateArgs> {
        let (mut repo, mut template, mut prompt) = (None, None, None);
        let mut count = 2u32;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--repo" => {
                    repo = args.get(i + 1).cloned();
                    i += 2;
                }
                "--template" => {
                    template = args.get(i + 1).cloned();
                    i += 2;
                }
                "--prompt" => {
                    prompt = args.get(i + 1).cloned();
                    i += 2;
                }
                "--count" => {
                    count = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(2);
                    i += 2;
                }
                _ => i += 1,
            }
        }
        match (repo, template, prompt) {
            (Some(repo), Some(template), Some(prompt)) => Ok(CreateArgs { repo, template, prompt, count }),
            _ => Err(io::Error::other(
                "usage : wimux batch create --repo <chemin> --template <nom> --prompt \"…\" [--count N]",
            )),
        }
    }

    /// Extrait `-g <group>` et `-i <index>` (ou `-s <session>`).
    pub fn parse_target(args: &[String]) -> (Option<String>, Option<u32>, Option<String>, Vec<String>) {
        let (mut group, mut index, mut session) = (None, None, None);
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-g" | "--group" => {
                    group = args.get(i + 1).cloned();
                    i += 2;
                }
                "-i" | "--index" => {
                    index = args.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                "-s" | "--session" => {
                    session = args.get(i + 1).cloned();
                    i += 2;
                }
                other => {
                    rest.push(other.to_string());
                    i += 1;
                }
            }
        }
        (group, index, session, rest)
    }
}
```

- [ ] **Step 4 : Router `batch` et écrire `create` / `list`**

Dans le `match cmd` de `main()`, ajouter avant `Some(other)` :

```rust
        Some("batch") => cmd_batch(&args[1..]),
```

Et ajouter les fonctions (près de `cmd_agent`) :

```rust
fn cmd_batch(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("create") => batch_create(&args[1..]),
        Some("list") => batch_list(),
        Some("review") => batch_review(&args[1..]),
        Some("diff") => batch_diff(&args[1..]),
        Some("pr") => batch_pr(&args[1..]),
        _ => Err(io::Error::other(
            "usage : wimux batch <create|list|review|diff|pr> …",
        )),
    }
}

fn batch_create(args: &[String]) -> io::Result<()> {
    let a = batch::parse_create(args)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(
        &mut w,
        &ClientMessage::CreateAgentBatch {
            template: a.template,
            prompt: a.prompt,
            base_repo: a.repo,
            count: a.count,
        },
    )?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BatchCreated { group, sessions } => {
            let names: Vec<String> = sessions
                .iter()
                .map(|s| format!("\"{}\"", agent::json_escape(s)))
                .collect();
            println!(
                "{{\"group\":\"{}\",\"sessions\":[{}]}}",
                agent::json_escape(&group),
                names.join(",")
            );
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn batch_list() -> io::Result<()> {
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ListBatches)?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::Batches(batches) => {
            let items: Vec<String> = batches
                .iter()
                .map(|b| {
                    let sessions: Vec<String> = b
                        .sessions
                        .iter()
                        .map(|s| format!("\"{}\"", agent::json_escape(s)))
                        .collect();
                    format!(
                        "{{\"group\":\"{}\",\"base_repo\":\"{}\",\"base_branch\":\"{}\",\"sessions\":[{}]}}",
                        agent::json_escape(&b.group),
                        agent::json_escape(&b.base_repo),
                        agent::json_escape(&b.base_branch),
                        sessions.join(",")
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}
```

**Note** : `batch_review`/`batch_diff`/`batch_pr` sont écrits en Task 8. Pour
compiler cette task, ajouter trois stubs TEMPORAIRES qui seront REMPLACÉS :

```rust
fn batch_review(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("wimux batch review : implémenté en Task 8"))
}
fn batch_diff(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("wimux batch diff : implémenté en Task 8"))
}
fn batch_pr(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("wimux batch pr : implémenté en Task 8"))
}
```

- [ ] **Step 5 : Lancer, fmt, clippy, commit**

Run: `cargo test -p wimux-cli && cargo clippy -p wimux-cli -- -D warnings`
Expected: PASS, clippy clean.

```bash
cargo fmt -p wimux-cli
git add crates/wimux-cli/src/main.rs
git commit -m "feat(batch): CLI wimux batch create/list + parsing

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 8 : CLI `review` / `diff` / `pr`

**Files:**
- Modify: `crates/wimux-cli/src/main.rs`

**Interfaces:**
- Consumes: `batch::parse_target`, `ReviewBatch`/`DiffAgent`/`OpenPr`.

- [ ] **Step 1 : Écrire un helper de résolution + les trois commandes**

Remplacer les trois stubs par :

```rust
/// Résout la cible en nom de session : `-s <session>` direct, sinon
/// `-g <group> -i <index>` via `ReviewBatch`.
fn resolve_agent(group: Option<String>, index: Option<u32>, session: Option<String>) -> io::Result<String> {
    if let Some(s) = session {
        return Ok(s);
    }
    let (Some(group), Some(index)) = (group, index) else {
        return Err(io::Error::other(
            "cible manquante : passez -s <session> ou -g <group> -i <index>",
        ));
    };
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ReviewBatch { group: group.clone() })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BatchReview(v) => v
            .into_iter()
            .find(|a| a.index == index)
            .map(|a| a.session)
            .ok_or_else(|| io::Error::other(format!("aucun agent d'index {index} dans le lot {group}"))),
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn batch_review(args: &[String]) -> io::Result<()> {
    let (group, _, _, _) = batch::parse_target(args);
    let group = group.ok_or_else(|| io::Error::other("usage : wimux batch review -g <group>"))?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::ReviewBatch { group })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::BatchReview(v) => {
            let items: Vec<String> = v
                .iter()
                .map(|a| {
                    let status = a
                        .status
                        .map(|s| format!("\"{s:?}\""))
                        .unwrap_or_else(|| "null".into());
                    format!(
                        "{{\"session\":\"{}\",\"index\":{},\"branch\":\"{}\",\"status\":{},\
                         \"files_changed\":{},\"insertions\":{},\"deletions\":{},\
                         \"untracked\":{},\"has_commits\":{}}}",
                        agent::json_escape(&a.session),
                        a.index,
                        agent::json_escape(&a.branch),
                        status,
                        a.files_changed,
                        a.insertions,
                        a.deletions,
                        a.untracked,
                        a.has_commits
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn batch_diff(args: &[String]) -> io::Result<()> {
    let (group, index, session, _) = batch::parse_target(args);
    let session = resolve_agent(group, index, session)?;
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::DiffAgent { session })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::AgentDiff(text) => {
            println!("{text}");
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}

fn batch_pr(args: &[String]) -> io::Result<()> {
    let (group, index, session, rest) = batch::parse_target(args);
    let session = resolve_agent(group, index, session)?;
    // --title / --body sont lus dans le reliquat.
    let (mut title, mut body) = (None, None);
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--title" => {
                title = rest.get(i + 1).cloned();
                i += 2;
            }
            "--body" => {
                body = rest.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }
    let conn = connected()?;
    let mut w: &PipeConn = &conn;
    send(&mut w, &ClientMessage::OpenPr { session, title, body })?;
    let mut r: &PipeConn = &conn;
    match recv::<_, ServerMessage>(&mut r)? {
        ServerMessage::PrOpened { url } => {
            println!("{{\"url\":\"{}\"}}", agent::json_escape(&url));
            Ok(())
        }
        ServerMessage::Error(e) => Err(io::Error::other(e)),
        _ => Err(io::Error::other("réponse inattendue du serveur")),
    }
}
```

- [ ] **Step 2 : Ajouter la ligne d'aide**

Dans `print_help`, section `COMMANDES :`, après la ligne `agent <sous-cmd>` :

```
             batch <sous-cmd>    Lots d'agents (create/list/review/diff/pr)\n    \
```

- [ ] **Step 3 : Compiler + tests + qualité**

Run: `cargo build -p wimux-cli && cargo test -p wimux-cli && cargo clippy -p wimux-cli -- -D warnings && cargo fmt -p wimux-cli`
Expected: tout vert.

- [ ] **Step 4 : Test manuel de bout en bout (sans PR réelle)**

Rebuild release, redémarrer le daemon, puis sur un repo git jouet :
```bash
wimux batch create --repo C:\chemin\vers\repo-jouet --template <un-template> --prompt "ajoute un fichier NOTES.md" --count 2
wimux batch list
wimux batch review -g batch0
wimux batch diff -g batch0 -i 0
```
Expected : `create` renvoie group + sessions ; `list` montre le lot ; `review`
donne des chiffres par agent ; `diff` montre le travail. **Ne pas lancer `pr`**
(créerait une vraie PR) — le tester plus tard sur un dépôt jetable.

- [ ] **Step 5 : Commit**

```bash
git add crates/wimux-cli/src/main.rs
git commit -m "feat(batch): CLI wimux batch review/diff/pr + aide

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Phase M4.5 — Skill & correctif

### Task 9 : Correctif — branche d'un worktree

**Files:**
- Modify: `crates/wimux-server/src/git.rs`
- Test: `crates/wimux-server/src/git.rs`

**Interfaces:**
- Produces: `git_branch` gère désormais le `.git` fichier (`gitdir:`).

- [ ] **Step 1 : Remplacer le test qui attend `None` par le comportement attendu**

Dans le module de tests de `git.rs`, **remplacer** le test
`git_fichier_worktree_donne_none` par :

```rust
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
```

- [ ] **Step 2 : Vérifier l'échec**

Run: `cargo test -p wimux-server git_fichier_worktree_suit_gitdir`
Expected: FAIL — `git_branch` renvoie `None` (comportement actuel).

- [ ] **Step 3 : Implémenter le suivi de `gitdir:`**

Dans `git.rs`, remplacer la branche `if git.is_file()` de `git_branch` :

```rust
        if git.is_file() {
            // `.git` fichier = worktree lié : il contient `gitdir: <chemin>`.
            // On suit ce chemin pour lire le HEAD du worktree (M4).
            return read_gitdir_pointer(&git).and_then(|d| read_head_branch(&d));
        }
```

Et ajouter la fonction, à côté de `read_head_branch` :

```rust
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
```

`read_head_branch` renvoie déjà `None` si le `HEAD` est absent — le cas
« gitdir pointant nulle part » est donc couvert sans code supplémentaire.

- [ ] **Step 4 : Lancer, fmt, clippy, commit**

Run: `cargo test -p wimux-server git_fichier_worktree && cargo test -p wimux-server`
Expected: PASS (les 2 nouveaux + non-régression).

```bash
cargo fmt -p wimux-server && cargo clippy -p wimux-server -- -D warnings
git add crates/wimux-server/src/git.rs
git commit -m "fix(batch): git_branch suit gitdir: — les sessions de lot affichent leur branche

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

### Task 10 : Skill — section « revue de lot »

**Files:**
- Modify: `skills/wimux/SKILL.md`, `skills/wimux/references/commands.md`

- [ ] **Step 1 : Ajouter la section au `SKILL.md`**

Insérer avant la section « ## Bonnes pratiques » :

```markdown
## Revue d'un lot d'agents (fan-out)

Quand une tâche mérite plusieurs tentatives indépendantes, lance un **lot** :
chaque agent travaille dans son propre worktree git isolé.

1. **Lancer** : `wimux batch create --repo <chemin> --template claude --prompt "<tâche>" --count 3`
   → `{"group":"batch0","sessions":[…]}`
2. **Suivre** : `wimux batch list`, et l'avancement de chaque agent via
   `wimux agent list` / `wimux agent logs`.
3. **Résumer** : `wimux batch review -g batch0` → par agent : fichiers changés,
   `+/-`, non suivis, présence de commits, statut.
4. **Comparer** : `wimux batch diff -g batch0 -i <n>` pour lire le travail d'un
   agent en détail.
5. **Intégrer le gagnant** :
   `wimux batch pr -g batch0 -i <n> --title "<titre>" --body "<pourquoi celui-ci>"`
   → commite son travail en cours, pousse sa branche, ouvre la PR, renvoie son URL,
   et **supprime les perdants**. Le gagnant reste vivant pour traiter la revue.

**Deux règles :**
- Passe **toujours par `review` avant `diff`** : le résumé coûte quelques lignes,
  un diff complet peut être énorme. Ne lis en détail que les agents plausibles.
- Fournis **toujours `--title` et `--body`** : tu viens de lire les diffs, tu es
  le seul à pouvoir écrire un titre utile et expliquer pourquoi cette tentative
  l'emporte. wimux ajoute de lui-même un pied de page de provenance.
```

- [ ] **Step 2 : Ajouter la référence des commandes**

Ajouter à la fin de `skills/wimux/references/commands.md` :

```markdown
## Lots d'agents (`wimux batch`)

### create
`wimux batch create --repo <chemin> --template <nom> --prompt "…" [--count N]`
Lance N agents (défaut 2), chacun dans un worktree git isolé du dépôt.
Sortie : `{"group":"…","sessions":[…]}`.

### list
`wimux batch list` → `[{"group","base_repo","base_branch","sessions":[…]}]`.

### review
`wimux batch review -g <group>`
→ `[{"session","index","branch","status","files_changed","insertions","deletions","untracked","has_commits"}]`.

### diff
`wimux batch diff -g <group> -i <index>` (ou `-s <session>`)
Diff complet : fichiers suivis vs la base + contenu des fichiers non suivis.

### pr
`wimux batch pr -g <group> -i <index> [--title "…"] [--body "…"]`
Commite le travail en cours du gagnant, pousse sa branche, ouvre la PR (via `gh`),
renvoie `{"url":"…"}`, puis supprime les agents perdants. Refuse proprement si
`gh` est absent/non authentifié, s'il n'y a pas de remote `origin`, ou si l'agent
n'a rien produit.
```

- [ ] **Step 3 : Commit**

```bash
git add skills/wimux/SKILL.md skills/wimux/references/commands.md
git commit -m "docs(batch): skill — section revue de lot (create/review/diff/pr)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Revue finale (toute la branche)

- [ ] **Step 1 : Suite complète + qualité**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cd wimux-gui && npm run build
```
Expected: tout vert. *(Note : `wimux-gui/src-tauri/src/lib.rs` porte un décalage
`rustfmt` PRÉ-EXISTANT hérité de `main`, sans rapport avec M4.)*

- [ ] **Step 2 : Rebuild release + redémarrage du daemon**

```bash
cargo build --release
./target/release/wimux.exe kill-server
```

- [ ] **Step 3 : Démo bout-en-bout sur un dépôt jetable**

Sur un dépôt git **jetable** poussé sur GitHub (pour pouvoir aller jusqu'à `pr`
sans polluer un vrai projet) : `create` → laisser travailler → `review` → `diff`
→ `pr`. Vérifier : la PR existe, les perdants ont disparu (worktrees supprimés),
le gagnant est toujours listé.

- [ ] **Step 4 : Mémoire projet** — consigner M4 fait dans `wimux-etat-avancement`.

---

## Notes de conception (rappels)

- **La collecte ne mute rien** : `diff --numstat`, `ls-files --others`,
  `diff --no-index`. Le seul point qui écrit est `commit_wip`, sur le chemin PR
  uniquement. *(Une variante `git add -A -N` a été écartée : elle fausse le
  comptage des non-suivis au second appel.)*
- **`--numstat` plutôt que `--stat`** : format machine, robuste à parser ;
  les binaires (`-\t-\t`) comptent comme fichiers changés sans lignes.
- **Toutes les gardes avant le moindre effet de bord** dans `open_pr` ; si le push
  réussit mais la PR échoue, l'erreur dit l'état réel et **rien n'est nettoyé**.
- **Nettoyage des perdants seulement** : leurs branches ne sont jamais poussées,
  donc rien de publié n'est détruit ; le gagnant survit pour itérer sur la revue.
- **wimux fait la plomberie, Claude apporte le sens** : titre et corps de PR
  viennent de Claude (repli mécanique sinon), wimux ajoute la provenance.
