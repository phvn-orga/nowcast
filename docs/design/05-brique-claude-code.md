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

1. à son entrée dans une conversation (démarrage, `/clear`, reprise, `02-clients.md`),
   s'abonne à `session` avec l'identifiant de la conversation, et y envoie aussitôt
   ses premières mesures : une nouvelle conversation montre sa fenêtre de contexte
   vide au lieu du contexte de la précédente, une conversation reprise le sien dès
   l'entrée ;
2. envoie une entrée `claude-code.measure/v1` sur ce flux
   (`POST /v1/flows/{flow}/input`, `03-contrats.md`) après chaque requête au modèle
   et à chaque `session.measure`, en regroupant les envois rapprochés ;
3. à l'entrée dans une autre conversation, se désabonne du flux de l'ancien
   identifiant et s'abonne à celui du nouveau.

Un rechargement du mod n'est pas une entrée : il n'envoie rien avant la mesure
suivante.

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
  "side_tokens": { "input": 120, "output": 40, "cache_creation": 2100, "cache_read": 610000 },
  "side_since": "2026-11-07T14:02:51Z",
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
| `context` | Remplissage de la fenêtre de contexte | `$.session.usage()` à l'entrée, puis `session.measure`, champ `context` |
| `tokens` | Tokens que le mod compte, par catégorie : les transcriptions, plus ce qu'il a vu depuis son chargement | transcriptions, puis somme des `usage` de `turn.step`, `model.fork`, `model.complete` et `session.compact` |
| `side_tokens` | La part de `tokens` que les transcriptions ne gardent pas, comptée depuis le chargement du mod | somme des `usage` de `model.fork`, `model.complete` et `session.compact` |
| `side_since` | Instant du chargement du mod, qui date ce comptage | horloge du mod |
| `last_request_at` | Instant de la dernière requête qui a lu ou écrit le cache du fil principal | transcription au chargement, puis `turn.step`, `model.fork` (plus bas) |
| `cache_ttl` | Durée de vie du cache du fil principal, `5m` ou `1h` ; facultatif | transcription (plus bas) |
| `rate_limits` | Fenêtres de quota de la dernière réponse ; vide hors abonnement | `session.measure`, champ `rateLimits` |
| `cost_usd` | Coût de la session ; facultatif | `session.measure`, champ `cost.usd` |
| `worktrees` | Worktrees où la session a travaillé, le plus récent d'abord, celui de son répertoire marqué ; facultatif | `tool.call` (« Les worktrees de la session ») |

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
somme l'`usage` de chaque réponse, une fois par identifiant de message. Dans celle du
fil principal, la dernière réponse qui a lu ou écrit le cache donne
`last_request_at` (sa date d'écriture, à peine après l'envoi de la requête), et la
dernière qui l'a écrit `cache_ttl`. Il ajoute ensuite chaque requête qu'il voit, et
une requête vue depuis l'emporte sur la transcription. Une transcription illisible ou dans un format inattendu le fait
compter depuis son chargement, sans erreur. Après un `/clear`, la nouvelle session a
une transcription vide et repart de zéro.

Les forks, les completions et les résumés de compaction ne sont pas écrits dans les
transcriptions : un mod rechargé ne les retrouve pas. Il les compte donc aussi à part,
dans `side_tokens`, depuis son chargement, que `side_since` date. La brique fait le
reste. Elle garde sur disque la somme des comptages des chargements précédents
(plus bas). Quand une mesure arrive avec un `side_since` plus récent que celui qu'elle
connaît, elle y ajoute le dernier `side_tokens` du chargement précédent. Elle publie
`tokens` et `side_tokens` augmentés de cette somme : le total ne recule ni après un
rechargement du mod, ni après une reprise, ni après un redémarrage de la machine. Une
mesure dont le `side_since` est plus ancien que celui que la brique connaît vient d'un
chargement déjà remplacé : ses tokens sont ignorés.

Le mod n'a ainsi rien à relire. La réponse à un abonnement ne lui rendrait pas la
valeur gardée : le service la construit quand la brique confirme l'abonnement, avant
la publication qui suit.

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
  "side_tokens": { "input": 360, "output": 95, "cache_creation": 2100, "cache_read": 1840000 },
  "last_request_at": "2026-11-07T15:31:10Z",
  "cache_ttl": "1h",
  "cache_expires_at": "2026-11-07T16:31:10Z",
  "cost_usd": 4.12,
  "worktrees": []
}
```

`worktrees` est recopié tel quel de la dernière mesure, liste vide quand elle n'en
porte pas.

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

### Ce que la brique garde sur disque

La brique garde chaque session dans
`$XDG_STATE_HOME/nowcast/bricks/claude-code/sessions/<id>.json`
(`~/.local/state/…` par défaut) : son entrée, la somme des comptages `side_tokens` des
chargements précédents, et le dernier comptage reçu avec son `side_since`. Le fichier
est réécrit à chaque mesure par un fichier temporaire renommé : un arrêt brutal laisse
l'ancienne version ou la nouvelle, jamais un fichier coupé. C'est le seul état de
nowcast qui survit à un arrêt du service. Il existe parce que rien d'autre ne garde ce
que les transcriptions n'ont pas.

- Au démarrage, la brique relit ces fichiers et supprime ceux dont `updated_at` a plus
  de 30 jours, la durée pendant laquelle Claude Code garde ses transcriptions par
  défaut. Un fichier illisible est supprimé et journalisé.
- Une session relue du disque ne devient vivante qu'à sa mesure suivante : elle entre
  alors dans `claude-code.sessions/v1` avec son entrée gardée, `ended_at` remis à
  `null`. Avant, ni son flux ni `claude-code/sessions/<id>` ne publient rien.
- La fin d'une session (`release` de son flux) garde le fichier : une reprise le
  retrouve.
- Sans `XDG_STATE_HOME` absolu ni `HOME`, ou si l'écriture échoue, la brique
  journalise et continue en mémoire seulement.

## Les worktrees de la session

Une session ancrée dans un worktree, souvent le principal sur `main`, peut écrire
dans d'autres par leur chemin sans jamais s'y déplacer. Un consommateur qui suit le
répertoire de la session, comme une bande qui affiche la PR et la CI de sa branche,
montre alors la mauvaise. Le mod central relève donc les worktrees où sa session
travaille, marque celui où se trouve son répertoire, et suit leurs flux forge
(`02-clients.md`) ; le consommateur choisit lesquels montrer.

### Ce qui compte

- **Une écriture.** Un appel `Edit` ou `Write` qui a abouti (son résultat n'a pas
  `isError`), du fil principal comme d'un sous-agent : son `file_path` désigne le
  worktree. Une lecture par `Read` ne compte pas : une session qui lit `main` pour
  comparer n'y travaille pas.
- **Une commande Bash qui a tourné**, réussie ou non : une commande en échec
  (tests rouges, build cassé) est du travail là où elle a tourné. Chaque
  répertoire où elle s'exécute désigne un worktree : celui où elle part (pour un
  sous-agent, le sien), et ceux où un `cd` ou un `git -C` mène un de ses
  programmes. Une commande refusée n'a pas tourné et ne compte pas.
- **Le répertoire de la session.** Le worktree qui le contient est l'entrée
  active, relevée dès que le répertoire y entre, comme un travail du fil
  principal, qu'il y ait écrit ou non : une session qui entre dans un worktree
  (EnterWorktree) pour seulement y pousser ou y ouvrir une PR l'y trouve. Le mod
  relit ce répertoire à l'entrée dans une conversation (`02-clients.md`), après
  EnterWorktree, ExitWorktree et les appels Bash du fil principal, et au début et à
  la fin de chacun de ses tours : une session montre son worktree avant son premier
  tour. Il ne s'appuie pas
  sur `CwdChanged` : sur un compte d'organisation, le plugin intégré
  `cc-plugin-sec-default` peut faire passer les événements classiques avant les
  plugins installés. Rester dans le même worktree ne compte qu'une fois.
- Un fichier ou un répertoire hors de tout dépôt git ne compte pas.

### Du fichier au worktree

Pour un répertoire qu'il n'a pas encore vu, le mod demande à git la racine du
worktree et le répertoire git commun (`git rev-parse --show-toplevel
--git-common-dir`), et garde la réponse. À chaque travail relevé, il relit la branche,
l'URL du remote `origin` et la clé forge de ce worktree : un `git checkout` lancé
dans un terminal est ainsi pris au travail suivant.

Pour ne pas lancer jusqu'à sept commandes git à chaque travail, il garde ces réponses
avec une empreinte des fichiers dont elles dépendent, le SHA-256 de leurs contenus
(un fichier absent se distingue d'un fichier vide). Une date
et une taille laisseraient passer deux écritures de même taille dans la même
milliseconde ; le contenu ne laisse rien passer. Les fichiers :

- le `HEAD` du worktree, dans son répertoire git propre (`.bare/worktrees/<nom>/`
  pour un worktree lié), que donne `git rev-parse --git-dir` ;
- sa `reftable/tables.list` : au format de refs `reftable`, le fichier `HEAD`
  reste `ref: refs/heads/.invalid` quelle que soit la branche ;
- les fichiers de config que git lit : ceux qu'il cite (`git config --list
  --show-origin`, `includeIf` compris), la config du dépôt et celle du worktree, la
  config globale et `/etc/gitconfig`, et les cibles de `include.path` et
  `includeIf.<condition>.path`, même absents ;
- le fichier `.git` de la racine, qui désigne le répertoire git : un worktree
  recréé au même endroit avec un autre répertoire git est relu.

Tant que l'empreinte ne change pas, il ne redemande que `@{push}` : sa réponse
dépend aussi de la branche de suivi distante, qu'un `git push` ou un `git fetch`
crée ou supprime sans toucher ces fichiers. Un `git checkout` ou un `git config`
lancé dans un terminal change l'empreinte, et reste pris au travail suivant.

Il relit aussi les entrées après une commande de son agent qui peut déplacer une
branche ou changer où `git push` l'envoie : `git switch`, `git checkout`, `git
branch -m`, `-u` ou `--unset-upstream`, `git push -u`, `gh pr checkout`. Le
répertoire de la commande, `cd` et `git -C` compris, désigne l'entrée, déjà
relevée comme un travail (plus haut). Toutes sont relues, sans changer leur rang
ni leur date, quand ce répertoire reste inconnu, quand un
`git branch` nomme une autre branche que la courante (un autre worktree peut la
porter), et après `git worktree add`, `move`, `remove` ou `prune`. Une commande
qui a échoué compte aussi : elle a pu changer de branche avant d'échouer. Une
relecture fait sortir toute entrée dont le worktree n'existe plus. La brique git
(#51) pourra remplacer ces appels.

### La liste

```json
"worktrees": [
  { "path": "/home/…/nowcast/design-session-worktrees",
    "repo": "/home/…/nowcast/.bare",
    "branch": "design-session-worktrees",
    "remote": "git@github.com:phvn-orga/nowcast.git",
    "written_at": "2026-11-10T08:58:12Z",
    "forge": { "remote": "git@github.com:phvn-orga/nowcast.git",
               "branch": "design-session-worktrees" },
    "writers": ["main"],
    "active": true }
]
```

| Champ | Ce que c'est |
|---|---|
| `path` | Racine du worktree |
| `repo` | Répertoire git commun, le même pour tous les worktrees d'un dépôt |
| `branch` | Branche courante, `null` en HEAD détaché |
| `remote` | URL du remote `origin` telle que git l'a, `null` sans lui. Pas la clé forge : voir `forge` |
| `written_at` | Dernier travail relevé dans ce worktree |
| `active` | `true` pour le worktree qui contient le répertoire de la session, au plus une entrée ; aucune quand ce répertoire n'est dans aucun worktree git, le consommateur ne montre alors ni PR ni CI. `false` sur une entrée écrite par un mod plus ancien |
| `forge` | Clé des flux `pr` et `pipelines` de la brique forge, `{remote, branch}` : la branche où `git push` l'enverrait et l'URL de push de ce remote (`02-clients.md`). `null` en HEAD détaché ou sans remote de push ; absent d'une entrée écrite par un mod plus ancien. Le schéma de `claude-code.measure/v1` le décrit |
| `writers` | Les sortes de boucle qui y ont travaillé (un `Edit` ou un `Write` qui a abouti, une commande Bash ; `main` quand le répertoire de la session y est entré) : `main` (le fil principal), `subagent`, `workflow`. Une boucle est celle d'un workflow quand un `agent.spawn` portant `workflow` a démarré son `agentId`, ou quand une boucle d'un workflow l'a démarrée (`parentAgentId`), à toute profondeur ; tout autre `agentId` est un sous-agent. Absent d'une entrée écrite par un mod plus ancien, et le reste aux écritures suivantes : qui y a écrit avant reste inconnu |

- **Triée par travail**, le plus récent d'abord.
- **Au plus 16 worktrees.** Au-delà, celui dont le dernier travail est le plus
  ancien sort, jamais l'entrée active. Aucun délai : une fenêtre de temps (les worktrees des dix dernières
  minutes, par exemple) est l'affaire du consommateur, qui a les dates.
- **Un worktree dont la racine n'existe plus sort de la liste**, quoi qui l'ait
  supprimé : vérifié à chaque travail relevé, à chaque relecture, et au début et
  à la fin de chaque tour du fil principal. Les tours prennent ce qu'aucune
  commande relevée ne dit : `wt remove`, qui supprime en arrière-plan, un terminal,
  une autre session. Le contrôle ne demande que l'existence de chaque racine, sans
  appel à git.
- **L'entrée active dit où est la session.** Un consommateur montre sa PR et sa
  CI, et les autres entrées comme des worktrees annexes, sans rien suivre
  lui-même.
- **Elle appartient à la session.** Après un `/clear`, la nouvelle session part
  d'une liste vide. Un rechargement du mod la retrouve dans son état.
- **Elle dit qui y a travaillé** (`writers`). Un long workflow crée beaucoup de worktrees
  et suit lui-même leur CI : un consommateur qui réveille la session à la fin d'un
  pipeline peut ignorer un worktree où seuls les agents d'un workflow ont travaillé.

### Où la lire

- **Dans l'état du mod central**, clé `worktrees`, pour un mod de la même session.
  Elle y est tenue que la brique claude-code soit activée ou non.
- **Dans la session publiée par la brique**, champ `worktrees` de
  `claude-code.session/v1`, pour une autre session ou un tableau de bord. Le mod
  l'envoie avec ses mesures ; la brique la recopie sans l'interpréter.
- **Les flux forge d'un worktree, dans les abonnements du mod central.** Brique
  forge activée, il s'abonne aux flux `pr` et `pipelines` de la clé de chaque
  entrée, sous les noms `worktree-pr:<path>` et `worktree-pipelines:<path>`
  (`02-clients.md`). Un mod d'affichage lit l'abonnement
  `nowcast/worktree-pr:<path>` de la famille `subscriptions`, puis la valeur de son
  flux dans `flows`.

### Limites connues

- **Une commande Bash qui ne fait que lire compte.** Un `git -C ../main log` lancé
  pour comparer relève `main` : il faudrait analyser la commande pour savoir si
  elle écrit.
- **Les agents d'un workflow ne sont connus qu'en mémoire.** Après un rechargement
  du mod en plein workflow, le travail de ses agents compte comme `subagent` :
  la session est réveillée comme avant `writers`.
- **Un agent de workflow relancé après un blocage compte comme `subagent`.** Sa
  relance ne lève pas de nouvel `agent.spawn` : son nouvel `agentId` reste inconnu.
- **Des options courtes groupées** (`git push -fu`) ne sont pas vues comme
  déplaçant la branche : l'entrée n'est relue qu'à l'écriture suivante.
- **Un nom de ref local ambigu**, une branche locale nommée comme une branche
  distante (`origin/x`), peut faire retomber la clé sur le nom local au lieu de
  celle que donne `@{push}`.
- **Une clé dont la première demande échoue par une erreur passagère**, puis que
  la brique refuse, reste retentée : le mod central garde l'abonnement, et ne le
  retire que sur un `subscription_refused` reçu à la première demande.

### Plus tard

- **`NotebookEdit` et les outils MCP** qui écrivent un fichier.
- **Une reprise** part d'une liste vide. Les transcriptions portent les `Edit` et
  `Write` passés, avec leur date, et le mod les lit déjà à son chargement pour les
  tokens.

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
