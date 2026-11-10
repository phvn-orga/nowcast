# La brique forge

État au 2026-11-10 : conception refaite, implémentation à reprendre sous #52. Brique
native (`01-briques.md`), qui remplace le premier jet décrit dans `00-cadrage.md`.

La brique est organisée par usages : une PR, des pipelines, des issues. Chaque
forge (GitHub et Azure DevOps d'abord, GitLab ensuite) est un provider compilé dans
la brique, derrière une seule interface. Le contrat publié ne nomme aucune forge :
un client lit une PR, des pipelines ou des issues de la même façon partout, et ce
qui diverge d'une forge à l'autre se règle dans son provider.

Une seule lecture sert toutes les sessions abonnées à la même cible. La brique ne
lit que les API publiques des forges, par polling. Aucune forge ne pousse vers une
machine sans adresse publique, et le push demande des droits d'administration (voir
« Plus tard : le push »). Le polling est donc réglé pour coûter peu et réagir vite :
rapide quand quelque chose bouge, lent sinon.

## Ce qu'elle publie

| Sorte | Clé | Flux | Publie |
|---|---|---|---|
| `pr` | `{"remote": "<URL du remote>", "branch": "<branche>"}` | `forge/pr/<dépôt>#<branche>` | `forge.pr/v1` |
| `pipelines` | `{"remote": "<URL du remote>", "branch": "<branche>"}` | `forge/pipelines/<dépôt>#<branche>` | `forge.pipelines/v1` |
| `issues` | `{"remote": "<URL du remote>"}` | `forge/issues/<suivi>` | `forge.issues/v1` |
| `issue` | `{"remote": "<URL du remote>", "number": 52}` | `forge/issue/<suivi>#52` | `forge.issue/v1` |

- **`<dépôt>` est l'identité normalisée du dépôt**, quelle que soit l'écriture du
  remote (SSH, HTTPS, alias SSH). Le provider qui reconnaît le remote la donne :
  `github.com/<propriétaire>/<dépôt>`, `dev.azure.com/<organisation>/<projet>/<dépôt>`.
  Deux écritures d'un même dépôt donnent le même flux.
- **`<suivi>` est l'identité de ce qui porte les issues du dépôt**, donnée aussi par
  le provider : le dépôt lui-même sur GitHub (`github.com/<propriétaire>/<dépôt>`), le
  projet sur Azure DevOps (`dev.azure.com/<organisation>/<projet>`), dont les work
  items appartiennent au projet. Deux dépôts d'un même projet Azure DevOps partagent
  donc un flux d'issues.
- **`branch` est le nom de la branche sur la forge.** La casse est gardée.
- **Un remote qu'aucun provider ne reconnaît est refusé** au `subscribe`, par
  l'erreur `unsupported_remote` (`422` `subscription_refused` pour le client). C'est
  le seul refus : la brique ne contacte pas la forge pour accepter un abonnement. Un
  dépôt inaccessible ou une branche absente se lisent dans le détail (`source`).
- Un identifiant de flux qui dépasserait 256 octets est refusé (`key_too_long`).

`pr` et `pipelines` ont la même clé et se lisent ensemble (voir « Le contrat d'un
provider ») : un
client s'abonne à l'un, à l'autre, ou aux deux, sans lecture de plus.

## `forge.pr/v1`

```json
{
  "repo": "github.com/phvn-orga/nowcast",
  "branch": "design-forge-brick",
  "head_sha": "36a89dd…",
  "pr": {
    "number": 111, "state": "open", "draft": false, "base": "design",
    "head_sha": "36a89dd…", "url": "https://github.com/phvn-orga/nowcast/pull/111",
    "auto_merge": { "enabled_by": "hvn-p", "method": "squash" },
    "queue": null,
    "dequeued": null
  },
  "ci": { "state": "running", "required_state": "running", "sha": "36a89dd…" },
  "source": { "status": "ok", "as_of": "2026-11-09T06:39:02Z" }
}
```

- **`head_sha`** est la tête de la branche sur la forge, `null` si la branche n'y
  existe pas (pas encore poussée). Un consommateur la compare à son HEAD local
  pour savoir si ce qu'il voit porte sur ses derniers commits.
- **`pr`** est la PR de la branche, ouverte de préférence, sinon la plus récente ;
  `null` sans PR. `state` vaut `open`, `merged` ou `closed`.
- **`ci` est la synthèse de la CI**, la même que celle de `forge.pipelines/v1` pour
  la même branche : `state` et `required_state` (voir « La synthèse »), et `sha`, le
  commit qu'elle couvre. `null` quand il n'y a pas de commit à lire. Le détail par
  pipeline et par job n'est que dans `forge.pipelines/v1`.
- **`auto_merge`** dit que la PR se mergera seule une fois ses conditions remplies :
  qui l'a demandé (`enabled_by`) et la stratégie (`method` : `merge`, `squash`,
  `rebase`, ou la valeur de la forge). `null` sinon.
- **`queue`** dit que la PR est dans la file de merge de sa base, `null` sinon, et
  toujours `null` sur une forge sans file :

  ```json
  {
    "state": "awaiting_checks", "position": 2,
    "enqueued_at": "2026-11-09T07:40:00Z", "estimated_merge_at": "2026-11-09T07:52:00Z",
    "sha": "9c1e7f0…",
    "ci": { "state": "running", "required_state": "running", "sha": "9c1e7f0…" }
  }
  ```

  `state` vaut `queued`, `awaiting_checks`, `mergeable`, `unmergeable` ou `locked`.
  `sha` est le commit que la file teste (la PR fusionnée avec sa base et les PR qui
  la précèdent) ; `ci` en est la synthèse. Une PR en file reste `open` tant qu'elle
  n'est pas mergée.
- **`dequeued`** dit pourquoi la PR est sortie de la file sans être mergée :
  `{"reason": "…", "at": "…"}`, la raison telle que la forge la donne (CI en échec,
  conflit, retrait à la main). Il disparaît quand la PR rentre dans la file ou
  change de tête.

## `forge.pipelines/v1`

```json
{
  "repo": "github.com/phvn-orga/nowcast",
  "branch": "design-forge-brick",
  "sha": "36a89dd…",
  "pr": 111,
  "state": "running",
  "required_state": "running",
  "missing_required": ["check-agents-md"],
  "jobs": { "done": 7, "total": 9 },
  "pipelines": [
    {
      "id": "42574314636", "name": "CI", "definition": ".github/workflows/ci.yml",
      "trigger": "pull_request", "raw_trigger": "pull_request", "attempt": 1,
      "sha": "36a89dd…", "ref": "refs/pull/111/merge",
      "state": "running", "warning": false, "required": null,
      "started_at": "2026-11-09T06:38:47Z", "finished_at": null,
      "url": "https://github.com/phvn-orga/nowcast/actions/runs/42574314636",
      "jobs": [
        { "id": "129018527491", "name": "check", "stage": null,
          "state": "running", "warning": false, "required": true,
          "started_at": "2026-11-09T06:38:57Z", "finished_at": null, "url": "…" }
      ]
    }
  ],
  "source": { "status": "ok", "as_of": "2026-11-09T06:39:02Z" }
}
```

**Les pipelines qui ont testé la tête actuelle de la branche**, ceux de sa PR
compris. `sha` est cette tête : celle de la PR ouverte quand il y en a une, sinon
celle de la branche ; `null` sans commit à lire, et la liste est alors vide. `pr` est
le numéro de la PR ouverte dont la CI est lue, `null` sans PR ouverte.

**L'unité est le pipeline, pas le contrôle** : une exécution d'une définition de CI
(un workflow run, un build). Plusieurs tournent sur un même commit (plusieurs
définitions, déclenchement par le push et par la PR, relance à la main). Une liste
plate de contrôles les confond : sur un commit de nowcast, 5 suites et 14 check runs,
dont `changes` quatre fois. Elle cache aussi un pipeline sans job, comme un run qui
attend une approbation.

- **Un pipeline par définition et par déclencheur**, le plus récent. Seule sa
  dernière tentative est publiée (`attempt`).
- **`sha` et `ref` disent ce que le pipeline a testé**, qui n'est pas toujours la
  tête : un pipeline de PR teste souvent le commit de merge (`ref` de la PR). Un
  provider qui ne connaît pas ce commit donne la tête.
- **`trigger`** : `push`, `pull_request`, `merge_queue`, `manual`, `schedule`,
  `downstream`, `external` ou `other`. La valeur de la forge reste dans `raw_trigger`.
- **Les statuts posés par des outils extérieurs** forment un pipeline de `trigger`
  `external`, un job par contexte.
- **Les jobs** d'un pipeline, avec `stage` quand la forge regroupe ses jobs en
  étapes, `null` sinon. Pas les tâches d'un job : une v1 s'arrête aux jobs.
- **`required`** vaut `true` ou `false` quand la forge dit ce qui est obligatoire
  pour la PR, `null` sinon, et toujours `null` sans PR ouverte : l'obligation vient
  des règles de la base. Il vit au niveau où la forge le pose, un job ou un pipeline.

### Les éléments obligatoires absents

Un élément obligatoire peut ne jamais apparaître : une définition filtrée par chemin
ne crée aucun pipeline, alors que la PR attend le contrôle. Un job sauté par sa
condition, lui, est publié `skipped` et satisfait l'exigence.

`missing_required` nomme les obligatoires qui manquent sur le commit. Tant qu'une CI
est attendue ou tourne (voir « Le contrat d'un provider »), un manque est normal : un pipeline peut
encore arriver. Une fois tout terminé, un manque est un blocage (`missing`, plus
bas).

### Les états

`queued`, `waiting`, `running`, `pass`, `fail`, `cancelled`, `skipped`, `unknown`.
`waiting` dit qu'une personne doit agir (approbation, job manuel). `warning` à `true`
marque un succès avec avertissements. Chaque provider ramène les états de sa forge à
ceux-là (voir « Les providers »).

### La synthèse

`state` résume les pipelines, `required_state` les seuls éléments obligatoires
(`null` quand rien ne dit ce qui l'est) :

1. `none` sans aucun pipeline, ni obligatoire absent, ni CI attendue ;
2. sinon `fail` si un élément échoue ou a été annulé ;
3. sinon `running` si un élément tourne ou attend son tour, ou si une CI est
   attendue ;
4. sinon `missing` s'il manque un obligatoire (`required_state` seulement) ;
5. sinon `waiting` si un élément attend une personne ;
6. sinon `pass` si un élément passe, `none` si tous ont été sautés.

Un élément sauté ou d'état inconnu ne compte pas. `jobs` compte les jobs terminés et
le total, pour une progression.

## `forge.issues/v1` et `forge.issue/v1`

```json
{
  "tracker": "github.com/phvn-orga/nowcast",
  "account": "hvn-p",
  "mine": [ { "number": 52, "title": "Design the forge brick…", "type": null, "url": "…" } ],
  "others": [ { "number": 51, "title": "Design the git brick…", "type": null, "url": "…" } ],
  "truncated": false,
  "source": { "status": "ok", "as_of": "…" }
}
```

Une issue est un élément de suivi de la forge : une issue sur GitHub, un work item
sur Azure DevOps.

- `tracker` est le `<suivi>` de la clé ; `mine`, les issues ouvertes assignées au
  compte qui lit (`account`) ; `others`, les autres. Au plus 200 issues, les plus
  récemment créées, pour voir ce qui vient d'être ouvert ; `truncated` dit qu'il y en a d'autres.
- `number` est le numéro que la forge affiche ; `type`, le type de l'élément tel que
  la forge le nomme (`Bug`, `User Story`), `null` quand elle n'en donne pas. Les work
  items ont toujours un type, les issues d'un dépôt d'organisation GitHub peuvent en
  avoir un aussi.
- **Ouverte ou fermée** : le provider décide à partir de ce que sa forge sait, y
  compris quand ses états sont personnalisés.

`forge.issue/v1` vaut `{"tracker", "number", "title", "type", "state", "url",
"source"}`, où `state` vaut `open` ou `closed`. Pour un numéro qui n'existe pas, les
champs valent `null` et `source.status` vaut `not_found`.

## `source`

L'état de la lecture, le même pour toutes les sortes : `pending` (pas encore lue),
`ok`, `no_credentials` (pas de jeton), `unauthorized` (jeton refusé), `not_found`
(dépôt inexistant ou invisible au compte), `rate_limited`, `unreachable` (réseau),
`unreadable` (réponse dans un format inattendu). `as_of` date la dernière lecture
réussie. Une erreur garde la dernière valeur lue : le flux reste servi, ce n'est pas
un `flow-error`.

## Le cœur et les providers

### Le cœur : un registre

Le cœur ne connaît aucune forge ni aucun rythme. Il tient :

- **le registre des providers**, compilés dans la brique. Au `subscribe`, il demande
  à chacun s'il reconnaît le remote ; celui qui le reconnaît prend l'abonnement et
  donne l'identité du dépôt ou du suivi, d'où l'identifiant de flux ;
- **les flux** : un par identifiant, quel que soit le nombre d'abonnés. Il transmet
  au provider ce qui les concerne (un flux ouvert, un `release`, un `refresh`) ;
- **la publication** : il ne publie un flux que si son détail a changé, sauf après
  un `refresh`, toujours suivi d'une publication.

### Le contrat d'un provider

Le contrat est celui de la sortie : pour chaque sorte, le provider produit le détail
de son type (`forge.pr/v1`, `forge.pipelines/v1`, `forge.issues/v1`,
`forge.issue/v1`), avec les états neutres et les valeurs de `source.status`. Il
s'engage aussi sur un comportement :

- **un `refresh` déclenche une lecture immédiate**, ou dès que la limite de la
  forge le permet ; des `refresh` rapprochés ne font qu'une lecture ;
- **une CI qui tourne se suit de près, une tête neuve se voit vite**, dans les
  limites de ce que la forge accorde. Une attente humaine (une approbation) ne
  compte pas comme une CI qui tourne ;
- **une CI est attendue** quand le provider estime que des pipelines vont apparaître
  sur une tête neuve ; la synthèse et `missing_required` en tiennent compte ;
- **la synthèse de `pr` et le détail de `pipelines`** d'une même branche viennent
  d'une même lecture et s'accordent ;
- **une lecture en échec garde la dernière valeur** et met à jour `source` ;
- **rien n'est lu sans flux** qui en dépend ;
- **rien n'est écrit**, ni sur la forge, ni dans la configuration de ses outils.

Le reste lui appartient : le protocole (REST, GraphQL, autre), le moment de lire, le
regroupement des lectures entre flux, le cache, la gestion de ses quotas.

### Des outils, pas un tronc commun

La brique offre quelques outils, qu'un provider prend s'ils lui vont :

- un client HTTP avec requêtes conditionnelles (`ETag`, `If-None-Match`) et la
  lecture des en-têtes de limite ;
- le calcul de la synthèse à partir de pipelines dans le modèle neutre (règles de
  « La synthèse ») ;
- une machine à rythme, que les providers paramètrent de leurs durées. Ses états,
  le premier qui s'applique l'emportant :

  | État | Quand | Jusqu'à |
  |---|---|---|
  | **CI attendue** | une tête neuve (push, PR ouverte) sans aucun pipeline encore | un pipeline apparaît, ou 2 min sans rien : pas de CI pour cet événement |
  | **CI en cours** | un pipeline ou un job `queued` ou `running`, PR ou file de merge | plus rien ne tourne |
  | **fenêtre rapide** | un `refresh` n'a rien montré de neuf | 30 s |
  | **normal** | | |

  `git push` ne rend la main qu'une fois la branche mise à jour sur la forge : la
  lecture qui suit le `refresh` voit la tête neuve et passe en « CI attendue », qui
  couvre le délai entre le push et la création des pipelines. Si l'API n'a pas
  encore la tête, la fenêtre rapide la rattrape. Une lecture en échec repasse au
  rythme normal, ou attend ce que la limite demande. Le rythme normal voit ce que la
  machine ne voit pas : un push depuis un terminal ou par quelqu'un d'autre, une
  relecture, un merge, une mise en file.

Rien n'oblige un provider à les prendre : un outil qui ne colle pas à une forge est
contourné, pas plié pour elle.

### Les écarts, réglés chez le provider

| Ce qui diverge | GitHub | Azure DevOps | Dans le contrat |
|---|---|---|---|
| Ce qui porte les issues | le dépôt | le projet (work items) | `<suivi>` |
| États d'une issue | ouvert, fermé | personnalisés, rangés en catégories | `open`, `closed` |
| File de merge | oui | non | `queue` toujours `null` sans file |
| Étapes d'un pipeline | non | oui | `stage` `null` sans étape |
| Niveau de l'obligation | un nom de contrôle, donc un job | une politique de build, donc un pipeline | `required` au niveau donné |
| Commit testé par un pipeline de PR | non donné par l'API REST | donné | `sha` : la tête quand il manque |
| Statuts extérieurs | statuts de commit | statuts de PR | pipeline `external` |
| Coût d'une lecture | gratuite quand rien ne change (`304`) | toujours comptée (TSTU) | rythme propre au provider |

Un provider à venir ajoute sa colonne, pas un champ : une notion qu'une seule forge
connaît prend une valeur neutre ailleurs, ou n'entre pas dans le contrat.

## Les providers

### GitHub

**Identité.** Les remotes `github.com` en HTTPS, `ssh://` ou scp ; un alias SSH est
résolu par `ssh -G <alias>` (son `hostname`). Le dépôt et le suivi valent
`github.com/<propriétaire>/<dépôt>`, en minuscules.

**Jeton.** `gh auth token --hostname github.com --user <propriétaire>`, sinon le
compte actif de `gh`. Le propriétaire d'un dépôt d'organisation n'est pas un compte :
c'est alors le compte actif qui lit. Le jeton est redemandé après un `401`. La brique
n'écrit jamais dans la configuration de `gh` ; sans lui ou sans compte,
`no_credentials`.

**Des requêtes conditionnelles.** Le provider renvoie l'`ETag` de chaque réponse
(`If-None-Match`) ; une réponse `304` ne compte pas dans la limite de 5 000 requêtes
par heure (vérifié le 2026-11-09 sur tous les endpoints ci-dessous). Les paramètres
d'une requête restent identiques d'un appel à l'autre, sinon l'`ETag` ne sert plus.

| Ce qui est lu | Endpoint | Partagé |
|---|---|---|
| Les PR ouvertes | `pulls?state=open&per_page=100` | par dépôt, toutes branches |
| La PR fermée d'une branche, quand elle quitte la liste | `pulls?head=<propriétaire>:<branche>&state=closed` | non, à la demande |
| La tête d'une branche sans PR ouverte | `branches/<branche>` | non |
| Les pipelines d'un commit | `actions/runs?head_sha=<sha>&per_page=100` | par commit |
| Leurs jobs | `commits/<sha>/check-runs?per_page=100`, reliés aux pipelines par `check_suite.id` | par commit |
| Les statuts de commit | `commits/<sha>/status` | par commit |
| Les rulesets d'une base | `rules/branches/<base>` | par base |
| Les issues ouvertes | `issues?state=open&sort=created&per_page=100`, deux pages au plus | par dépôt |
| Une issue | `issues/<numéro>` | non |

Le reste passe par GraphQL, seul à le donner :

| Ce qui est lu | Requête | Partagé |
|---|---|---|
| Les contrôles obligatoires de la protection de branche | `ref(qualifiedName:) { refUpdateRule { requiredStatusCheckContexts } }` | par base |
| La file de merge d'une base, `null` sans file | `mergeQueue(branch:) { entries { position state enqueuedAt estimatedTimeToMerge headCommit { oid } pullRequest { number } } }` | par base |
| La raison d'une sortie de file | `pullRequest(number:) { timelineItems(last: 1, itemTypes: REMOVED_FROM_MERGE_QUEUE_EVENT) { … reason } }` | non, à la demande |

- **GraphQL n'a pas d'`ETag`** : chaque requête coûte au moins un point, sur un
  budget de 5 000 par heure distinct de celui de REST. Le provider n'en fait que pour
  ce que REST ne donne pas, et les regroupe par base.
- **Obligatoire** : un job l'est quand son nom figure dans la réunion des contrôles
  de la protection de branche (« viewable by non-admins », vérifié le 2026-11-09) et
  des règles `required_status_checks` des rulesets de la base. Lu à l'abonnement,
  puis au rythme normal.
- **Une file se lit par base** : une requête rend toutes ses entrées. Le provider
  demande à l'abonnement si la base d'une PR a une file, et ne la relit ensuite que
  pour une base qui en a une : au rythme normal, et toutes les 10 s tant qu'une PR
  suivie y est (environ 360 points par heure et par base, sur 5 000). La CI de la
  file se lit par REST, comme celle d'un commit, sur le `sha` de l'entrée, à partir
  de ses pipelines `merge_queue` (branche temporaire `gh-readonly-queue/<base>/…`).
  `queue.state` reprend celui de GitHub en minuscules.
- **Le commit d'un pipeline de PR** : l'API REST ne donne pas le commit de merge
  qu'un run `pull_request` a testé ; `sha` reste la tête de la PR, `ref` vaut
  `refs/pull/<n>/merge`.
- **Un commit sans statut** répond `state: pending` avec `total_count: 0` sur
  `commits/<sha>/status` : c'est l'absence de statut, pas un statut en attente.
- **Une check run sans workflow run** (une app qui n'est pas GitHub Actions) forme
  un pipeline au nom de son app.
- **Issues** : les PR, que l'API des issues mêle aux issues, en sont retirées. `type`
  est le type d'issue de l'organisation, `null` sans type.

| État neutre | GitHub (`status`, `conclusion` ; statut de commit) |
|---|---|
| `queued` | `queued`, `requested`, `pending` |
| `waiting` | `waiting` ; `action_required` |
| `running` | `in_progress` ; statut `pending` |
| `pass` | `success` |
| `fail` | `failure`, `timed_out`, `startup_failure` ; statut `failure`, `error` |
| `cancelled` | `cancelled` |
| `skipped` | `skipped`, `neutral` |
| `unknown` | `stale`, toute autre valeur |

**Rythme** : la machine à rythme, avec CI attendue 2 s pendant 30 s puis 5 s, CI en
cours 5 s, fenêtre rapide 2 s, normal 60 s. Une lecture qui ne change rien est
gratuite, d'où le rythme rapide ; `gh run watch` interroge toutes les 3 s. Les limites secondaires (débit par
minute) n'ont pas été vérifiées sur des `304` : le provider ralentit sur un `403` ou
un `429` qui les invoque, et respecte `Retry-After`. Une lecture partagée (les PR ouvertes d'un dépôt) suit
la branche la plus rapide qui en dépend. Les issues se lisent au rythme normal.

### Azure DevOps

**Identité.** Les remotes `dev.azure.com` en HTTPS et en SSH
(`git@ssh.dev.azure.com:v3/<organisation>/<projet>/<dépôt>`) ; les anciennes
adresses `*.visualstudio.com` et `vs-ssh.visualstudio.com` y sont ramenées. Le dépôt
vaut `dev.azure.com/<organisation>/<projet>/<dépôt>`, le suivi
`dev.azure.com/<organisation>/<projet>`, en minuscules : Azure DevOps ne distingue
pas la casse des noms.

**Jeton.** `az account get-access-token --resource
499b84ac-1321-427f-aa17-267ca6975798`, l'identifiant d'application d'Azure DevOps
([documentation](https://learn.microsoft.com/en-us/azure/devops/cli/entra-tokens?view=azure-devops)).
Le jeton expire : le provider en redemande un avant son échéance (`expiresOn`).

**Pas d'`ETag`** (`cache-control: no-store`) : le provider compare le contenu pour
savoir ce qui a changé. Le budget est de 200 TSTU par fenêtre glissante de
5 minutes. `X-RateLimit-Cost` n'accompagne pas toutes les réponses : absent sur la
liste des PR, présent sur les builds, le timeline et WIQL (de 0,015 à 0,03). Les
en-têtes de budget (`X-RateLimit-Remaining`…) n'apparaissent que quand il s'épuise.
Une lecture de branche fait environ 4 appels (mesuré le 2026-11-10 sur un projet
réel).

| Ce qui est lu | Endpoint | Partagé |
|---|---|---|
| Les PR actives | `git/repositories/<dépôt>/pullrequests?searchCriteria.status=active` | par dépôt |
| La PR d'une branche, quand elle quitte la liste | `…/pullrequests?searchCriteria.sourceRefName=refs/heads/<branche>&searchCriteria.status=all` | non |
| La tête d'une branche sans PR active | `git/repositories/<dépôt>/refs?filter=heads/<branche>` | non |
| Ce qui est obligatoire, et le build de chaque politique | `policy/evaluations?artifactId=vstfs:///CodeReview/CodeReviewId/<projet>/<pr>` | non |
| Les pipelines d'une PR hors politique | `build/builds?branchName=refs/pull/<id>/merge`, retenus sur `sourceVersion` égal au `lastMergeCommit` de la PR | non |
| Les pipelines d'une branche | `build/builds?branchName=refs/heads/<branche>`, retenus sur `sourceVersion` | non |
| Leurs étapes et jobs | `build/builds/<id>/timeline?changeId=<dernier>` : seuls les enregistrements changés depuis | par build |
| Les statuts de PR | `git/repositories/<dépôt>/pullRequests/<id>/statuses` | non |
| Les états d'un type de work item, avec leur catégorie | `wit/workitemtypes/<type>/states` | par projet |
| Les work items ouverts | `wit/wiql` (WIQL), puis leurs champs par lot | par projet |
| Un work item | `wit/workitems/<id>` | non |

- **La PR** : `active`, `completed` et `abandoned` donnent `open`, `merged` et
  `closed` ; `isDraft` donne `draft`. La complétion automatique se lit sur
  `autoCompleteSetBy` (qui l'a demandée) et `completionOptions.mergeStrategy`.
- **La CI d'une PR est celle de ses politiques de build et de statut.** Une
  politique de build (type `0609b952-1397-4640-95ec-e00a01b2c241`, vérifié le
  2026-11-10) désigne un pipeline (`settings.buildDefinitionId`), et
  `configuration.isBlocking` dit s'il est obligatoire. Le build retenu est celui
  que l'évaluation désigne (`context.buildId`), celui qu'Azure DevOps montre sur la
  PR. Une politique sans build publie un pipeline sans job, à l'état de la
  politique : `queued`, ou `waiting` pour une politique à déclenchement manuel. Une
  politique dont le filtre de chemins ne correspond pas est `notApplicable`, donc
  `skipped`. Les autres politiques (relecteurs, commentaires, stratégie de merge)
  disent si la PR peut être mergée, pas si sa CI passe.
- **Une politique expirée attend une personne.** Un build réussi cesse de valoir
  après sa durée de validité (`validDuration`) : l'évaluation repasse `queued` avec
  `context.isExpired: true`, et rien ne tourne tant que personne ne relance le build.
  Elle est publiée `waiting`, pas `queued` : sinon la synthèse dirait `running` et le
  rythme resterait sur « CI en cours » indéfiniment.
- **Ce qu'un pipeline de PR a testé** : `sourceVersion` est le commit de merge de la
  PR (`lastMergeCommit`), `ref` vaut `refs/pull/<id>/merge`.
- **`trigger`**, d'après `reason` : `individualCI` et `batchedCI` donnent `push`,
  `pullRequest` `pull_request`, `manual` `manual`, `schedule` `schedule`,
  `resourceTrigger` `downstream`, toute autre valeur `other`.
- **Du timeline, l'unité de job est l'enregistrement `Phase`**, groupé sous son
  `Stage`. Un job en attente ou sauté n'existe que comme `Phase` ; un `Job` interne
  sans `Phase` (« Finalize build ») est ignoré ; les tâches ne sont pas gardées. Le
  timeline d'un build terminé ne se relit que si son statut change : une relance de
  jobs ou d'étape garde l'identifiant du build.
- **Les statuts de PR s'accumulent** : une entrée par envoi et par itération. Le
  provider garde le plus récent de chaque contexte (`context.genre`/`context.name`),
  sur la dernière itération. Un statut qu'un build de la PR pose lui-même
  (`codecoverage`) ne doit pas reformer un pipeline `external`.
- **Work items.** WIQL ne filtre pas sur les catégories d'état :
  `[System.StateCategory]` n'existe pas (`TF51005`), et `[System.State] NOT IN GROUP
  'Completed'` est accepté mais ne filtre rien, `IN GROUP` ne visant que les
  catégories de types (vérifié le 2026-11-10). Le provider lit donc les états de
  chaque type, dont chacun a sa catégorie fixe
  ([documentation](https://learn.microsoft.com/en-us/azure/devops/boards/work-items/workflow-and-state-categories?view=azure-devops)),
  et construit `[System.State] NOT IN (…)` par type, en excluant les états
  `Completed` et `Removed`. Il écarte les types qui ne sont pas des tickets (Test
  Case, Test Plan, Test Suite, Shared Steps, Code Review, Feedback) par `NOT IN
  GROUP 'Microsoft.HiddenCategory'` et les catégories de test : sur un projet réel,
  1 212 éléments ouverts, 547 une fois ceux-là écartés. `[System.AssignedTo] = @Me`
  sépare `mine` des autres, le tri se fait sur `System.CreatedDate`, et la requête
  ne rend que des identifiants
  ([documentation](https://learn.microsoft.com/en-us/azure/devops/boards/queries/wiql-syntax)).
  `type` est `System.WorkItemType`.
- Sur un en-tête `Retry-After` ou un budget presque épuisé (`X-RateLimit-Remaining`),
  le provider ralentit et publie `rate_limited`.

| État neutre | Azure DevOps (build, enregistrement du timeline, politique, statut de PR) |
|---|---|
| `queued` | `notStarted`, `postponed`, `pending` ; politique `queued` non expirée ; `notSet` |
| `waiting` | approbation en attente sur une étape ; politique `queued` expirée (`context.isExpired`) |
| `running` | `inProgress`, `cancelling` ; politique `running` ; statut `pending` |
| `pass` | `succeeded` ; `partiallySucceeded` et `succeededWithIssues` avec `warning` ; politique `approved` |
| `fail` | `failed` ; politique `rejected`, `broken` ; statut `failed`, `error` |
| `cancelled` | `canceled` |
| `skipped` | `skipped` ; politique et statut `notApplicable` |
| `unknown` | toute autre valeur |

**Rythme** : la machine à rythme, avec CI attendue 5 s pendant 30 s puis 10 s, CI en
cours 10 s, fenêtre rapide 5 s, normal 120 s. Sur une PR, « CI attendue » n'a pas à
être deviné : une évaluation de politique de build `queued`, non expirée, dit qu'un
build va venir, et la fenêtre de 2 minutes ne sert que pour une branche sans PR.
Chaque lecture est comptée, et le provider ralentit quand le budget restant baisse.
Les work items se lisent au rythme normal.

## La détection côté client

Le service ne connaît aucun agent (`00-cadrage.md`) : reconnaître une action d'agent
qui peut changer une PR ou une issue revient au client. **Le mod central de Claude
Code s'en charge** (`02-clients.md`). Après un appel d'outil qui a abouti, il demande
le rafraîchissement de tous les flux forge d'une sorte (`POST
/v1/bricks/forge/refresh`, `03-contrats.md`) :

- `git push`, `gh pr …`, `az repos pr …`, et les outils de PR des serveurs MCP
  Azure DevOps : les sortes `pr` et `pipelines` ;
- la relance d'un pipeline (`gh run rerun`, `az pipelines …`, les outils de
  pipelines des serveurs MCP Azure DevOps), qui suit une politique expirée : les
  sortes `pr` et `pipelines` ;
- `gh issue …` qui écrit, `gh pr merge` (une PR peut fermer une issue),
  `az boards work-item …` qui écrit, et les outils de work items des serveurs MCP
  Azure DevOps : les sortes `issues` et `issue`.

**Tous les flux, pas ceux de la session.** Une session sur un dépôt peut agir sur un
autre (`gh issue create -R`, `git -C`), que suit une autre session. Le mod n'a pas à
reconnaître le dépôt visé : la brique relit toutes les cibles de la sorte.

Sans brique forge activée, le service répond `unknown_brick` et le mod n'insiste
pas ; sans flux, rien ne part. Un push depuis un terminal n'est pas vu : le rythme
normal le rattrape.

## Ce qui existe déjà

Aucun outil ne couvre à la fois PR, CI par job et issues sur GitHub et Azure DevOps
(recherche du 2026-11-09). Tous les outils côté client pollent.

| Outil | Ce qu'il apporte | Ce qui manque |
|---|---|---|
| Pipedash (Rust, GPL-3.0) | Un greffon par CI, Azure DevOps compris | Ni PR ni issues, pas de liste de jobs |
| git-spice (Go, GPL-3.0) | L'état des PR sur six forges | CI d'Azure DevOps non implémentée ; paquet interne |
| drone/go-scm (Go) | Une interface commune à de nombreuses forges | Statuts et issues non supportés sur Azure DevOps |
| Renovate (TypeScript, AGPL-3.0) | Dix plateformes, un état agrégé de branche | Pas une bibliothèque ; pas de jobs |
| Conventions OpenTelemetry CI/CD et VCS | Un vocabulaire d'états de pipeline et de changement | Un format de traces, pas un état publié |
| `azure_devops_rust_api` | Le client Rust généré d'Azure DevOps | Archivé par Microsoft (2026-08) |

Chaque provider écrit donc ses quelques appels REST lui-même. `gh run watch`
interroge toutes les 3 s, Gitify toutes les 60 s.

## Plus tard : le push

Écarté pour l'instant, gardé ouvert. Une forge ne pousse que par un webhook vers une
adresse publique :

- **Un relais sur un serveur** recevrait les webhooks, les garderait, et la brique
  s'y connecterait en sortie pour rejouer ce qu'elle a manqué. Un relais de passage
  (smee) perd ce qui arrive en l'absence de la machine, et une livraison échouée
  n'est pas relivrée par GitHub.
- **Il faut des droits d'administration.** Sur GitHub, une GitHub App installée par
  le propriétaire d'un compte ou d'une organisation couvre tous ses dépôts. Sur
  Azure DevOps, un abonnement de service hook demande la permission *Edit
  subscriptions*, aux administrateurs du projet par défaut.
- **Écartés** : `gh webhook forward`, réservé au développement, un par dépôt, qui
  s'arrête à chaque coupure (cli/gh-webhook#43, ouvert au 2026-11-09) ; le canal
  SignalR de l'interface d'Azure DevOps, qui pousse les jobs d'un build sans droits
  d'administration mais n'est pas documenté.

Le jour venu, un événement poussé ne serait qu'un signal de relire plus tôt, comme un
`refresh` : la lecture reste celle des API, et rien de ce que publie la brique ne
change.

## À trancher

- **GitLab** : un provider de plus, avec des pipelines enfants et des étapes. Ses
  `304` comptent dans sa limite de requêtes. Non instruit ici.
- **D'autres cibles pour `pipelines`** : un commit donné, ou une définition sur une
  branche. La v1 ne lit que la tête d'une branche.
- **Le détail de la CI d'une file de merge** : la v1 n'en publie que la synthèse,
  dans `queue.ci`.
- **Le volume des work items** : un gros projet Azure DevOps a bien plus de 200
  éléments ouverts (547 tickets sur un projet réel), et `others` n'en montre alors qu'une partie. Filtrer (par zone,
  par itération) n'est pas décidé.

## À vérifier à l'implémentation

- Que `gh auth token` lit son trousseau depuis le service lancé par systemd.
- Sur Azure DevOps, le type d'une approbation en attente dans le timeline, et
  l'état d'une politique de build à déclenchement manuel : aucun cas réel observé.
- Comment reconnaître un statut de PR qu'un build de la PR pose lui-même, pour ne
  pas le publier deux fois.
- La file de merge sur un dépôt qui en a une : la forme des entrées, la branche
  temporaire de leurs pipelines, et la raison d'une sortie de file
  (`RemovedFromMergeQueueEvent.reason`). nowcast n'a pas de file ; seul le schéma
  GraphQL a été lu.
- Que `refUpdateRule` rend bien les contrôles obligatoires à un compte sans droits
  d'administration (vérifié seulement avec un compte administrateur).

## Sources

- https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api
- https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
- https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/using-the-github-cli-to-forward-webhooks-for-testing
- https://docs.github.com/en/apps/using-github-apps/installing-a-github-app-from-a-third-party
- https://github.com/cli/gh-webhook/issues/43
- https://github.com/cli/cli/blob/trunk/pkg/cmd/run/watch/watch.go
- https://learn.microsoft.com/en-us/azure/devops/integrate/concepts/rate-limits?view=azure-devops
- https://learn.microsoft.com/en-us/azure/devops/service-hooks/overview?view=azure-devops
- https://learn.microsoft.com/en-us/rest/api/azure/devops/build/builds/list?view=azure-devops-rest-7.1
- https://learn.microsoft.com/en-us/rest/api/azure/devops/build/timeline/get?view=azure-devops-rest-7.1
- https://learn.microsoft.com/en-us/rest/api/azure/devops/policy/evaluations/list?view=azure-devops-rest-7.1
- https://learn.microsoft.com/en-us/azure/devops/cli/entra-tokens?view=azure-devops
- https://learn.microsoft.com/en-us/azure/devops/boards/queries/wiql-syntax
- https://learn.microsoft.com/en-us/azure/devops/boards/work-items/workflow-and-state-categories?view=azure-devops
- https://opentelemetry.io/docs/specs/semconv/registry/attributes/cicd/
- https://github.com/hcavarsan/pipedash, https://github.com/abhinav/git-spice, https://github.com/drone/go-scm
