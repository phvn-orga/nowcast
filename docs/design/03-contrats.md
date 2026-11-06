# Les contrats

État au 2026-11-06 : contrats décidés, délais fixés, arrêt du service à vide décrit.

Ce document fixe les contrats dont dépend le développement en parallèle : le fichier
de déclaration d'une brique, la découverte des briques, l'API des clients, le
protocole des briques et la déclaration d'un type de client. Il applique les décisions de `00-cadrage.md`,
`01-briques.md` et `02-clients.md` sans les rediscuter.

## 1. Le fichier de déclaration d'une brique

Un fichier TOML par brique, qui vit dans le dépôt de la brique. Son nom y est libre :
c'est le lien qui l'active qui porte le nom de la brique (section 2).

| Champ | Type | Rôle |
|---|---|---|
| `manifest_version` | entier | Version du format de ce fichier. Vaut `1`. |
| `name` | chaîne, `[a-z0-9-]+` | Nom unique de la brique. Sert dans l'API (`"brick": "forge"`) et dans `source`. |
| `version` | chaîne semver | Version de la brique elle-même. |
| `description` | chaîne | Une phrase. |
| `protocol` | entier | Version majeure du protocole brique qu'elle parle (section 4). |
| `[run] command` | tableau de chaînes | Commande à lancer. Le premier élément est un chemin s'il contient un `/` : relatif, il se résout depuis le répertoire réel du fichier de déclaration ; absolu, il est pris tel quel. Sans `/`, c'est un nom de programme cherché dans le `PATH` du service, comme dans un shell (`python3`). Les arguments passent tels quels. |
| `[run] env` | table, facultatif | Variables d'environnement ajoutées au lancement. |
| `[subscriptions.<sorte>]` | table, une par sorte d'abonnement | `description` ; `schema`, chemin du JSON Schema du contenu de l'abonnement (c'est la documentation de la clé) ; `publishes`, le type de donnée publié par ce flux. |
| `[types."<type>"]` | table, une par type publié | `schema`, chemin du JSON Schema du détail. Il devient le `dataschema` des événements. |

Chaque `publishes` doit nommer un type déclaré dans `[types]`, et chaque `schema` doit
mener à un fichier JSON lisible, sinon le fichier est refusé au chargement.

La brique est lancée avec pour répertoire de travail le répertoire réel de son fichier
de déclaration : `command = ["python3", "brick.py"]` lance le `brick.py` posé à côté du
fichier.

Exemple, une brique forge :

```toml
manifest_version = 1
name = "forge"
version = "0.1.0"
description = "État d'une PR et de sa CI sur GitHub et Azure DevOps."
protocol = 1

[run]
command = ["bin/nowcast-forge"]
env = { RUST_LOG = "info" }

[subscriptions.pr]
description = "La PR d'une branche et ses contrôles de CI."
schema = "schemas/subscription-pr.json"
publishes = "forge.pr/v1"

[types."forge.pr/v1"]
schema = "schemas/forge-pr-v1.json"
```

`schemas/subscription-pr.json` décrit la clé, par exemple :

```json
{
  "type": "object",
  "required": ["remote", "branch"],
  "properties": {
    "remote": { "type": "string", "description": "URL du remote git (SSH, HTTPS ou alias SSH)." },
    "branch": { "type": "string", "description": "Nom de la branche." }
  },
  "additionalProperties": false
}
```

## 2. La découverte des briques

- Le répertoire de déclarations est `$XDG_CONFIG_HOME/nowcast/bricks/`
  (`~/.config/nowcast/bricks/` par défaut).
- Activer une brique, c'est y poser un lien symbolique vers son fichier de
  déclaration, comme on active une unité systemd. La désactiver, c'est retirer le
  lien.
- Le lien s'appelle `<name>.toml`, où `<name>` est le `name` que déclare le fichier.
  Un lien nommé autrement est refusé et journalisé. Deux briques ne peuvent donc pas
  porter le même nom, et le nom d'un lien retiré suffit à savoir quelle brique
  s'arrête.
- Les chemins relatifs du fichier (`command`, `schema`) se résolvent depuis son
  emplacement réel, lien résolu. La brique reste donc dans son dépôt ou son répertoire
  d'installation.
- Le service lit le répertoire à son démarrage, puis suit ses changements (inotify).
  Une brique retirée alors qu'elle tourne s'arrête comme à la perte de son dernier
  abonné, et ses flux reçoivent `flow-ended`.
- Un fichier invalide est ignoré et journalisé.

## 3. L'API des clients

HTTP/1.1 sur le socket Unix du service. Préfixe `/v1`. Corps en JSON. Une erreur a
toujours la forme :

```json
{ "error": { "code": "session_required", "message": "Déclarez la session avant toute autre requête." } }
```

Un identifiant de flux apparaît dans un chemin d'URL encodé en pourcentage (il peut
contenir `/`).

### Session

`PUT /v1/session` avec `{"id": "<identifiant de session>"}` répond `204`.

C'est le premier appel obligatoire pour un processus d'ancrage, répété à chaque
changement de session (`/clear`, reprise). Tout autre appel avant lui répond `428`
avec le code `session_required`.

Le service oublie les sessions à chaque arrêt, l'arrêt à vide compris (section 4). Un
`428` reçu en cours de session signifie donc que le service a été relancé : le client
déclare à nouveau sa session, puis renvoie sa requête.

### Abonnements

`POST /v1/subscriptions` :

```json
{ "brick": "forge", "kind": "pr", "params": { "remote": "git@github.com:org/repo.git", "branch": "main" } }
```

répond `201` :

```json
{ "subscription": "s-7f3a", "flow": "forge/github.com/org/repo#main", "last": null }
```

- Le service valide `params` contre le JSON Schema déclaré pour cette sorte
  d'abonnement : `400` `invalid_params` sinon. `404` `unknown_brick` ou
  `unknown_kind` si la brique ou la sorte n'existe pas.
- `503` `brick_unavailable` si la brique ne répond pas `ready` à temps (section 4).
- `422` `subscription_refused` si la brique répond `error` au `subscribe` (section 4).
  Le `message` reprend le code et le message de la brique : `"no_such_repo: …"`.
- `last` porte la dernière valeur du flux (un événement, voir plus bas), ou `null`.
- L'identifiant d'abonnement est propre à chaque abonné. Deux mods d'une même session
  abonnés au même flux ont deux abonnements : le désabonnement de l'un ne coupe pas
  l'autre. Le flux est commun.

`DELETE /v1/subscriptions/{subscription}` répond `204`.

### Flux

- `POST /v1/flows/{flow}/refresh` répond `202`. Le service relaie la demande à la
  brique.
- `GET /v1/flows/{flow}` répond `200` avec le dernier événement, ou `204` si le flux
  n'a encore rien publié. Seulement pour un flux auquel le processus d'ancrage est
  abonné (`404` `unknown_flow` sinon).

### Le flux d'événements

`GET /v1/stream` avec `Accept: text/event-stream` ouvre le flux SSE de la session : tous
les flux auxquels le processus d'ancrage est abonné, abonnements à venir compris.

Trois sortes d'événements SSE (`event:`) :

- `update` : `data` est un événement CloudEvents en JSON ;
- `flow-error` : la brique ne peut pas servir un flux. `data` vaut
  `{"flow": "<identifiant de flux>", "message": "…"}` ;
- `flow-ended` : le flux n'existe plus (brique retirée, abonnement supprimé). `data`
  vaut `{"flow": "<identifiant de flux>"}`.

Seul `update` porte le champ SSE `id`, qui reprend l'`id` de l'événement. `flow-error`
et `flow-ended` sont des signaux : ils ne sont pas rejoués à la reconnexion.

À l'ouverture, le service envoie d'abord la dernière valeur de chaque flux abonné :
toutes sans `Last-Event-ID`, seulement celles plus récentes que cet identifiant avec
lui. La dernière valeur suffit, il n'y a pas d'historique à rejouer. Un client peut
donc s'abonner avant ou après avoir ouvert le flux sans perdre de valeur ; il reçoit
parfois en double une valeur déjà lue dans `last`, reconnaissable à son `id`.

```
event: update
id: 42
data: {"specversion":"1.0","id":"42","source":"nowcast://bricks/forge","type":"forge.pr/v1","subject":"forge/github.com/org/repo#main","time":"2026-11-05T13:30:00Z","dataschema":"nowcast://bricks/forge/types/forge.pr/v1","data":{"number":123,"state":"open","draft":false,"checks":[{"name":"build","state":"pass"}]}}
```

Attributs de l'événement :

| Attribut | Valeur |
|---|---|
| `specversion` | `"1.0"` |
| `id` | Attribué par le service, à partir d'un seul compteur pour tous les flux : un `Last-Event-ID` se compare donc à tous les flux à la fois. |
| `source` | `nowcast://bricks/<nom>` |
| `type` | Le type publié, par exemple `forge.pr/v1`. |
| `subject` | L'identifiant de flux. |
| `time` | Instant de la publication, RFC 3339. |
| `dataschema` | Le schéma du type, tiré du fichier de déclaration. |
| `data` | Le détail JSON de la brique, injecté tel quel après la vérification de sa syntaxe. |

### Clients à base de hooks

Un agent qui n'a que des hooks ponctuels (Codex, Droid) déclare sa session et s'abonne
dans son hook de début de session. Les abonnements vivent avec le processus d'ancrage,
pas avec le hook. Ses hooks suivants lisent la dernière valeur par
`nowcast get <flux>`.

## 4. Le protocole des briques

Sur l'entrée et la sortie standard de la brique. Chaque trame est une longueur sur
4 octets (u32 petit-boutiste) suivie d'une table MessagePack. La sortie d'erreur est
réservée aux journaux : le service la recueille et la transmet au journal.

Clés communes à tous les messages :

- `v` : version du protocole ;
- `t` : type du message ;
- `req` : identifiant de corrélation (u64) choisi par l'émetteur, présent sur une
  requête et sur sa réponse.

Du service vers la brique :

| `t` | Champs | Réponse attendue |
|---|---|---|
| `hello` | `name`, `protocol` | `ready` |
| `subscribe` | `req`, `kind`, `params` (`bin` : les octets JSON du contenu) | `subscribed` {`req`, `flow`} ou `error` {`req`, `code`, `message`} |
| `release` | `flow` | aucune : le dernier abonné du flux est parti |
| `refresh` | `flow` | aucune : la brique republie quand elle a relu |
| `shutdown` | | aucune : la brique se termine |

De la brique vers le service :

| `t` | Champs | Réponse |
|---|---|---|
| `ready` | | |
| `publish` | `req`, `flow`, `type`, `detail` (`bin` : les octets JSON) | `ack` {`req`} ou `error` {`req`, `code`} |
| `flow-error` | `flow`, `message` | aucune : relayé aux clients en `flow-error` |

Codes d'erreur sur `publish` : `invalid_json` (détail illisible), `unknown_flow` (flux
jamais attribué ou relâché), `undeclared_type` (type absent du fichier de
déclaration).

Exemple d'échange, en notation JSON pour la lisibilité :

```
service → {"v":1,"t":"hello","name":"forge","protocol":1}
brique  → {"v":1,"t":"ready"}
service → {"v":1,"t":"subscribe","req":1,"kind":"pr","params":<bin {"remote":"…","branch":"main"}>}
brique  → {"v":1,"t":"subscribed","req":1,"flow":"github.com/org/repo#main"}
brique  → {"v":1,"t":"publish","req":7,"flow":"github.com/org/repo#main","type":"forge.pr/v1","detail":<bin {…}>}
service → {"v":1,"t":"ack","req":7}
```

### L'identifiant de flux

- Une chaîne UTF-8 opaque pour le service, de 256 octets au plus.
- Le service la préfixe du nom de la brique (`forge/…`) avant de l'exposer aux
  clients : deux briques ne peuvent pas se marcher dessus.
- Elle est **déterministe** : le même contenu d'abonnement donne le même identifiant,
  y compris après un redémarrage de la brique. C'est ce qui permet au service de
  réabonner une brique relancée sans changer les flux vus par les clients.

### Le cycle de vie d'une brique

1. **Lancement** à la première souscription : le service envoie `hello` et attend
   `ready` dans un délai de 5 s. Sans réponse, la brique est arrêtée et
   la souscription échoue (`503` `brick_unavailable` côté client). Une brique prête
   répond ensuite à chaque `subscribe` en 10 s au plus, sinon la souscription échoue
   de la même façon.
2. **Arrêt** quand plus aucun flux n'a d'abonné, après un délai de grâce
   de 30 s qui évite de relancer une brique à chaque abonnement
   éphémère. Le service envoie `shutdown`, puis `SIGTERM` 5 s plus tard si la brique
   tourne encore, puis `SIGKILL` 5 s après. Les signaux visent le groupe de processus
   de la brique, ses enfants compris.
3. **Plantage** : chaque flux de la brique reçoit `flow-error`. Tant qu'il reste des
   abonnés, le service relance la brique après une attente de 1 s, doublée à chaque
   plantage d'affilée jusqu'à 30 s, puis renvoie un `subscribe` pour chaque flux
   vivant. Le compte des plantages d'affilée repart de zéro quand la brique a tenu
   60 s après `ready`. Après 8 relances d'affilée, le service abandonne : le
   plantage suivant termine tous les flux de la brique, chacun avec un `flow-error`.
   Un abonnement ultérieur relance la brique à neuf, avec ses 8 relances.
   Pendant l'attente, une nouvelle souscription échoue en `503` `brick_unavailable`.
4. **Arrêt du service**, quelle qu'en soit la cause (arrêt à vide, `SIGTERM`,
   `SIGINT`) : le service ferme d'abord toutes ses connexions, puis arrête toutes ses
   briques à la fois par la même séquence, `shutdown`, `SIGTERM`, `SIGKILL`, avec les
   mêmes délais, sans `release` préalable. L'unité systemd porte `KillMode=mixed` :
   `systemctl stop` n'envoie `SIGTERM` qu'au service, et les briques reçoivent
   `shutdown` d'abord.

L'arrêt à vide a lieu après 60 s sans connexion ouverte ni abonnement, de n'importe
quel processus d'ancrage. Une session déclarée sans connexion ni abonnement ne compte
pas. Le délai dépasse celui de l'arrêt d'une brique (30 + 5 + 5 s) : les briques se
sont normalement arrêtées d'elles-mêmes avant. Il n'est actif par défaut que sous
activation par socket ; `NOWCAST_IDLE_TIMEOUT` (en secondes, décimales permises) le
remplace et l'active pour un service lancé à la main. Une valeur invalide empêche le
service de démarrer.

## 5. La déclaration d'un type de client

Un type de client (voir `00-cadrage.md`) se déclare hors du code du cœur, comme une
brique. Un fichier TOML par type, `<type>.toml`.

Deux sources, chargées dans cet ordre :

1. Les déclarations fournies par le dépôt, rangées à côté de leur adaptateur
   (`clients/claude-code/claude-code.toml`, par exemple) et compilées dans le binaire.
   Un agent courant est reconnu sans configuration.
2. Les déclarations de l'utilisateur, dans `$XDG_CONFIG_HOME/nowcast/clients/`
   (`~/.config/nowcast/clients/` par défaut). Un fichier qui porte le même nom qu'une
   déclaration embarquée la remplace : c'est ainsi qu'on corrige des motifs devenus
   faux sans attendre une nouvelle version de nowcast.

| Champ | Rôle |
|---|---|
| `name` | Identifiant du type, `[a-z0-9-]+`, égal au nom du fichier. |
| `description` | Une ligne pour un humain. |
| `[anchor] exe` | Liste de motifs (glob) sur le chemin de l'exécutable du processus d'ancrage. Des motifs, parce que plusieurs versions d'un agent tournent en même temps. |

La déclaration embarquée pour Claude Code, un motif par façon de l'installer :

```toml
name = "claude-code"
description = "Sessions Claude Code"

[anchor]
exe = [
  # Installateur natif : un fichier par version.
  "*/claude/versions/*",
  # Paquet npm : le binaire natif que le paquet met en place.
  "*/@anthropic-ai/claude-code/bin/claude.exe",
  # Paquet npm lancé par son repli `cli-wrapper.cjs` : le binaire du paquet de plateforme.
  "*/@anthropic-ai/claude-code-*/claude",
  # Paquets apt, dnf et apk.
  "/usr/bin/claude",
]
```

Ne sont pas couverts : le Claude Code fourni avec l'application de bureau, ou un
lanceur qui copie le binaire ailleurs. Une déclaration utilisateur du même nom la
remplace, avec les motifs manquants en plus.

Règle de rattachement : à la connexion, le service examine le processus qui se
connecte, puis ses parents, et retient **le plus proche** dont l'exécutable correspond
à un motif d'un type déclaré, quel que soit ce type. Un agent qui se connecte depuis
son propre processus est donc son propre ancrage. Ce processus devient l'ancrage de la
connexion. Si aucun ne correspond, l'ancrage est le processus connecté
lui-même : ses abonnements meurent avec lui, et il déclare sa propre session comme
n'importe quel client. C'est le cas de `nowcast watch` lancé depuis un terminal. Un
refus ne protégerait rien, puisque tout se passe sous le même UID.
