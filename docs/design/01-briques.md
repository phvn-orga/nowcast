# Les briques

État au 2026-11-07 : modèle d'extension, contrat, transport, documentation de la clé
et découverte des briques décidés, pas d'action dans le service, historique borné des
flux, entrée d'un flux et briques natives. Le format du fichier
de déclaration et le protocole sont fixés dans `03-contrats.md`.

## Décisions prises

- **Une brique est un programme externe au service.** Elle vit dans son propre dépôt,
  dans le langage de son choix. Le dépôt de nowcast en fournit quelques-unes, sans
  privilège : le service ne distingue pas une brique de son dépôt d'une brique écrite
  ailleurs.
- **Le service est un relais.** Il met en relation briques et clients, sans imposer de
  règle sur la façon dont une brique obtient sa donnée. Fréquence, protocole et
  authentification auprès de la source sont l'affaire de la brique.
- **Pas de socle de polling dans le service.** Interroger une source (polling HTTP,
  WebSocket, protocole propre à un outil) est propre à chaque brique, qui se sert des
  bibliothèques de son choix. Le service n'offre rien de tel.
- **Le fichier de déclaration d'une brique vit dans son dépôt**, à côté de son code.
  Le code du service ne connaît aucune brique.
- **Deux transports indépendants.** Celui entre les briques et le service n'est pas
  celui entre le service et ses clients. Les clients peuvent être limités (un client
  qui ne lit qu'un flux HTTP, par exemple) ; cette limite ne s'impose pas aux briques,
  qui sont du code et peuvent parler ce qu'on choisit.
- **Un contrat à deux niveaux.**
  - *L'enveloppe* est le contrat du service, la seule partie qu'il lit pour router. Ses
    champs reprennent les noms de CloudEvents : `source` (la brique), `subject` (l'identifiant
    de flux), `type`, `id` (le numéro de séquence), `dataschema`.
  - *Le détail* est opaque : le service ne l'interprète jamais, il vérifie seulement
    que c'est du JSON bien formé. Son contrat appartient à
    un type de donnée (par exemple `forge.pr/v1`), pas à une brique. Plusieurs briques
    peuvent publier le même type ; seuls les briques et les consommateurs de ce type le
    connaissent et le font respecter.
- **Rien de métier hors du détail.** Pas de vocabulaire commun à toutes les briques (un
  état ou un libellé lisible par un client qui ne connaît pas le type) : un consommateur
  doit connaître le contrat du type qu'il lit. Écarté faute de besoin ; à rouvrir si des
  clients génériques apparaissent.
- **Le détail est toujours du JSON**, aucun autre format. Une brique convertit ce que
  parle sa source (un flux binaire, du Protobuf). Images et rendu appartiennent aux
  interfaces clientes, pas aux briques. Le service vérifie que chaque détail est un
  JSON valide avant de le relayer : une vérification de syntaxe, jamais une
  interprétation. Côté clients, le détail est placé tel quel dans le champ `data` de
  l'événement, sans conversion.
- **Une publication invalide est refusée.** Si le détail n'est pas du JSON valide, le
  service ne relaie pas la publication et répond à la brique par une erreur. La brique
  en fait ce qu'elle veut. Toute publication reçoit donc une réponse sur le canal de la
  brique : un accusé de réception ou une erreur.
- **Enveloppe en MessagePack, détail (les octets du JSON) porté en `bin`.** Préféré à un en-tête binaire fait
  main parce que c'est un standard. Mesure du 2026-11-04 : lire l'enveloppe coûte 46 ns
  en MessagePack contre 12 ns pour un en-tête binaire, deux coûts négligeables face à un
  aller-retour de 3 à 6 µs. En lignes JSON, ce coût croît avec le détail (2,2 µs à 5 Ko).
- **Pas de briques en shell.** Une brique est un programme qui parle MessagePack ; une
  bibliothèque existe pour cela dans tous les langages courants.
- **Le service trouve les briques par un répertoire de déclarations.** Installer une
  brique, c'est rendre son fichier de déclaration visible dans un répertoire que le
  service lit, comme une unité systemd. Le service ne connaît les briques que par ce
  répertoire, jamais dans son code. Emplacement et activation par lien symbolique :
  voir `03-contrats.md`.
- **Le service observe, il n'agit pas.** Une action sur une source (cocher une tâche,
  par exemple) reste dans le mod client, qui agit directement sur la source avec ses
  propres identifiants. Ces identifiants se partagent par l'outillage de la source ou
  le trousseau du système, jamais par le service. N'importe qui écrira des mods
  spécifiques : faire passer chaque action par le service n'aurait pas de sens. En
  dehors des abonnements, un client adresse deux requêtes à une brique, toujours sur un
  flux auquel il est abonné, et le service les relaie à la brique de ce flux : le
  rafraîchissement immédiat, typiquement après qu'un mod a agi sur la source, et
  l'entrée (ci-dessous).
- **L'entrée d'un flux : une donnée qu'un client envoie à la brique.** Certaines
  données n'existent que chez le client (les mesures d'une session d'agent, que seul
  son moteur connaît). Un abonné d'un flux peut y envoyer un détail JSON, d'un type
  que la sorte d'abonnement déclare accepter ; le service le vérifie contre le schéma
  de ce type et le relaie à la brique (`03-contrats.md`, sections 1, 3 et 4). Rien
  n'y est propre à un agent : c'est le pendant montant d'une publication.
  - **Sur un flux, pas sur une brique.** L'entrée hérite ainsi de ce que les
    abonnements savent déjà faire : la brique est lancée au premier abonné, elle
    apprend la fin d'un émetteur par le `release` de son flux, et seul un abonné peut
    écrire. Une brique qui doit distinguer ses émetteurs donne à chacun son flux, par
    la clé de l'abonnement. Écartée, une entrée adressée à la brique sans abonnement :
    elle demandait un signal de fin d'ancrage et une règle pour une entrée qui arrive
    quand la brique est arrêtée.
  - **Ce n'est pas une action.** Une entrée informe une brique ; elle n'agit sur
    aucune source.
- **Le dépôt fournit des briques natives, toutes facultatives.** claude-code d'abord
  (`05-brique-claude-code.md`), forge (`06-brique-forge.md`), d'autres ensuite (git).
  Aucune n'est requise : le cœur n'en dépend pas, et une brique native ne sert qu'une
  fois activée par l'utilisateur. Il en va de même des clients du dépôt : ce que le
  mod central de Claude Code fait pour une brique ne se déclenche que si elle est
  activée, et son absence ne produit ni erreur ni attente (`02-clients.md`).
  - **Sous-commandes du binaire unique.** Une brique native est une sous-commande,
    `nowcast brick <nom>`, qui parle le protocole des briques sur son entrée et sa
    sortie. Elle reste un processus à part, que le service lance à partir de sa
    déclaration comme n'importe quelle autre. Il n'y a toujours qu'une chose à
    installer, et une brique native tourne toujours à la version du service, que sa
    déclaration porte aussi, reçue du workspace à la compilation. Elle est donc
    écrite en Rust.
  - **Rangement.** Chacune vit dans `bricks/<nom>/` : sa crate
    (`nowcast-brick-<nom>`), son fichier de déclaration (`brick.toml`) et ses schémas.
    Déclaration et schémas sont embarqués dans le binaire, comme les déclarations de
    types de client. Seul le binaire dépend des crates des briques.
  - **Activation explicite, par lien.** `nowcast bricks enable <nom>` écrit la
    déclaration embarquée et ses schémas dans un répertoire de données, puis pose le
    lien qui active la brique ; `nowcast bricks disable <nom>` le retire
    (`03-contrats.md`, section 2). Rien n'est actif sans ce geste.
  - **Aucun privilège dans le service.** Il trouve une brique native par le même
    répertoire de liens, la lance par la même déclaration et lui parle le même
    protocole. Deux différences seulement, hors du routage : l'outil sait activer une
    brique native par son nom, et le programme `nowcast` d'une déclaration désigne
    l'exécutable du service lui-même.
  - **Chacune passe les tests de conformité du protocole côté brique**, comme la
    brique de test.
- **Le service lance chaque brique**, au premier consommateur, et l'arrête au dernier.
  nowcast est le centre : une brique sans consommateur ne sert à rien, elle n'existe pas
  hors de lui, et aucune ne tourne pour rien. Une brique toujours active pourra devenir
  un besoin plus tard ; ce n'en est pas un aujourd'hui.
- **Un processus par brique, qui sert toutes ses clés.** La brique gère sa liste de
  clés et peut mutualiser ses appels entre elles.
- **La clé appartient à la brique ; le service ne connaît que des flux.** Le contenu
  d'un abonnement est libre et opaque pour le service, qui le transmet à la brique. La
  brique l'interprète et répond par un identifiant de flux opaque : deux abonnements qui
  visent la même chose (le même dépôt écrit de deux façons, par exemple) reçoivent le
  même identifiant. Le service ne fait que comparer ces identifiants : il envoie chaque
  publication marquée d'un identifiant à tous les abonnements rattachés à ce flux, et
  prévient la brique quand le dernier abonnement d'un flux s'en va. Ce qui est
  obligatoire dans le protocole, c'est l'identifiant de flux. Ce qu'est une clé (un ou
  plusieurs champs, la branche d'un dépôt, un lieu pour la météo) reste l'affaire de la
  brique ; une brique à cible unique répond toujours par le même identifiant.
- **La clé est documentée par le fichier de déclaration.** Pour chaque sorte
  d'abonnement, la brique y déclare un JSON Schema de son contenu, avec la description
  de chaque champ. C'est à la fois la documentation de la clé pour l'auteur d'un client
  et un contrat vérifiable. La règle qui ramène deux écritures d'une même cible au même
  flux reste dans le code de la brique ; sa documentation la décrit.
- **Canal : l'entrée et la sortie standard de la brique**, en flux d'octets, messages
  préfixés par leur longueur. Pipe, socket Unix et socket à messages donnent le même
  aller-retour à la mesure : le choix du canal ne joue pas sur la performance.
- **Le service garde un historique borné de chaque flux**, ses 100 derniers
  événements, en mémoire (le détail dans `03-contrats.md`, section 3). Un nouvel abonné
  reçoit la dernière valeur sans attendre la prochaine publication, un client à base de
  hooks peut la lire en un appel (voir `02-clients.md`), et un client qui se reconnecte
  reçoit ce qu'il a manqué. Les briques publient des suites d'événements autant que des
  états : un commit, une PR mergée, une tâche terminée ne se résument pas à la dernière
  valeur.
  - **Dans le service, pas dans les briques.** Le service tient l'historique sans
    connaître le contenu, une fois pour tous les types, et il numérote déjà les
    événements de tous les flux d'un seul compteur, ce que demande la reprise après un
    `Last-Event-ID`. Une brique qui joindrait ses derniers événements à chaque état
    publié laisserait chaque type réinventer sa fenêtre.
  - **En mémoire, pas sur disque.** Un flux ne publie que tant qu'il a un abonné : un
    historique gardé d'un arrêt du service à l'autre aurait de toute façon un trou pour
    tout ce qui s'est passé sans abonné. Le cas qui compte, un client qui se reconnecte
    pendant que sa session tourne, n'en a pas besoin. Comme les identifiants croissent
    d'une exécution à l'autre, un stockage sur disque peut s'ajouter plus tard sans
    changer le contrat.

## Ce qui existe déjà

Aucun outil trouvé ne fait ce que fait nowcast en entier. Plusieurs ont résolu une partie
du problème, et leurs choix convergent.

| Outil | Ce qu'il montre |
|---|---|
| Telegraf (plugins `execd`) | Des plugins externes, écrits dans n'importe quel langage, lancés et lus par l'hôte. |
| go-plugin (Terraform, Vault) | Chaque plugin est un sous-processus joint en RPC ; un plugin qui plante n'emporte pas l'hôte. |
| Steampipe | Environ 150 plugins sur ce modèle, avec un SDK qui fournit le serveur. |
| eww | Deux primitives : `defpoll` (une commande à intervalle) et `deflisten` (une commande lue en continu). |
| Home Assistant | Chaque intégration déclare `local_polling`, `local_push`, `cloud_polling` ou `cloud_push` ; un coordinateur partage un même polling entre tous ceux qui écoutent. |
| Varlink | Protocole d'IPC de systemd : JSON sur socket Unix, interfaces lisibles à l'exécution. |
| Nushell (plugins) | Le plugin annonce son encodage dès ses premiers octets, JSON ou MessagePack. |
| CloudEvents | Les attributs de contexte d'un événement se lisent sans décoder ses données. |

## À trancher

- **Les abonnements plus larges ou partiels** : s'abonner à tout un dépôt plutôt qu'à
  une branche, par exemple, et ce que cela implique pour le service.
