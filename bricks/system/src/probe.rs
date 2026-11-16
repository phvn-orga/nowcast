//! ICMP probes, through an unprivileged ICMP socket (`SOCK_DGRAM`,
//! `IPPROTO_ICMP`), which `net.ipv4.ping_group_range` permits; systemd opens
//! it to every group.

use std::collections::VecDeque;
use std::io;
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use socket2::{Domain, Protocol, Socket, Type};

/// The public resolvers the internet probe pings, from two operators: one
/// operator's incident does not read as an outage.
pub const INTERNET: [Ipv4Addr; 2] = [Ipv4Addr::new(9, 9, 9, 9), Ipv4Addr::new(1, 1, 1, 1)];

/// How long a ping waits for its reply.
pub const TIMEOUT: Duration = Duration::from_secs(1);

/// The samples a probe's figures cover: 30 s at one sample every 2 s.
const WINDOW: usize = 15;

/// The gateway of the IPv4 default route, from `/proc/net/route`.
pub fn gateway(route: &str) -> Option<Ipv4Addr> {
    // RTF_UP | RTF_GATEWAY.
    const FLAGS: u32 = 0x1 | 0x2;
    route.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (destination, gateway, flags) = (fields.get(1)?, fields.get(2)?, fields.get(3)?);
        let flags = u32::from_str_radix(flags, 16).ok()?;
        if *destination != "00000000" || flags & FLAGS != FLAGS {
            return None;
        }
        let gateway = u32::from_str_radix(gateway, 16).ok()?;
        // The kernel writes the address in its own byte order.
        Some(Ipv4Addr::from(gateway.to_le_bytes()))
    })
}

/// What a ping gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ping {
    Answered(Duration),
    Lost,
    /// The machine does not permit unprivileged ICMP.
    NotPermitted,
}

/// One ping to `target`, blocking for up to [`TIMEOUT`].
pub fn ping(target: Ipv4Addr, seq: u16) -> Ping {
    let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
        Ok(socket) => socket,
        Err(e) if matches!(e.raw_os_error(), Some(libc::EACCES | libc::EPERM)) => {
            return Ping::NotPermitted;
        }
        Err(_) => return Ping::Lost,
    };
    match exchange(&socket, target, seq) {
        Ok(Some(rtt)) => Ping::Answered(rtt),
        Ok(None) | Err(_) => Ping::Lost,
    }
}

/// Sends an echo request and waits for its reply. The kernel fills in the
/// identifier and the checksum, and hands this socket only its own replies.
fn exchange(socket: &Socket, target: Ipv4Addr, seq: u16) -> io::Result<Option<Duration>> {
    let deadline = Instant::now() + TIMEOUT;
    let [hi, lo] = seq.to_be_bytes();
    let request = [8, 0, 0, 0, 0, 0, hi, lo];
    let sent = Instant::now();
    socket.send_to(&request, &SocketAddrV4::new(target, 0).into())?;
    let mut buf = [MaybeUninit::<u8>::uninit(); 64];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        socket.set_read_timeout(Some(left))?;
        let n = match socket.recv(&mut buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        // SAFETY: `recv` initialised the first `n` bytes.
        let reply: Vec<u8> = buf[..n]
            .iter()
            .map(|b| unsafe { b.assume_init() })
            .collect();
        // An echo reply to this request.
        if n >= 8 && reply[0] == 0 && reply[6..8] == [hi, lo] {
            return Ok(Some(sent.elapsed()));
        }
    }
}

/// The last samples of a probe: the fastest answer of each, or `None`.
#[derive(Debug, Default)]
pub struct Window {
    samples: VecDeque<Option<Duration>>,
}

impl Window {
    pub fn push(&mut self, sample: Option<Duration>) {
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// `rtt_ms`, the median of the answered samples, and `loss`, the share of
    /// samples with no answer, in percent. Empty before the first sample.
    pub fn figures(&self) -> Map<String, Value> {
        let mut figures = Map::new();
        if self.samples.is_empty() {
            return figures;
        }
        let mut answered: Vec<Duration> = self.samples.iter().flatten().copied().collect();
        answered.sort();
        if !answered.is_empty() {
            let mid = answered.len() / 2;
            let median = if answered.len().is_multiple_of(2) {
                (answered[mid - 1] + answered[mid]) / 2
            } else {
                answered[mid]
            };
            figures.insert(
                "rtt_ms".into(),
                json!((median.as_secs_f64() * 1000.0).round() as i64),
            );
        }
        let lost = self.samples.len() - answered.len();
        let loss = (lost as f64 * 100.0 / self.samples.len() as f64).round() as i64;
        figures.insert("loss".into(), json!(loss));
        figures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gateway_is_the_default_route_s() {
        let route = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
wlp99s0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
";
        assert_eq!(gateway(route), Some(Ipv4Addr::new(192, 168, 1, 1)));
        let no_default = "Iface\tDestination\tGateway\tFlags\n\
                          docker0\t000011AC\t00000000\t0001\n";
        assert_eq!(gateway(no_default), None);
    }

    #[test]
    fn figures_are_the_median_and_the_loss() {
        let mut window = Window::default();
        assert!(window.figures().is_empty());
        for ms in [Some(10), None, Some(30), Some(20)] {
            window.push(ms.map(Duration::from_millis));
        }
        assert_eq!(
            Value::Object(window.figures()),
            json!({ "rtt_ms": 20, "loss": 25 })
        );
        window.push(Some(Duration::from_micros(900)));
        // 0.9, 10, 20, 30: (10 + 20) / 2.
        assert_eq!(
            Value::Object(window.figures()),
            json!({ "rtt_ms": 15, "loss": 20 })
        );
    }

    #[test]
    fn no_answer_has_no_rtt() {
        let mut window = Window::default();
        window.push(None);
        assert_eq!(Value::Object(window.figures()), json!({ "loss": 100 }));
    }

    #[test]
    fn the_window_keeps_the_last_30_seconds() {
        let mut window = Window::default();
        window.push(None);
        for _ in 0..WINDOW {
            window.push(Some(Duration::from_millis(5)));
        }
        assert_eq!(
            Value::Object(window.figures()),
            json!({ "rtt_ms": 5, "loss": 0 })
        );
    }
}
