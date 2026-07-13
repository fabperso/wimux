# Design — wimux GUI G4 : indicateurs d'activité des sessions

- **Date** : 2026-07-13
- **Statut** : validé (design), en attente du plan d'implémentation
- **Sous-projet de** : `docs/superpowers/specs/2026-07-13-wimux-gui-foundation-design.md`
- **Prérequis** : G1 (mode GUI), G2 (rail de sessions), G3 (volets graphiques) faits et fusionnés dans `main`.

## Objectif

Afficher dans le rail de `wimux-gui` une **pastille d'activité** par session
inactive : la session a produit de la sortie depuis la dernière fois qu'on l'a
regardée. Distinguer une **cloche** (un programme a émis le caractère BEL, ex.
fin de build) par une pastille distincte. C'est le suivi « chaud » **léger** des
sessions non affichées — on garde le « chargement au clic » de G2 (seule la
session active diffuse son flux complet).

## Décisions (validées)

| Sujet | Décision |
|---|---|
| Sessions « chaudes » | **Suivi d'activité léger** (pas de diffusion de fond) ; chargement au clic de G2 conservé |
| Granularité | **Niveau session** (rail) uniquement ; pas d'indicateur par volet (les volets de la fenêtre active sont déjà tous diffusés/visibles) |
| Signal | Deux états distincts : **activité** (sortie non vue) et **cloche** (BEL explicite) |
| Transport | **Extension du sondage `List`** existant (~1 s) : champs `activity`/`bell` ajoutés à `SessionInfo` |
| Détection cloche | Via le **`Perform::bell` de vte** dans `wimux-vt` (pas un scan brut de `0x07`, qui apparaît aussi dans les terminateurs OSC) |

## Architecture & modèle de données

**Source du signal d'activité.** Chaque session possède déjà un
[`Notifier`](../../crates/wimux-server/src/pane.rs) partagé par ses volets, dont
`generation()` s'incrémente à chaque sortie (le `reader_loop` d'un volet appelle
`notifier.bump()`). Pour une session **inactive**, personne ne manipule son
layout, donc sa génération n'avance que sur de vraies sorties de volet → c'est un
proxy fiable de « sortie non vue ».

**État serveur ajouté.**
- Par session : `last_seen_gen: u64` (la génération vue par la GUI la dernière
  fois) et un drapeau cloche.
- Global (sur le `Server`) : `gui_viewed: Mutex<Option<String>>` — la session
  actuellement affichée par la connexion GUI persistante. Posé à l'`AttachGui`,
  effacé à la déconnexion de cette connexion.

**Cloche.** Le drapeau cloche vit sur le `Notifier` de la session (partagé) :
`signal_bell()` (posé par le `reader_loop` d'un volet), `bell()` (lecture),
`clear_bell()`. `wimux-vt` gagne `Terminal::take_bell() -> bool` (miroir de
`take_responses`) alimenté par `impl Perform::bell`. Le `reader_loop`, après
`terminal.advance(buf)`, fait `if terminal.take_bell() { pane.notifier.signal_bell(); }`.

**Calcul des indicateurs** (au moment du `List`, cf. transport) :
- Pour la session **affichée** (`Some(name) == gui_viewed`) : `activity = false`,
  et on rafraîchit son baseline (`last_seen_gen = generation()`) et on efface sa
  cloche — ce qu'on regarde n'est jamais « non vu ».
- Pour les **autres** : `activity = generation() > last_seen_gen` ;
  `bell = notifier.bell()`.

**Effacement.** À la bascule (`AttachGui { X }`), le serveur pose
`X.last_seen_gen = X.generation()` et `X.clear_bell()` → la pastille de `X`
s'éteint dès qu'on la regarde. Le rafraîchissement lazy du baseline de la session
affichée (à chaque `List`) garantit qu'en basculant *hors* d'une session, la
sortie qu'on vient d'y voir n'est pas comptée comme « non vue ».

## Protocole (ajouts minimes)

`SessionInfo` (dans `wimux-protocol`) gagne deux champs :

```
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub activity: bool, // sortie non vue depuis la dernière vue GUI
    pub bell: bool,     // BEL explicite reçu depuis la dernière vue GUI
}
```

Réutilisés tels quels : `ClientMessage::List` → `ServerMessage::Sessions(Vec<SessionInfo>)`
(le serveur calcule `activity`/`bell` par session), et `AttachGui { session }`
(qui, en plus de son rôle G3, marque la session vue et efface ses indicateurs).

Le `ls` du TUI consomme aussi `SessionInfo` : il peut afficher un marqueur ou
ignorer les nouveaux champs. **Rebuild conjoint serveur + CLI** (les deux
partagent la struct ; cf. le piège du daemon persistant : rebuild release +
redémarrage requis pour que les nouveaux champs prennent effet).

## Serveur

1. **`wimux-vt`** : `impl Perform for Screen { fn bell(&mut self) { self.bell_pending = true; } }` ;
   `Terminal::take_bell(&mut self) -> bool` (retourne et remet à faux le drapeau).
2. **`pane.rs`** : sur le `Notifier`, ajouter `bell: AtomicBool` + `signal_bell()` /
   `bell()` / `clear_bell()`. Dans `reader_loop`, après `advance`, remonter la
   cloche : `if st.terminal.take_bell() { … }` puis (hors verrou du volet)
   `pane.notifier.signal_bell()`.
3. **`session.rs`** : `last_seen_gen: AtomicU64` (ou `Mutex<u64>`) ; méthodes
   `mark_seen(&self)` (pose `last_seen_gen = notifier.generation()`, `clear_bell`),
   `has_activity(&self) -> bool` (`generation() > last_seen_gen`),
   `has_bell(&self) -> bool`. `gui_attach_window`/`AttachGui` appellent
   `mark_seen`.
4. **`daemon.rs`** : `Server` gagne `gui_viewed: Mutex<Option<String>>` ; posé dans
   le bras `AttachGui` (`*gui_viewed = Some(session)`) et effacé en fin de
   `handle_client` (quand la connexion GUI persistante se termine). `Server::list`
   calcule `activity`/`bell` par session en tenant compte de `gui_viewed` (session
   vue → `activity=false` + `mark_seen`).

## Frontend (rail)

- Purement additif au rail G2 (`renderRail`, sondage 1 s). Le type `SessionDto`
  gagne `activity`/`bell` (relayés par la commande Tauri `list_sessions`). Pour
  chaque entrée de session, ajouter une **pastille** à droite du nom :
  - `activity` (et pas `bell`) → point discret (ex. gris/bleu).
  - `bell` → pastille distincte (ex. point orange ou icône 🔔), prioritaire sur
    l'activité.
  - session active → aucune pastille.
- **Effacement optimiste** : à la bascule (`switchTo`), effacer localement la
  pastille de la session cible sans attendre le prochain sondage.
- Le pont Tauri (`list_sessions`) mappe les nouveaux champs dans `SessionDto`.

## Tests

- **`wimux-vt` (unitaire)** : `take_bell` — `advance(b"\x07")` → `true` puis
  `false` au 2e appel ; `advance(b"\x1b]0;titre\x07")` (OSC terminé par BEL) →
  **pas** de cloche (vte ne déclenche pas `bell()` pour un terminateur OSC).
- **Serveur (intégration, `gui_mode.rs` ou dédié)** :
  - Activité : créer 2 sessions, attacher la GUI à l'une, injecter de la sortie
    dans l'AUTRE (`SendKeys`/`Command`), vérifier via `List` que l'autre a
    `activity=true` et l'affichée `false` ; basculer (`AttachGui`) sur l'autre →
    `activity=false`.
  - Cloche : injecter un BEL dans une session inactive → `List` la marque
    `bell=true` ; la voir → `bell=false`.
- **Frontend** : validé **manuellement** (comme G1/G2/G3), documenté dans
  `wimux-gui/README.md`.
- **Non-régression** : suites TUI + G1 + G2 + G3 vertes ; fmt + clippy
  `-D warnings` propres ; `npm run build` OK. Le `ls` du TUI reste fonctionnel
  avec les champs `SessionInfo` étendus.

## Hors-périmètre (rappel)

Diffusion de fond des sessions inactives / aperçu miniature live (écartés) ;
notification système hors-fenêtre (OS toast) ; indicateurs par volet ; suivi
multi-GUI précis (l'état « vue » est **global** au serveur, donc correct pour une
seule GUI attachée — plusieurs GUIs simultanées partageraient l'état « vue ») ;
multi-fenêtres.

## Risques

| Risque | Parade |
|---|---|
| Faux positifs de cloche (BEL dans un terminateur OSC) | Détection via `Perform::bell` de vte, pas un scan brut `0x07` ; test unitaire dédié |
| État « vue » global (`gui_viewed`) incorrect avec plusieurs GUIs | Documenté hors-périmètre ; correct pour une GUI (cas visé) |
| `generation()` avance sur des événements non-sortie (resize, zoom…) | Pour une session **inactive**, ces événements ne se produisent pas (personne ne la manipule) → génération ≈ sortie ; acceptable |
| Latence ~1 s de la pastille (sondage) | Acceptable pour un indicateur d'activité ; push temps réel possible plus tard si besoin |
| Extension de `SessionInfo` casse un vieux client/daemon | Rebuild conjoint serveur + CLI ; cf. piège du daemon persistant (redémarrage requis) |
