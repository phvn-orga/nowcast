//! Graphics cards: `/sys/class/drm/card*` for the list, the `amdgpu` files
//! for AMD, `nvidia-smi` for NVIDIA. NVML is out of reach: the release binary
//! is static, and `dlopen` loads nothing there.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::sensors::{self, Sensor};

const AMD: u16 = 0x1002;
const NVIDIA: u16 = 0x10de;
const INTEL: u16 = 0x8086;

/// How long `nvidia-smi` has to answer.
const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// The PCI address, as sysfs writes it: `0000:65:00.0`.
    pub id: String,
    pub vendor: u16,
    pub device: Option<u16>,
    /// The real path of the card's PCI device.
    real: PathBuf,
    /// Asleep (runtime power management): reading it would wake it.
    pub suspended: bool,
}

/// The graphics cards under `root` (PCI class `0x03`), by address.
pub fn cards(root: &Path) -> Vec<Card> {
    let Ok(entries) = fs::read_dir(root.join("sys/class/drm")) else {
        return Vec::new();
    };
    let mut cards: Vec<Card> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|e| {
            let device = e.path().join("device");
            let class = fs::read_to_string(device.join("class")).ok()?;
            if !class.trim().starts_with("0x03") {
                return None;
            }
            let vendor = hex(&fs::read_to_string(device.join("vendor")).ok()?)?;
            let real = fs::canonicalize(&device).ok()?;
            Some(Card {
                id: real.file_name()?.to_string_lossy().into_owned(),
                vendor,
                device: fs::read_to_string(device.join("device"))
                    .ok()
                    .and_then(|d| hex(&d)),
                suspended: suspended(&real),
                real,
            })
        })
        .collect();
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards.dedup_by(|a, b| a.id == b.id);
    cards
}

/// Whether the device at `real` is asleep or falling asleep. A GPU that
/// has no runtime power management reads `unsupported` or has no file.
fn suspended(real: &Path) -> bool {
    fs::read_to_string(real.join("power/runtime_status"))
        .is_ok_and(|s| matches!(s.trim(), "suspended" | "suspending"))
}

fn hex(text: &str) -> Option<u16> {
    u16::from_str_radix(text.trim().strip_prefix("0x")?, 16).ok()
}

/// The device's name in `pci.ids`, the format of `/usr/share/hwdata/pci.ids`.
pub fn name_in(pci_ids: &str, vendor: u16, device: u16) -> Option<String> {
    let vendor = format!("{vendor:04x}");
    let device = format!("{device:04x}");
    let mut in_vendor = false;
    for line in pci_ids.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('\t') {
            if in_vendor
                && !rest.starts_with('\t')
                && let Some(name) = rest.strip_prefix(&device)
            {
                return Some(name.trim().to_owned());
            }
        } else if in_vendor || line.starts_with("C ") {
            // The next vendor, or the device classes at the end.
            return None;
        } else {
            in_vendor = line.starts_with(&vendor);
        }
    }
    None
}

/// What `nvidia-smi` says of one card. A value it cannot give is `None`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct NvidiaReading {
    pub name: Option<String>,
    pub usage: Option<i64>,
    pub vram_used_mib: Option<i64>,
    pub vram_total_mib: Option<i64>,
    pub temp_c: Option<i64>,
}

/// The query `nvidia-smi` answers, in this order.
pub const NVIDIA_QUERY: &str =
    "pci.bus_id,name,utilization.gpu,memory.used,memory.total,temperature.gpu";

/// Parses `nvidia-smi --query-gpu=<NVIDIA_QUERY> --format=csv,noheader,nounits`,
/// by card address in sysfs's form.
pub fn parse_nvidia_smi(output: &str) -> HashMap<String, NvidiaReading> {
    let mut readings = HashMap::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let [bus, name, usage, used, total, temp] = fields[..] else {
            continue;
        };
        let Some(id) = pci_address(bus) else {
            continue;
        };
        let number = |s: &str| s.parse::<f64>().ok().map(|n| n.round() as i64);
        readings.insert(
            id,
            NvidiaReading {
                name: (!name.is_empty() && !name.starts_with('[')).then(|| name.to_owned()),
                usage: number(usage),
                vram_used_mib: number(used),
                vram_total_mib: number(total),
                temp_c: number(temp),
            },
        );
    }
    readings
}

/// `00000000:01:00.0`, as NVIDIA writes it, in sysfs's form: `0000:01:00.0`.
fn pci_address(bus: &str) -> Option<String> {
    let (domain, rest) = bus.split_once(':')?;
    let domain = u32::from_str_radix(domain, 16).ok()?;
    let (bus, rest) = rest.split_once(':')?;
    let (device, function) = rest.split_once('.')?;
    Some(format!(
        "{domain:04x}:{:02x}:{:02x}.{:x}",
        u8::from_str_radix(bus, 16).ok()?,
        u8::from_str_radix(device, 16).ok()?,
        u8::from_str_radix(function, 16).ok()?,
    ))
}

/// Runs `nvidia-smi` from the service's `PATH`. `None` when it is missing,
/// fails or takes too long.
pub async fn run_nvidia_smi() -> Option<HashMap<String, NvidiaReading>> {
    let child = tokio::process::Command::new("nvidia-smi")
        .arg(format!("--query-gpu={NVIDIA_QUERY}"))
        .arg("--format=csv,noheader,nounits")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(NVIDIA_SMI_TIMEOUT, child)
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout)))
}

/// Whether to run `nvidia-smi`: for an NVIDIA card, unless one of them is
/// asleep, which `nvidia-smi` would wake.
pub fn wants_nvidia_smi(cards: &[Card]) -> bool {
    let mut nvidia = cards.iter().filter(|c| c.vendor == NVIDIA).peekable();
    nvidia.peek().is_some() && nvidia.all(|c| !c.suspended)
}

/// A card's entry in the detail. `name` comes from `pci.ids`. An asleep
/// card is marked `suspended` and has no measures: reading them would wake
/// it.
pub fn entry(
    card: &Card,
    name: Option<&str>,
    sensors: &[Sensor],
    nvidia: Option<&HashMap<String, NvidiaReading>>,
) -> Value {
    let mut entry = Map::new();
    entry.insert("id".into(), json!(card.id));
    let vendor = match card.vendor {
        AMD => "amd",
        NVIDIA => "nvidia",
        INTEL => "intel",
        _ => "other",
    };
    entry.insert("vendor".into(), json!(vendor));
    let nvidia = nvidia.and_then(|n| n.get(&card.id));
    if let Some(name) = name
        .map(str::to_owned)
        .or_else(|| nvidia.and_then(|n| n.name.clone()))
    {
        entry.insert("name".into(), json!(name));
    }
    if card.suspended {
        entry.insert("suspended".into(), json!(true));
        return Value::Object(entry);
    }
    let mut put = |key: &str, value: Option<i64>| {
        if let Some(value) = value {
            entry.insert(key.into(), json!(value));
        }
    };
    match card.vendor {
        AMD => {
            let read = |file: &str| -> Option<u64> {
                fs::read_to_string(card.real.join(file))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            let mib = |file: &str| read(file).map(|b| (b >> 20) as i64);
            put("usage", read("gpu_busy_percent").map(|u| u as i64));
            put("temp_c", sensors::of_device(sensors, &card.real, "edge"));
            put("vram_used_mib", mib("mem_info_vram_used"));
            put("vram_total_mib", mib("mem_info_vram_total"));
            put("gtt_used_mib", mib("mem_info_gtt_used"));
            put("gtt_total_mib", mib("mem_info_gtt_total"));
        }
        NVIDIA => {
            if let Some(n) = nvidia {
                put("usage", n.usage);
                put("temp_c", n.temp_c);
                put("vram_used_mib", n.vram_used_mib);
                put("vram_total_mib", n.vram_total_mib);
            }
        }
        _ => {}
    }
    Value::Object(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Tree;

    const PCI_IDS: &str = "\
# comment
0e11  Compaq Computer Corporation
\t0001  PCI to EISA Bridge
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t150e  Strix [Radeon 880M / 890M]
\t\t1043 1e2f  a subsystem
10de  NVIDIA Corporation
\t2684  AD102 [GeForce RTX 4090]
8086  Intel Corporation
\t150e  82580 Gigabit Network Connection
C 00  Unclassified device
";

    #[test]
    fn names_come_from_the_vendor_s_devices_only() {
        assert_eq!(
            name_in(PCI_IDS, 0x1002, 0x150e).as_deref(),
            Some("Strix [Radeon 880M / 890M]")
        );
        assert_eq!(
            name_in(PCI_IDS, 0x10de, 0x2684).as_deref(),
            Some("AD102 [GeForce RTX 4090]")
        );
        assert_eq!(name_in(PCI_IDS, 0x1002, 0x9999), None);
        assert_eq!(name_in(PCI_IDS, 0x1234, 0x150e), None);
    }

    fn card(tree: &Tree, card: &str, address: &str, class: &str, vendor: &str) -> PathBuf {
        let device = tree.dir(&format!("sys/devices/pci0000:00/{address}"));
        tree.file(&format!("sys/devices/pci0000:00/{address}/class"), class);
        tree.file(&format!("sys/devices/pci0000:00/{address}/vendor"), vendor);
        tree.file(
            &format!("sys/devices/pci0000:00/{address}/device"),
            "0x150e\n",
        );
        tree.link(&format!("sys/class/drm/{card}/device"), &device);
        device
    }

    #[test]
    fn lists_graphics_cards_and_not_their_connectors() {
        let tree = Tree::new();
        card(&tree, "card1", "0000:65:00.0", "0x030000\n", "0x1002\n");
        // A connector of the same card, and something that is no GPU.
        tree.link(
            "sys/class/drm/card1-eDP-1/device",
            &tree.root().join("sys/devices/pci0000:00/0000:65:00.0"),
        );
        card(&tree, "card2", "0000:66:00.0", "0x020000\n", "0x8086\n");
        let cards = cards(tree.root());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "0000:65:00.0");
        assert_eq!(cards[0].vendor, 0x1002);
        assert_eq!(cards[0].device, Some(0x150e));
    }

    #[test]
    fn an_amd_card_publishes_usage_memories_and_temperature() {
        let tree = Tree::new();
        let device = card(&tree, "card1", "0000:65:00.0", "0x030000\n", "0x1002\n");
        let base = "sys/devices/pci0000:00/0000:65:00.0";
        tree.file(&format!("{base}/gpu_busy_percent"), "33\n");
        tree.file(&format!("{base}/mem_info_vram_used"), "494280704\n");
        tree.file(&format!("{base}/mem_info_vram_total"), "536870912\n");
        tree.file(&format!("{base}/mem_info_gtt_used"), "3094347776\n");
        // No GTT total: absent, not zero.
        tree.sensor(
            "hwmon3",
            "amdgpu",
            Some(&device),
            &[(1, Some("edge"), "50000")],
        );
        let cards = cards(tree.root());
        let sensors = sensors::list(tree.root());
        let entry = entry(&cards[0], Some("Strix"), &sensors, None);
        assert_eq!(
            entry,
            json!({
                "id": "0000:65:00.0", "vendor": "amd", "name": "Strix",
                "usage": 33, "temp_c": 50,
                "vram_used_mib": 471, "vram_total_mib": 512, "gtt_used_mib": 2951
            })
        );
    }

    #[test]
    fn an_asleep_card_is_marked_and_not_read() {
        let tree = Tree::new();
        let device = card(&tree, "card0", "0000:01:00.0", "0x030000\n", "0x1002\n");
        let base = "sys/devices/pci0000:00/0000:01:00.0";
        tree.file(&format!("{base}/power/runtime_status"), "suspended\n");
        tree.file(&format!("{base}/gpu_busy_percent"), "0\n");
        tree.sensor(
            "hwmon0",
            "amdgpu",
            Some(&device),
            &[(1, Some("edge"), "40000")],
        );
        let cards = cards(tree.root());
        assert!(cards[0].suspended);
        let sensors = sensors::list(tree.root());
        assert_eq!(
            entry(&cards[0], Some("Navi"), &sensors, None),
            json!({ "id": "0000:01:00.0", "vendor": "amd", "name": "Navi", "suspended": true })
        );
        tree.file(&format!("{base}/power/runtime_status"), "active\n");
        assert!(!super::cards(tree.root())[0].suspended);
    }

    #[test]
    fn nvidia_smi_runs_only_with_every_nvidia_card_awake() {
        let nvidia = |suspended| Card {
            id: "0000:01:00.0".into(),
            vendor: NVIDIA,
            device: None,
            real: PathBuf::from("/nowhere"),
            suspended,
        };
        let amd = Card {
            vendor: AMD,
            ..nvidia(false)
        };
        assert!(wants_nvidia_smi(&[amd.clone(), nvidia(false)]));
        assert!(!wants_nvidia_smi(&[amd.clone(), nvidia(true)]));
        assert!(!wants_nvidia_smi(&[nvidia(false), nvidia(true)]));
        assert!(!wants_nvidia_smi(&[amd]));
    }

    #[test]
    fn nvidia_smi_output_maps_to_sysfs_addresses() {
        let output = "00000000:01:00.0, NVIDIA GeForce RTX 4090, 12, 1024, 24564, 41\n\
                      00000000:02:00.0, [N/A], [N/A], [Not Supported], 8192, 39\n\
                      garbage\n";
        let readings = parse_nvidia_smi(output);
        assert_eq!(
            readings["0000:01:00.0"],
            NvidiaReading {
                name: Some("NVIDIA GeForce RTX 4090".into()),
                usage: Some(12),
                vram_used_mib: Some(1024),
                vram_total_mib: Some(24564),
                temp_c: Some(41),
            }
        );
        assert_eq!(
            readings["0000:02:00.0"],
            NvidiaReading {
                name: None,
                usage: None,
                vram_used_mib: None,
                vram_total_mib: Some(8192),
                temp_c: Some(39),
            }
        );
        assert_eq!(readings.len(), 2);
    }

    #[test]
    fn an_nvidia_card_without_nvidia_smi_has_no_measures() {
        let card = Card {
            id: "0000:01:00.0".into(),
            vendor: NVIDIA,
            device: None,
            real: PathBuf::from("/nowhere"),
            suspended: false,
        };
        assert_eq!(
            entry(&card, None, &[], None),
            json!({ "id": "0000:01:00.0", "vendor": "nvidia" })
        );
        let readings = parse_nvidia_smi("00000000:01:00.0, RTX, 5, 10, 20, 30\n");
        assert_eq!(
            entry(&card, None, &[], Some(&readings)),
            json!({
                "id": "0000:01:00.0", "vendor": "nvidia", "name": "RTX",
                "usage": 5, "temp_c": 30, "vram_used_mib": 10, "vram_total_mib": 20
            })
        );
    }
}
