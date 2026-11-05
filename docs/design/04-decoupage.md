# Le découpage du développement

Validé le 2026-11-05. Le cœur sera développé par des agents d'IA, en parallèle autant
que possible. Chaque élément ci-dessous deviendra une issue GitHub.

## Principe

- **Les contrats sont figés d'abord.** Chaque module a une frontière écrite avant son
  code (`03-contrats.md` pour l'extérieur, des traits Rust entre modules). Un agent
  développe contre ces frontières, sans attendre les modules voisins.
- **Une crate Rust par module**, dans un workspace Cargo. Des agents en parallèle, chacun
  dans son worktree et sa crate, touchent rarement les mêmes fichiers.
- **Des faux en mémoire des voisins.** Chaque trait vient avec un faux, pour tester un
  module seul.
- **Des tests de conformité partagés par contrat.** Une brique, un client ou un module
  qui les passe respecte le contrat.

## Les vagues

```
Vague 0 (séquentielle, courte) : les contrats
  workspace Cargo + CI (fmt, clippy, tests)
  crate de types : enveloppe, identifiants, erreurs
  suites de conformance, tirées de 03-contrats.md
  traits Rust entre modules + leurs faux en mémoire, dont le trait de session
  et l'erreur d'expiration de `ready` (le 503)
        │
        ▼
Vague 1 (parallèle, chacun contre les contrats)
  ├─ codec brique : trame + MessagePack + messages
  ├─ identité du pair : SO_PEERPIDFD, remontée des ancêtres, types de client, pidfd
  ├─ routeur : flux, abonnements par ancrage, diffusion, dernière valeur, compteurs,
  │  signal de flux vide
  ├─ serveur HTTP/SSE : routes, format CloudEvents JSON (contre un faux routeur)
  ├─ découverte : déclarations des briques et des types de client (embarquées puis
  │  utilisateur), validation des schémas
  ├─ outil en ligne de commande : watch, get, subscribe, ancré sur lui-même
  │  (contre un faux serveur)
  └─ brique de test : parle le protocole, publie sur commande
        │
        ▼
Vague 2 (assemblage)
  ├─ superviseur de briques : codec + découverte + lancement et arrêt à la demande,
  │  délais d'arrêt et de backoff
  ├─ registre des sessions : identité + routeur, déclaration obligatoire, nettoyage à la mort
  └─ service : câblage, activation par socket systemd
        │
        ▼
Vague 3
  ├─ adaptateur Claude Code (TypeScript) : mod central
  ├─ adaptateur Claude Code (TypeScript) : bibliothèque pour les mods + déclaration
  │  du type de client
  └─ tests de bout en bout : vraie brique de test, vrai service, vrai client
```

La vague 0 est le seul goulot. Elle ne contient que des types, des suites de
conformance et des traits, aucune logique.

La découverte charge les déclarations de types de client, embarquées puis
utilisateur ; l'identité du pair les reçoit déjà chargées et les compare à la
chaîne d'ancêtres.

Le routeur compte les abonnés de chaque flux et signale quand un flux n'en a plus ; le
superviseur décide de lancer ou d'arrêter une brique. Les délais encore ouverts dans
`03-contrats.md` (arrêt, backoff) reviennent au superviseur, avec des valeurs par
défaut documentées.

## Risque

Une vague 0 qui s'étire bloque tout le reste. Des contrats imparfaits et versionnés
valent mieux que des contrats parfaits qui retardent la vague 1.
