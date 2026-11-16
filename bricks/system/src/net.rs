//! Physical network interfaces, from `/sys/class/net`, and the Wi-Fi link,
//! through nl80211.

use std::fs;
use std::path::Path;

use futures::TryStreamExt;
use wl_nl80211::{Nl80211Attr, Nl80211RateInfo, Nl80211StationInfo};

/// An interface's counters, from its `statistics/`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Errors, both directions.
    pub errors: u64,
    /// Dropped packets, both directions.
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub name: String,
    pub wifi: bool,
    /// The kernel's `operstate`.
    pub link: String,
    pub index: Option<u32>,
    pub counters: Option<Counters>,
}

/// The physical interfaces under `root`: those with a `device`. That leaves
/// out `lo`, bridges, `veth` pairs and tunnels.
pub fn interfaces(root: &Path) -> Vec<Interface> {
    let Ok(entries) = fs::read_dir(root.join("sys/class/net")) else {
        return Vec::new();
    };
    let mut interfaces: Vec<Interface> = entries
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            if !dir.join("device").exists() {
                return None;
            }
            let read = |file: &str| fs::read_to_string(dir.join(file)).ok();
            let count = |file: &str| -> Option<u64> {
                read(&format!("statistics/{file}"))?.trim().parse().ok()
            };
            let counters = (|| {
                Some(Counters {
                    rx_bytes: count("rx_bytes")?,
                    tx_bytes: count("tx_bytes")?,
                    errors: count("rx_errors")? + count("tx_errors")?,
                    dropped: count("rx_dropped")? + count("tx_dropped")?,
                })
            })();
            Some(Interface {
                name: e.file_name().to_string_lossy().into_owned(),
                wifi: dir.join("wireless").exists() || dir.join("phy80211").exists(),
                link: read("operstate").map_or_else(|| "unknown".into(), |s| s.trim().to_owned()),
                index: read("ifindex").and_then(|i| i.trim().parse().ok()),
                counters,
            })
        })
        .collect();
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
}

/// The radio link of a Wi-Fi interface to its access point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wifi {
    pub signal_dbm: Option<i64>,
    pub rx_mbit_s: Option<i64>,
    pub tx_mbit_s: Option<i64>,
}

/// The link of the interface `index`, `None` without a station (not
/// connected) or when nl80211 does not answer.
pub async fn wifi(index: u32) -> Option<Wifi> {
    let (connection, handle, _) = wl_nl80211::new_connection().ok()?;
    let connection = tokio::spawn(connection);
    let link = async {
        let mut stations = handle.station().dump(index).execute().await;
        let message = stations.try_next().await.ok()??;
        message
            .payload
            .attributes
            .into_iter()
            .find_map(|a| match a {
                Nl80211Attr::StationInfo(infos) => Some(from_station(&infos)),
                _ => None,
            })
    };
    let link = tokio::time::timeout(std::time::Duration::from_secs(1), link)
        .await
        .ok()
        .flatten();
    connection.abort();
    link
}

/// The driver's signal average (steadier than the last packet's), and the
/// negotiated rates.
fn from_station(infos: &[Nl80211StationInfo]) -> Wifi {
    let mut signal = None;
    let mut average = None;
    let mut rx = None;
    let mut tx = None;
    for info in infos {
        match info {
            Nl80211StationInfo::Signal(s) => signal = Some(i64::from(*s)),
            Nl80211StationInfo::SignalAvg(s) => average = Some(i64::from(*s)),
            Nl80211StationInfo::RxBitrate(rate) => rx = mbit_s(rate),
            Nl80211StationInfo::TxBitrate(rate) => tx = mbit_s(rate),
            _ => {}
        }
    }
    Wifi {
        signal_dbm: average.or(signal),
        rx_mbit_s: rx,
        tx_mbit_s: tx,
    }
}

/// A rate in Mbit/s, from its 100 kbit/s units.
fn mbit_s(rate: &[Nl80211RateInfo]) -> Option<i64> {
    let wide = rate.iter().find_map(|r| match r {
        Nl80211RateInfo::Bitrate32(b) => Some(u64::from(*b)),
        _ => None,
    });
    let narrow = rate.iter().find_map(|r| match r {
        Nl80211RateInfo::Bitrate(b) => Some(u64::from(*b)),
        _ => None,
    });
    wide.or(narrow).map(|b| (b as f64 / 10.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Tree;

    fn interface(tree: &Tree, name: &str, physical: bool, wireless: bool) {
        let dir = format!("sys/class/net/{name}");
        tree.file(&format!("{dir}/operstate"), "up\n");
        tree.file(&format!("{dir}/ifindex"), "3\n");
        if physical {
            tree.dir(&format!("{dir}/device"));
        }
        if wireless {
            tree.dir(&format!("{dir}/wireless"));
        }
        for (file, value) in [
            ("rx_bytes", "1000"),
            ("tx_bytes", "200"),
            ("rx_errors", "1"),
            ("tx_errors", "2"),
            ("rx_dropped", "3"),
            ("tx_dropped", "4"),
        ] {
            tree.file(&format!("{dir}/statistics/{file}"), value);
        }
    }

    #[test]
    fn only_physical_interfaces_are_listed() {
        let tree = Tree::new();
        interface(&tree, "wlp99s0", true, true);
        interface(&tree, "eno1", true, false);
        interface(&tree, "lo", false, false);
        interface(&tree, "docker0", false, false);
        let interfaces = interfaces(tree.root());
        let names: Vec<(&str, bool)> = interfaces
            .iter()
            .map(|i| (i.name.as_str(), i.wifi))
            .collect();
        assert_eq!(names, [("eno1", false), ("wlp99s0", true)]);
        assert_eq!(interfaces[0].link, "up");
        assert_eq!(interfaces[0].index, Some(3));
        assert_eq!(
            interfaces[0].counters,
            Some(Counters {
                rx_bytes: 1000,
                tx_bytes: 200,
                errors: 3,
                dropped: 7,
            })
        );
    }

    #[test]
    fn the_station_gives_the_signal_average_and_rates() {
        let infos = [
            Nl80211StationInfo::Signal(-40),
            Nl80211StationInfo::SignalAvg(-37),
            Nl80211StationInfo::TxBitrate(vec![
                Nl80211RateInfo::Bitrate(2867),
                Nl80211RateInfo::Bitrate32(2867),
            ]),
            Nl80211StationInfo::RxBitrate(vec![Nl80211RateInfo::Bitrate(2064)]),
        ];
        assert_eq!(
            from_station(&infos),
            Wifi {
                signal_dbm: Some(-37),
                rx_mbit_s: Some(206),
                tx_mbit_s: Some(287),
            }
        );
        assert_eq!(
            from_station(&[Nl80211StationInfo::Signal(-50)]),
            Wifi {
                signal_dbm: Some(-50),
                rx_mbit_s: None,
                tx_mbit_s: None,
            }
        );
    }
}
