# Design — wimux multi-agents M1 : couche de statut d'agent

- **Date** : 2026-07-16
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : la fonctionnalité **multi-agents** de wimux (vision façon CMUX)
- **Prérequis** : GUI G1→G4 faits et fusionnés dans `main` (le mode GUI, le rail de sessions, les volets graphiques, les indicateurs d'activité/cloche).

## Contexte : décomposition multi-agents

La fonctionnalité multi-agents complète est découpée en sous-projets, chacun
livrable et testable seul, construits dans l'ordre :

- **M1** *(ce document)* — **couche serveur de statut d'agent** : le serveur sait
  qu'une session est un « agent » et calcule son statut (travaille / au repos /
  attention / terminé / erreur). Fondation.
- **M2** — **création + lanceur + affichage** : créer une session agent (commande
  racine), dialogue/templates/prompt GUI, et le glyphe de statut dans le rail.
- **M3** — **orchestration fan-out** : dispatcher une tâche à N agents, tableau de
  bord agrégé.
- **M4** — **revue & agrégation des résultats**.

## Objectif (M1)

Ajouter au serveur la notion de **session agent** et le **calcul de son statut**,
exposé comme les indicateurs G4 (champs sur `SessionInfo`, sondage `List`). M1 est
une couche serveur pure, **sans création exposée ni frontend** (les deux arrivent
en M2). Elle est validée par des tests unitaires (calcul du statut) et
d'intégration serveur (non-reap + statut de fin).

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Modèle d'agent | Le **processus racine EST l'agent** (la session est lancée avec la commande agent à la place du shell) → détection de fin fiable par code de sortie |
| Détection de statut | **Hybride** : inactivité (horodatage de dernière sortie) + cloche G4 + affinage optionnel par motif (reporté ; M1 = base inactivité+cloche+sortie) |
| Jeu de statuts | *Travaille* · *Au repos* · *Attention* · *Terminé* · *Erreur* |
| Fin de vie | Une session agent **ne se fait pas reaper** à la sortie du processus : elle reste visible (statut *Terminé*/*Erreur*) jusqu'à fermeture manuelle |
| Périmètre M1 | Couche **serveur** uniquement (drapeau agent + calcul + non-reap + exposition `SessionInfo`) ; création, lanceur GUI et glyphe rail = **M2** |
| Transport | Extension de `SessionInfo` via le sondage `List` (~1 s), comme G4 |

## Modèle : session agent

Une **session agent** est une session dont le volet racine exécute la commande de
l'agent au lieu du shell par défaut (wimux réutilise le paramètre `shell` de
`Session::new` en y passant la commande). Un drapeau **`agent: bool`** sur la
session indique qu'il faut calculer et exposer son statut.

En M1, ce drapeau n'est posé par **aucun** chemin client (la création exposée est
en M2). Il est posé par un **setter interne** (`Session::mark_agent`), exercé
uniquement par les tests de M1. M2 le posera à la création réelle.

**Non-reap.** Aujourd'hui, `Session::reap` retire les fenêtres dont tous les volets
sont morts, et une session sans fenêtre est retirée par le serveur. Pour une
**session agent**, on **n'exécute pas** ce reap : la fenêtre (et son volet mort)
est conservée, la session reste vivante (`is_alive` vrai) et visible, avec le
statut *Terminé*/*Erreur* lu sur le code de sortie du volet racine. Elle disparaît
uniquement sur `kill_session` (commande G2 existante).

## Statut : `AgentStatus` et calcul

Nouvel enum sérialisable (dans `wimux-protocol`) :

```
enum AgentStatus { Working, Idle, Attention, Done, Error }
```

`Session::agent_status(&self, idle_threshold: Duration) -> Option<AgentStatus>` :
- Si la session **n'est pas** un agent → `None`.
- Sinon, dans l'ordre de priorité :
  1. Le volet racine a **quitté** → `Some(Done)` si code de sortie `0`, sinon
     `Some(Error)`.
  2. **Cloche** reçue depuis la dernière vue (réutilise le drapeau cloche du
     `Notifier`, G4) → `Some(Attention)`.
  3. Sortie récente (`now - last_output_at < idle_threshold`) → `Some(Working)`.
  4. Sinon (vivant + silencieux depuis ≥ seuil) → `Some(Idle)`.

Notes :
- *Terminé*/*Erreur* sont **terminaux** et priment, y compris pour la session
  actuellement affichée par la GUI.
- *Attention* s'appuie sur le drapeau cloche, **effacé à la vue** (`mark_seen`,
  G4) : un agent qu'on regarde n'affiche donc pas *Attention* (on le voit déjà) ;
  il affiche *Travaille*/*Au repos*/*Terminé*.
- La part floue (*Au repos* = « attend une entrée » **ou** « réfléchit
  longuement ») est assumée ; l'affinage par motif d'invite est reporté (config
  optionnelle, hors M1).

## Serveur (M1)

1. **Horodatage de dernière sortie.** Le `Notifier` (partagé par les volets d'une
   session, cf. G4) gagne `last_output_at: Mutex<Instant>`, mis à jour dans
   `bump()`. Pour une session agent inactive côté layout, `bump` ≈ sortie de
   volet (même raisonnement que la génération G4), donc `last_output_at` reflète
   la dernière sortie. Accès `Notifier::last_output_elapsed() -> Duration`.
2. **Code de sortie du volet racine.** `Pane` expose `exit_code(&self) ->
   Option<u32>` (le champ existe déjà dans `PaneState`). Le « volet racine » d'une
   session agent est l'unique volet de sa fenêtre initiale ; `Session` fournit un
   accès à ce volet pour lire son état de fin.
3. **`Session`** : champs `agent: bool` (via `Mutex`/atomique) + `mark_agent(&self)`
   (setter interne) ; méthode `is_agent()` ; `agent_status(idle_threshold)`
   (calcul ci-dessus). Le reap est **court-circuité** pour les sessions agent.
4. **`Server::list`** : pour chaque session, renseigne `SessionInfo.agent =
   s.is_agent()` et `SessionInfo.agent_status = s.agent_status(seuil)` (le seuil
   vient de la config). Cohabite avec le calcul `activity`/`bell` de G4.

## Protocole

`SessionInfo` (dans `wimux-protocol`) gagne :

```
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub activity: bool,          // G4
    pub bell: bool,              // G4
    pub agent: bool,             // M1 : est-ce une session agent ?
    pub agent_status: Option<AgentStatus>, // M1 : None si pas un agent
}
```

Nouvel enum `AgentStatus` (Serialize/Deserialize/Clone/Copy/Debug/PartialEq).
Le `ls` du CLI lit `SessionInfo` sans le construire → il peut ignorer les nouveaux
champs (rebuild conjoint serveur + CLI ; cf. le piège du daemon persistant).

## Config

`agent-idle-seconds` (façon tmux config, défaut **4**) : le seuil qui sépare
*Travaille* de *Au repos*. L'affinage optionnel par motif d'invite/erreur est
reporté (hors M1).

## Tests

- **Unitaires (`session.rs`)** sur `agent_status`, tous les cas via des états
  construits directement :
  - session non-agent → `None` ;
  - agent, volet racine sorti code 0 → `Done` ; code ≠ 0 → `Error` ;
  - agent vivant + cloche → `Attention` ;
  - agent vivant + `bump` récent → `Working` ;
  - agent vivant + silence simulé au-delà du seuil → `Idle` ;
  - priorité : sortie prime sur cloche prime sur travaille prime sur repos.
  (Le seuil et l'horodatage sont pilotables dans le test — p. ex. seuil très
  court + `sleep`, ou un `mark_agent` puis lecture immédiate.)
- **Test lib serveur (`session.rs`, module `#[cfg(test)]`)** : **non-reap +
  statut de fin** — construire **directement** une `Session::new(name, cols, rows,
  cmd)` où `cmd` est une commande qui se termine vite (ex. `cmd /c exit 0` ; puis
  `cmd /c exit 3` pour l'erreur), la marquer agent (`mark_agent`), attendre la
  sortie du processus, appeler `reap()` et vérifier que `is_alive()` reste **vrai**
  (la session agent survit à son processus mort) et que `agent_status` vaut `Done`
  (resp. `Error`). Sans le drapeau agent, la même session serait reapée
  (`is_alive()` faux). Ce test reste au niveau lib (construction directe de
  `Session`), **sans message protocole client** — la création exposée est M2.
- **Non-régression** : suites TUI + G1/G2/G3/G4 vertes ; fmt + clippy
  `-D warnings` propres. Le `ls` du CLI reste fonctionnel avec `SessionInfo`
  étendu.
- **Pas de test frontend** (aucun frontend en M1).

## Hors-périmètre M1 (rappel)

Création exposée d'une session agent (message protocole + spawn côté client) ;
dialogue/templates/prompt de lancement GUI ; **glyphe de statut dans le rail** —
tout cela est **M2** (démontrable ensemble). Orchestration fan-out (**M3**),
revue agrégée (**M4**). Affinage du statut par motif d'invite/erreur (config
optionnelle, ultérieure). Multi-volets agent (un agent = un volet racine en M1).

## Risques

| Risque | Parade |
|---|---|
| *Au repos* confond « attend une entrée » et « réfléchit longuement » | Assumé ; la cloche (*Attention*) et un motif d'invite optionnel (ultérieur) affinent |
| Première dépendance temporelle serveur (`Instant`) | Isolée dans le `Notifier` ; le serveur utilise déjà `Duration` (pas d'interdiction côté serveur, contrairement au bac à sable des scripts de workflow) |
| M1 non démontrable en réel sans M2 (pas de création exposée) | Couche serveur fondation, entièrement couverte par tests unitaires + intégration serveur (via point d'entrée de test) ; démonstration end-to-end en M2 |
| Non-reap → sessions agent mortes qui s'accumulent | Fermeture manuelle via `kill_session` (G2) ; M2 pourra ajouter un « fermer les terminés » |
| `SessionInfo` étendu casse un vieux client/daemon | Rebuild conjoint serveur + CLI ; cf. piège du daemon persistant (redémarrage requis) |
