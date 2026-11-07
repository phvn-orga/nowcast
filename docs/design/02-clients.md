# Les clients

État au 2026-11-07 : architecture des clients, format du flux et déclaration de
session posés ; mod central et bibliothèque des mods vérifiés, le rechargement à chaud
et le rafraîchissement aussi en session interactive ; entrée d'un flux et mesures
des sessions Claude Code ajoutées.

## Décisions prises

- **Le détail arrive en JSON**, tel que la brique l'a publié et que le service l'a
  vérifié (voir `01-briques.md`). Il est placé tel quel dans le champ `data` de
  l'événement, sans conversion ni base64.
- **Des clients minces : l'état vit dans le service.** Le service garde, pour chaque
  processus d'ancrage, ses abonnements et l'identifiant de session en cours, et pour
  chaque flux ses derniers événements (`03-contrats.md`, section 3). Une coupure du flux
  ne fait rien perdre dans la limite de cet historique, et au-delà le client en est
  averti (`flow-gap`). Un client ne fait que
  six choses : déclarer sa session, s'abonner, se désabonner, lire son flux,
  demander le rafraîchissement immédiat d'un flux et envoyer une entrée sur un flux
  (voir `01-briques.md`). Le service n'exécute aucune action
  sur une source : un mod qui agit le fait lui-même (voir `01-briques.md`). Ce qui
  n'est pas dans le client n'a pas à être porté d'un agent à l'autre.
- **Pas d'abonnement sans session déclarée.** Le client déclare sa session au démarrage
  (`PUT /v1/session`, voir `03-contrats.md`), avant toute autre requête, puis à chaque
  changement (`/clear`, reprise). Un `428` en cours de session signale un service
  relancé, après un arrêt à vide par exemple : le client déclare à nouveau sa session
  et renvoie sa requête. La mort du processus d'ancrage termine tout.
- **Les abonnements suivent le processus d'ancrage, pas l'identifiant de session.** Un
  `/clear` ne coupe donc rien : le client se désabonne de ce qu'il ne veut plus. Le
  service n'a pas à deviner ce que signifie un changement de session.
- **Un flux par session, des requêtes en appels HTTP.** La session tient un seul flux
  ouvert, qui regroupe tous ses abonnements. S'abonner, se désabonner, demander un
  rafraîchissement, envoyer une entrée, lire la dernière valeur d'un flux ou annoncer
  un changement de session sont des appels HTTP ordinaires sur le même socket. Le service rattache
  appels et flux à la même session par leur processus d'ancrage commun (voir
  `00-cadrage.md`) : ni jeton ni identifiant de connexion à échanger.
- **Le flux est en SSE** (Server-Sent Events) sur HTTP/1.1, sur le socket Unix du
  service : une réponse HTTP qui reste ouverte. Chaque événement suit le format JSON
  de CloudEvents, l'enveloppe en attributs, le détail dans `data`. Les raisons et les
  candidats écartés sont plus bas.
- **Une requête ponctuelle « dernière valeur d'un flux »**, pour les clients qui
  lisent à un instant donné plutôt qu'en continu. Avec `after`, elle rend l'historique
  du flux après un identifiant : ce dont a besoin un client à base de hooks, ou un mod
  qui s'abonne en cours de session à un flux déjà suivi.
- **Un outil en ligne de commande fourni avec nowcast, commun à tous les agents.** Par
  exemple `nowcast watch` (tient le flux, se reconnecte), `nowcast get <flux>` (dernière
  valeur, en un appel ; avec `--after <id>`, l'historique après cet identifiant), `nowcast subscribe`, `nowcast refresh <flux>` (rafraîchissement
  immédiat, rien d'affiché en cas de succès), `nowcast input <flux>` (envoie le détail
  JSON lu sur l'entrée standard), `nowcast bricks enable <nom>` et
  `nowcast bricks disable <nom>` (activer ou désactiver une brique). La reconnexion et la conduite à tenir
  quand le service est absent sont écrites une fois, là.
- **`nowcast watch` se lit par programme.** `--last-event-id <ID>` reprend le flux
  après cet identifiant : chaque événement gardé depuis, plutôt que toutes les
  dernières valeurs.
  L'identifiant à lui passer est l'`id` de l'événement CloudEvents, dans `data`, égal
  à l'`id` SSE. Sous `--json`, des lignes d'état disent aussi où en est la connexion :
  `{"status":"connected"}` à chaque connexion, la première comprise, et
  `{"status":"retrying","reason":"..."}`. Un client suit ainsi l'état du flux et
  reprend après une coupure sans analyser de messages destinés à un humain.
- **Un seul binaire, `nowcast`** : l'outil en ligne de commande, et le service sous la
  sous-commande `nowcast daemon`, que lance l'unité systemd. L'outil et le service
  sortent toujours à la même version : une seule chose à installer. Homebrew le place
  dans son répertoire `bin`, qui est dans le `PATH`, et l'unité qu'installe la formule
  le lance par le chemin `opt` de Homebrew, stable d'une version à l'autre. Depuis les
  sources, pour le développement, il s'installe dans `~/.local/bin`
  (`cargo install --root ~/.local`), d'ordinaire dans le `PATH`, contrairement à
  `~/.cargo/bin`. L'adaptateur Claude Code lance `nowcast` depuis le `PATH`.
- **Un adaptateur mince par agent, dans le dépôt de nowcast** (par exemple
  `clients/claude-code/`, plus tard `clients/codex/`). Il traduit les événements de
  l'agent (début de session, changement de session, fin) en requêtes au service, et
  s'appuie sur l'outil commun pour le reste. Un agent qui n'a que des hooks ponctuels
  s'en sert par `nowcast get`.
- **Pour Claude Code, l'adaptateur est le mod central de la session ; seul le flux est
  centralisé.** Le mod central lance `nowcast watch`, tient l'unique flux SSE de la
  session, déclare la session et ses changements (début, `/clear`, reprise), et écrit
  chaque flux reçu dans son état de plugin (`$.state`). Les requêtes (s'abonner, se
  désabonner, demander un rafraîchissement) partent directement du processus de
  l'agent vers le service en HTTP : tous les mods tournent dans le même processus
  d'ancrage, donc le service les rattache à la même session que le flux. Un mod qui
  s'abonne reçoit l'identifiant du flux en réponse et le lit ensuite dans l'état du
  mod central. Pas d'API de requêtes entre mods à concevoir. Validé par un spike, puis
  par l'implémentation (voir « Ce que peuvent les clients »).
- **Le mod central envoie les mesures de sa session** à la brique claude-code, quand
  elle est activée : il s'abonne au flux de sa session, y envoie une entrée après
  chaque requête au modèle et à chaque `session.measure`, et y relit ce que la brique
  en retient (`05-brique-claude-code.md`). Ce flux suit
  l'identifiant de session : après un `/clear`, le mod se désabonne de l'ancien et
  s'abonne au nouveau.
- **Un `watch` par identifiant de session.** Comme `/clear` vide l'état du plugin, le
  mod central arrête alors `watch` et le relance sous le nouvel identifiant, sans
  `Last-Event-ID` : le service envoie à nouveau chaque dernière valeur, qui remplit
  l'état vidé. Il ne se réabonne pas : les abonnements suivent l'ancrage.
- **La bibliothèque des mods est `$.nowcast`, un *noun* de l'API plugin ajouté par le
  mod central.** Un mod qui déclare `nowcast` dans ses dépendances reçoit
  `subscribe`, `unsubscribe`, `refresh`, `input`, `last` et `history`, typés par le contrat du mod
  central. `history` lit l'historique d'un flux après un identifiant
  (`GET /v1/flows/{flow}?after=<id>`) : c'est ainsi qu'un mod qui s'abonne en cours de
  session à un flux déjà suivi lit ce qui a précédé. L'état partagé, lui, ne garde que
  la dernière valeur de chaque flux et ne note pas les `flow-gap`. Chaque appel rend un
  résultat (succès, ou statut et code d'erreur), jamais une exception. Les requêtes
  partent toujours du processus de l'agent, directement vers le service. Écartés :
  - un fichier que chaque mod copie : un mod n'importe que des fichiers de son propre
    dossier, et le moteur refuse un module qui passe `$` à une fonction qu'il ne peut
    pas suivre ;
  - un plugin séparé qui ajouterait le nom : à côté du flux, la bibliothèque écrit la
    dernière valeur rendue par un abonnement dans l'état partagé, visible de tous les
    lecteurs, et sait quand le flux revient pour vérifier ses abonnements et se
    réabonner à ceux que le service a perdus. Un mod qui lit des flux dépend déjà du
    mod central.

  En contrepartie, le code de la bibliothèque évolue avec le mod central, un mod
  chargé sans lui n'a pas `$.nowcast`, et tout plugin peut observer ou réécrire les
  appels, qui sont des événements du moteur.
- **Aucun mécanisme interne non documenté d'un agent** (variable d'environnement,
  fichier de session) n'entre dans le modèle : seules les API publiques des agents
  comptent. Une exception, dans l'adaptateur Claude Code et pour une mesure
  seulement : le mod central lit les transcriptions de la session, au chargement
  pour retrouver les tokens consommés avant lui, et après chaque tour pour la durée
  de vie du cache, deux valeurs qu'aucune API ne donne (`05-brique-claude-code.md`).
  Illisibles, il compte depuis son chargement et la brique suppose une heure.

## Ce que peuvent les clients

- **Plugin Claude Code.** Vérifié dans les types de son API plugin (version 2.1.318).
  - Le code des plugins tourne dans le processus de l'agent : un `$.http.fetch` part de
    ce processus, un sous-processus lancé par `$.process.spawn` en est un enfant
    direct. Les deux remontent au même ancrage.
  - `$.http.fetch` sait passer par un socket Unix (`socketPath`) mais ne rend la
    réponse qu'une fois complète ; il ne lit pas un flux.
  - `$.process.spawn` lit la sortie d'un processus au fil de l'eau, mais n'écrit sur
    son entrée qu'une fois, puis la ferme. Les commandes passent donc par HTTP, pas
    par l'outil lancé.
  - Tout arrive en texte, jamais en octets bruts : un protocole binaire n'atteint pas
    ce client directement.
  - `$.session.id()` donne l'identifiant de session ; l'événement de fin de session
    porte sa raison, dont `clear`. Après un `/clear`, aucun événement de début n'est
    émis. Observé sur 2.1.319 : un `/clear` se présente comme une fin de session de
    raison `clear`, et le nouvel identifiant apparaît dans `$.session.id()` peu après.
  - Le contexte, le cache, le modèle et l'effort d'une session n'existent que dans le
    moteur, qui les donne aux plugins par ses événements : le service ne peut pas les
    lire lui-même, le client les lui transmet.
  - Tout plugin lit l'état (`$.state`) de n'importe quel autre ; seul le propriétaire
    l'écrit, et une écriture est un événement qu'on peut intercepter.
  - L'état d'un plugin appartient à la session : `/clear` le vide. Observé sur
    2.1.319 : une valeur posée avant un `/clear` se relit en version 0 après.
  - Les `$.state.get` d'un même traitement d'événement lisent tous le même instant
    (documentation de l'API) : une boucle longue qui relit l'état voit des valeurs
    figées.

  Spike du 2026-11-05 (Claude Code 2.1.318, session sans interface `claude -p`, deux
  mods de test), les trois points sont validés :
  - Un mod suit les écritures d'un autre mod dans son état par un hook `state.set` et
    relit la valeur. Le sens inverse marche aussi : un mod écrit le flux qu'il veut
    dans son propre état, le mod central intercepte l'écriture. Écrire dans l'état
    d'un autre mod est refusé.
  - Un `$.http.fetch` par `socketPath` arrive avec le processus `claude` lui-même pour
    pair ; un `curl` lancé par `$.process.spawn` a `claude` pour parent direct. Même
    ancrage pour les requêtes et le flux.
  - Un `curl -N` lancé par un mod livre les événements SSE un par un, 0,1 à 3,3 ms
    après leur émission, sans regroupement.

  Contraintes à respecter dans la conception :
  - Le hook `state.set` d'un observateur s'exécute de façon synchrone, imbriqué dans
    l'écriture du propriétaire : un observateur lent ralentit le mod central, et un
    observateur peut réécrire ou refuser la valeur. Les hooks des mods restent donc
    minimaux. C'est une question de robustesse, pas de sécurité : tous les mods
    partagent déjà le même processus.
  - Hors rendu d'interface, ce hook est le seul moyen de suivre l'état d'un autre mod.
  - Une référence à l'état d'un autre mod (`{plugin, key}`) doit être un littéral.

  Revérifié avec l'implémentation (Claude Code 2.1.319, `claude -p`, vrai service,
  brique de test, mod central, mod d'exemple et mod observateur) : un mod
  s'abonne par `$.nowcast` et demande un rafraîchissement, la mise à jour lui arrive
  par le flux du mod central, ce qui n'est possible que si requête et flux partagent
  un ancrage ; après un `/clear`, le nouvel identifiant est déclaré et la dernière
  valeur réécrite dans l'état vidé, sans nouvel abonnement.

  Non vérifié : une session interactive (comportement attendu identique), plusieurs
  observateurs sur la même clé, et le rechargement à chaud du mod central.
- **Codex et Droid.** Leurs hooks sont des commandes ponctuelles, lancées sur des
  événements de session ; ils lisent à un instant donné, pas en continu. Les hooks
  descendent du processus de l'agent, donc l'identité du noyau les rattache à leur
  session.

## Pourquoi SSE

C'est le seul candidat qu'un plugin Claude Code consomme sans rien installer, et un
hook ponctuel s'en sert avec un simple `curl`. `Last-Event-ID` correspond au numéro de
séquence de l'enveloppe, ce qui permet de reprendre un flux après une coupure. HTTP
permettra plus tard de négocier un autre encodage par `Accept` sans rien casser.

| Candidat | Plugin Claude Code | Bidirectionnel | Verdict |
|---|---|---|---|
| SSE + HTTP sur socket Unix | `fetch` pour les requêtes, un processus lancé pour le flux | flux descendant + appels séparés | retenu |
| Varlink (réponses en flux) | tout passe par `varlinkctl` | non : un appel en flux occupe la connexion | second choix |
| JSON-RPC ou lignes JSON sur socket brut | via `nc -U`, selon la variante installée | oui | possible, découpage maison |
| MessagePack-RPC | inutilisable : tout arrive en texte | oui | réservé aux clients compilés |
| WebSocket sur socket Unix | pas d'upgrade dans `fetch` | oui | n'apporte rien sans navigateur |
| gRPC en flux | non (HTTP/2) | oui | écarté |
| D-Bus | via `busctl` | oui | écarté : bus partagé, contraire au socket privé |

Mesure du 2026-11-04 (serveurs de test mono-thread) : 5 000 événements SSE d'environ
750 octets reçus par `curl -N` en 17 ms, contre 66 ms par `varlinkctl`. Pour une
requête ponctuelle, c'est le lancement du processus client qui domine, pas le
protocole.

## À trancher

- **Vérifier en session interactive `/clear` et plusieurs observateurs sur la même
  clé.** Le rechargement à chaud et le rafraîchissement y ont été vérifiés (Claude Code
  2.1.319) ; `/clear` seulement en session sans interface (`claude -p`).

## Hors périmètre pour l'instant

- **Les tableaux de bord dans un navigateur.** Pas un besoin aujourd'hui, gardé en tête :
  ce qui compte, c'est que Claude Code ne sera pas le seul client. Une page web ne peut
  pas joindre un socket Unix ; sans port TCP, deux voies existeraient : une extension de
  navigateur avec native messaging, ou un tableau de bord natif qui ouvre le socket.

## Sources

- https://learn.chatgpt.com/docs/hooks
- https://docs.factory.com/reference/hooks-reference.md
- https://varlink.org/Method-Call
- https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
- https://github.com/cloudevents/spec/blob/main/cloudevents/formats/json-format.md
- https://lwn.net/Articles/992991 (`PIDFD_GET_INFO`)
