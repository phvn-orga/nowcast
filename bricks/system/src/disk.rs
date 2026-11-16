//! Disks, from `/sys/block`, and filesystems, from
//! `/proc/self/mountinfo` and `statvfs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// A disk's counters, from `/sys/block/<disk>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub read_sectors: u64,
    pub write_sectors: u64,
    /// Milliseconds with an operation in flight.
    pub io_ticks: u64,
}

/// A physical disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Disk {
    pub name: String,
    pub counters: Counters,
    /// The real path of the disk's device, which its temperature sensor
    /// points to.
    pub device: Option<PathBuf>,
}

/// The fields of a `stat` file: the kernel's `Documentation/block/stat.rst`.
pub fn parse_stat(text: &str) -> Option<Counters> {
    let fields: Vec<u64> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    Some(Counters {
        read_sectors: *fields.get(2)?,
        write_sectors: *fields.get(6)?,
        io_ticks: *fields.get(9)?,
    })
}

/// The physical disks under `root`: not under `/sys/devices/virtual`, which
/// holds `loop` (snaps) and `dm` (LVM, encryption) devices.
pub fn disks(root: &Path) -> Vec<Disk> {
    let Ok(entries) = fs::read_dir(root.join("sys/block")) else {
        return Vec::new();
    };
    let virtual_devices = root.join("sys/devices/virtual");
    let virtual_devices = fs::canonicalize(&virtual_devices).unwrap_or(virtual_devices);
    let mut disks: Vec<Disk> = entries
        .flatten()
        .filter_map(|e| {
            let real = fs::canonicalize(e.path()).ok()?;
            if real.starts_with(&virtual_devices) {
                return None;
            }
            Some(Disk {
                name: e.file_name().to_string_lossy().into_owned(),
                counters: parse_stat(&fs::read_to_string(real.join("stat")).ok()?)?,
                device: fs::canonicalize(real.join("device")).ok(),
            })
        })
        .collect();
    disks.sort_by(|a, b| a.name.cmp(&b.name));
    disks
}

/// A mounted block device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub point: String,
}

/// The block devices `mountinfo` mounts, at their first mount point,
/// `squashfs` aside (snaps, always full).
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut seen = BTreeSet::new();
    let mut mounts = Vec::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left: Vec<&str> = left.split(' ').collect();
        let right: Vec<&str> = right.split(' ').collect();
        let (Some(device), Some(point), Some(fstype), Some(source)) =
            (left.get(2), left.get(4), right.first(), right.get(1))
        else {
            continue;
        };
        if !source.starts_with("/dev/") || *fstype == "squashfs" || !seen.insert(*device) {
            continue;
        }
        mounts.push(Mount {
            point: unescape(point),
        });
    }
    mounts
}

/// Mount points escape space, tab, newline and backslash in octal.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(code) = bytes
                .get(i + 1..i + 4)
                .and_then(|o| std::str::from_utf8(o).ok())
                .and_then(|o| u8::from_str_radix(o, 8).ok())
        {
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Total and available space of the filesystem at `path`, in MiB. Available
/// is what an unprivileged user can still write, as `df` shows.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let mib = |blocks: u64| blocks.saturating_mul(stat.f_frsize) >> 20;
    Some((mib(stat.f_blocks), mib(stat.f_bavail)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Tree;

    const STAT: &str = "  183599    44689 10180115    23987   102152   104070  4878714    77966        0    28136   104457       50        0  5863672       14     6048     2488\n";

    #[test]
    fn stat_gives_sectors_and_busy_time() {
        assert_eq!(
            parse_stat(STAT),
            Some(Counters {
                read_sectors: 10180115,
                write_sectors: 4878714,
                io_ticks: 28136,
            })
        );
        assert_eq!(parse_stat("1 2 3"), None);
    }

    #[test]
    fn only_physical_disks_are_listed() {
        let tree = Tree::new();
        let nvme = tree.dir("sys/devices/pci0000:00/0000:61:00.0/nvme/nvme0/nvme0n1");
        tree.file(
            "sys/devices/pci0000:00/0000:61:00.0/nvme/nvme0/nvme0n1/stat",
            STAT,
        );
        let controller = tree.dir("sys/devices/pci0000:00/0000:61:00.0/nvme/nvme0");
        tree.link(
            "sys/devices/pci0000:00/0000:61:00.0/nvme/nvme0/nvme0n1/device",
            &controller,
        );
        tree.link("sys/block/nvme0n1", &nvme);
        for virtual_disk in ["loop0", "dm-0"] {
            let dir = tree.dir(&format!("sys/devices/virtual/block/{virtual_disk}"));
            tree.file(
                &format!("sys/devices/virtual/block/{virtual_disk}/stat"),
                STAT,
            );
            tree.link(&format!("sys/block/{virtual_disk}"), &dir);
        }
        let disks = disks(tree.root());
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "nvme0n1");
        assert_eq!(disks[0].device, Some(fs::canonicalize(controller).unwrap()));
    }

    #[test]
    fn mountinfo_keeps_block_devices_once_and_drops_snaps() {
        let text = "\
26 1 252:0 / / rw,relatime shared:1 - ext4 /dev/mapper/vg-root rw
27 26 0:24 / /proc rw shared:2 - proc proc rw
30 26 7:0 / /snap/bare/5 ro shared:3 - squashfs /dev/loop0 ro
31 26 259:2 / /boot rw shared:4 - ext4 /dev/nvme0n1p2 rw
32 31 259:1 / /boot/efi rw shared:5 - vfat /dev/nvme0n1p1 rw
40 26 252:0 /home /mnt/my\\040disk rw shared:6 - ext4 /dev/mapper/vg-root rw
41 26 259:3 / /mnt/my\\040disk rw shared:7 - ext4 /dev/nvme0n1p3 rw
";
        let points: Vec<String> = parse_mountinfo(text).into_iter().map(|m| m.point).collect();
        assert_eq!(points, ["/", "/boot", "/boot/efi", "/mnt/my disk"]);
    }

    #[test]
    fn space_reads_a_real_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let (total, available) = space(dir.path()).unwrap();
        assert!(total > 0 && available <= total);
        assert_eq!(space(Path::new("/nonexistent/path")), None);
    }
}
