# Les contrats

État au 2026-11-05 : contrats décidés ; restent les valeurs de quelques délais.

Ce document fixe les contrats dont dépend le développement en parallèle : le fichier
de déclaration d'une brique, la découverte des briques, l'API des clients, le
protocole des briques et la déclaration d'un type de client. Il applique les décisions de `00-cadrage.md`,
`01-briques.md` et `02-clients.md` sans les rediscuter.

## 1. Le fichier de déclaration d'une brique

Un fichier TOML par brique, nommé `<nom>.toml`, qui vit dans le dépôt de la brique.

| Champ | Type | Rôle |
|---|---|---|
| `manifest_version` | entier | Version du format de ce fichier. Vaut `1`. |
| `name` | chaîne, `[a-z0-9-]+` | Nom unique de la brique. Sert dans l'API (`"brick": "forge"`) et dans `source`. |
| `version` | chaîne semver | Version de la brique elle-même. |
| `description` | chaîne | Une phrase. |
| `protocol` | entier | Version majeure du protocole brique qu'elle parle (section 4). |
| `[run] command` | tableau de chaînes | Commande à lancer. Un chemin relatif se résout depuis le répertoire réel du fichier de déclaration ; un chemin absolu est pris tel quel. |
| `[run] env` | table, facultatif | Variables d'environnement ajoutées au lancement. |
| `[subscriptions.<sorte>]` | table, une par sorte d'abonnement | `description` ; `schema`, chemin du JSON Schema du contenu de l'abonnement (c'est la documentation de la clé) ; `publishes`, le type de donnée publié par ce flux. |
| `[types."<type>"]` | table, une par type publié | `schema`, chemin du JSON Schema du détail. Il devient le `dataschema` des événements. |

Chaque `publishes` doit nommer un type déclaré dans `[types]`, sinon le fichier est
refusé.

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
- Les chemins relatifs du fichier (`command`, `schema`) se résolvent depuis son
  emplacement réel, lien résolu. La brique reste donc dans son dépôt ou son répertoire
  d'installation.
- Le service lit le répertoire à son démarrage, puis suit ses changements (inotify).
  Une brique retirée alors qu'elle tourne s'arrête comme à la perte de son dernier
  abonné, et ses flux reçoivent `flow-ended`.
- Un fichier invalide est ignoré et journalisé. Deux fichiers qui déclarent le même
  `name` sont refusés tous les deux, et journalisés : aucun ne gagne en silence.

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
- `flow-error` : la brique ne peut pas servir un flux, avec un message ;
- `flow-ended` : le flux n'existe plus (brique retirée, abonnement supprimé).

Le champ SSE `id` reprend l'`id` de l'événement. À la reconnexion, le service lit
`Last-Event-ID` et renvoie la dernière valeur de chaque flux plus récente que cet
identifiant : la dernière valeur suffit, il n'y a pas d'historique à rejouer.

```
event: update
id: 42
data: {"specversion":"1.0","id":"42","source":"nowcast://bricks/forge","type":"forge.pr/v1","subject":"forge/github.com/org/repo#main","time":"2026-11-05T13:30:00Z","dataschema":"nowcast://bricks/forge/types/forge.pr/v1","data":{"number":123,"state":"open","draft":false,"checks":[{"name":"build","state":"pass"}]}}
```

Attributs de l'événement :

| Attribut | Valeur |
|---|---|
| `specversion` | `"1.0"` |
| `id` | Attribué par le service, croissant pour un flux donné. |
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
   la souscription échoue (`503` côté client).
2. **Arrêt** quand plus aucun flux n'a d'abonné, après un délai de grâce
   de 30 s qui évite de relancer une brique à chaque abonnement
   éphémère. Le service envoie `shutdown`, puis `SIGTERM`, puis `SIGKILL`, chacun après
   un délai.
3. **Plantage** : chaque flux de la brique reçoit `flow-error`. Tant qu'il reste des
   abonnés, le service relance la brique avec une attente croissante entre les
   tentatives, puis renvoie un `subscribe` pour chaque flux vivant.

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

La déclaration embarquée pour Claude Code :

```toml
name = "claude-code"
description = "Sessions Claude Code"

[anchor]
exe = ["*/claude/versions/*"]
```

Règle de rattachement : à la connexion, le service remonte les parents du processus
qui se connecte et retient **l'ancêtre le plus proche** dont l'exécutable correspond à
un motif d'un type déclaré, quel que soit ce type. Ce processus devient l'ancrage de la
connexion. Si aucun ancêtre ne correspond, la connexion est refusée.

## À trancher

- Les délais entre `shutdown`, `SIGTERM` et `SIGKILL` à l'arrêt d'une brique.
- Les paramètres de l'attente croissante entre deux relances d'une brique plantée
  (première attente, plafond, abandon éventuel).
