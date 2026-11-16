//! Temperature sensors, from `/sys/class/hwmon`. Some sensors query the
//! hardware on every read (about 1 ms for an NVMe drive, up to 2.5 ms for a
//! memory module): only the sensors the brick publishes are read.

use std::fs;
use std::path::{Path, PathBuf};

/// A sensor directory, with what ties it to a device.
#[derive(Debug, Clone)]
pub struct Sensor {
    /// The driver's name: `k10temp`, `amdgpu`, `nvme`...
    pub name: String,
    /// The real path of the sensor's device, to match it to a GPU or a disk.
    pub device: Option<PathBuf>,
    dir: PathBuf,
}

/// Every sensor under `root`. Reads names and links, no temperature.
pub fn list(root: &Path) -> Vec<Sensor> {
    let Ok(entries) = fs::read_dir(root.join("sys/class/hwmon")) else {
        return Vec::new();
    };
    let mut sensors: Vec<Sensor> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let name = fs::read_to_string(dir.join("name")).ok()?.trim().to_owned();
            let device = fs::canonicalize(dir.join("device")).ok();
            Some(Sensor { name, device, dir })
        })
        .collect();
    sensors.sort_by(|a, b| a.dir.cmp(&b.dir));
    sensors
}

impl Sensor {
    /// The temperature labelled `label`, in whole degrees Celsius; the first
    /// input (`temp1`) when no input carries that label.
    pub fn temp(&self, label: &str) -> Option<i64> {
        let input = (1..=32)
            .find(|n| {
                fs::read_to_string(self.dir.join(format!("temp{n}_label")))
                    .is_ok_and(|l| l.trim() == label)
            })
            .unwrap_or(1);
        let raw = fs::read_to_string(self.dir.join(format!("temp{input}_input"))).ok()?;
        let millis: f64 = raw.trim().parse().ok()?;
        Some((millis / 1000.0).round() as i64)
    }
}

/// The CPU's package temperature: `k10temp` (AMD) or `coretemp` (Intel).
pub fn cpu(sensors: &[Sensor]) -> Option<i64> {
    sensors.iter().find_map(|s| match s.name.as_str() {
        "k10temp" => s.temp("Tctl"),
        "coretemp" => s.temp("Package id 0"),
        _ => None,
    })
}

/// The temperature of the sensor whose device is `device`, read under
/// `label`.
pub fn of_device(sensors: &[Sensor], device: &Path, label: &str) -> Option<i64> {
    sensors
        .iter()
        .find(|s| s.device.as_deref() == Some(device))
        .and_then(|s| s.temp(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Tree;

    #[test]
    fn reads_the_cpu_package_and_a_device_sensor_by_label() {
        let tree = Tree::new();
        let gpu = tree.dir("sys/devices/pci0000:00/0000:65:00.0");
        tree.sensor("hwmon0", "acpitz", None, &[(1, None, "53000")]);
        tree.sensor("hwmon1", "k10temp", None, &[(1, Some("Tctl"), "53500")]);
        tree.sensor(
            "hwmon2",
            "amdgpu",
            Some(&gpu),
            &[(1, Some("junction"), "61000"), (2, Some("edge"), "49600")],
        );
        let sensors = list(tree.root());
        assert_eq!(cpu(&sensors), Some(54));
        let gpu = fs::canonicalize(gpu).unwrap();
        assert_eq!(of_device(&sensors, &gpu, "edge"), Some(50));
        // No input labelled so: the first one.
        assert_eq!(of_device(&sensors, &gpu, "Composite"), Some(61));
        assert_eq!(of_device(&sensors, Path::new("/nowhere"), "edge"), None);
    }

    #[test]
    fn no_sensor_gives_nothing() {
        let tree = Tree::new();
        assert!(list(tree.root()).is_empty());
        assert_eq!(cpu(&[]), None);
    }
}
