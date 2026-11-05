# Les clients

État au 2026-11-05 : architecture des clients et format du flux posés, détails du
protocole de session à fixer.

## Décisions prises

- **Le détail arrive en JSON**, tel que la brique l'a publié et que le service l'a
  vérifié (voir `01-briques.md`). Il est placé tel quel dans le champ `data` de
  l'événement, sans conversion ni base64.
- **Des clients minces : l'état vit dans le service.** Le service garde, pour chaque
  session, ses abonnements et l'identifiant de session en cours, et pour chaque flux sa
  dernière valeur. Une coupure du flux ne fait rien perdre. Un client ne fait que
  quatre choses : déclarer sa session, s'abonner, se désabonner, lire son flux. Ce qui
  n'est pas dans le client n'a pas à être porté d'un agent à l'autre.
- **Un flux par session, des requêtes en appels HTTP.** La session tient un seul flux
  ouvert, qui regroupe tous ses abonnements. S'abonner, se désabonner, demander un
  rafraîchissement, lire la dernière valeur d'un flux ou annoncer un changement de
  session sont des appels HTTP ordinaires sur le même socket. Le service rattache
  appels et flux à la même session par leur processus d'ancrage commun (voir
  `00-cadrage.md`) : ni jeton ni identifiant de connexion à échanger.
- **Le flux est en SSE** (Server-Sent Events) sur HTTP/1.1, sur le socket Unix du
  service : une réponse HTTP qui reste ouverte. Chaque événement suit le format JSON
  de CloudEvents, l'enveloppe en attributs, le détail dans `data`. Les raisons et les
  candidats écartés sont plus bas.
- **Une requête ponctuelle « dernière valeur d'un flux »**, pour les clients qui
  lisent à un instant donné plutôt qu'en continu.
- **Un outil en ligne de commande fourni avec pulse, commun à tous les agents.** Par
  exemple `pulse watch` (tient le flux, se reconnecte), `pulse get <flux>` (dernière
  valeur, en un appel), `pulse subscribe`. La reconnexion et la conduite à tenir quand
  le service est absent sont écrites une fois, là.
- **Un adaptateur mince par agent, dans le dépôt de pulse** (par exemple
  `clients/claude-code/`, plus tard `clients/codex/`). Il traduit les événements de
  l'agent (début de session, changement de session, fin) en requêtes au service, et
  s'appuie sur l'outil commun pour le reste. Un agent qui n'a que des hooks ponctuels
  s'en sert par `pulse get`.
- **Pour Claude Code, l'adaptateur est le mod central de la session.** Un plugin lance
  `pulse watch`, traduit les événements de la session (début, `/clear`, fin) et
  redistribue les flux aux autres mods de la session par l'état des plugins
  (`$.state`). Les autres mods ne se connectent pas eux-mêmes : une seule connexion et
  un seul émetteur des changements de session par session.
- **Aucun mécanisme interne non documenté d'un agent** (variable d'environnement,
  fichier de session) n'entre dans le modèle : seules les API publiques des agents
  comptent.

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
    émis.
  - Le contexte, le cache, le modèle et l'effort d'une session n'existent que dans le
    moteur, qui les donne aux plugins par ses événements : le service ne peut pas les
    lire lui-même, le client les lui transmet.
  - Tout plugin lit l'état (`$.state`) de n'importe quel autre ; seul le propriétaire
    l'écrit, et une écriture est un événement qu'on peut intercepter. Le mod central
    peut donc publier les flux dans son état, et un mod écrire dans le sien les flux
    qu'il veut, que le mod central intercepte. Déduit des types, pas testé.
- **Codex et Droid.** Leurs hooks sont des commandes ponctuelles, lancées sur des
  événements de session ; ils lisent à un instant donné, pas en continu. Les hooks
  descendent du processus de l'agent, donc l'identité du noyau les rattache à leur
  session.
- **Tableau de bord dans un navigateur.** Une page web ne peut pas joindre un socket
  Unix.

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

- **La déclaration de session** : forme de la requête, ce qui se passe entre un
  `/clear` et la déclaration suivante, sort des abonnements d'une session qui se
  termine alors que son processus continue.
- **L'échange par `$.state`** entre le mod central et les autres mods : à tester.
- **L'outil en ligne de commande** : son nom, sa distribution (binaire à part ou
  sous-commande du service).
- **Les tableaux de bord dans un navigateur.** Sans port TCP, exclu par décision, deux
  voies : une extension de navigateur avec native messaging (le navigateur lance un
  programme local et lui parle par l'entrée et la sortie standard), ou un tableau de
  bord natif qui ouvre le socket directement.
- **Le nom.** Le répertoire `$XDG_RUNTIME_DIR/pulse/` est déjà pris par PipeWire-Pulse
  (le socket de compatibilité PulseAudio). Il faut un autre nom de répertoire, et plus
  largement se demander si le nom « pulse » tient sous Linux.

## Sources

- https://learn.chatgpt.com/docs/hooks
- https://docs.factory.com/reference/hooks-reference.md
- https://varlink.org/Method-Call
- https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
- https://github.com/cloudevents/spec/blob/main/cloudevents/formats/json-format.md
- https://lwn.net/Articles/992991 (`PIDFD_GET_INFO`)
