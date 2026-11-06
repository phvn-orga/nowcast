# Cadrage de nowcast

État au 2026-11-07 : premières décisions prises, conception à poursuivre. Ce document
est le point de départ de la conception, pas une spécification.

## Objet

nowcast est un service local qui fournit de la donnée prête à l'emploi : l'état d'une
PR et de sa CI, un quota d'usage, des tâches, la mémoire d'un processus. Ceux qui
l'utilisent (les mods et extensions d'un agent comme Claude Code, Codex ou Droid, un
tableau de bord, toute autre application) se connectent, s'abonnent à
des briques et reçoivent leurs mises à jour en temps réel. Ils peuvent aussi adresser
des requêtes au service.

nowcast existe pour lui-même. Ses données ne sont taillées pour aucun client en
particulier : un mod ou un tableau de bord se branche dessus, comme sur n'importe quel
service externe.

Ce qu'il remplace : aujourd'hui, ce qu'une session d'agent surveille autour d'elle (PR,
CI, quotas, tâches) se calcule dans chaque session. D'où trois limites :

- **Duplication.** Dix sessions ouvertes, ce sont dix boucles de polling, dix lectures
  de la même CI et de la même liste de tâches.
- **Pas d'état partagé.** Une donnée qui appartient au compte, comme un quota d'usage,
  est tenue session par session et se perd quand la session repart de zéro.
- **Un seul consommateur.** La logique vit dans la session ; rien d'autre ne peut s'en
  servir.

## Vocabulaire

- **Service** : le processus nowcast, unique sur la machine.
- **Client** : ce qui se connecte au service.
- **Type de client** : une famille de clients (les sessions Claude Code, par exemple),
  déclarée hors du code du cœur, avec la règle qui reconnaît son processus d'ancrage.
- **Processus d'ancrage** : le processus auquel le service rattache une connexion (pour
  une session d'agent, le processus de l'agent). Ses connexions lui appartiennent, et
  tout ce qui s'y rattache s'arrête à sa mort.
- **Session** : une session d'agent, identifiée par l'identifiant que le client
  déclare. Un même processus d'ancrage peut porter plusieurs sessions à la suite.
- **Brique** : un programme externe, lancé par le service, qui surveille une source
  (une forge et sa CI, un quota, un gestionnaire de tâches, la mémoire d'un processus)
  et publie ce qu'il observe. Chacun peut en ajouter une.
- **Abonnement** : le lien entre un client et une brique. Son contenu (par exemple
  dépôt et branche) n'est lu que par la brique.
- **Flux** : ce que la brique publie pour une cible donnée. La brique rattache chaque
  abonnement à un flux par un identifiant opaque pour le service.

## Décisions prises

- **Le projet s'appelle nowcast.** En météorologie, le *nowcast* est la prévision du
  présent : l'état courant, prêt à l'emploi. Le premier nom, pulse, entrait en
  collision avec PipeWire-Pulse, dont le socket de compatibilité PulseAudio occupe déjà
  `$XDG_RUNTIME_DIR/pulse/`, et le mot évoque PulseAudio sous Linux. Au 2026-11-05,
  nowcast était libre sur crates.io, npm, Debian, Arch et AUR (scopes npm et marques
  déposées non vérifiés). Le nom vaut pour le service, le socket, l'outil en ligne de
  commande et le dépôt.
- **Indépendant de tout client.** Le protocole ne contient aucune notion propre à un
  agent. Ce qui sert un agent donné est une brique, un type de client ou un client,
  jamais le cœur.
- **Un client est connecté**, pas un simple émetteur. Il tient un flux ouvert, déclare
  sa session, s'abonne et envoie des requêtes. Le service sait à tout instant quels
  clients sont connectés.
- **Transport local : un socket Unix**, dans `$XDG_RUNTIME_DIR/nowcast/` (sous le
  répertoire privé que Linux crée pour chaque utilisateur connecté), droits `0600`. Aucun port TCP : ni le
  réseau ni une page web ouverte dans le navigateur ne peuvent l'atteindre.
- **Rattachement par l'identité du noyau.** Sur un socket Unix, le noyau donne au
  service l'UID et un `pidfd` du processus qui se connecte (`SO_PEERPIDFD`, Linux 6.5
  et plus), sans que le client puisse mentir et sans risque de confusion avec un PID
  réutilisé. Le service remonte les parents de ce processus, en revérifiant chaque
  maillon, jusqu'à un processus d'ancrage. Cette identité sert à router une connexion
  vers son ancrage, pas à authentifier (voir la limite ci-dessous).
- **Les types de client sont déclarés hors du code du cœur**, comme les briques. Chacun porte
  une règle qui reconnaît son processus d'ancrage, par exemple un motif sur le chemin
  de l'exécutable (un motif, parce que plusieurs versions d'un agent tournent en même
  temps). Le cœur ne fait qu'une remontée générique des parents et une comparaison avec
  ces règles. Ajouter un agent, c'est ajouter une déclaration. Un processus dont
  aucun ancêtre ne correspond est son propre ancrage (voir `03-contrats.md`).
- **Le dépôt fournit les déclarations des agents courants, embarquées dans le
  binaire.** Claude Code est reconnu sans configuration. Ces déclarations restent des
  données rangées à côté de leur adaptateur (`clients/claude-code/`), pas du code du
  cœur ; un fichier de l'utilisateur qui porte le même nom les remplace.
- **Un processus n'est pas une session.** Un agent peut changer de session sans que
  son processus meure : après un `/clear` ou une reprise, Claude Code continue sous un
  nouvel identifiant de session, sans événement de démarrage. Le processus est
  l'ancrage ; c'est le client qui déclare la session en cours et annonce ses
  changements. Aucun abonnement sans session déclarée, mais les abonnements suivent
  l'ancrage : un changement de session ne les coupe pas (voir `02-clients.md`).
- **Détection d'une fermeture brutale** par deux signaux, sans battement de cœur : le
  `pidfd` du processus d'ancrage (le noyau prévient à sa mort, même sur `kill -9`), et
  la coupure du flux. Un changement de session, lui, n'est connu que par le client.
- **Lancé à la demande.** L'activation par socket de systemd (unité utilisateur) tient
  le socket et ne démarre le service qu'à la première connexion. Hors usage, il n'y a
  pas de processus : le service s'arrête de lui-même après 60 s sans connexion ouverte
  ni abonnement, et systemd le relance à la connexion suivante. Cet arrêt n'est actif
  par défaut que sous activation par socket : un service lancé à la main qui
  s'arrêterait seul ne serait relancé par personne. `NOWCAST_IDLE_TIMEOUT` (en
  secondes) remplace le délai, et active l'arrêt pour un service lancé à la main.
  L'arrêt des briques et l'oubli des sessions qui s'ensuit sont décrits dans
  `03-contrats.md`.
- **Observation, pas action.** Le service écoute et publie ; il n'agit jamais sur une
  source. Les actions restent dans les clients (voir `01-briques.md`).
- **Mêmes droits pour tous les clients**, pour l'instant (voir la limite ci-dessous).
- **Briques dormantes sans abonné.** Le service compte les abonnés de chaque brique par
  flux. Il lance la brique au premier abonné et l'arrête au dernier, lui transmet chaque
  abonnement et la prévient quand un flux n'a plus d'abonné (voir `01-briques.md`).
- **Le cœur est en Rust**, et seulement en Rust : le service comme l'outil en ligne de
  commande. Les briques sont écrites dans le langage de leur choix. L'adaptateur
  Claude Code est en TypeScript, parce que les plugins s'exécutent en JavaScript dans
  le moteur de l'agent.
- **Pas de périmètre de première version.** Les fonctionnalités arrivent l'une après
  l'autre. Le cœur vient d'abord, puis une brique de test, nécessaire pour tester le
  projet ; les vraies briques viennent ensuite. Le découpage est décrit dans
  `04-decoupage.md`.
- **Côté clients, un flux SSE et des requêtes HTTP** sur le socket Unix, événements au
  format JSON de CloudEvents. L'architecture des clients (état par session dans le
  service, un flux par session, outil en ligne de commande commun, un adaptateur par
  agent) est décrite dans `02-clients.md`.
- **Distribution par binaire précompilé.** Un tag de version (`v0.1.0`) fait
  construire par la CI un binaire statique (`x86_64-unknown-linux-musl`), publié avec
  les unités systemd dans une release GitHub. Homebrew l'installe depuis une formule
  rangée dans le dépôt même (`Formula/`), sans dépôt de tap à part. Installer ne
  demande aucune compilation ; `cargo install` reste la voie du développement (voir
  `02-clients.md`).

## Limite assumée de la sécurité

Le socket privé et l'identité du noyau protègent contre les autres comptes, les autres
machines et le navigateur. Ils ne prouvent pas qu'un client est « vraiment » une session
d'agent face à un programme qui tourne sous le même utilisateur : un tel programme peut
lancer l'agent lui-même, remplacer son exécutable (il appartient à l'utilisateur) ou
s'y injecter. Aucune authentification locale ne tient contre ce cas, qui relève de la
sécurité de la machine. Pour de la lecture, le risque est faible : un tel programme
peut déjà lire directement ce que lisent les briques. Le service n'offre aucune action
sur une source ; s'il en offrait un jour d'autres que le rafraîchissement, il faudrait
limiter ce que chaque type de client a le droit de demander. Ces droits s'arrêtent à la
session : le noyau ne distingue pas deux mods d'une même session, qui partagent le
même processus.

## Premier jet : la brique forge

Un premier contrat a déjà servi dans un client et a tenu sur GitHub (`gh`) et Azure
DevOps (`az`). Il sépare ce qui est commun de ce qui est propre à chaque hébergeur :

- reconnaître un remote git de l'hébergeur (SSH, HTTPS, alias SSH) ;
- trouver la PR d'une branche, de préférence ouverte, avec un état neutre
  (`open`, `merged`, `closed`) et un drapeau brouillon ;
- lire les contrôles d'une PR, ou à défaut ceux d'un commit, sous des états neutres
  (`pass`, `fail`, `pending`, `skip`). Sur Azure DevOps, la CI d'une PR est celle de ses
  politiques de build et de statut, pas les autres (relecteurs, commentaires, stratégie
  de merge) ;
- reconnaître les appels d'outils d'un agent qui peuvent créer ou modifier une PR, pour
  relire sans attendre.

## À trancher

- **Le schéma des messages** et son versionnement. Côté briques, l'enveloppe, l'encodage
  et le canal sont décidés dans `01-briques.md` ; côté clients, voir `02-clients.md`.
- **Des droits différents selon le type de client.** Pour l'instant, tous les clients
  ont les mêmes droits. Le service n'offrant aucune action sur une source, la question
  ne se pose qu'au-delà du rafraîchissement. Les requêtes elles-mêmes sont
  décrites dans `02-clients.md`.
- **Le comportement quand le service est absent** ou en échec : il vit dans l'outil
  en ligne de commande commun aux clients (voir `02-clients.md`), reste à le définir.
- **La CI en temps réel.** Sans webhook, c'est du polling, mutualisé et mieux réglé.
  Des webhooks GitHub ou Azure DevOps demanderaient une adresse joignable depuis
  Internet, donc un tunnel.
- **Un processus orphelin** (lancé en double fork) perd sa filiation et ne remonte plus
  à son ancrage : il devient son propre ancrage et ne suit plus la session de son
  agent. À rattacher autrement si le besoin apparaît.
