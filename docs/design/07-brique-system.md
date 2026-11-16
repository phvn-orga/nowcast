# La brique system

État au 2026-11-16 : conçue sous #75. Brique native (`01-briques.md`).

Elle publie ce que fait la machine, en direct : CPU, mémoire, GPU et pression, pour
n'importe quel consommateur (la ligne d'état d'un agent, un tableau de bord). Ce que
coûte chaque session de Claude Code est l'affaire de la brique claude-code (#76).

## Ce qu'elle publie

| Sorte | Clé | Flux | Publie | Accepte en entrée |
|---|---|---|---|---|
| `machine` | `{}` | `system/machine` | `system.machine/v1` | rien |

- **Un seul flux pour toute la machine.** Un abonnement suffit, et chaque événement
  est un instantané cohérent : CPU, mémoire, GPU et pression lus au même
  échantillon. La brique lit donc tout dès qu'un client lit quoi que ce soit ; ces
  lectures sont des fichiers de `/proc` et de `/sys`, qui coûtent peu.
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
    "load": [1.52, 1.10, 0.95]
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
      "usage": 33,
      "vram_used_mib": 471, "vram_total_mib": 512,
      "gtt_used_mib": 2951, "gtt_total_mib": 15341
    }
  ],
  "pressure": {
    "cpu": { "some": 0 },
    "memory": { "some": 0, "full": 0 },
    "io": { "some": 1, "full": 0 }
  }
}
```

- **Un champ que la machine ne donne pas est absent**, jamais à zéro : un zéro est
  une mesure. Un GPU dont le pilote ne dit rien n'a que `id`, `vendor` et `name`.
- **`cpu.usage`** est l'usage de tous les cœurs, `cores` celui de chacun, en
  pourcentage entier. **`load`** est la charge moyenne sur 1, 5 et 15 minutes
  (`/proc/loadavg`), à deux décimales comme le noyau la donne.
- **`memory`** : `available_mib` est `MemAvailable`, ce que le système peut donner
  sans swapper. La mémoire utilisée vaut `total_mib - available_mib` (le calcul de
  `sysinfo`) ; elle n'est pas publiée à part. Une machine sans swap publie
  `swap_total_mib` à `0`.
- **`gpus`** a une entrée par carte graphique (`/sys/class/drm/card*`, classe PCI
  `0x03`), identifiée par son adresse PCI, vide sans GPU. `vendor` vaut `amd`,
  `nvidia`, `intel` ou `other` ; `name` vient de `/usr/share/hwdata/pci.ids`, absent
  s'il n'y figure pas. `usage` est l'occupation du GPU en pourcentage entier.
- **`gtt`** est la mémoire système que le GPU s'est réservée. Sur un GPU intégré, la
  VRAM n'est qu'une petite réserve fixe et l'essentiel passe par la GTT : sur la
  machine de développement (Radeon 890M, 2026-11-16), 512 Mo de VRAM presque pleins
  et 2,9 Go de GTT. Un GPU dédié a surtout de la VRAM.
- **`pressure`** est la part du temps où des tâches ont attendu une ressource, sur
  les 10 dernières secondes (`avg10` de `/proc/pressure/*`), en pourcentage entier.
  `some` : au moins une tâche attendait ; `full` : toutes les tâches attendaient. Elle
  dit si la machine manque d'une ressource, ce que les pourcentages d'usage ne disent
  pas. `cpu` n'a que `some` : au niveau du système, le noyau rend `full` toujours nul.
  Sans `/proc/pressure` (noyau sans PSI), `pressure` est absent.

## Les sources

| Domaine | Source |
|---|---|
| CPU, mémoire, swap, charge | `sysinfo`, sans ses fonctions par défaut, avec `system` seulement |
| GPU AMD | `/sys/class/drm/card*/device/` : `gpu_busy_percent`, `mem_info_vram_used`, `mem_info_vram_total`, `mem_info_gtt_used`, `mem_info_gtt_total` |
| GPU NVIDIA | `nvidia-smi --query-gpu=pci.bus_id,name,utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits` |
| Pression | `/proc/pressure/{cpu,memory,io}` |

- **Le GPU ne passe pas par `sysinfo`.** Sa fonction `gpu` lit sous Linux les mêmes
  fichiers AMD, sans la GTT, et atteint NVIDIA et les autres par `dlopen` de
  `libnvidia-ml.so.1` et `libvulkan.so.1` (lu dans les sources de la 0.40.3). Le
  binaire publié est statique (musl), où `dlopen` ne charge rien. `nvml-wrapper` a la
  même limite.
- **NVIDIA passe par `nvidia-smi`**, lancé à chaque échantillon quand une carte
  NVIDIA (vendeur `0x10de`) est présente et que le programme est dans le `PATH` du
  service. Sans lui, la carte est listée sans mesures. Un processus de plus toutes les
  2 s.
- **Intel et les autres** sont listés sans mesures : le pilote i915 n'expose pas
  d'occupation simple dans `/sys`.

## L'échantillonnage

- **Toutes les 2 s, à rythme fixe**, tant que le flux a un abonné. Rien ne se
  règle : un client qui veut moins souvent ignore des événements.
- **L'usage du CPU demande deux lectures** espacées d'au moins 200 ms (le minimum de
  `sysinfo`). La première publication part à la deuxième lecture, 200 ms après
  l'ouverture du flux ; les suivantes toutes les 2 s.
- **Publié seulement quand le détail change.** Les valeurs sont arrondies (pourcentages
  entiers, mémoire au Mio, charge à deux décimales) et la brique compare le détail à
  celui qu'elle a publié en dernier. Le détail ne porte pas d'instant de lecture, qui
  le changerait à chaque fois : tant que le flux vit sans `flow-error`, l'absence
  d'événement veut dire que rien n'a bougé, et le `time` de l'événement date le
  dernier changement.
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

- **D'autres domaines**, si le besoin apparaît : disques, réseau, températures, les
  processus les plus lourds (une sorte à part, plus haut).
- **Les limites d'un cgroup** : une machine dont le service tourne dans un conteneur
  publie la mémoire de l'hôte, pas la sienne.

## À vérifier à l'implémentation

- Que `dlopen` échoue bien dans le binaire statique musl : c'est ce qui écarte NVML,
  non testé.
- `nvidia-smi` : les champs de la requête, leur forme sans unités, et le coût d'un
  lancement toutes les 2 s. Aucune machine NVIDIA n'était disponible.
- `gpu_busy_percent` sur un GPU AMD en veille (runtime PM) : la lecture peut le
  réveiller ou échouer.
