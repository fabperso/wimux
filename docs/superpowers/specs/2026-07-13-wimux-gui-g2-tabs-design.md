# Design — wimux GUI G2 : onglets verticaux (rail des sessions)

- **Date** : 2026-07-13
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : `docs/superpowers/specs/2026-07-13-wimux-gui-foundation-design.md`
- **Prérequis** : G1 fait (app Tauri `wimux-gui`, mode GUI du protocole : `AttachGui`/`PaneInput`/`PaneSnapshot`/`PaneOutput`).

## Objectif

Ajouter le **rail d'onglets verticaux** (style CMUX) à `wimux-gui` : lister les
sessions, changer de session, en créer, en fermer, en renommer. C'est ce qui
commence à donner le look CMUX par-dessus la fondation G1.

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Disposition | Rail vertical à gauche (décidé en G1), thème sombre/bleu |
| Onglets inactifs | **Chargement au clic** : seule la session active diffuse son flux ; à la bascule, on récupère l'instantané actuel de la nouvelle session. Les inactives tournent côté serveur (rien perdu). Le suivi live des inactives = G4. |
| Zone terminal | **Une** instance xterm.js réutilisée (réécrite à chaque bascule) |
| Actions du rail | changer · créer (`+`) · fermer (croix au survol) · renommer (double-clic) |
| Liste à jour | sondage ~1 s de `List` (une session créée/fermée ailleurs, ex. TUI, apparaît/disparaît) |

## Comportement UX

- Rail : une entrée par session (nom + point d'état neutre — les vraies pastilles
  d'activité sont G4). Session active surlignée (barre bleue à gauche).
- Cliquer un onglet ⇒ (ré)attache cette session ⇒ instantané + reprise du flux ;
  l'ancienne est détachée (continue de tourner côté serveur).
- `+` ⇒ crée une session (nom auto `0/1/2…` comme la CLI) qui devient active.
- Croix au survol d'un onglet ⇒ tue la session ; le rail se met à jour.
- Double-clic sur le nom ⇒ champ éditable ⇒ Entrée valide le renommage.

## Protocole (ajouts minimes, réutilisation maximale)

Réutilisés tels quels : `ClientMessage::List` → `ServerMessage::Sessions(Vec<SessionInfo>)` ;
`ClientMessage::Kill { name }` → `Ok` ; `ClientMessage::AttachGui { session }` →
`PaneSnapshot` + `PaneOutput`.

Nouveaux :
- `ClientMessage::CreateSession { name: Option<String> }` → crée une session **sans
  attacher** et répond `ServerMessage::SessionCreated { name }` (ou `Error`).
- `ClientMessage::RenameSession { from: String, to: String }` → renomme, répond
  `ServerMessage::Ok` (ou `Error` si `from` inconnue / `to` déjà pris).

Sémantique **bascule** de `AttachGui` : le serveur arrête proprement la
retransmission GUI en cours **sur cette connexion** avant de démarrer la nouvelle.

## Serveur

1. **Cycle de vie de l'attachement GUI par connexion.** Aujourd'hui (G1) le thread
   de retransmission `PaneOutput` est *fire-and-forget*. G2 le remplace par un
   attachement suivi : drapeau d'arrêt (`AtomicBool`) + `JoinHandle`. À chaque
   `AttachGui`, on signale l'arrêt de l'ancien, on le `join`, puis on démarre le
   nouveau. À la déconnexion, on arrête. La boucle de retransmission lit
   `rx.recv_timeout(200ms)` pour pouvoir vérifier le drapeau. **Corrige le point
   différé de G1.**
2. **Création sans attache** : `CreateSession` réutilise la logique de
   `create_session` du serveur (nom auto si `None`), sans le flux TUI.
3. **Renommage** : rendre le nom de session mutable — remplacer `pub name: String`
   de `Session` par un `Mutex<String>` (ou équivalent) exposé via `fn name(&self)
   -> String` et `fn set_name(&self, s: String)`. Mettre à jour les sites de
   lecture (démon `list`, barre de statut TUI). `RenameSession` : vérifier que
   `from` existe et `to` est libre, déplacer l'entrée dans la table, appeler
   `set_name`.

## Frontend / pont Tauri

- **Une connexion persistante** dédiée au flux de la session active
  (`AttachGui`/`PaneInput`, réception `PaneSnapshot`/`PaneOutput`). Bascule =
  `AttachGui { session }` sur cette connexion.
- **Commandes de contrôle** (`list` / `create` / `kill` / `rename`) via des
  **connexions courtes jetables** (une par appel : connect → handshake → send →
  recv → close), comme la CLI, pour ne pas s'entrelacer avec le flux.
- Commandes Tauri exposées au frontend :
  - `list_sessions() -> Vec<SessionInfoDto>` (nom + attaché)
  - `attach_session(name: String)` (bascule sur la connexion persistante)
  - `create_session(name: Option<String>) -> String` (renvoie le nom créé)
  - `kill_session(name: String)`
  - `rename_session(from: String, to: String)`
- **Frontend** : composant rail (liste + surbrillance active + `+` + croix +
  double-clic renommer) ; une instance xterm.js réutilisée ; sondage ~1 s de
  `list_sessions` pour rafraîchir le rail.

## Jalons de construction

- **G2a** : liste des sessions dans le rail + **bascule** (attach), avec l'arrêt
  propre du flux précédent côté serveur.
- **G2b** : **créer** (`+`) + **fermer** (croix).
- **G2c** : **renommer** (double-clic) + support serveur du renommage.

## Tests

- **Serveur (intégration)** :
  - Bascule : attacher GUI à la session A, envoyer `AttachGui { B }`, vérifier
    qu'on reçoit le `PaneSnapshot` de B et **plus** de `PaneOutput` de A après la
    bascule (le flux de A est arrêté).
  - `CreateSession` : crée et renvoie un nom, la session apparaît dans `List`.
  - `RenameSession` : renomme ; `List` reflète le nouveau nom ; `to` déjà pris →
    `Error`.
- **Frontend** : validé **manuellement** (comme G1) — cliquer entre sessions,
  créer, fermer, renommer.
- **Non-régression** : la suite existante (TUI + G1) reste verte ; fmt + clippy
  `-D warnings` propres.

## Hors périmètre (rappel)

Indicateurs d'activité live des onglets inactifs (**G4**) ; volets graphiques
dans une session (**G3**) ; navigateur intégré ; multi-agents.

## Risques

| Risque | Parade |
|---|---|
| Bascule laisse fuir l'ancien flux/abonné | Drapeau d'arrêt + `join` du thread de retransmission à chaque `AttachGui` |
| Refactor `name` mutable touche plusieurs sites | Accès centralisé via `name()`/`set_name()` ; sites de lecture peu nombreux (démon `list`, barre de statut) |
| Sondage 1 s trop coûteux | Requête `List` très légère (local, Named Pipe) ; ajustable ; poussée possible plus tard |
| Interleaving contrôle/flux | Commandes de contrôle sur connexions jetables séparées |
