# ADR-0004 — IPC : Named Pipes en I/O overlapped

- **Statut** : accepté
- **Date** : 2026-07-10

## Contexte

Le client et le serveur dialoguent sur un Named Pipe. L'usage est intrinsèquement
**bidirectionnel et concurrent** : le serveur pousse des frames d'affichage
pendant que le client envoie des frappes clavier. Chaque extrémité a donc un
thread lecteur et un thread écrivain opérant sur le **même** handle de pipe.

La première implémentation ouvrait les pipes en I/O **synchrone** (non
overlapped) et implémentait `Read`/`Write` pour `&PipeConn`, partagé via `Arc`
entre les deux threads. Le test d'intégration détach/reattach se **bloquait** :
le client restait figé dès qu'il tentait d'envoyer une frappe.

## Diagnostic

Sur Windows, un handle ouvert **sans** `FILE_FLAG_OVERLAPPED` effectue des
opérations d'I/O **sérialisées** : le système maintient une position de fichier
et ne laisse **pas** deux opérations se dérouler en parallèle sur le handle. Quand
le thread lecteur est bloqué dans un `ReadFile` (en attente de la prochaine
frame), un `WriteFile` concurrent depuis un autre thread **attend la fin de la
lecture**. Comme la lecture ne se termine que lorsqu'une frame arrive — et que la
frame suivante peut ne jamais venir tant que l'utilisateur n'a rien tapé — on
obtient un **interblocage**.

La supposition initiale (« un pipe duplex en mode octet autorise lecture et
écriture concurrentes depuis deux threads ») n'est vraie **qu'en I/O
overlapped**.

## Décision

**Ouvrir tous les pipes avec `FILE_FLAG_OVERLAPPED` et faire de l'I/O
overlapped.** Chaque opération (`ReadFile`/`WriteFile`) fournit sa propre
structure `OVERLAPPED` avec un évènement dédié ; si l'opération est en attente
(`ERROR_IO_PENDING`), on attend l'évènement puis on récupère le résultat via
`GetOverlappedResult`. Les opérations deviennent indépendantes : lecture et
écriture peuvent réellement se dérouler en parallèle sur le même handle.

Détails d'implémentation (`crates/wimux-protocol/src/transport.rs`) :
- côté client : `CreateFileW(..., FILE_FLAG_OVERLAPPED, ...)` ;
- côté serveur : `CreateNamedPipeW(PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED, ...)`
  et `ConnectNamedPipe` avec une `OVERLAPPED` (l'acceptation devient asynchrone) ;
- un évènement manuel (`CreateEventW`) par opération, attendu via
  `WaitForSingleObject`, résultat via `GetOverlappedResult` ;
- `ERROR_BROKEN_PIPE` / `ERROR_PIPE_NOT_CONNECTED` sont traduits en fin de flux
  propre (0 octet), ce qui donne un détachement propre côté serveur quand le
  client se déconnecte.

## Conséquences

- Le detach/reattach fonctionne (jalon J2 validé par test d'intégration).
- La déconnexion d'un client vaut détachement : le serveur détecte l'EOF, retire
  l'attachement, et la session continue de vivre — c'est exactement la sémantique
  recherchée.
- Coût : un évènement noyau créé/détruit par opération d'I/O. Acceptable à ce
  stade ; optimisable plus tard (évènement mis en cache par thread/direction).
- Alternative écartée : deux connexions distinctes par client (une par sens).
  Plus simple à écrire mais complique la corrélation des deux flux à une session.
