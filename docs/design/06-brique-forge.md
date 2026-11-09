# La brique forge

État au 2026-11-09 : conception, implémentation à suivre sous #52. Brique native
(`01-briques.md`), qui remplace le premier jet décrit dans `00-cadrage.md`.

Elle publie, pour une branche, sa PR et sa CI, et pour un dépôt, ses issues
ouvertes, sur GitHub et Azure DevOps. Une seule lecture sert toutes les sessions
abonnées à la même cible.

Elle ne lit que les API publiques des forges, par polling. Aucune forge ne pousse
vers une machine sans adresse publique, et le push demande des droits
d'administration (voir « Plus tard : le push »). Le polling est donc réglé pour
coûter peu et réagir vite : rapide quand quelque chose bouge, lent sinon.

## Ce qu'elle publie

| Sorte | Clé | Flux | Publie |
|---|---|---|---|
| `pr` | `{"remote": "<URL du remote>", "branch": "<branche>"}` | `forge/<dépôt>#<branche>` | `forge.pr/v1` |
| `issues` | `{"remote": "<URL du remote>"}` | `forge/<dépôt>/issues` | `forge.issues/v1` |
| `issue` | `{"remote": "<URL du remote>", "number": 52}` | `forge/<dépôt>/issues/52` | `forge.issue/v1` |

- **`<dépôt>` est l'identité normalisée du dépôt**, quelle que soit l'écriture du
  remote (SSH, HTTPS, alias SSH) : `github.com/<propriétaire>/<dépôt>` en
  minuscules, `dev.azure.com/<organisation>/<projet>/<dépôt>`. Les anciennes
  adresses `*.visualstudio.com` et `vs-ssh.visualstudio.com` y sont ramenées. Un
  alias SSH est résolu par `ssh -G <alias>` (son `hostname`). Deux écritures
  d'un même dépôt donnent le même flux.
- **`branch` est le nom de la branche sur la forge.** La casse est gardée.
- **Un remote non reconnu est refusé** au `subscribe`, par l'erreur
  `unsupported_remote` (`422` `subscription_refused` pour le client). C'est le seul
  refus : la brique ne contacte pas la forge pour accepter un abonnement. Un dépôt
  inaccessible ou une branche absente se lisent dans le détail (plus bas).
- **`issues` et `issue` sont refusés sur Azure DevOps** (`no_issues`) : ses work
  items appartiennent au projet, pas au dépôt.
- Un identifiant de flux qui dépasserait 256 octets est refusé (`key_too_long`).

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
  "ci": {
    "state": "running",
    "required_state": "running",
    "missing_required": ["check-agents-md"],
    "jobs": { "done": 7, "total": 9 },
    "pipelines": [
      {
        "id": "42574314636", "name": "CI", "definition": ".github/workflows/ci.yml",
        "trigger": "pull_request", "raw_trigger": "pull_request", "attempt": 1,
        "sha": "4be01c2…", "ref": "refs/pull/111/merge",
        "state": "running", "warning": false, "required": null,
        "started_at": "2026-11-09T06:38:47Z", "finished_at": null,
        "url": "https://github.com/phvn-orga/nowcast/actions/runs/42574314636",
        "jobs": [
          { "id": "129018527491", "name": "check", "stage": null,
            "state": "running", "warning": false, "required": true,
            "started_at": "2026-11-09T06:38:57Z", "finished_at": null, "url": "…" }
        ]
      }
    ]
  },
  "source": { "status": "ok", "as_of": "2026-11-09T06:39:02Z" }
}
```

- **`head_sha`** est la tête de la branche sur la forge, `null` si la branche n'y
  existe pas (pas encore poussée). Un consommateur la compare à son HEAD local
  pour savoir si ce qu'il voit porte sur ses derniers commits.
- **`pr`** est la PR de la branche, ouverte de préférence, sinon la plus récente ;
  `null` sans PR. `state` vaut `open`, `merged` ou `closed`.
- **`ci` porte sur un commit** : la tête de la PR quand il y en a une, sinon la
  tête de la branche. `null` quand il n'y a pas de commit à lire.

### La fusion automatique et la file de merge

- **`auto_merge`** dit que la PR se mergera seule une fois ses conditions remplies :
  qui l'a demandé (`enabled_by`) et la stratégie (`method` : `merge`, `squash`,
  `rebase`, ou la valeur de la forge). `null` sinon. C'est l'*auto-merge* de GitHub
  et la complétion automatique d'Azure DevOps.
- **`queue`** dit que la PR est dans la file de merge de sa base (GitHub ; Azure
  DevOps n'en a pas), `null` sinon :

  ```json
  {
    "state": "awaiting_checks", "position": 2,
    "enqueued_at": "2026-11-09T07:40:00Z", "estimated_merge_at": "2026-11-09T07:52:00Z",
    "sha": "9c1e7f0…",
    "ci": { "state": "running", "…": "…" }
  }
  ```

  `state` reprend celui de la forge en minuscules : `queued`, `awaiting_checks`,
  `mergeable`, `unmergeable`, `locked`. `sha` est le commit que la file teste (la
  PR fusionnée avec sa base et les PR qui la précèdent), sur une branche temporaire
  `gh-readonly-queue/<base>/…` ; `ci` en est la CI, de même forme que celle de la
  PR, ses pipelines déclenchés par `merge_queue`. Une PR en file reste `open` tant
  qu'elle n'est pas mergée.
- **`dequeued`** dit pourquoi la PR est sortie de la file sans être mergée :
  `{"reason": "…", "at": "…"}`, la raison telle que la forge la donne (CI en échec,
  conflit, retrait à la main). Il disparaît quand la PR rentre dans la file ou
  change de tête.

### Les pipelines

**L'unité est le pipeline, pas le contrôle** : un workflow run sur GitHub, un build
sur Azure DevOps. Plusieurs tournent sur un même commit (plusieurs fichiers de
workflow, déclencheurs `push` et `pull_request`, relance à la main). Une liste plate
de contrôles les confond : sur un commit de nowcast, 5 suites et 14 check runs,
dont `changes` quatre fois. Elle cache aussi un pipeline sans job, comme un run qui
attend une approbation.

- **Un pipeline par définition et par déclencheur**, le plus récent. Seule sa
  dernière tentative est publiée (`attempt`).
- **`sha` et `ref` disent ce que le pipeline a testé**, qui n'est pas toujours ce à
  quoi il est rattaché : un pipeline de PR teste le commit de merge
  (`refs/pull/N/merge`), pas la tête de la PR. Sur GitHub, `sha` reste la tête de la
  PR : l'API REST ne donne pas le commit de merge qu'un run `pull_request` a testé.
- **`trigger`** : `push`, `pull_request`, `merge_queue`, `manual`, `schedule`,
  `downstream`, `external` ou `other`. La valeur de la forge reste dans `raw_trigger`.
- **Les statuts de commit** (GitHub) et les statuts de PR (Azure DevOps), que
  posent des outils extérieurs, forment un pipeline de `trigger` `external`, un job
  par contexte.
- **Les jobs** d'un pipeline, avec `stage` quand la forge en a (Azure DevOps). Pas
  les étapes : une v1 s'arrête aux jobs.
- **`required`** vaut `true` ou `false` quand la forge le dit, `null` sinon. Il
  vit au niveau où la forge le pose : un job sur GitHub (un nom de contrôle), un
  pipeline sur Azure DevOps (une politique de build).

### Les éléments obligatoires absents

Un élément obligatoire peut ne jamais apparaître. Sur GitHub, un workflow filtré
par chemin (`paths:`) ne crée aucun run : il n'est même pas `skipped`, et la PR
reste bloquée sur un contrôle attendu, alors que tout ce qui a tourné passe. Un job
sauté par sa condition (`if:`), lui, est publié `skipped` et satisfait l'exigence.

La brique compare donc la liste des obligatoires à ce qui a été publié sur le
commit. `missing_required` nomme ceux qui manquent. Tant qu'une CI est attendue ou
tourne (voir « Le rythme »), un manque est normal : un pipeline peut encore
arriver. Une fois tout terminé, un manque est un blocage (`missing`, plus bas).

- **GitHub** : les noms de contrôles obligatoires de la base, réunion de deux
  sources lisibles sans droits d'administration. La protection de branche par
  GraphQL (`ref.refUpdateRule.requiredStatusCheckContexts`, « viewable by
  non-admins », vérifié le 2026-11-09), et les rulesets par REST
  (`rules/branches/<base>`, règles `required_status_checks`). Un job est
  obligatoire quand son nom y figure. Lu à l'abonnement, puis au rythme normal.
- **Azure DevOps** : chaque politique de build bloquante de la PR désigne un
  pipeline. Une politique sans build publie un pipeline sans job, à l'état de la
  politique : `queued`, ou `waiting` pour une politique à déclenchement manuel. Une
  politique dont le filtre de chemins ne correspond pas est `notApplicable`, donc
  `skipped`.

### Les états

`queued`, `waiting`, `running`, `pass`, `fail`, `cancelled`, `skipped`, `unknown`.
`waiting` dit qu'une personne doit agir (approbation, job manuel). `warning` à
`true` marque un succès avec avertissements.

| État | GitHub (`status`, `conclusion` ; statut de commit) | Azure DevOps (build, enregistrement du timeline, politique, statut de PR) |
|---|---|---|
| `queued` | `queued`, `requested`, `pending` | `notStarted`, `postponed`, `pending` ; politique `queued` ; `notSet` |
| `waiting` | `waiting` ; `action_required` | approbation en attente sur une étape |
| `running` | `in_progress` ; statut `pending` | `inProgress`, `cancelling` ; politique `running` ; statut `pending` |
| `pass` | `success` | `succeeded` ; `partiallySucceeded` et `succeededWithIssues` avec `warning` ; politique `approved` |
| `fail` | `failure`, `timed_out`, `startup_failure` ; statut `failure`, `error` | `failed` ; politique `rejected`, `broken` ; statut `failed`, `error` |
| `cancelled` | `cancelled` | `canceled` |
| `skipped` | `skipped`, `neutral` | `skipped` ; politique et statut `notApplicable` |
| `unknown` | `stale`, toute autre valeur | toute autre valeur |

### La synthèse

`ci.state` résume les pipelines, `ci.required_state` les seuls éléments
obligatoires (`null` quand la forge ne dit rien de ce qui l'est) :

1. `none` sans aucun pipeline, ni obligatoire absent, ni CI attendue ;
2. sinon `fail` si un élément échoue ou a été annulé ;
3. sinon `running` si un élément tourne ou attend son tour, ou si une CI est
   attendue ;
4. sinon `missing` s'il manque un obligatoire (`required_state` seulement) ;
5. sinon `waiting` si un élément attend une personne ;
6. sinon `pass` si un élément passe, `none` si tous ont été sautés.

Un élément sauté ou d'état inconnu ne compte pas. `ci.jobs` compte les jobs terminés et le total, pour une progression.

### `source`

L'état de la lecture, comme `usage_api` dans la brique claude-code : `pending`
(pas encore lue), `ok`, `no_credentials` (pas de jeton), `unauthorized` (jeton
refusé), `not_found` (dépôt inexistant ou invisible au compte), `rate_limited`,
`unreachable` (réseau), `unreadable` (réponse dans un format inattendu). `as_of`
date la dernière lecture réussie. Une erreur garde la dernière valeur lue : le flux
reste servi, ce n'est pas un `flow-error`.

## `forge.issues/v1` et `forge.issue/v1`

```json
{
  "repo": "github.com/phvn-orga/nowcast",
  "account": "hvn-p",
  "mine": [ { "number": 52, "title": "Design the forge brick…", "url": "…" } ],
  "others": [ { "number": 51, "title": "Design the git brick…", "url": "…" } ],
  "truncated": false,
  "source": { "status": "ok", "as_of": "…" }
}
```

- `mine` : les issues ouvertes assignées au compte qui lit (`account`) ; `others` :
  les autres. Les PR, que l'API des issues de GitHub mêle aux issues, en sont
  retirées.
- Au plus 200 issues, les plus récentes ; `truncated` dit qu'il y en a d'autres.

`forge.issue/v1` vaut `{"repo", "number", "title", "state", "url", "source"}`, où
`state` vaut `open` ou `closed`, et `null` avec `source.status` `not_found` pour un
numéro qui n'existe pas.

## Lire la forge

### Le jeton

La brique appelle les API REST elle-même. Les outils des forges ne lui servent qu'à
obtenir un jeton, qu'elle garde en mémoire :

- **GitHub** : `gh auth token --hostname github.com --user <propriétaire>`, sinon le
  compte actif de `gh`. Le propriétaire d'un dépôt d'organisation n'est pas un
  compte : c'est alors le compte actif qui lit, comme aujourd'hui dans le toolkit.
  La brique redemande le jeton après un `401`.
- **Azure DevOps** : `az account get-access-token --resource
  499b84ac-1321-427f-aa17-267ca6975798`, l'identifiant d'application d'Azure DevOps
  ([documentation](https://learn.microsoft.com/en-us/azure/devops/cli/entra-tokens?view=azure-devops)).
  Le jeton expire : la brique en redemande un avant son échéance (`expiresOn`).

Elle n'écrit jamais dans la configuration de ces outils. Absents ou non connectés,
`source.status` vaut `no_credentials`.

### GitHub

**Des requêtes conditionnelles.** La brique renvoie l'`ETag` de chaque réponse
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
| Les issues ouvertes | `issues?state=open&per_page=100`, deux pages au plus | par dépôt |
| Une issue | `issues/<numéro>` | non |

La fusion automatique vient avec les PR ouvertes (`auto_merge` de chaque PR). Le
reste passe par GraphQL, seul à le donner :

| Ce qui est lu | Requête | Partagé |
|---|---|---|
| Les contrôles obligatoires de la protection de branche | `ref(qualifiedName:) { refUpdateRule { requiredStatusCheckContexts } }` | par base |
| La file de merge d'une base, `null` sans file | `mergeQueue(branch:) { entries { position state enqueuedAt estimatedTimeToMerge headCommit { oid } pullRequest { number } } }` | par base |
| La raison d'une sortie de file | `pullRequest(number:) { timelineItems(last: 1, itemTypes: REMOVED_FROM_MERGE_QUEUE_EVENT) { … reason } }` | non, à la demande |

- **GraphQL n'a pas d'`ETag`** : chaque requête coûte au moins un point, sur un
  budget de 5 000 par heure distinct de celui de REST. La brique n'en fait donc que
  pour ce que REST ne donne pas, et les regroupe par base.
- **Une file se lit par base, pas par PR** : une requête rend toutes ses entrées.
  La brique demande à l'abonnement si la base d'une PR a une file, et ne la relit
  ensuite que pour une base qui en a une.
- **La CI de la file** se lit par REST, comme celle d'un commit, sur le `sha` de
  l'entrée.
- **Un commit sans statut** répond `state: pending` avec `total_count: 0` sur
  `commits/<sha>/status` : c'est l'absence de statut, pas un statut en attente.
- **Une check run sans workflow run** (une app qui n'est pas GitHub Actions) forme
  un pipeline au nom de son app.

### Azure DevOps

Azure DevOps ne renvoie pas d'`ETag` (`cache-control: no-store`). Chaque réponse
annonce son coût (`X-RateLimit-Cost`), sur un budget de 200 TSTU par fenêtre
glissante de 5 minutes : environ 0,02 pour une liste de builds, 0,2 pour le timeline
complet d'un gros build (mesuré le 2026-11-09 sur un projet public). La brique
compare le contenu pour savoir ce qui a changé.

| Ce qui est lu | Endpoint | Partagé |
|---|---|---|
| Les PR actives | `git/repositories/<dépôt>/pullrequests?searchCriteria.status=active` | par dépôt |
| La PR d'une branche, quand elle quitte la liste | `…/pullrequests?searchCriteria.sourceRefName=refs/heads/<branche>&searchCriteria.status=all` | non |
| La tête d'une branche sans PR active | `git/repositories/<dépôt>/refs?filter=heads/<branche>` | non |
| Les pipelines d'une PR | `build/builds?branchName=refs/pull/<id>/merge`, retenus sur `triggerInfo["pr.sourceSha"]` égal à la tête | non |
| Les pipelines d'une branche | `build/builds?branchName=refs/heads/<branche>`, retenus sur `sourceVersion` | non |
| Leurs étapes et jobs | `build/builds/<id>/timeline?changeId=<dernier>` : seuls les enregistrements changés depuis | par build |
| Ce qui est obligatoire | `policy/evaluations?artifactId=vstfs:///CodeReview/CodeReviewId/<projet>/<pr>` | non |
| Les statuts de PR | `git/repositories/<dépôt>/pullRequests/<id>/statuses` | non |

- **La CI d'une PR est celle de ses politiques de build et de statut.** Une
  politique de build désigne un pipeline (`settings.buildDefinitionId`), et
  `configuration.isBlocking` dit s'il est obligatoire. Les autres politiques
  (relecteurs, commentaires, stratégie de merge) disent si la PR peut être mergée,
  pas si sa CI passe.
- **La complétion automatique** se lit sur la PR : `autoCompleteSetBy` (qui l'a
  demandée) et `completionOptions.mergeStrategy`.
- Du timeline, la v1 garde les étapes (`Stage`) et les jobs (`Job`), pas les tâches.
  Un build terminé ne se relit plus.
- Sur un en-tête `Retry-After` ou un budget presque épuisé
  (`X-RateLimit-Remaining`), la brique ralentit et publie `rate_limited`.

## Le rythme

Chaque flux a son rythme, déduit de ce que la brique vient de lire. Le premier
état qui s'applique l'emporte :

| État du flux | GitHub | Azure DevOps | Jusqu'à |
|---|---|---|---|
| **CI attendue** : une tête neuve (push, PR ouverte) sans aucun pipeline encore | 2 s pendant 30 s, puis 5 s | 5 s pendant 30 s, puis 10 s | un pipeline apparaît, ou 2 min sans rien : pas de CI pour cet événement |
| **CI en cours** : un pipeline `queued`, `running` ou `waiting`, PR ou file de merge | 5 s | 10 s | tout est terminé |
| **fenêtre rapide** : un `refresh` n'a rien montré de neuf | 2 s | 5 s | 30 s |
| **normal** | 60 s | 120 s | |

- **Un `refresh` déclenche une lecture immédiate**, rien de plus : le protocole
  ne porte pas d'autre information (`03-contrats.md`). C'est ce que la lecture
  observe qui fixe la suite. `git push` ne rend la main qu'une fois la branche mise
  à jour sur la forge : la lecture voit la tête neuve et passe en « CI attendue »,
  qui couvre le délai entre le push et la création des pipelines. Si l'API n'a pas
  encore la nouvelle tête, la fenêtre rapide la rattrape. Des `refresh` rapprochés
  ne font qu'une lecture.
- **Le même état sert sans `refresh`** : un push depuis un terminal ou par quelqu'un
  d'autre, une fois que le rythme normal a vu la tête neuve.
- **Le rythme normal** voit ce que la machine ne voit pas : une relecture, un merge,
  une PR ouverte dans le navigateur, une mise en file.
- **La file de merge** (une requête GraphQL par base) se relit au rythme normal, et
  toutes les 10 s tant qu'une PR suivie y est : environ 360 points par heure et par
  base, sur 5 000.
- **Les lectures partagées** (les PR ouvertes d'un dépôt) suivent le flux le plus
  rapide qui en dépend.
- Sur GitHub, une lecture qui ne change rien est gratuite, d'où le rythme rapide à
  5 s ; `gh run watch` interroge toutes les 3 s. Les limites secondaires de GitHub
  (débit par minute) n'ont pas été vérifiées sur des `304` : la brique ralentit sur
  un `403` ou un `429` qui les invoque, et respecte `Retry-After`.
- Les issues n'ont que le rythme normal.

## La détection côté client

Le service ne connaît aucun agent (`00-cadrage.md`) : reconnaître une action d'agent
qui peut changer une PR revient au client. **Le mod central de Claude Code s'en
charge** (`02-clients.md`). Après un appel d'outil qui a abouti, il demande le
rafraîchissement de tous les flux forge d'une sorte (`POST
/v1/bricks/forge/refresh`, `03-contrats.md`) :

- `git push`, `gh pr …`, `az repos pr …`, et l'outil d'écriture de PR des serveurs
  MCP Azure DevOps : la sorte `pr` ;
- `gh issue …` qui écrit, `gh pr merge` (une PR peut fermer une issue) : les sortes
  `issues` et `issue`.

**Tous les flux, pas ceux de la session.** Une session sur un dépôt peut agir sur un
autre (`gh issue create -R`, `git -C`), que suit une autre session. Le mod n'a pas à
reconnaître le dépôt visé : la brique relit tous les flux de la sorte. Sur GitHub,
une relecture sans changement ne coûte rien ; sur Azure DevOps, de l'ordre de
0,1 TSTU par flux.

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

La brique écrit donc ses quelques appels REST elle-même. `gh run watch` interroge
toutes les 3 s, Gitify toutes les 60 s.

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

- **GitLab** : même modèle, avec des pipelines enfants. Ses `304` comptent dans sa
  limite de requêtes.

## À vérifier à l'implémentation

- Que `gh auth token` lit son trousseau depuis le service lancé par systemd.
- Le nom exact des paramètres `searchCriteria` d'Azure DevOps, le type d'une
  approbation en attente dans le timeline, et l'état d'une politique de build à
  déclenchement manuel.
- La file de merge sur un dépôt qui en a une : la forme des entrées, la branche
  temporaire de leurs pipelines, et la raison d'une sortie de file
  (`RemovedFromMergeQueueEvent.reason`). nowcast n'a pas de file ; seul le schéma
  GraphQL a été lu.
- Que `refUpdateRule` rend bien les contrôles obligatoires à un compte sans droits
  d'administration (vérifié seulement avec un compte administrateur).
- L'identifiant de type de la politique de build
  (`0609b952-1397-4640-95ec-e00a01b2c241`), confirmé seulement par une source tierce
  et par le toolkit.

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
- https://opentelemetry.io/docs/specs/semconv/registry/attributes/cicd/
- https://github.com/hcavarsan/pipedash, https://github.com/abhinav/git-spice, https://github.com/drone/go-scm
