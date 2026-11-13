# La brique forge

État au 2026-11-13 : conception refaite sous #52, implémentation dans #125 ;
détection des changements, clés dynamiques et rafraîchissement ciblé conçus le
2026-11-13, implémentés dans #132 (la détection sur GitHub seulement). Brique
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
la brique interroge peu d'endpoints par dépôt pour savoir ce qui a bougé, et ne lit
en détail que les cibles touchées (voir « Voir un changement »).

## Ce qu'elle publie

| Sorte | Clé | Flux | Publie |
|---|---|---|---|
| `pr` | `{"remote": "<URL du remote>", "branch": "<branche>"}` ou `{"remote": "<URL du remote>", "pr": 111}` | `forge/pr/<dépôt>#<branche>` ou `forge/pr/<dépôt>!111` | `forge.pr/v1` |
| `pipelines` | la même que `pr` | `forge/pipelines/<dépôt>#<branche>` ou `forge/pipelines/<dépôt>!111` | `forge.pipelines/v1` |
| `issues` | `{"remote": "<URL du remote>"}` | `forge/issues/<suivi>` | `forge.issues/v1` |
| `issue` | `{"remote": "<URL du remote>", "number": 52}` | `forge/issue/<suivi>#52` | `forge.issue/v1` |

- **Un client envoie ce qu'il sait** : le remote, plus une branche, un numéro de PR
  ou un numéro d'issue. La brique en fait sa cible. Elle ne contacte pas la forge
  pour accepter un abonnement, si bien que l'identifiant de flux dérive de la clé
  telle que donnée : `{remote, pr: 111}` et le `{remote, branch}` de la tête de la
  PR 111 sont deux flux et deux cibles, lues chacune de son côté. Sur GitHub, elles
  partagent leurs requêtes par le cache d'`ETag` ; sur Azure DevOps, chaque lecture
  est comptée deux fois.
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
provider ») : un client s'abonne à l'un, à l'autre, ou aux deux, sans lecture de
plus.

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
- **Sous une clé `{remote, pr}`**, `pr` est cette PR quel que soit son état, et
  `branch` et `head_sha` sont ceux de sa tête, `null` avant la première lecture. Une
  PR qui n'existe pas se lit dans `source` (`not_found`). La PR d'un fork publie
  `branch` à `null` : sa tête n'est pas une branche de ce dépôt.
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
  change de tête. Un retrait antérieur à la dernière entrée de la PR dans la file
  est ignoré.

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
le numéro de la PR ouverte dont la CI est lue, `null` sans PR ouverte. Sous une clé
`{remote, pr}`, ce sont les pipelines de la tête de cette PR.

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
- **`required` vit sur les jobs** : il dit, job par job, ce qui bloque le merge de
  la PR. Il vaut `true` ou `false` quand la forge dit ce qui est obligatoire, `null`
  sinon, et toujours `null` sans PR ouverte, l'obligation venant des règles de la
  base. Une forge qui rend obligatoire un pipeline entier rend obligatoires tous ses
  jobs. Le `required` d'un pipeline ne sert que tant qu'il n'a aucun job (une
  politique qui attend son build) : il porte alors l'obligation à leur place. Il vaut
  `null` dès qu'un job existe.

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

Pour `required_state`, un obligatoire absent donne `running`, pas `missing`, tant
qu'un élément quelconque tourne ou qu'une CI est attendue : un job obligatoire peut
attendre un job qui tourne sans l'être. `missing` ne vient qu'une fois que plus rien
ne tourne.

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
  compte qui lit (`account`, `null` avant la première lecture) ; `others`, les
  autres. Au plus 200 issues, les plus
  récemment créées, pour voir ce qui vient d'être ouvert ; `truncated` dit qu'il y en a d'autres.
- `number` est le numéro que la forge affiche ; `type`, le type de l'élément tel que
  la forge le nomme (`Bug`, `User Story`), `null` quand elle n'en donne pas. Les work
  items ont toujours un type, les issues d'un dépôt d'organisation GitHub peuvent en
  avoir un aussi.
- **Ouverte ou fermée** : le provider décide à partir de ce que sa forge sait, y
  compris quand ses états sont personnalisés.

`forge.issue/v1` vaut `{"tracker", "number", "title", "type", "state", "url",
"source"}`, où `state` vaut `open` ou `closed`. Pour un numéro qui n'existe pas, les
champs valent `null` et `source.status` vaut `not_found`. Une issue lue puis devenue
`not_found` (supprimée ou plus visible, `410` compris) a aussi ses champs à `null`,
contrairement aux autres échecs, qui gardent la dernière valeur.

## `source`

L'état de la lecture, le même pour toutes les sortes : `pending` (pas encore lue),
`ok`, `no_credentials` (pas de jeton : forge non configurée, ou source choisie
muette), `unauthorized` (jeton refusé), `not_found`
(dépôt inexistant ou invisible au compte), `rate_limited`, `unreachable` (réseau),
`unreadable` (réponse dans un format inattendu). `as_of` date la dernière lecture
réussie. Une erreur garde la dernière valeur lue (sauf le `not_found` d'une issue,
plus haut) : le flux reste servi, ce n'est pas un `flow-error`.

## Le cœur et les providers

### Le cœur : un registre

Le cœur ne connaît aucune forge ni aucun rythme. Il tient :

- **le registre des providers**, compilés dans la brique. Au `subscribe`, il demande
  à chacun s'il reconnaît le remote ; celui qui le reconnaît prend l'abonnement et
  donne l'identité du dépôt ou du suivi, d'où l'identifiant de flux ;
- **les flux** : un par identifiant, quel que soit le nombre d'abonnés. Il transmet
  au provider ce qui les concerne (un flux ouvert, un `release`, un `refresh`) ;
- **les portées de rafraîchissement** : il normalise le remote d'une portée par le
  provider qui le reconnaît, et la lui transmet (voir « Le rafraîchissement
  ciblé ») ;
- **la publication** : il ne publie un flux que si son détail a changé, sauf après
  le `refresh` de ce flux, toujours suivi d'une publication.

### Le contrat d'un provider

Le contrat est celui de la sortie : pour chaque sorte, le provider produit le détail
de son type (`forge.pr/v1`, `forge.pipelines/v1`, `forge.issues/v1`,
`forge.issue/v1`), avec les états neutres et les valeurs de `source.status`. Il
s'engage aussi sur un comportement :

- **un `refresh` déclenche une lecture immédiate**, ou dès que la limite de la
  forge le permet ; des `refresh` rapprochés ne font qu'une lecture ;
- **une portée de rafraîchissement fait regarder aussitôt ce qui a bougé** dans ce
  dépôt et pour ces sortes, puis lire les cibles touchées ; une portée qu'aucun flux
  ne suit ne coûte rien ;
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
  rythme normal, ou attend ce que la limite demande. Ce que la machine ne voit pas
  (un push depuis un terminal ou par quelqu'un d'autre, une relecture, un merge, une
  mise en file) se voit par la détection du provider (voir « Voir un changement »),
  et à défaut au rythme normal.

Rien n'oblige un provider à les prendre : un outil qui ne colle pas à une forge est
contourné, pas plié pour elle.

### Les écarts, réglés chez le provider

| Ce qui diverge | GitHub | Azure DevOps | Dans le contrat |
|---|---|---|---|
| Ce qui porte les issues | le dépôt | le projet (work items) | `<suivi>` |
| États d'une issue | ouvert, fermé | personnalisés, rangés en catégories | `open`, `closed` |
| File de merge | oui | non | `queue` toujours `null` sans file |
| Étapes d'un pipeline | non | oui | `stage` `null` sans étape |
| Niveau de l'obligation | un nom de contrôle, donc un job | une politique de build, donc tout un pipeline | `required` sur chaque job |
| Commit testé par un pipeline de PR | non donné par l'API REST | donné | `sha` : la tête quand il manque |
| Statuts extérieurs | statuts de commit | statuts de PR | pipeline `external` |
| Coût d'une lecture | gratuite quand rien ne change (`304`) | toujours comptée (TSTU) | rythme propre au provider |
| Voir ce qui a bougé | trois listes par dépôt, en requêtes conditionnelles | comparaison du contenu, non vérifiée | détection propre au provider |

Un provider à venir ajoute sa colonne, pas un champ : une notion qu'une seule forge
connaît prend une valeur neutre ailleurs, ou n'entre pas dans le contrat.

## Les identifiants

Pour chaque forge, l'utilisateur choisit d'où vient le jeton : la CLI de la forge
(`gh`, `az`), ou un jeton qu'il donne. C'est un choix, pas une suite de replis.

- **Le choix se fait à la configuration de la brique** (`nowcast bricks configure
  forge`, que `enable` lance aussi, `03-contrats.md`), forge par forge. La brique
  garde le mode choisi dans `$XDG_CONFIG_HOME/nowcast/forge/config.toml`, non
  secret : une table par forge utilisée, `source = "cli"` ou `"token"`.
- **Un jeton va dans le trousseau Secret Service**, sous les attributs
  `application=nowcast`, `brick=forge` et `forge=github` ou `forge=azure`. La brique
  ne déverrouille jamais un trousseau verrouillé ; seul `configure` le fait.
- **Seule la source choisie est lue.** Si elle ne donne pas de jeton, le flux publie
  `no_credentials` ; si la forge refuse le jeton, `unauthorized`. La brique ne
  bascule jamais d'elle-même vers l'autre source. Une forge non configurée publie
  `no_credentials`, et un client qui le lit peut montrer `nowcast bricks configure
  forge`.
- **Une configuration refaite sert sans redémarrer la brique** : après un
  `no_credentials` ou un `unauthorized`, la lecture suivante relit la configuration.
- **Le service lancé par systemd atteint le trousseau** : `gh auth token`, dont le
  jeton y est rangé, répond depuis une unité `systemd-run --user` (vérifié le
  2026-11-10). Le jeton propre à la brique passe par le même trousseau, lu par
  `oo7` : non vérifié depuis systemd.
- La brique n'écrit jamais dans la configuration de `gh` ou d'`az`.

## Les providers

### GitHub

**Identité.** Les remotes `github.com` en HTTPS, `ssh://` ou scp ; un alias SSH est
résolu par `ssh -G <alias>` (son `hostname`). Le dépôt et le suivi valent
`github.com/<propriétaire>/<dépôt>`, en minuscules.

**Jeton.** Par la CLI : `gh auth token --hostname github.com --user
<propriétaire>`, sinon le compte actif de `gh`. Le propriétaire d'un dépôt
d'organisation n'est pas un compte : c'est alors le compte actif qui lit. Le jeton est
redemandé après un `401`. Par un jeton : un jeton personnel, envoyé en
`Authorization: Bearer`.

**Des requêtes conditionnelles.** Le provider renvoie l'`ETag` de chaque réponse
(`If-None-Match`) ; une réponse `304` ne compte pas dans la limite de 5 000 requêtes
par heure (vérifié le 2026-11-09 sur tous les endpoints ci-dessous). Les paramètres
d'une requête restent identiques d'un appel à l'autre, sinon l'`ETag` ne sert plus.
Les réponses pèsent de 150 à 600 Ko, environ dix fois moins en gzip : le provider
demande gzip.

**La détection.** Trois détecteurs par dépôt suivi, interrogés en requêtes
conditionnelles toutes les 10 s au départ. Mesuré par un spike le 2026-11-12, sur
quatre dépôts (NixOS/nixpkgs, home-assistant/core, microsoft/vscode, phvn-orga/nowcast), pendant
7 minutes à partir de 13:38 UTC, environ 525 requêtes authentifiées :

| Détecteur | Endpoint | Changement vu après son `updated_at` (médiane à p90) |
|---|---|---|
| pipelines | `actions/runs?per_page=20` | 5 à 11 s |
| PR | `pulls?state=all&sort=updated&direction=desc&per_page=20` | 8 à 24 s |
| issues | `issues?state=all&sort=updated&direction=desc&per_page=20` | 8 à 24 s |

- **Un `304` est gratuit** (mesuré), et chaque `200` du spike portait un vrai
  changement.
- **Le coût** est nul sur un dépôt calme. Sur un dépôt chargé, l'`ETag` change sur
  50 à 90 % des interrogations : environ 700 points par heure et par dépôt pour les
  trois, 14 % du budget de 5 000 par heure. Sur nowcast, 40 s de fonctionnement
  (une branche et les issues du dépôt, le 2026-11-13) ont coûté 10 points : un par
  `200`, les 9 interrogations des détecteurs ayant répondu `304`.
- **Un détecteur chargé ralentit.** Sa période part de 10 s. Une fois qu'il a 6
  réponses à sa période actuelle, plus de 3 `200` la doublent (60 s au plus), 1 au
  plus la divise par deux (10 s au moins), sinon la plus ancienne réponse sort du
  compte. Un changement de période remet le compte à zéro : chaque période est
  jugée sur ses propres réponses.
- **Un détecteur dit ce qui a bougé** en comparant sa liste à la précédente : un
  élément nouveau dans la liste, ou dont l'un de ces champs a changé. Sa première
  réponse ne fait que poser cette référence et ne réveille rien.

  | Détecteur | Champs comparés |
  |---|---|
  | pipelines | `updated_at`, `status`, `conclusion`, `head_sha` |
  | PR | `updated_at`, `state`, le sha de la tête |
  | issues | `updated_at`, `state` |

  Un run ou une PR ne touche une branche que si son dépôt de tête est celui-ci. Le
  provider lit ensuite en détail les seules cibles touchées : une PR par son numéro,
  la tête d'une branche, une issue par son numéro. Les PR que l'API des issues mêle
  aux issues en sont retirées.
- **Un rafraîchissement fait d'abord interroger les détecteurs** dont la cible
  dépend, en les attendant 5 s au plus, puis la cible se lit. Il ouvre leur fenêtre
  rapide (toutes les 2 s pendant 30 s), et celle de la cible elle-même (une lecture
  toutes les 2 s pendant 30 s), qui rattrape une tête que l'API ne montre pas
  encore.
- **La progression d'une CI se lit encore sur le commit.** Le `updated_at` d'un run
  ne bouge pas quand ses jobs se terminent, et sur un dépôt chargé un run en cours
  sort des 20 premiers de `actions/runs` en 4 minutes environ. Tant qu'une CI
  tourne, le provider lit les check runs du commit, qui montrent chaque job avec 8 à
  15 s de retard.
- **Écarté : le flux d'événements du dépôt.** Son interrogation est plafonnée à
  60 s (`X-Poll-Interval`) ; pendant le spike, les événements frais arrivaient avec
  20 à 66 s de retard (un lot avec 532 s), d'anciens événements revenaient comme
  nouveaux, l'ordre n'était pas chronologique, et aucun type d'événement ne couvre
  les workflow runs ni les check runs. La
  [documentation](https://docs.github.com/en/rest/activity/events?apiVersion=2022-11-28)
  annonce une latence de 30 s à 6 h et dit qu'il n'est « not built to serve
  real-time use cases ».

Les lectures de détail, à l'abonnement, quand un détecteur dit qu'une cible a bougé,
après un `refresh`, et tant qu'une CI est attendue ou tourne :

| Ce qui est lu | Endpoint | Partagé |
|---|---|---|
| Les PR ouvertes | `pulls?state=open&per_page=100` | par dépôt, toutes branches |
| La PR fermée d'une branche, quand elle quitte la liste | `pulls?head=<propriétaire>:<branche>&state=closed` | non, à la demande |
| La tête d'une branche sans PR ouverte | `branches/<branche>` | non |
| Les pipelines d'un commit | `actions/runs?head_sha=<sha>&per_page=100` | par commit |
| Leurs jobs | `commits/<sha>/check-runs?per_page=100`, reliés aux pipelines par `check_suite.id` | par commit |
| Les statuts de commit | `commits/<sha>/status` | par commit |
| Les rulesets d'une base | `rules/branches/<base>?per_page=100` | par base |
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
  des règles `required_status_checks` des rulesets de la base. Quand ni l'une ni les
  autres n'en nomment, la forge ne dit rien : `required` reste `null`. Les noms sont
  gardés par base et relus au plus une fois par période du rythme normal ; l'échec
  d'une des deux sources garde ses derniers noms connus et ne fait jamais échouer la
  lecture. Le `main` de nowcast exige `check`, `check-agents-md` et `claude-code` par
  la protection de branche, sans ruleset (lu le 2026-11-10 avec un compte
  administrateur).
- **Une file se lit par base** : une requête rend toutes ses entrées. Une base sans
  file est revérifiée au rythme normal, une file pouvant y être ajoutée ; une base
  qui en a une se relit au rythme normal, et toutes les 10 s tant qu'une PR suivie y
  est (environ 360 points par heure et par base, sur 5 000). La CI de la file se lit
  par REST, comme celle d'un commit, sur le `sha` de l'entrée, à partir de ses
  pipelines `merge_queue` (branche temporaire `gh-readonly-queue/<base>/…`). Elle est
  attendue pendant 2 min tant que ce commit n'a aucun pipeline. Un échec de lecture
  de cette CI ne touche ni la PR ni sa CI propre : `queue.ci` garde sa dernière
  valeur. `queue.state` reprend celui de GitHub en minuscules (`QUEUED`,
  `AWAITING_CHECKS`, `MERGEABLE`, `UNMERGEABLE`, `LOCKED`, lus par introspection du
  schéma le 2026-11-10). `estimated_merge_at` vaut l'heure de lecture plus
  `estimatedTimeToMerge`, nullable dans le schéma, arrondie à la minute. La raison
  d'une sortie de file est redemandée après un échec.
- **Le commit d'un pipeline de PR** : l'API REST ne donne pas le commit de merge
  qu'un run `pull_request` a testé ; `sha` reste la tête de la PR, `ref` vaut
  `refs/pull/<n>/merge`.
- **Un commit sans statut** répond `state: pending` avec `total_count: 0` sur
  `commits/<sha>/status` : c'est l'absence de statut, pas un statut en attente.
- **Une check run sans workflow run** (une app qui n'est pas GitHub Actions) forme
  un pipeline au nom de son app.
- **Issues** : les PR, que l'API des issues mêle aux issues, en sont retirées. Elles
  comptent pourtant dans les pages de GitHub : moins de 200 issues peuvent être
  listées, et `truncated` vaut `true` quand la deuxième page de 100 est pleine.
  `type` vient du champ REST `type`, un objet portant `name` quand l'organisation a
  des types d'issue, `null` sinon (vérifié le 2026-11-10).

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
gratuite, d'où le rythme rapide ; `gh run watch` interroge toutes les 3 s. Les
détecteurs font lire ce qu'ils voient bouger, et les cibles gardent un rythme de
sûreté pour ce qu'ils manqueraient :

- une branche ou une PR se relit toutes les 60 s, toutes les 5 s tant que sa CI
  tourne ;
- les cibles d'issues se relisent toutes les 5 min après une lecture réussie, au
  rythme normal (60 s) après un échec ;
- un détecteur sans jeton attend le rythme normal avant de réessayer, `gh auth
  token` redemandé plus tôt n'en donnant pas davantage.

Les contrôles obligatoires et la file d'une base, qu'aucun détecteur ne voit, se
relisent au rythme normal. Les limites secondaires (débit par minute) n'ont pas été
vérifiées sur des `304` : le provider ralentit sur un `403` ou un `429` qui les
invoque, et respecte `Retry-After`. Une lecture partagée (les PR ouvertes d'un dépôt)
suit la branche la plus rapide qui en dépend.

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
Par la CLI, le jeton expire : le provider en redemande un avant son échéance
(`expiresOn`), et l'envoie en `Bearer`. Par un jeton : un jeton d'accès personnel
(PAT), envoyé en authentification Basic.

**Compte.** `account` vient de `/<organisation>/_apis/connectionData`, non documenté
et sans `api-version` : `properties.Account.$value`, sinon `providerDisplayName`, en
minuscules.

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
| Ce qui est obligatoire, et le build de chaque politique | `policy/evaluations?artifactId=vstfs:///CodeReview/CodeReviewId/<GUID du projet>/<pr>` | non |
| Les pipelines d'une PR hors politique | `build/builds?branchName=refs/pull/<id>/merge`, retenus sur `sourceVersion` égal au `lastMergeCommit` de la PR | non |
| Les pipelines d'une branche, aussi avec une PR active | `build/builds?branchName=refs/heads/<branche>`, retenus sur `sourceVersion` égal à la tête | non |
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
  `configuration.isBlocking` dit s'il est obligatoire : tous les jobs du pipeline le
  sont alors, ou aucun. Le build retenu est celui
  que l'évaluation désigne (`context.buildId`), celui qu'Azure DevOps montre sur la
  PR. Une politique sans build publie un pipeline sans job, à l'état de la
  politique : `queued`, ou `waiting` pour une politique à déclenchement manuel. Une
  évaluation `queued` dont le `context.buildId` désigne un build terminé publie aussi
  un pipeline `queued` sans job, le build suivant n'étant pas encore visible : la CI
  reste attendue. Une politique dont le filtre de chemins ne correspond pas est
  `notApplicable`, donc `skipped`. Une politique de statut (type
  `cbdc66da-9728-4af8-aada-9a5a32e4a226`, non vérifié) qui ne s'applique pas ne
  bloque pas. Les autres politiques (relecteurs, commentaires, stratégie de merge)
  disent si la PR peut être mergée, pas si sa CI passe.
- **Avec une PR active, les builds de la branche sur sa tête sont lus aussi**, et
  publiés non obligatoires.
- **Une politique expirée attend une personne.** Un build réussi cesse de valoir
  après sa durée de validité (`validDuration`) : l'évaluation repasse `queued` avec
  `context.isExpired: true`, et rien ne tourne tant que personne ne relance le build.
  Elle est publiée `waiting`, pas `queued` : sinon la synthèse dirait `running` et le
  rythme resterait sur « CI en cours » indéfiniment. Comme une politique à
  déclenchement manuel, elle forme un pipeline `waiting` sans job, qui porte
  `required` ; les jobs de l'ancien build ne sont pas publiés.
- **Ce qu'un pipeline de PR a testé** : `sourceVersion` est le commit de merge de la
  PR (`lastMergeCommit`), `ref` vaut `refs/pull/<id>/merge`.
- **`trigger`**, d'après `reason` : `individualCI` et `batchedCI` donnent `push`,
  `pullRequest` `pull_request`, `manual` `manual`, `schedule` `schedule`,
  `resourceTrigger` `downstream`, toute autre valeur `other`. `individualCI` et
  `batchedCI` forment un seul pipeline `push` ; un build `other` se regroupe sur sa
  `reason` brute.
- **Du timeline, l'unité de job est l'enregistrement `Phase`**, groupé sous son
  `Stage`. Un job en attente ou sauté n'existe que comme `Phase` ; un `Job` interne
  sans `Phase` (« Finalize build ») est ignoré ; les tâches ne sont pas gardées. Une
  approbation en attente (enregistrement `Checkpoint.Approval` `inProgress`) rend
  `waiting` les `Phase` en attente. Le
  timeline d'un build terminé ne se relit que si son statut change : une relance de
  jobs ou d'étape garde l'identifiant du build.
- **Les statuts de PR s'accumulent** : une entrée par envoi et par itération. Le
  provider garde le plus récent de chaque contexte (`context.genre`/`context.name`),
  sur la dernière itération. Un statut dont le `targetUrl` désigne un build publié
  (`codecoverage`) appartient à ce build : il ne forme pas de pipeline `external`.
- **Work items.** WIQL ne filtre pas sur les catégories d'état :
  `[System.StateCategory]` n'existe pas (`TF51005`), et `[System.State] NOT IN GROUP
  'Completed'` est accepté mais ne filtre rien, `IN GROUP` ne visant que les
  catégories de types (vérifié le 2026-11-10). Le provider lit donc les états de
  chaque type, dont chacun a sa catégorie fixe
  ([documentation](https://learn.microsoft.com/en-us/azure/devops/boards/work-items/workflow-and-state-categories?view=azure-devops)),
  et construit `[System.State] NOT IN (…)` par type, en excluant les états
  `Completed` et `Removed`. Il écarte les types qui ne sont pas des tickets (Test
  Case, Test Plan, Test Suite, Shared Steps, Code Review, Feedback) par `NOT IN
  GROUP` sur `Microsoft.HiddenCategory`, `Microsoft.TestCaseCategory`,
  `Microsoft.TestPlanCategory`, `Microsoft.TestSuiteCategory` et
  `Microsoft.SharedStepCategory` : sur un projet réel, 1 212 éléments ouverts, 547
  une fois ceux-là écartés. Les types et leurs états sont gardés 30 min. `[System.AssignedTo] = @Me`
  sépare `mine` des autres, le tri se fait sur `System.CreatedDate`, et la requête
  ne rend que des identifiants
  ([documentation](https://learn.microsoft.com/en-us/azure/devops/boards/queries/wiql-syntax)).
  `type` est `System.WorkItemType`. Un work item d'un autre projet est `not_found`.
- **Limite** : une réponse réussie qui porte `Retry-After`, ou un
  `X-RateLimit-Remaining` sous 10 % de `X-RateLimit-Limit`, donne `rate_limited`.
  Le provider attend `Retry-After`, sinon jusqu'à `X-RateLimit-Reset`, sinon 120 s.

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

**La détection, non vérifiée.** Même principe qu'avec GitHub, autre mécanique, et
rien n'a tourné : cette conception est écrite sans accès à Azure DevOps, et un spike
tournera depuis une machine qui a accès à un projet réel (voir « À vérifier sur Azure
DevOps (spike) »). Sans `ETag`, le provider compare le contenu, et chaque appel est
compté : aux 0,015 à 0,03 TSTU mesurés sur les appels de liste, trois détecteurs
interrogés toutes les 10 s coûteraient environ 3 TSTU par fenêtre de 5 minutes, sur
200 (une estimation tirée de ces mesures). Les candidats :

- les PR actives listées par projet, un appel pour tous les dépôts du projet ;
- les derniers builds du projet ;
- une requête WIQL sur `System.ChangedDate` depuis la dernière lecture, qui ne rend
  que les identifiants des work items changés.

La progression d'une CI se lit par le timeline de chaque build, par incréments avec
`changeId`, comme aujourd'hui. Jusqu'au spike, le provider garde sa lecture par
branche et comprend le rafraîchissement ciblé en relisant les flux de la portée :
ceux du dépôt (du projet pour les issues), des sortes nommées, et du numéro quand il
est donné. L'interface du mod central est ainsi la même pour les deux forges.

**Rythme** : la machine à rythme, avec CI attendue 5 s pendant 30 s puis 10 s, CI en
cours 10 s, fenêtre rapide 5 s, normal 120 s. Sur une PR, « CI attendue » n'a pas à
être deviné : une évaluation de politique de build `queued`, non expirée, dit qu'un
build va venir, et la fenêtre de 2 minutes ne sert que pour une branche sans PR.
Chaque lecture est comptée, et le provider ralentit quand le budget restant baisse.
Les work items se lisent au rythme normal.

## Voir un changement

**Un changement ne se voit qu'en lisant la forge.** Tout le reste est un indice qui
veut dire « lis maintenant » : il ne porte aucun état et ne remplace jamais une
lecture. Un changement a trois origines :

| Origine | Qui la voit | Indice envoyé à la brique |
|---|---|---|
| L'action d'une session d'agent (`git push`, `gh pr create`, un outil MCP) | le mod de cette session | un rafraîchissement ciblé |
| Un push depuis la machine, hors d'un agent | un hook git, plus tard, avec la brique git (#51) | un rafraîchissement ciblé, à venir |
| Le reste du monde (une autre machine, le navigateur, la CI, un merge automatique) | personne : seul le polling le voit | aucun |

### Par dépôt et par sorte

Pour chaque dépôt dont un flux est suivi, la brique interroge un détecteur par
sorte : les pipelines, les PR, les issues. Un détecteur dit ce qui a bougé ; la
brique lit ensuite en détail les seules cibles touchées (une PR par son numéro, la
tête d'une branche, une issue par son numéro) et publie les flux dont le détail a
changé. Un flux `pr` ou `pipelines` dépend des détecteurs des PR et des pipelines de
son dépôt, un flux `issues` ou `issue` de celui des issues de son suivi. Un détecteur
n'existe que tant qu'un flux en dépend. La mécanique appartient au provider : mesurée
sur GitHub, à vérifier sur Azure DevOps (voir « Les providers »).

### Le rafraîchissement ciblé

`POST /v1/bricks/forge/refresh` peut porter une portée, que le service relaie sans
l'interpréter (`03-contrats.md`) :

```json
{ "scope": { "remote": "git@github.com:phvn-orga/nowcast.git", "types": ["pr", "pipelines"], "number": 111 } }
```

- **`remote`** est normalisé comme celui d'une clé. Un remote qu'aucun provider ne
  reconnaît ne rafraîchit rien.
- **`types`** parmi `pr`, `pipelines` et `issues`, au moins un, toutes les sortes
  s'il est absent ; `issues` vise aussi les flux `issue`.
- **`number`**, facultatif, est le numéro d'une PR ou d'une issue que l'action a
  touchée. Il compte parmi les cibles touchées, s'il nourrit un flux. Il écarte une
  cible seulement quand sa PR est connue et porte un autre numéro : une branche sans
  PR ouverte connue se relit quand même (`gh pr create`, la base qui avance sur `gh
  pr merge`). Quand la portée nomme aussi `pr` ou `pipelines`, le numéro ne
  restreint que les cibles de branche et de PR, pas les issues.
- **La brique interroge aussitôt les détecteurs** de ce dépôt pour ces sortes, puis
  lit les cibles touchées. Une portée qu'aucun flux ne suit ne coûte rien. Seuls les
  flux dont le détail a changé sont publiés.
- **Une portée illisible** est journalisée et ignorée.
- **Sans portée, rien ne change** : chaque flux de la brique, ou de la sorte nommée,
  reçoit un `refresh` et se relit.

### Ce que reconnaît le client

Le service ne connaît aucun agent (`00-cadrage.md`) : reconnaître une action d'agent
qui peut changer la forge revient au client, et dans Claude Code au mod central
(`02-clients.md`). Après un appel d'outil qui a abouti, il en fait une portée :

| Appel | `types` |
|---|---|
| `git push` | `pr`, `pipelines` |
| `gh pr …` | `pr`, `pipelines` ; avec `issues` pour `gh pr merge`, qui peut fermer une issue |
| `gh run …`, `gh workflow …` | `pr`, `pipelines` |
| `gh issue …` | `issues` |
| `az repos …`, `az pipelines …` | `pr`, `pipelines` |
| `az boards …` | `issues` |
| `gh api`, `curl` vers l'API d'une forge | d'après le chemin : PR, pipelines ou builds, issues ou work items |
| Outils des serveurs MCP GitHub et Azure DevOps | d'après l'outil : PR, pipelines, issues ou work items |

- **Le dépôt vient des arguments** : `-R`, l'URL que `gh` affiche, le chemin de
  l'API, les paramètres de l'outil MCP. À défaut, c'est celui du répertoire de
  travail de la commande. Le numéro vient des mêmes sources.
- **Un dépôt introuvable** fait demander le rafraîchissement de tous les flux des
  sortes visées, comme avant les portées.
- **La portée nomme le dépôt visé**, pas celui de la session : une session qui crée
  une issue dans un autre dépôt (`gh issue create -R`) rafraîchit ce qu'y suit une
  autre session, sans connaître ses flux.

Sans brique forge activée, le service répond `unknown_brick` et le mod n'insiste
pas ; sans flux, rien ne se lit. Un push depuis un terminal n'envoie aucun indice :
les détecteurs le voient, comme le reste du monde.

### La vie d'une session

| Étape | Ce qui change sur la forge | Qui le voit | Réaction |
|---|---|---|---|
| Un worktree, une nouvelle branche | rien | le mod central, si son agent a lancé la commande | abonnement aux clés de la branche là où `git push` l'enverrait (`02-clients.md`) ; `head_sha` est `null` |
| Des commits locaux | rien | personne | aucune ; un consommateur compare `head_sha` à son HEAD |
| `git push` | la branche est créée ou avance, une CI démarre | le mod central | la clé ne change pas, `head_sha` se remplit ; portée `pr`, `pipelines` ; CI attendue |
| `gh pr create` | une PR s'ouvre, sa CI démarre | le mod central, le numéro tiré de l'URL affichée | portée avec `number` : la brique résout la PR, le flux de la branche la montre |
| D'autres pushs | la tête avance, la PR change | le mod central | comme `git push` |
| La CI tourne | runs et jobs changent | la brique seule | « CI en cours » : sur GitHub, les check runs du commit, chaque job vu 8 à 15 s après |
| Une relecture dans le navigateur | la PR change | le détecteur des PR | rien de publié ne change : `forge.pr/v1` ne porte pas les relectures |
| Le merge, par `gh pr merge`, le navigateur, le merge automatique ou la file | la PR passe `merged`, la base avance | le mod central pour `gh pr merge` ; sinon les détecteurs des PR et des pipelines | rien de propre au merge : c'est un changement de PR et un push sur la base, que les détecteurs voient |
| Le retour sur `main` | rien | le mod central, si son agent a lancé `git switch` ; sinon personne avant la brique git (#51) | abonnement aux clés de `main` ; un worktree supprimé sort de la liste, ses abonnements avec |
| Une issue fermée par le merge (`Closes #N`) | l'issue passe `closed` | le détecteur des issues, interrogé aussitôt par la portée de `gh pr merge` | les flux `issues` et `issue` concernés se relisent |
| Des issues créées dans un autre dépôt | une issue s'ouvre ailleurs | le mod central | portée sur ce dépôt-là ; les montrer dans la session n'est pas nécessaire pour l'instant |

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
- **D'autres cibles pour `pipelines`** : un commit donné, ou un pipeline précis,
  suivi jusqu'à sa fin (un run) ou run après run (une définition sur une branche).
  La v1 ne lit que la tête d'une branche ou d'une PR.
- **Garder les objets de la forge sur lesquels une session a agi**, pour montrer par
  exemple les issues qu'elle a créées dans un autre dépôt. Pas nécessaire
  aujourd'hui.
- **Le détail de la CI d'une file de merge** : la v1 n'en publie que la synthèse,
  dans `queue.ci`.
- **Le volume des work items** : un gros projet Azure DevOps a bien plus de 200
  éléments ouverts (547 tickets sur un projet réel), et `others` n'en montre alors qu'une partie. Filtrer (par zone,
  par itération) n'est pas décidé.

## À vérifier à l'implémentation

- Les permissions minimales d'un jeton personnel, sur chaque forge, pour tout ce que
  le provider lit.
- La file de merge sur un dépôt qui en a une : la forme des entrées, la branche
  temporaire de leurs pipelines, et la raison d'une sortie de file
  (`RemovedFromMergeQueueEvent.reason`). nowcast n'a pas de file ; seul le schéma
  GraphQL a été lu, aucune file réelle.
- Que `refUpdateRule` rend bien les contrôles obligatoires à un compte sans droits
  d'administration (vérifié seulement avec un compte administrateur).

## À vérifier sur Azure DevOps (spike)

Le provider Azure DevOps est construit d'après cette conception sans avoir tourné sur
un projet réel. Ces points sont à vérifier depuis une machine qui a accès à un tel
projet, chacun avec sa date et ce qui a été observé.

La détection :

- si la liste des builds se trie par dernier changement, et pas seulement par heure
  de mise en file ;
- si la liste des PR reflète les votes et les statuts, ou s'il faut lire chaque PR
  active ;
- si les listes par projet (PR, builds) fonctionnent et couvrent tous les dépôts du
  projet ;
- le délai et le coût réel (`X-RateLimit-Cost`) de chaque détecteur ;
- si une requête WIQL sur `System.ChangedDate` voit tous les changements d'un work
  item (état, assignation, commentaires), et avec quel délai.

Le provider tel que conçu :

- les quatre sortes sur un vrai dépôt ;
- les politiques expirées, et l'état d'une politique à déclenchement manuel ;
- le filtre des work items ;
- le type de la politique de statut ;
- l'enregistrement d'une approbation en attente ;
- le `targetUrl` d'un statut posé par un build ;
- si une PR active peut manquer de `lastMergeSourceCommit` ;
- le compte rendu par `connectionData`.

## Sources

- https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api
- https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
- https://docs.github.com/en/rest/activity/events?apiVersion=2022-11-28
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
