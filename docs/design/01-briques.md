# Les briques

État au 2026-11-05 : modèle d'extension, contrat et transport des briques posés, socle
des briques à concevoir.

## Décisions prises

- **Une brique est un programme externe au service.** Elle vit dans son propre dépôt,
  dans le langage de son choix. Le dépôt de pulse en fournit quelques-unes, sans
  privilège : le service ne distingue pas une brique de son dépôt d'une brique écrite
  ailleurs.
- **Le service est un relais.** Il met en relation briques et clients, sans imposer de
  règle sur la façon dont une brique obtient sa donnée. Fréquence, protocole et
  authentification auprès de la source sont l'affaire de la brique.
- **Le service peut offrir un socle aux briques**, dont elles se servent si elles le
  veulent. Premier exemple : un polling HTTP générique, une requête toutes les N
  secondes, N choisi par la brique. Ce socle s'arrête où commence le spécifique d'une
  source : un WebSocket authentifié ou le protocole propre à un outil restent dans la
  brique.
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
- **Enveloppe en MessagePack, détail (les octets du JSON) porté en `bin`.** Préféré à un en-tête binaire fait
  main parce que c'est un standard. Mesure du 2026-11-04 : lire l'enveloppe coûte 46 ns
  en MessagePack contre 12 ns pour un en-tête binaire, deux coûts négligeables face à un
  aller-retour de 3 à 6 µs. En lignes JSON, ce coût croît avec le détail (2,2 µs à 5 Ko).
- **Le service lance chaque brique**, au premier consommateur, et l'arrête au dernier.
  pulse est le centre : une brique sans consommateur ne sert à rien, elle n'existe pas
  hors de lui.
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
- **Canal : l'entrée et la sortie standard de la brique**, en flux d'octets, messages
  préfixés par leur longueur. Pipe, socket Unix et socket à messages donnent le même
  aller-retour à la mesure : le choix du canal ne joue pas sur la performance.
- **Le service garde la dernière valeur de chaque flux.** Un nouvel abonné la reçoit
  sans attendre la prochaine publication, et un client à base de hooks peut la lire en
  un appel (voir `02-clients.md`).

## Ce qui existe déjà

Aucun outil trouvé ne fait ce que fait pulse en entier. Plusieurs ont résolu une partie
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

- **Les briques qui ne savent pas parler MessagePack** (un script shell) : un mode en
  lignes JSON, plus lent, annoncé au démarrage comme chez Nushell, ou un adaptateur qui
  traduit pour elles.
- **Où vit le socle des briques.** Trois emplacements, non exclusifs :
  - *Dans le service.* Une brique se déclare dans un fichier (URL, fréquence, filtre) et
    le service exécute le polling. Aucun code à écrire, mais le service exécute alors la
    logique d'une brique.
  - *Dans une bibliothèque* que la brique importe : boucle de polling, attente
    croissante après erreur, détection de changement. La brique reste un programme ;
    il faut une bibliothèque par langage.
  - *Dans des briques génériques*, livrées par le dépôt et paramétrées par instance
    (une brique de polling HTTP, par exemple). Le service reste un relais pur.
- **Les signaux que le service envoie à une brique en marche**, en plus de
  « abonnement transmis » et « flux abandonné » : un rafraîchissement immédiat demandé
  par un client (relire une PR qui vient de changer), et ce qui manquerait encore.
- **Décrire la clé pour les auteurs de briques** : ce que peut contenir un abonnement,
  et comment une brique en tire son identifiant de flux.
- **Les abonnements plus larges ou partiels** : s'abonner à tout un dépôt plutôt qu'à
  une branche, par exemple, et ce que cela implique pour le service.
- **Un détail invalide** : ce que fait le service d'une publication dont le détail
  n'est pas du JSON valide (l'écarter, le journaliser, prévenir les abonnés, signaler
  la brique).
- **Comment une brique se fait connaître** du service : par un fichier de déclaration.
  Restent à fixer l'endroit où il vit et ce qu'il contient.
