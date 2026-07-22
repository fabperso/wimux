//! Moteur navigateur pilotable par CDP (B2.1) : un Chromium externe visible,
//! possédé par le daemon via un thread tokio dédié, piloté par `chromiumoxide`.
//!
//! Le daemon reste synchrone : il parle au thread moteur par canaux (le pont
//! `BrowserEngine::exec` bloque jusqu'à la réponse). La logique métier (découverte
//! du binaire, garde d'URL, rendu de l'arbre d'accessibilité) est en fonctions
//! PURES, découplées des types `chromiumoxide` pour rester testables sans navigateur.

use std::path::PathBuf;

/// Renvoie le premier chemin candidat qui existe sur le disque, ou `None`.
pub fn find_browser_binary(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

/// Chemins d'installation standard, Chrome d'abord puis Edge (toujours présent
/// sur Windows 11). L'ordre encode la préférence.
pub fn default_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    // Chrome
    v.push(PathBuf::from(&pf).join(r"Google\Chrome\Application\chrome.exe"));
    v.push(PathBuf::from(&pf86).join(r"Google\Chrome\Application\chrome.exe"));
    if !local.is_empty() {
        v.push(PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe"));
    }
    // Edge (repli)
    v.push(PathBuf::from(&pf86).join(r"Microsoft\Edge\Application\msedge.exe"));
    v.push(PathBuf::from(&pf).join(r"Microsoft\Edge\Application\msedge.exe"));
    v
}

/// N'autorise que les schémas `http`/`https` (casse insensible). Refuse `file:`,
/// `javascript:`, `data:`, `about:`, et toute chaîne sans schéma.
pub fn is_allowed_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Nœud d'accessibilité simplifié (B2.1), découplé des types `chromiumoxide`.
#[derive(Debug, Clone, PartialEq)]
pub struct AxSnapshotNode {
    pub node_id: String,
    pub role: String,
    pub name: Option<String>,
    pub states: Vec<String>,
    pub child_ids: Vec<String>,
}

impl AxSnapshotNode {
    /// Un nœud est décoratif (élagable) s'il n'apporte rien : rôle ignoré/none,
    /// pas de nom — quel que soit son nombre d'enfants. Un nœud générique
    /// (ex. wrapper `<div>`) avec de vrais enfants reste décoratif : ses
    /// enfants sont promus d'un cran (voir `render_node`), sa propre ligne
    /// n'est pas imprimée.
    fn est_decoratif(&self) -> bool {
        (self.role == "none" || self.role == "ignored" || self.role.is_empty())
            && self.name.is_none()
    }
}

/// Rend l'arbre d'accessibilité en texte indenté : `rôle "nom" [états]`, un nœud
/// par ligne, profondeur = indentation de 2 espaces. Les nœuds décoratifs sont
/// élagués. La racine est le premier nœud (CDP renvoie la racine en tête).
pub fn render_ax_tree(nodes: &[AxSnapshotNode]) -> String {
    use std::collections::{HashMap, HashSet};
    if nodes.is_empty() {
        return String::new();
    }
    let index: HashMap<&str, &AxSnapshotNode> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let mut out = String::new();
    // Garde anti-cycle : les données viennent de la PAGE (via `map_ax_node`,
    // Task 5), donc non fiables. Un `child_ids` cyclique (A -> B -> A) sans
    // cette garde ferait déborder la pile et planterait le daemon entier.
    let mut visites: HashSet<&str> = HashSet::new();
    render_node(&nodes[0], &index, 0, &mut out, &mut visites);
    out.trim_end().to_string()
}

fn render_node<'a>(
    node: &'a AxSnapshotNode,
    index: &std::collections::HashMap<&'a str, &'a AxSnapshotNode>,
    depth: usize,
    out: &mut String,
    visites: &mut std::collections::HashSet<&'a str>,
) {
    // Marqué visité avant de descendre : chaque nœud est traité au plus une
    // fois, ce qui garantit la terminaison même en présence d'un cycle.
    if !visites.insert(node.node_id.as_str()) {
        return;
    }
    if !node.est_decoratif() {
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push_str(&node.role);
        if let Some(name) = &node.name {
            out.push_str(&format!(" \"{name}\""));
        }
        if !node.states.is_empty() {
            out.push_str(&format!(" [{}]", node.states.join(", ")));
        }
        out.push('\n');
    }
    // Un nœud décoratif ne consomme pas de profondeur : ses enfants remontent
    // (promotion), qu'il ait ou non déjà des enfants propres.
    let child_depth = if node.est_decoratif() {
        depth
    } else {
        depth + 1
    };
    for cid in &node.child_ids {
        if let Some(child) = index.get(cid.as_str()) {
            // `render_node` re-vérifie et ignore silencieusement si déjà visité.
            render_node(child, index, child_depth, out, visites);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn find_binary_renvoie_le_premier_existant() {
        // Le binaire de test courant existe forcément ; un chemin bidon non.
        let moi = std::env::current_exe().unwrap();
        let bidon = PathBuf::from("Z:/inexistant/xyz.exe");
        assert_eq!(
            find_browser_binary(&[bidon.clone(), moi.clone()]),
            Some(moi)
        );
        assert_eq!(find_browser_binary(&[bidon]), None);
    }

    #[test]
    fn url_autorisee_http_https_seulement() {
        assert!(is_allowed_url("http://localhost:8899/"));
        assert!(is_allowed_url("https://example.com/x"));
        assert!(is_allowed_url("HTTPS://Example.com")); // casse insensible sur le schéma
        assert!(!is_allowed_url("file:///C:/x"));
        assert!(!is_allowed_url("javascript:alert(1)"));
        assert!(!is_allowed_url("data:text/html,x"));
        assert!(!is_allowed_url("about:blank"));
        assert!(!is_allowed_url("localhost:8899")); // sans schéma = refusé
    }

    #[test]
    fn render_ax_tree_indente_role_nom_etats_et_elague() {
        let nodes = vec![
            AxSnapshotNode {
                node_id: "1".into(),
                role: "RootWebArea".into(),
                name: Some("Page de test".into()),
                states: vec![],
                child_ids: vec!["2".into(), "3".into()],
            },
            AxSnapshotNode {
                node_id: "2".into(),
                role: "button".into(),
                name: Some("Continuer".into()),
                states: vec!["focusable".into()],
                child_ids: vec![],
            },
            // Nœud décoratif : role "none", sans nom, sans enfant -> élagué.
            AxSnapshotNode {
                node_id: "3".into(),
                role: "none".into(),
                name: None,
                states: vec![],
                child_ids: vec![],
            },
        ];
        let out = render_ax_tree(&nodes);
        assert!(
            out.contains("RootWebArea \"Page de test\""),
            "racine : {out}"
        );
        assert!(
            out.contains("  button \"Continuer\" [focusable]"),
            "bouton indenté : {out}"
        );
        assert!(
            !out.contains("none"),
            "le nœud décoratif doit être élagué : {out}"
        );
    }

    #[test]
    fn render_ax_tree_vide_donne_chaine_vide() {
        assert_eq!(render_ax_tree(&[]), "");
    }

    #[test]
    fn render_ax_tree_promeut_les_enfants_dun_noeud_decoratif_avec_enfants() {
        let nodes = vec![
            AxSnapshotNode {
                node_id: "1".into(),
                role: "RootWebArea".into(),
                name: Some("Page".into()),
                states: vec![],
                child_ids: vec!["2".into()],
            },
            // Nœud générique (wrapper), sans nom, mais AVEC de vrais enfants :
            // doit rester décoratif et promouvoir ses enfants d'un cran.
            AxSnapshotNode {
                node_id: "2".into(),
                role: "none".into(),
                name: None,
                states: vec![],
                child_ids: vec!["3".into(), "4".into()],
            },
            AxSnapshotNode {
                node_id: "3".into(),
                role: "button".into(),
                name: Some("A".into()),
                states: vec![],
                child_ids: vec![],
            },
            AxSnapshotNode {
                node_id: "4".into(),
                role: "link".into(),
                name: Some("B".into()),
                states: vec![],
                child_ids: vec![],
            },
        ];
        let out = render_ax_tree(&nodes);
        assert!(!out.contains("none"), "le wrapper élagué : {out}");
        // Le wrapper "none" n'a pas consommé de profondeur : button/link sont
        // au même niveau d'indentation que si "none" n'avait jamais existé,
        // c'est-à-dire un seul cran (2 espaces) sous la racine — pas deux.
        assert!(
            out.contains("\n  button \"A\"\n") || out.ends_with("\n  button \"A\""),
            "button indenté d'un seul cran : {out:?}"
        );
        assert!(
            out.contains("\n  link \"B\"\n") || out.ends_with("\n  link \"B\""),
            "link indenté d'un seul cran : {out:?}"
        );
        assert!(
            !out.contains("    button") && !out.contains("    link"),
            "pas de double indentation : {out:?}"
        );
    }

    #[test]
    fn render_ax_tree_cycle_termine_sans_deborder_la_pile() {
        // A et B se pointent mutuellement : sans garde anti-cycle, récursion
        // infinie -> débordement de pile -> crash du binaire de test.
        let nodes = vec![
            AxSnapshotNode {
                node_id: "a".into(),
                role: "generic".into(),
                name: Some("A".into()),
                states: vec![],
                child_ids: vec!["b".into()],
            },
            AxSnapshotNode {
                node_id: "b".into(),
                role: "generic".into(),
                name: Some("B".into()),
                states: vec![],
                child_ids: vec!["a".into()],
            },
        ];
        let out = render_ax_tree(&nodes);
        // La fonction doit retourner (pas de panic/débordement) et chaque
        // nœud n'apparaît qu'une seule fois.
        assert_eq!(out.matches("generic \"A\"").count(), 1, "sortie : {out}");
        assert_eq!(out.matches("generic \"B\"").count(), 1, "sortie : {out}");
    }
}
