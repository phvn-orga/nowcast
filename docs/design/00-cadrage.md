# Cadrage de pulse

État au 2026-11-04 : premières décisions prises, conception à poursuivre. Ce document
est le point de départ de la conception, pas une spécification.

## Objet

pulse est un service local qui fournit de la donnée prête à l'emploi : l'état d'une
PR et de sa CI, un quota d'usage, des tâches, la mémoire d'un processus. Ceux qui
l'utilisent (les mods et extensions d'un agent comme Claude Code, Codex ou Droid, un
tableau de bord, toute autre application) se connectent, s'authentifient, s'abonnent à
des briques et reçoivent leurs mises à jour en temps réel. Ils peuvent aussi adresser
des requêtes au service.

pulse existe pour lui-même. Ses données ne sont taillées pour aucun client en
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

- **Service** : le processus pulse, unique sur la machine.
- **Client** : ce qui se connecte au service. Une **session** est un client qui
  représente une session d'agent.
- **Brique** : un module du service qui surveille une source (une forge et sa CI, un
  quota, un gestionnaire de tâches, la mémoire d'un processus) et publie ce qu'il
  observe. Chacun peut en ajouter une.
- **Abonnement** : le lien entre un client et une brique, avec une clé (par exemple
  dépôt et branche).

## Décisions prises

- **Indépendant de tout client.** Le protocole ne contient aucune notion propre à un
  agent. Ce qui sert un agent donné est une brique ou un client, jamais le cœur.
- **Un client est connecté**, pas un simple émetteur. Il ouvre une connexion,
  s'authentifie, s'abonne, reçoit un flux et peut envoyer des requêtes. Le service sait
  à tout instant quels clients sont connectés.
- **Transport local : un socket Unix**, dans `$XDG_RUNTIME_DIR` (répertoire privé que
  Linux crée pour chaque utilisateur connecté), droits `0600`. Aucun port TCP : ni le
  réseau ni une page web ouverte dans le navigateur ne peuvent l'atteindre.
- **Authentification par l'identité du noyau.** Sur un socket Unix, le noyau donne au
  service le PID et l'UID du processus qui se connecte (`SO_PEERCRED`), sans que le
  client puisse mentir. En remontant ses processus parents, le service retrouve le
  processus de l'agent, donc la session.
- **Détection d'une fermeture brutale** par deux signaux, sans battement de cœur :
  un `pidfd` ouvert sur le processus de la session (le noyau prévient à sa mort, même
  sur `kill -9`, sans risque de confusion avec un PID réutilisé), et la coupure de la
  connexion du flux.
- **Lancé à la demande.** L'activation par socket de systemd (unité utilisateur) tient
  le socket et ne démarre le service qu'à la première connexion. Hors usage, il n'y a
  pas de processus.
- **Briques dormantes sans abonné.** Chaque brique compte ses abonnés par clé. Elle
  démarre au premier et s'arrête au dernier.

## Limite assumée de la sécurité

Le socket privé et l'identité du noyau protègent contre les autres comptes, les autres
machines et le navigateur. Ils ne prouvent pas qu'un client est « vraiment » une session
d'agent face à un programme qui tourne sous le même utilisateur : un tel programme peut
lancer l'agent lui-même ou lire ce qu'il lit. Aucune authentification locale ne tient
contre ce cas, qui relève de la sécurité de la machine. Conséquence : plus le service
offre d'actions (ouvrir un port, lancer une commande), plus il faut limiter ce que
chaque client a le droit de demander.

## Contraintes d'un client Claude Code

Vérifiées le 2026-11-04 dans les types de l'API plugin de Claude Code 2.1.318.

- Le contexte, le cache, le modèle et l'effort d'une session n'existent que dans le
  moteur, qui les donne aux plugins par ses événements. pulse ne peut pas les lire
  lui-même : le client les lui transmet.
- `$.http.fetch` sait passer par un socket Unix (`socketPath`), mais ne rend la réponse
  qu'une fois complète. Il ne lit pas un flux.
- `$.process.spawn` lit la sortie d'un processus au fil de l'eau. Un client comme
  `curl -N` sur le flux du service suffit donc à recevoir des événements en continu.

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

- **Le protocole temps réel** : SSE (Server-Sent Events, une connexion HTTP ouverte où
  le serveur envoie des événements au fil de l'eau) sur le socket Unix est la piste la
  plus simple côté client Claude Code. WebSocket, gRPC ou un protocole maison restent à
  comparer.
- **Le schéma des messages** et son versionnement entre le service et ses clients.
- **Les requêtes qu'un client peut adresser** au service, et les droits de chacun.
- **Le comportement quand le service est absent** ou en échec, côté client.
- **Le langage.** Rust est le choix souhaité pour un démon léger. À confirmer.
- **Le périmètre d'une première version** : quelles briques, quels clients.
- **La CI en temps réel.** Sans webhook, c'est du polling, mutualisé et mieux réglé.
  Des webhooks GitHub ou Azure DevOps demanderaient une adresse joignable depuis
  Internet, donc un tunnel.
- **L'installation et la distribution** (paquet, unité systemd fournie).
- **Les autres agents.** Non instruit ici : si Codex ou Droid offrent de quoi
  brancher un client.
