# La brique system

État au 2026-11-16 : conçue sous #75, sources vérifiées par un spike le même jour.
Brique native (`01-briques.md`).

Elle publie ce que fait la machine, en direct : CPU, mémoire, GPU, disques, réseau,
températures et pression, pour n'importe quel consommateur (la ligne d'état d'un
agent, un tableau de bord). Ce que coûte chaque session de Claude Code est l'affaire
de la brique claude-code (#76).

## Ce qu'elle publie

| Sorte | Clé | Flux | Publie | Accepte en entrée |
|---|---|---|---|---|
| `machine` | `{}` | `system/machine` | `system.machine/v1` | rien |

- **Un seul flux pour toute la machine.** Un abonnement suffit, et chaque événement
  est un instantané cohérent, tout étant lu au même échantillon. La brique lit donc
  tout dès qu'un client lit quoi que ce soit, sondes réseau comprises (voir
  « Latence et pertes »). Le reste se lit dans `/proc` et `/sys`, qui coûtent peu.
- **Un domaine coûteux aura sa propre sorte.** Les processus les plus lourds, s'ils
  viennent, demandent de parcourir tout `/proc` : ils ne grossiront pas `machine`,
  pour ne pas peser sur un client qui n'affiche que le CPU.
- **Pas de configuration, pas d'entrée, pas de portée de rafraîchissement.**

## `system.machine/v1`

```json
{
  "cpu": {
    "usage": 23,
    "cores": [12, 40, 8, 31],
    "load": [1.52, 1.10, 0.95],
    "temp_c": 54
  },
  "memory": {
    "total_mib": 30682,
    "available_mib": 16228,
    "swap_total_mib": 8191,
    "swap_used_mib": 0
  },
  "gpus": [
    {
      "id": "0000:65:00.0", "vendor": "amd", "name": "Strix [Radeon 880M / 890M]",
      "usage": 33, "temp_c": 50,
      "vram_used_mib": 471, "vram_total_mib": 512,
      "gtt_used_mib": 2951, "gtt_total_mib": 15341
    }
  ],
  "disks": [
    { "name": "nvme0n1", "read_kib_s": 0, "write_kib_s": 340, "busy": 2, "temp_c": 25 }
  ],
  "filesystems": [
    { "mount": "/", "total_mib": 934912, "available_mib": 807936 },
    { "mount": "/boot", "total_mib": 2048, "available_mib": 1740 }
  ],
  "network": {
    "interfaces": [
      {
        "name": "wlp99s0", "kind": "wifi", "link": "up",
        "rx_kib_s": 120, "tx_kib_s": 8, "errors": 0, "dropped": 0,
        "wifi": { "signal_dbm": -32, "rx_mbit_s": 206, "tx_mbit_s": 287 }
      },
      {
        "name": "eno1", "kind": "ethernet", "link": "down",
        "rx_kib_s": 0, "tx_kib_s": 0, "errors": 0, "dropped": 0
      }
    ],
    "probes": {
      "gateway": { "target": "192.168.1.1", "rtt_ms": 1, "loss": 0 },
      "internet": { "targets": ["9.9.9.9", "1.1.1.1"], "rtt_ms": 9, "loss": 0 }
    }
  },
  "pressure": {
    "cpu": { "some": 0 },
    "memory": { "some": 0, "full": 0 },
    "io": { "some": 1, "full": 0 }
  }
}
```

- **Un champ que la machine ne donne pas est absent**, jamais à zéro : un zéro est
  une mesure. Un GPU dont le pilote ne dit rien n'a que `id`, `vendor` et `name` ;
  une machine sans capteur n'a pas de `temp_c`.
- **Les débits** (`*_kib_s`) et **`busy`** se calculent entre deux échantillons,
  comme l'usage du CPU.

### CPU et mémoire

- **`cpu.usage`** est l'usage de tous les cœurs, `cores` celui de chacun, en
  pourcentage entier. **`load`** est la charge moyenne sur 1, 5 et 15 minutes
  (`/proc/loadavg`), à deux décimales comme le noyau la donne.
- **`memory`** : `available_mib` est `MemAvailable`, ce que le système peut donner
  sans swapper. La mémoire utilisée vaut `total_mib - available_mib` (le calcul de
  `sysinfo`) ; elle n'est pas publiée à part. Une machine sans swap publie
  `swap_total_mib` à `0`.

### GPU

- **`gpus`** a une entrée par carte graphique (`/sys/class/drm/card*`, classe PCI
  `0x03`), identifiée par son adresse PCI, vide sans GPU. `vendor` vaut `amd`,
  `nvidia`, `intel` ou `other` ; `name` vient de `/usr/share/hwdata/pci.ids`, absent
  s'il n'y figure pas. `usage` est l'occupation du GPU en pourcentage entier.
- **`gtt`** est la mémoire système que le GPU s'est réservée. Sur un GPU intégré, la
  VRAM n'est qu'une petite réserve fixe et l'essentiel passe par la GTT : sur la
  machine de développement (Radeon 890M, 2026-11-16), 512 Mo de VRAM presque pleins
  et 2,9 Go de GTT. Un GPU dédié a surtout de la VRAM.

### Disques et systèmes de fichiers

- **`disks`** a une entrée par disque physique : un périphérique de `/sys/block` dont
  le chemin réel n'est pas sous `/sys/devices/virtual`, ce qui écarte les `loop` (les
  snaps) et les `dm` (LVM, chiffrement). `read_kib_s` et `write_kib_s` viennent des
  secteurs lus et écrits (`/sys/block/<disque>/stat`, secteurs de 512 octets).
  `busy` est la part du temps où le disque avait une opération en cours, en
  pourcentage entier (le `%util` d'`iostat`).
- **`filesystems`** a une entrée par périphérique bloc monté, au premier point de
  montage (`/proc/self/mountinfo`), hors `squashfs` (les snaps, toujours pleins).
  `available_mib` est ce qu'un utilisateur sans privilège peut encore écrire
  (`f_bavail` de `statvfs`), ce que montre `df`. Un système de fichiers ne se relie
  pas à son disque : sous LVM, il en est séparé par un `dm`.

### Réseau

- **`interfaces`** a une entrée par interface physique (`/sys/class/net/<nom>/device`
  existe), ce qui écarte `lo`, les ponts et les `veth` de Docker. `kind` vaut `wifi`
  quand l'interface a un répertoire `wireless`, `ethernet` sinon. `link` reprend
  `operstate` (`up`, `down`, ou la valeur du noyau).
- **`rx_kib_s` et `tx_kib_s`** sont le débit que la machine utilise, reçu et envoyé,
  pas celui dont elle dispose. **`errors` et `dropped`** comptent les paquets en
  erreur et les paquets perdus pendant le dernier échantillon, dans les deux sens
  (`statistics/` de l'interface).
- **`wifi`** est la liaison radio de l'interface connectée : `signal_dbm`, la
  moyenne que tient le pilote (`SignalAvg`, moins agitée que le signal du dernier
  paquet), et `rx_mbit_s`, `tx_mbit_s`, le débit que la radio a négocié avec le
  point d'accès. Lu par netlink (nl80211), sans rien émettre. Absent sans liaison.

### Latence et pertes

Un débit nul ne dit pas si la connexion a sauté ou si la machine ne télécharge rien.
Les sondes le disent, et situent la panne : une passerelle qui répond mal désigne le
Wi-Fi ou le câble, une passerelle saine avec internet en perte désigne le
fournisseur.

- **Deux sondes**, `gateway` (la passerelle de la route par défaut,
  `/proc/net/route`) et `internet`. Chaque cible reçoit un ping ICMP par
  échantillon, attendu 1 s au plus.
- **`internet` vise deux hôtes fixes**, les résolveurs publics 9.9.9.9 (Quad9) et
  1.1.1.1 (Cloudflare), d'opérateurs différents. Un échantillon compte comme
  répondu dès que l'un des deux répond, avec le temps du plus rapide : l'incident
  d'un seul opérateur ne se lit pas comme une coupure d'internet. Les hôtes ne se
  configurent pas ; ils le deviendront si un réseau les bloque ou si le besoin
  apparaît.
- **La brique émet du trafic** : trois pings d'une centaine d'octets toutes les
  2 s, tant que le flux a un abonné. C'est sa seule action vers l'extérieur.
- **IPv4 seulement.** Une panne du seul IPv6 (une box qui le perd) ne se voit pas :
  la machine de développement n'a pas d'IPv6 (ni adresse publique, ni route par
  défaut, 2026-11-16), et rien ne permettait d'y tester des sondes IPv6.
- **Un VPN actif est traversé par la sonde `internet`**, pas par `gateway` : les
  VPN courants (WireGuard, OpenVPN, Tailscale) laissent la route par défaut de la
  table principale vers la box et font passer le reste par le tunnel (non vérifié,
  pas de VPN sur la machine de développement). Une passerelle saine avec internet en
  perte peut alors venir du VPN, pas du fournisseur. Le tunnel lui-même n'est pas
  publié : son trafic se lit, chiffré, sur l'interface physique.
- **`rtt_ms`** est la médiane des échantillons répondus sur les 30 dernières
  secondes, en millisecondes entières, absente si aucun n'a répondu. **`loss`** est
  la part des échantillons sans réponse sur la même fenêtre, en pourcentage entier.
- **Sans privilège.** La brique ouvre un socket ICMP non privilégié
  (`SOCK_DGRAM`, `IPPROTO_ICMP`), permis quand le groupe du service est dans
  `net.ipv4.ping_group_range`. systemd ouvre ce réglage à tous les groupes
  (`/usr/lib/sysctl.d/50-default.conf`) ; une distribution qui le referme rend la
  sonde absente, comme l'absence de route par défaut.
- **Vérifié le 2026-11-16** depuis un binaire statique lancé par `systemd-run
  --user`, comme le service : 5 pings sur 5 vers la passerelle (environ 1 ms),
  1.1.1.1 (21 ms) et 9.9.9.9 (8,5 ms).

### Températures

| Où | Capteur (`/sys/class/hwmon`) |
|---|---|
| `cpu.temp_c` | `k10temp`, `Tctl` (AMD) ; `coretemp`, `Package id 0` (Intel) |
| `gpus[].temp_c` | `amdgpu`, `edge`, relié à la carte par le `device` du capteur ; NVIDIA par `nvidia-smi` |
| `disks[].temp_c` | `nvme`, `Composite`, relié au disque par son contrôleur |

En degrés entiers. Les liens entre capteurs et périphériques sont vérifiés sur la
machine de développement (2026-11-16) pour AMD et NVMe ; `coretemp` ne l'est pas.

**La brique ne lit que ces capteurs.** Certains interrogent le matériel à chaque
lecture : mesuré le 2026-11-16, 1 ms par lecture pour le NVMe, 1 à 2,5 ms pour ceux
de la mémoire (`spd5118`), de la puce Wi-Fi et de l'ACPI, contre 0,02 ms pour
`k10temp` et `amdgpu` une fois lus. Lire tous les capteurs à chaque échantillon
coûterait une dizaine de millisecondes.

### Pression

**`pressure`** est la part du temps où des tâches ont attendu une ressource, sur les
10 dernières secondes (`avg10` de `/proc/pressure/*`), en pourcentage entier.
`some` : au moins une tâche attendait ; `full` : toutes les tâches attendaient. Elle
dit si la machine manque d'une ressource, ce que les pourcentages d'usage ne disent
pas. `cpu` n'a que `some` : au niveau du système, le noyau rend `full` toujours nul.
Sans `/proc/pressure` (noyau sans PSI), `pressure` est absent.

## Les sources

| Domaine | Source |
|---|---|
| CPU, mémoire, swap, charge | `sysinfo`, sans ses fonctions par défaut, avec `system` seulement |
| GPU AMD | `/sys/class/drm/card*/device/` : `gpu_busy_percent`, `mem_info_vram_used`, `mem_info_vram_total`, `mem_info_gtt_used`, `mem_info_gtt_total` |
| GPU NVIDIA | `nvidia-smi --query-gpu=pci.bus_id,name,utilization.gpu,memory.used,memory.total,temperature.gpu --format=csv,noheader,nounits` |
| Disques | `/sys/block/<disque>/stat` |
| Systèmes de fichiers | `/proc/self/mountinfo`, `statvfs` |
| Interfaces | `/sys/class/net/<nom>/` (`operstate`, `statistics/`) |
| Wi-Fi | nl80211, par `wl-nl80211` (0.9.0, du projet rust-netlink) |
| Sondes | socket ICMP non privilégié ; passerelle par `/proc/net/route` |
| Températures | `/sys/class/hwmon/hwmon*/` |
| Pression | `/proc/pressure/{cpu,memory,io}` |

- **Le GPU ne passe pas par `sysinfo`.** Sa fonction `gpu` lit sous Linux les mêmes
  fichiers AMD, sans la GTT, et atteint NVIDIA et les autres par `dlopen` de
  `libnvidia-ml.so.1` et `libvulkan.so.1` (lu dans les sources de la 0.40.3). Le
  binaire publié est statique (musl), où `dlopen` ne charge rien : vérifié le
  2026-11-16 avec le conteneur de la release, `libvulkan.so.1` et même `libc.so.6`
  répondent « Dynamic loading not supported », alors que le même programme lié à la
  glibc charge `libvulkan.so.1`. `nvml-wrapper` a la même limite.
- **NVIDIA passe par `nvidia-smi`**, lancé à chaque échantillon quand une carte
  NVIDIA (vendeur `0x10de`) est présente et que le programme est dans le `PATH` du
  service. Sans lui, la carte est listée sans mesures. Un processus de plus toutes les
  2 s.
- **Intel et les autres** sont listés sans mesures : le pilote i915 n'expose pas
  d'occupation simple dans `/sys`.
- **Disques, réseau et températures se lisent directement**, sans les fonctions
  `disk`, `network` et `component` de `sysinfo` : il faut relier chaque capteur à
  son périphérique, et écarter les périphériques virtuels selon les règles plus haut.
- **Pas d'`iw`** pour le Wi-Fi : sa documentation demande de ne pas analyser sa
  sortie.
- **Vérifié le 2026-11-16, binaire statique lancé par `systemd-run --user`** :
  `sysinfo` avec `system` seul compile pour musl, et un échantillon CPU et mémoire
  lui coûte 0,2 à 0,3 ms ; `wl-nl80211` rend le signal, sa moyenne et les deux
  débits de la station connectée, en 1 à 3 ms par lecture. Il tire `tokio`, déjà
  dans les briques.

## L'échantillonnage

- **Toutes les 2 s, à rythme fixe**, tant que le flux a un abonné. Rien ne se
  règle : un client qui veut moins souvent ignore des événements.
- **Les usages et les débits demandent deux lectures** espacées d'au moins 200 ms (le
  minimum de `sysinfo` pour le CPU). La première publication part à la deuxième
  lecture, 200 ms après l'ouverture du flux ; les suivantes toutes les 2 s. Les
  sondes n'ont alors qu'une réponse.
- **Publié seulement quand le détail change.** Les valeurs sont arrondies (entiers,
  sauf la charge à deux décimales) et la brique compare le détail à celui qu'elle a
  publié en dernier. Le détail ne porte pas d'instant de lecture, qui le changerait à
  chaque fois : tant que le flux vit sans `flow-error`, l'absence d'événement veut
  dire que rien n'a bougé, et le `time` de l'événement date le dernier changement.
- **Sur une machine de bureau, presque chaque échantillon change.** Mesuré le
  2026-11-16 sur la machine de développement, 10 lectures à 2 s d'écart :
  `MemAvailable` bougeait de 30 à 950 Mio, l'occupation du GPU de 0 à 4 points. La
  comparaison n'épargne des publications que sur une machine calme. À 2 s, les 100
  événements que le service garde par flux couvrent alors un peu plus de 3 minutes.
- **Un `refresh` fait lire et publier aussitôt**, même sans changement, comme pour
  les autres briques.

## Rangement

`bricks/system/` : la crate `nowcast-brick-system`, `brick.toml` et les schémas
(`subscription-machine.json`, un objet vide sans propriété permise, et
`system-machine-v1.json`). Elle passe les tests de conformité du protocole côté
brique, comme les autres briques natives.

## À trancher

- **Des hôtes de sonde configurables**, si les hôtes fixes posent un problème un
  jour (un réseau qui bloque l'ICMP vers l'extérieur, par exemple).
- **Les tunnels VPN** (WireGuard, `tun`), publiés avec un `kind` à `vpn`, si leur
  débit ou leur état manque un jour.
- **Des sondes IPv6**, actives quand la machine a une route IPv6 par défaut, à
  concevoir sur une machine qui en a une.
- **Les processus les plus lourds** (une sorte à part, plus haut).
- **Les limites d'un cgroup** : une machine dont le service tourne dans un conteneur
  publie la mémoire de l'hôte, pas la sienne.

## À vérifier à l'implémentation

Ce que la machine de développement (CPU et GPU intégré AMD, NVMe, Wi-Fi) ne
permettait pas de vérifier :

- `nvidia-smi` : les champs de la requête, leur forme sans unités, et le coût d'un
  lancement toutes les 2 s.
- `gpu_busy_percent` sur un GPU AMD dédié en veille (runtime PM) : la lecture peut
  le réveiller ou échouer. Un GPU intégré reste `active`.
- Les capteurs Intel (`coretemp`), et ceux des GPU dédiés AMD, qui ont aussi
  `junction` et `mem`.
- Si la lecture de la température du NVMe, qui interroge le disque, l'empêche
  d'entrer dans ses états de basse consommation.
