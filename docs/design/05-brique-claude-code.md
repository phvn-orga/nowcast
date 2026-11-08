# La brique claude-code

État au 2026-11-07 : implémentation dans #62. Première brique native du dépôt, construite
sur les briques natives et l'entrée d'un flux (`01-briques.md`, `03-contrats.md`).

Elle publie ce qui appartient au compte Claude Code et à ses sessions. Chaque
consommateur lit une seule copie, tenue à jour, au lieu que chaque session lise la
sienne. Aujourd'hui, le quota d'une session ne bouge que lorsqu'elle envoie une
requête, alors que d'autres sessions, l'application de bureau ou claude.ai consomment
le même.

## Ce qu'elle publie

| Sorte | Clé | Flux | Publie | Accepte en entrée |
|---|---|---|---|---|
| `quotas` | aucune | `claude-code/quotas` | `claude-code.quotas/v1` | rien |
| `session` | `{"id": "<identifiant de session>"}` | `claude-code/session/<id>` | `claude-code.session/v1` | `claude-code.measure/v1` |
| `sessions` | `{}` | `claude-code/sessions` | `claude-code.sessions/v1` | rien |
| `sessions` | `{"id": "<identifiant de session>"}` | `claude-code/sessions/<id>` | `claude-code.session/v1` | rien |

- **`session` est le flux d'émission d'une session**, qui ne prend qu'un abonné :
  le premier. Elle y envoie ses mesures et y relit aussitôt ce que la brique en
  retient. Le `release` de ce flux dit à la brique que la session s'est arrêtée :
  désabonnement, `/clear`, ou mort de son processus.
- **Un second abonné est refusé** : tant que le flux vit, la brique répond au
  `subscribe` par l'erreur `taken`, que le client reçoit en `422`
  `subscription_refused`. Le service n'envoyant `release` qu'au départ du dernier
  abonné, un second abonné cacherait la fin de la session, et pourrait envoyer des
  mesures en son nom. Si un autre processus prend le flux avant la session, c'est
  elle qui est refusée et ses mesures ne partent pas.
- **`sessions` est la lecture**, de toutes les sessions ou d'une seule, par
  n'importe quel client : une autre session, un tableau de bord.

## Quotas

### Deux sources

- **L'API d'usage du compte.** `GET /api/oauth/usage`, l'adresse que lit `/usage`
  dans Claude Code (vu dans la version 2.1.319), avec le jeton OAuth que Claude Code
  garde dans `~/.claude/.credentials.json`. La réponse porte `five_hour` et
  `seven_day`, chacun avec `utilization` et `resets_at`, et des fenêtres
  hebdomadaires par modèle. Elle couvre tout ce que consomme le compte : claude.ai,
  l'application de bureau, les autres machines.
- **Les mesures des sessions.** L'événement `session.measure` de l'API plugin
  (2.1.319) porte les fenêtres que la dernière réponse de l'API a annoncées
  (`five_hour`, `seven_day`, avec `percentUsed` et `resetsAt`). Chaque session les
  envoie avec ses mesures (plus bas). Cette source est fraîche à chaque tour et ne
  dépend pas du jeton, mais elle ne voit pas les fenêtres par modèle.

S'y ajoute la copie que Claude Code garde de la réponse de l'API d'usage, dans
`~/.claude.json` (`cachedUsageUtilization`). Il ne l'écrit que quand il lit l'API
lui-même (sur `/usage`), mais elle est datée (`fetchedAtMs`) : la brique la prend
quand elle est plus récente que sa propre dernière lecture. Elle ne la relit que
tant que l'API d'usage n'est pas `ok` (avant la première lecture, jeton expiré,
attente après un `429`), et seulement quand le fichier a changé : il porte tous les
réglages de Claude Code.

Pour chaque fenêtre, la brique publie la lecture la plus récente de ces sources, avec
son instant et sa source. Elle publie quand un pourcentage, une échéance, une source
ou l'état de l'API change ; un `as_of` plus récent seul attend la publication
suivante, pour qu'un tour de session ne republie pas les quotas à chaque fois.

### Interrogation de l'API d'usage

- Seulement tant que le flux `quotas` a un abonné.
- Environ toutes les 60 s, et jamais deux fois en moins de 15 s. Claude Code
  s'attend à des réponses `429` sur cette adresse : sur un `429`, la brique respecte
  `Retry-After` quand il est présent, sinon double son intervalle jusqu'à 10 min, et
  revient à 60 s après une réponse réussie.
- Un `refresh` du flux `quotas` déclenche une lecture dès que ce rythme le permet.
  Des demandes rapprochées n'en font qu'une.
- Une API injoignable (réseau) laisse l'état tel quel : ce n'est pas un verdict sur
  la source, la lecture suivante vient à son tour.
- Les sessions n'ont pas à demander de rafraîchissement en fin de tour : leurs
  mesures portent déjà les fenêtres que leur dernière réponse a fait bouger.

### Le jeton

- La brique relit le fichier de Claude Code à chaque interrogation, puisque Claude
  Code le réécrit quand il renouvelle le jeton. Elle ne le renouvelle jamais
  elle-même : ce serait écrire dans le fichier d'un autre programme.
- Jeton expiré (date d'expiration passée, ou réponse `401`) : la brique n'interroge
  plus l'API, relit le fichier à chaque échéance et dès qu'il change (inotify), et
  reprend avec le jeton renouvelé. Les
  fenêtres continuent de vivre par les mesures des sessions. Comme Claude Code
  renouvelle son jeton quand il travaille, un jeton expiré signifie en pratique
  qu'aucune session Claude Code n'a tourné récemment sur la machine.
- L'état de cette source est publié dans le détail, à côté des fenêtres (plus bas).
  Ce n'est pas un `flow-error` : le flux reste servi, une de ses sources est à l'arrêt.

### `claude-code.quotas/v1`

```json
{
  "windows": [
    { "name": "five_hour", "percent": 23.5, "resets_at": "2026-11-07T18:00:00Z",
      "as_of": "2026-11-07T15:31:12Z", "source": "session" },
    { "name": "seven_day", "percent": 61, "resets_at": "2026-11-10T08:00:00Z",
      "as_of": "2026-11-07T15:30:40Z", "source": "usage-api" }
  ],
  "usage_api": { "status": "ok", "as_of": "2026-11-07T15:30:40Z" }
}
```

- `windows` : une entrée par fenêtre connue, sous le nom que lui donne sa source.
  `percent` va de 0 à 100. `source` vaut `usage-api` (lecture de la brique ou copie
  de Claude Code) ou `session`.
- `usage_api.status` : `pending` (pas encore lue), `ok`, `token_expired`,
  `no_credentials` (pas de fichier),
  `rate_limited` (en attente après un `429`) ou `unreadable` (réponse ou fichier dans
  un format inattendu). `as_of` date la dernière lecture réussie.

## Mesures des sessions

### Ce qu'une session envoie

Le mod central de Claude Code (`02-clients.md`), dans chaque session :

1. à sa première mesure, s'abonne à `session` avec l'identifiant de la session :
   une session qui ne fait aucun tour n'envoie aucune requête ;
2. envoie une entrée `claude-code.measure/v1` sur ce flux
   (`POST /v1/flows/{flow}/input`, `03-contrats.md`) après chaque requête au modèle
   et à chaque `session.measure`, en regroupant les envois rapprochés ;
3. après un `/clear` ou une reprise, se désabonne du flux de l'ancien identifiant et
   s'abonne à celui du nouveau, à sa première mesure.

Si la brique n'est pas activée, l'abonnement répond `404` `unknown_brick` : le mod
n'envoie rien pour cette session.

### `claude-code.measure/v1` (entrée)

```json
{
  "root": "/chemin/du/projet",
  "model": "claude-opus-5-5",
  "effort": "high",
  "context": { "tokens": 152340, "window": 200000, "percent": 76 },
  "tokens": { "input": 41210, "output": 88400, "cache_creation": 310500, "cache_read": 9120400 },
  "last_request_at": "2026-11-07T15:31:10Z",
  "cache_ttl": "1h",
  "rate_limits": [
    { "name": "five_hour", "percent": 23.5, "resets_at": "2026-11-07T18:00:00Z" }
  ],
  "cost_usd": 4.12
}
```

| Champ | Ce que c'est | Source dans l'API plugin (2.1.319) |
|---|---|---|
| `root` | Répertoire de la session | `$.session.root()` |
| `model` | Modèle du fil principal | `$.session.model()` |
| `effort` | Effort de la dernière requête du fil principal | `turn.step`, champ `effort` ; facultatif |
| `context` | Remplissage de la fenêtre de contexte | `session.measure`, champ `context` |
| `tokens` | Tokens consommés depuis le début de la session, par catégorie | somme des `usage` de `turn.step`, `model.fork`, `model.complete` et `session.compact` |
| `last_request_at` | Instant de la dernière requête qui a lu ou écrit le cache du fil principal | `turn.step`, `model.fork` (plus bas) |
| `cache_ttl` | Durée de vie du cache du fil principal, `5m` ou `1h` ; facultatif | transcription (plus bas) |
| `rate_limits` | Fenêtres de quota de la dernière réponse ; vide hors abonnement | `session.measure`, champ `rateLimits` |
| `cost_usd` | Coût de la session ; facultatif | `session.measure`, champ `cost.usd` |

`context` et `tokens` ne mesurent pas la même chose. `context` est ce que la prochaine
requête du fil principal renverra au modèle. `tokens` est tout ce que la session a
consommé avec ses identifiants, lectures du cache comprises, celles-ci à part. Le mod
prend l'`usage` de chaque requête (`input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`) là où l'API plugin le rend :

- `turn.step`, pour chaque requête du fil principal et de ses sous-agents (`agentId`
  distingue un sous-agent) ;
- `model.fork` et `model.complete`, pour les requêtes qu'un mod envoie lui-même. Le
  hook du mod voit celles des autres mods ;
- `session.compact`, pour la requête du résumé quand le moteur compacte lui-même.

Le mod tient la somme et envoie des totaux, pas des écarts : une entrée perdue ne
fausse pas la suivante. Deux dépenses restent hors du compte, faute d'`usage` dans
l'API plugin : `$.model.classify`, dont le résultat n'est qu'une étiquette, et les
autres requêtes annexes du moteur s'il en fait (titre de session par exemple).

`last_request_at` suit le vrai cache du fil principal : une requête le déplace si sa
réponse a lu ou écrit un cache, et si elle commence comme la dernière requête du fil
principal. Une requête qui n'a pas reçu de réponse n'a rien rafraîchi. Le fil
principal remplit la seconde condition. Un fork la remplit aussi quand il tourne à
l'effort du fil principal (aucun nommé, ou le même) : il relit son entrée de cache, ou
la recrée si elle a expiré. Le keep-warm d'un mod est ce cas. Un fork à un autre effort
peut avoir écrit une entrée à lui sans toucher celle du fil principal, quand le modèle
prend l'effort pour toute la requête et non par message. Ni son résultat ni son `usage`
ne disent lequel des deux s'est produit : il laisse l'heure telle quelle, quitte à
montrer une échéance plus proche que la vraie. Un fork termine parfois après un tour
qui a commencé après lui : l'heure ne fait qu'avancer.

**Au chargement du mod**, une session peut avoir déjà tourné : une reprise, ou un
rechargement du plugin en cours de session. Les requêtes d'avant n'ont déclenché aucun
`turn.step` chez lui. Le mod part alors des transcriptions de la session et de ses
sous-agents (`~/.claude/projects/*/<id>.jsonl` et `<id>/subagents/*.jsonl`) : il y
somme l'`usage` de chaque réponse, une fois par identifiant de message, et prend la
date de modification de la transcription pour `last_request_at`. Il ajoute ensuite
chaque requête qu'il voit. Une transcription illisible ou dans un format inattendu le fait
compter depuis son chargement, sans erreur. Après un `/clear`, la nouvelle session a
une transcription vide et repart de zéro.

**Après chaque tour**, le mod relit la fin de la transcription pour `cache_ttl`.
L'`usage` brut de chaque réponse y répartit les écritures en cache par durée
(`cache_creation.ephemeral_1h_input_tokens` et `ephemeral_5m_input_tokens`) : la
dernière réponse du fil principal qui a écrit en cache donne la durée. Une réponse
qui n'a rien écrit ne change pas la durée connue. L'`usage` de `turn.step` ne donne
que le total de ces écritures, et `cache_ttl` n'apparaît dans l'API plugin qu'à un
changement de modèle.

Ce sont les seules lectures d'un mécanisme interne de Claude Code, faute d'API
publique qui donne ces valeurs (voir `02-clients.md`). Tout le reste vient des API publiques du
moteur. `~/.claude/sessions/*.json` reste écarté, et le contexte vient du moteur, pas
d'un calcul sur la transcription.

### `claude-code.session/v1` et `claude-code.sessions/v1`

Une session, telle que la brique la retient :

```json
{
  "session": "4f1c…",
  "updated_at": "2026-11-07T15:31:12Z",
  "ended_at": null,
  "root": "/chemin/du/projet",
  "model": "claude-opus-5-5",
  "effort": "high",
  "context": { "tokens": 152340, "window": 200000, "percent": 76 },
  "tokens": { "input": 41210, "output": 88400, "cache_creation": 310500, "cache_read": 9120400 },
  "last_request_at": "2026-11-07T15:31:10Z",
  "cache_ttl": "1h",
  "cache_expires_at": "2026-11-07T16:31:10Z",
  "cost_usd": 4.12
}
```

`cache_expires_at` est l'échéance du cache du fil principal : `last_request_at` plus
`cache_ttl`, publié lui aussi. Sans durée connue (transcription illisible, aucune
écriture en cache encore), la brique suppose une heure, celle d'un abonnement Claude,
et publie `cache_ttl` à `null`.

`claude-code.sessions/v1` vaut `{"sessions": [ … ]}`, une entrée par session vivante.

- Une session apparaît à sa première mesure. Avant, `claude-code/sessions/<id>` ne
  publie rien.
- Au `release` de son flux d'émission, elle quitte la liste, et
  `claude-code/sessions/<id>` publie une dernière fois son entrée avec `ended_at`.
- Un consommateur trie et filtre lui-même, par exemple pour lister les sessions au
  contexte le plus rempli.

## Hors des sources documentées

`/api/oauth/usage` et le fichier du jeton ne sont pas documentés : leur format peut
changer sans préavis. La règle de `02-clients.md` sur les mécanismes internes
s'applique au modèle du cœur et aux clients ; une brique choisit ses sources
(`01-briques.md`). Elle le fait ici en connaissance de cause, et un changement de
format se voit dans `usage_api.status` (`unreadable`) au lieu de faire tomber le flux.

## Limite assumée

**Un seul compte par machine.** La brique suppose que le fichier de jeton et toutes
les sessions qui lui envoient des mesures appartiennent au même compte. Une session
connectée à un autre compte mélangerait ses fenêtres à celles du compte du fichier :
l'API plugin (2.1.319) n'expose pas le compte d'une session, et rien dans
`session.measure` ne l'identifie.
