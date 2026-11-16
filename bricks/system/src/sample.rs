//! One sample of the machine: the `system.machine/v1` detail. Usages and
//! rates compare two readings, so the sampler keeps the previous one.

use std::collections::HashMap;
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::probe::{self, Ping, Window};
use crate::{disk, gpu, net, pressure, sensors};

/// Where distributions keep `pci.ids`, relative to the root.
const PCI_IDS: [&str; 2] = ["usr/share/hwdata/pci.ids", "usr/share/misc/pci.ids"];

/// The counters of the previous reading.
struct Previous {
    at: Instant,
    disks: HashMap<String, disk::Counters>,
    interfaces: HashMap<String, net::Counters>,
}

pub struct Sampler {
    root: PathBuf,
    /// `None` when `sysinfo` cannot start: the CPU and memory are absent.
    system: Option<sysinfo::System>,
    previous: Option<Previous>,
    /// GPU names by PCI address, looked up once in `pci.ids`.
    names: HashMap<String, Option<String>>,
    pci_ids: Option<Option<String>>,
    gateway: Option<Ipv4Addr>,
    gateway_window: Window,
    internet_window: Window,
    seq: u16,
}

impl Sampler {
    /// A sampler of the machine whose `/proc` and `/sys` are under `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            system: sysinfo::System::new().ok(),
            previous: None,
            names: HashMap::new(),
            pci_ids: None,
            gateway: None,
            gateway_window: Window::default(),
            internet_window: Window::default(),
            seq: 0,
        }
    }

    /// Takes a reading that publishes nothing, for the usages and rates of
    /// the next sample.
    pub fn prime(&mut self) {
        if let Some(system) = &mut self.system {
            system.refresh_cpu_usage();
        }
        self.previous = Some(self.counters().2);
    }

    fn counters(&self) -> (Vec<disk::Disk>, Vec<net::Interface>, Previous) {
        let disks = disk::disks(&self.root);
        let interfaces = net::interfaces(&self.root);
        let previous = Previous {
            at: Instant::now(),
            disks: disks.iter().map(|d| (d.name.clone(), d.counters)).collect(),
            interfaces: interfaces
                .iter()
                .filter_map(|i| Some((i.name.clone(), i.counters?)))
                .collect(),
        };
        (disks, interfaces, previous)
    }

    /// Reads the machine. The probes, `nvidia-smi` and the Wi-Fi link run
    /// together, so a sample takes the longest of their timeouts at most:
    /// `nvidia-smi`'s 1.5 s.
    pub async fn sample(&mut self) -> Value {
        let mut detail = Map::new();
        let sensors = sensors::list(&self.root);
        detail.insert("cpu".into(), self.cpu(&sensors));
        detail.insert("memory".into(), self.memory());

        let (disks, interfaces, current) = self.counters();
        let previous = self.previous.replace(current);
        let elapsed = previous
            .as_ref()
            .map(|p| self.previous.as_ref().unwrap().at.duration_since(p.at));

        let cards = gpu::cards(&self.root);
        let nvidia = async {
            if gpu::any_nvidia(&cards) {
                gpu::run_nvidia_smi().await
            } else {
                None
            }
        };
        let wifi = futures::future::join_all(interfaces.iter().map(|i| async {
            match i.index {
                Some(index) if i.wifi && i.link == "up" => net::wifi(index).await,
                _ => None,
            }
        }));
        let (nvidia, wifi, probes) = tokio::join!(nvidia, wifi, self.probe());

        let gpus: Vec<Value> = cards
            .iter()
            .map(|card| {
                let name = self.name(card);
                gpu::entry(card, name.as_deref(), &sensors, nvidia.as_ref())
            })
            .collect();
        detail.insert("gpus".into(), Value::Array(gpus));

        let disks: Vec<Value> = disks
            .iter()
            .map(|d| {
                let before = previous.as_ref().and_then(|p| p.disks.get(&d.name));
                let temp = d
                    .device
                    .as_deref()
                    .and_then(|device| sensors::of_device(&sensors, device, "Composite"));
                disk_entry(d, before.zip(elapsed), temp)
            })
            .collect();
        detail.insert("disks".into(), Value::Array(disks));
        detail.insert("filesystems".into(), self.filesystems());

        let interfaces: Vec<Value> = interfaces
            .iter()
            .zip(wifi)
            .map(|(i, wifi)| {
                let before = previous.as_ref().and_then(|p| p.interfaces.get(&i.name));
                interface_entry(i, before.zip(elapsed), wifi)
            })
            .collect();
        let mut network = Map::new();
        network.insert("interfaces".into(), Value::Array(interfaces));
        if let Some(probes) = probes {
            network.insert("probes".into(), probes);
        }
        detail.insert("network".into(), Value::Object(network));

        if let Some(pressure) = pressure::read(&self.root) {
            detail.insert("pressure".into(), pressure);
        }
        Value::Object(detail)
    }

    fn cpu(&mut self, sensors: &[sensors::Sensor]) -> Value {
        let mut cpu = Map::new();
        if let Some(system) = &mut self.system {
            system.refresh_cpu_usage();
            cpu.insert("usage".into(), json!(percent(system.global_cpu_usage())));
            let cores: Vec<i64> = system.cpus().iter().map(|c| percent(c.usage())).collect();
            cpu.insert("cores".into(), json!(cores));
        }
        if let Ok(load) = sysinfo::System::load_average() {
            let two = |x: f64| (x * 100.0).round() / 100.0;
            cpu.insert(
                "load".into(),
                json!([two(load.one), two(load.five), two(load.fifteen)]),
            );
        }
        if let Some(temp) = sensors::cpu(sensors) {
            cpu.insert("temp_c".into(), json!(temp));
        }
        Value::Object(cpu)
    }

    fn memory(&mut self) -> Value {
        let Some(system) = &mut self.system else {
            return json!({});
        };
        system.refresh_memory();
        json!({
            "total_mib": system.total_memory() >> 20,
            "available_mib": system.available_memory() >> 20,
            "swap_total_mib": system.total_swap() >> 20,
            "swap_used_mib": system.used_swap() >> 20,
        })
    }

    fn filesystems(&self) -> Value {
        let Ok(mountinfo) = fs::read_to_string(self.root.join("proc/self/mountinfo")) else {
            return json!([]);
        };
        let entries: Vec<Value> = disk::parse_mountinfo(&mountinfo)
            .into_iter()
            .filter_map(|m| {
                let path = self.root.join(m.point.trim_start_matches('/'));
                let (total, available) = disk::space(&path)?;
                Some(json!({ "mount": m.point, "total_mib": total, "available_mib": available }))
            })
            .collect();
        Value::Array(entries)
    }

    fn name(&mut self, card: &gpu::Card) -> Option<String> {
        if let Some(name) = self.names.get(&card.id) {
            return name.clone();
        }
        let root = &self.root;
        let pci_ids = self.pci_ids.get_or_insert_with(|| {
            PCI_IDS
                .iter()
                .find_map(|p| fs::read_to_string(root.join(p)).ok())
        });
        let name = card
            .device
            .zip(pci_ids.as_deref())
            .and_then(|(device, ids)| gpu::name_in(ids, card.vendor, device));
        self.names.insert(card.id.clone(), name.clone());
        name
    }

    /// Pings the gateway and the internet hosts together. `None` when the
    /// machine does not permit unprivileged ICMP.
    async fn probe(&mut self) -> Option<Value> {
        let route = fs::read_to_string(self.root.join("proc/net/route")).unwrap_or_default();
        let gateway = probe::gateway(&route);
        if gateway != self.gateway {
            self.gateway = gateway;
            self.gateway_window.clear();
        }
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let ping = |target: Ipv4Addr| tokio::task::spawn_blocking(move || probe::ping(target, seq));
        let to_gateway = gateway.map(ping);
        let to_internet: Vec<_> = probe::INTERNET.into_iter().map(ping).collect();
        let gateway_ping = match to_gateway {
            Some(task) => Some(task.await.unwrap_or(Ping::Lost)),
            None => None,
        };
        let mut internet_pings = Vec::new();
        for task in to_internet {
            internet_pings.push(task.await.unwrap_or(Ping::Lost));
        }
        if gateway_ping == Some(Ping::NotPermitted) || internet_pings.contains(&Ping::NotPermitted)
        {
            self.gateway_window.clear();
            self.internet_window.clear();
            return None;
        }
        let mut probes = Map::new();
        if let (Some(address), Some(ping)) = (gateway, gateway_ping) {
            self.gateway_window.push(fastest(&[ping]));
            let mut entry = Map::new();
            entry.insert("target".into(), json!(address.to_string()));
            entry.extend(self.gateway_window.figures());
            probes.insert("gateway".into(), Value::Object(entry));
        }
        self.internet_window.push(fastest(&internet_pings));
        let mut entry = Map::new();
        let targets: Vec<String> = probe::INTERNET.iter().map(Ipv4Addr::to_string).collect();
        entry.insert("targets".into(), json!(targets));
        entry.extend(self.internet_window.figures());
        probes.insert("internet".into(), Value::Object(entry));
        Some(Value::Object(probes))
    }
}

fn percent(usage: f32) -> i64 {
    f64::from(usage).round().clamp(0.0, 100.0) as i64
}

/// The fastest answer among `pings`, `None` when none answered.
fn fastest(pings: &[Ping]) -> Option<Duration> {
    pings
        .iter()
        .filter_map(|p| match p {
            Ping::Answered(rtt) => Some(*rtt),
            _ => None,
        })
        .min()
}

/// `bytes` over `elapsed`, in whole KiB/s.
fn kib_s(bytes: u64, elapsed: Duration) -> i64 {
    (bytes as f64 / 1024.0 / elapsed.as_secs_f64()).round() as i64
}

/// A disk's entry. Rates need the previous counters and the time since.
fn disk_entry(
    disk: &disk::Disk,
    before: Option<(&disk::Counters, Duration)>,
    temp: Option<i64>,
) -> Value {
    let mut entry = Map::new();
    entry.insert("name".into(), json!(disk.name));
    if let Some((before, elapsed)) = before.filter(|(_, e)| !e.is_zero()) {
        let now = &disk.counters;
        let sectors = |now: u64, then: u64| now.saturating_sub(then) * 512;
        entry.insert(
            "read_kib_s".into(),
            json!(kib_s(
                sectors(now.read_sectors, before.read_sectors),
                elapsed
            )),
        );
        entry.insert(
            "write_kib_s".into(),
            json!(kib_s(
                sectors(now.write_sectors, before.write_sectors),
                elapsed
            )),
        );
        let busy_ms = now.io_ticks.saturating_sub(before.io_ticks) as f64;
        let busy = (busy_ms / elapsed.as_secs_f64() / 10.0).round().min(100.0) as i64;
        entry.insert("busy".into(), json!(busy));
    }
    if let Some(temp) = temp {
        entry.insert("temp_c".into(), json!(temp));
    }
    Value::Object(entry)
}

/// An interface's entry. Rates and counts need the previous counters.
fn interface_entry(
    interface: &net::Interface,
    before: Option<(&net::Counters, Duration)>,
    wifi: Option<net::Wifi>,
) -> Value {
    let mut entry = Map::new();
    entry.insert("name".into(), json!(interface.name));
    let kind = if interface.wifi { "wifi" } else { "ethernet" };
    entry.insert("kind".into(), json!(kind));
    entry.insert("link".into(), json!(interface.link));
    if let (Some(now), Some((before, elapsed))) =
        (interface.counters, before.filter(|(_, e)| !e.is_zero()))
    {
        entry.insert(
            "rx_kib_s".into(),
            json!(kib_s(now.rx_bytes.saturating_sub(before.rx_bytes), elapsed)),
        );
        entry.insert(
            "tx_kib_s".into(),
            json!(kib_s(now.tx_bytes.saturating_sub(before.tx_bytes), elapsed)),
        );
        entry.insert(
            "errors".into(),
            json!(now.errors.saturating_sub(before.errors)),
        );
        entry.insert(
            "dropped".into(),
            json!(now.dropped.saturating_sub(before.dropped)),
        );
    }
    if let Some(wifi) = wifi {
        let mut link = Map::new();
        for (key, value) in [
            ("signal_dbm", wifi.signal_dbm),
            ("rx_mbit_s", wifi.rx_mbit_s),
            ("tx_mbit_s", wifi.tx_mbit_s),
        ] {
            if let Some(value) = value {
                link.insert(key.into(), json!(value));
            }
        }
        entry.insert("wifi".into(), Value::Object(link));
    }
    Value::Object(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(read: u64, write: u64, ticks: u64) -> disk::Disk {
        disk::Disk {
            name: "nvme0n1".into(),
            counters: disk::Counters {
                read_sectors: read,
                write_sectors: write,
                io_ticks: ticks,
            },
            device: None,
        }
    }

    #[test]
    fn disk_rates_and_busy_time_cover_the_last_sample() {
        let before = disk(0, 1000, 100).counters;
        // 4096 sectors read, 2048 written, 50 ms busy, over 2 s.
        let entry = disk_entry(
            &disk(4096, 3048, 150),
            Some((&before, Duration::from_secs(2))),
            Some(25),
        );
        assert_eq!(
            entry,
            json!({ "name": "nvme0n1", "read_kib_s": 1024, "write_kib_s": 512, "busy": 3, "temp_c": 25 })
        );
    }

    #[test]
    fn a_disk_seen_for_the_first_time_has_no_rates() {
        assert_eq!(
            disk_entry(&disk(1, 2, 3), None, None),
            json!({ "name": "nvme0n1" })
        );
    }

    #[test]
    fn interface_rates_counts_and_wifi() {
        let interface = net::Interface {
            name: "wlp99s0".into(),
            wifi: true,
            link: "up".into(),
            index: Some(3),
            counters: Some(net::Counters {
                rx_bytes: 2048 * 2 + 100,
                tx_bytes: 1024 + 100,
                errors: 5,
                dropped: 1,
            }),
        };
        let before = net::Counters {
            rx_bytes: 100,
            tx_bytes: 100,
            errors: 5,
            dropped: 0,
        };
        let wifi = net::Wifi {
            signal_dbm: Some(-37),
            rx_mbit_s: None,
            tx_mbit_s: Some(287),
        };
        assert_eq!(
            interface_entry(
                &interface,
                Some((&before, Duration::from_secs(2))),
                Some(wifi)
            ),
            json!({
                "name": "wlp99s0", "kind": "wifi", "link": "up",
                "rx_kib_s": 2, "tx_kib_s": 1, "errors": 0, "dropped": 1,
                "wifi": { "signal_dbm": -37, "tx_mbit_s": 287 }
            })
        );
    }

    #[test]
    fn a_counter_that_went_back_reads_as_zero() {
        let interface = net::Interface {
            name: "eno1".into(),
            wifi: false,
            link: "down".into(),
            index: None,
            counters: Some(net::Counters::default()),
        };
        let before = net::Counters {
            rx_bytes: 5000,
            tx_bytes: 5000,
            errors: 3,
            dropped: 3,
        };
        assert_eq!(
            interface_entry(&interface, Some((&before, Duration::from_secs(2))), None),
            json!({
                "name": "eno1", "kind": "ethernet", "link": "down",
                "rx_kib_s": 0, "tx_kib_s": 0, "errors": 0, "dropped": 0
            })
        );
    }

    #[test]
    fn the_fastest_answer_counts() {
        let fast = Duration::from_millis(8);
        assert_eq!(
            fastest(&[
                Ping::Answered(Duration::from_millis(21)),
                Ping::Answered(fast)
            ]),
            Some(fast)
        );
        assert_eq!(fastest(&[Ping::Lost, Ping::Answered(fast)]), Some(fast));
        assert_eq!(fastest(&[Ping::Lost, Ping::Lost]), None);
    }
}
