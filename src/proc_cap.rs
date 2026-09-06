//! Always-on per-process capture primitives (daemon recorder backend).
//!
//! One background thread sniffs all eligible interfaces via AF_PACKET
//! (no libpcap), attributes each frame to the owning comm by snapshotting
//! `/proc/net/{tcp,tcp6,udp,udp6}` + `/proc/<pid>/fd/*` (nethogs/bandwhich
//! method). Attribution is best-effort: short flows that open and close
//! between snapshots land in `unknown`; UDP mapping is weaker. Needs
//! CAP_NET_RAW (+ DAC_READ_SEARCH/SYS_PTRACE for other users' sockets),
//! granted to the daemon via the system unit — viewers need no privileges.

use pnet::datalink::{self, Channel, DataLinkReceiver};
use pnet::packet::ethernet::{EtherTypes, EthernetPacket};
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::ipv4::Ipv4Packet;
use pnet::packet::ipv6::Ipv6Packet;
use pnet::packet::tcp::TcpPacket;
use pnet::packet::udp::UdpPacket;
use pnet::packet::Packet;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config;

/// pid → (comm, rx_bytes, tx_bytes), cumulative.
pub type Cum = HashMap<u32, (String, u64, u64)>;

pub struct Owners {
    by_ep: HashMap<(IpAddr, u16), u32>,
    comm: HashMap<u32, String>,
}

pub fn snapshot() -> Owners {
    let inode_owner = inode_to_pid();
    let mut o = Owners {
        by_ep: HashMap::new(),
        comm: HashMap::new(),
    };
    for proto in ["tcp", "tcp6", "udp", "udp6"] {
        let Ok(text) = std::fs::read_to_string(format!("/proc/net/{proto}")) else {
            continue;
        };
        let v6 = proto.ends_with('6');
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let Some((ip, port)) = parse_local(f[1], v6) else {
                continue;
            };
            let Ok(inode) = f[9].parse::<u64>() else {
                continue;
            };
            if inode == 0 {
                continue;
            }
            if let Some((pid, comm)) = inode_owner.get(&inode) {
                o.by_ep.insert((ip, port), *pid);
                o.comm.entry(*pid).or_insert_with(|| comm.clone());
            }
        }
    }
    o
}

pub fn parse_local(s: &str, v6: bool) -> Option<(IpAddr, u16)> {
    let (hex_ip, hex_port) = s.split_once(':')?;
    let port = u16::from_str_radix(hex_port, 16).ok()?;
    if v6 {
        if hex_ip.len() != 32 {
            return None;
        }
        let mut b = [0u8; 16];
        for (i, chunk) in b.chunks_mut(4).enumerate() {
            let w = u32::from_str_radix(&hex_ip[i * 8..i * 8 + 8], 16).ok()?;
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        Some((IpAddr::V6(Ipv6Addr::from(b)), port))
    } else {
        let raw = u32::from_str_radix(hex_ip, 16).ok()?;
        Some((IpAddr::V4(Ipv4Addr::from(raw.to_le_bytes())), port))
    }
}

/// inode → (pid, comm) via /proc/<pid>/fd symlink walk.
fn inode_to_pid() -> HashMap<u64, (u32, String)> {
    let mut map = HashMap::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return map;
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let Some(t) = target.to_str() else {
                continue;
            };
            if let Some(ino) = t
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
            {
                map.entry(ino).or_insert_with(|| (pid, comm.clone()));
            }
        }
    }
    map
}

fn l4_ports(proto: pnet::packet::ip::IpNextHeaderProtocol, l4: &[u8]) -> Option<(u16, u16)> {
    match proto {
        IpNextHeaderProtocols::Tcp => {
            TcpPacket::new(l4).map(|t| (t.get_source(), t.get_destination()))
        }
        IpNextHeaderProtocols::Udp => {
            UdpPacket::new(l4).map(|u| (u.get_source(), u.get_destination()))
        }
        _ => None,
    }
}

pub fn account(frame: &[u8], owners: &Owners, local: &[IpAddr], cum: &mut Cum) {
    let Some(eth) = EthernetPacket::new(frame) else {
        return;
    };
    let (src, dst, ports): (IpAddr, IpAddr, Option<(u16, u16)>) = match eth.get_ethertype() {
        EtherTypes::Ipv4 => {
            let Some(ip) = Ipv4Packet::new(eth.payload()) else {
                return;
            };
            let proto = ip.get_next_level_protocol();
            let ports = l4_ports(proto, ip.payload());
            (
                IpAddr::V4(ip.get_source()),
                IpAddr::V4(ip.get_destination()),
                ports,
            )
        }
        EtherTypes::Ipv6 => {
            let Some(ip) = Ipv6Packet::new(eth.payload()) else {
                return;
            };
            let proto = ip.get_next_header();
            let ports = l4_ports(proto, ip.payload());
            (
                IpAddr::V6(ip.get_source()),
                IpAddr::V6(ip.get_destination()),
                ports,
            )
        }
        _ => return,
    };
    let bytes = frame.len() as u64;
    let up = local.contains(&src);
    let (pid, comm): (u32, &str) = match ports {
        Some((sp, dp)) => {
            let (lip, lport) = if up { (src, sp) } else { (dst, dp) };
            match owners.by_ep.get(&(lip, lport)) {
                Some(pid) => {
                    let c = owners.comm.get(pid).map(String::as_str).unwrap_or("?");
                    (*pid, c)
                }
                None => (0, "unknown"),
            }
        }
        None => (0, "other"),
    };
    let e = cum.entry(pid).or_insert_with(|| (comm.to_string(), 0, 0));
    if e.0 == "?" || e.0 == "unknown" || e.0 == "other" {
        e.0 = comm.to_string();
    }
    if up {
        e.2 += bytes;
    } else {
        e.1 += bytes;
    }
}

// ---------- always-on daemon recorder ----------

/// Continuous capture thread feeding comm-keyed byte counters.
/// The daemon drains deltas via [`take`](ProcRecorder::take) each tick and
/// upserts them into `proc_hourly` — one writer, no IPC.
pub struct ProcRecorder {
    cum: Arc<Mutex<HashMap<String, (u64, u64)>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ProcRecorder {
    pub fn spawn(exclude: &[String]) -> Self {
        let cum = Arc::new(Mutex::new(HashMap::new()));
        let handle = Self::launch(exclude.to_vec(), Arc::clone(&cum));
        Self {
            cum,
            handle: Some(handle),
        }
    }

    /// Restart the thread if it died (e.g. transient netlink error).
    pub fn ensure_running(&mut self, exclude: &[String]) {
        let dead = self
            .handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true);
        if dead {
            tracing::warn!("proc capture thread died — restarting");
            self.handle = Some(Self::launch(exclude.to_vec(), Arc::clone(&self.cum)));
        }
    }

    /// Take the full cumulative map (capture keeps appending to a fresh one).
    pub fn take(&self) -> HashMap<String, (u64, u64)> {
        std::mem::take(&mut *self.cum.lock().unwrap())
    }

    fn launch(
        exclude: Vec<String>,
        shared: Arc<Mutex<HashMap<String, (u64, u64)>>>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut owners = snapshot();
            let mut last_snap = Instant::now();
            let mut cum: Cum = HashMap::new();
            let mut last_flush = Instant::now();
            let mut receivers: Vec<(String, Vec<IpAddr>, Box<dyn DataLinkReceiver>)> = vec![];
            loop {
                if receivers.is_empty() {
                    receivers = open_all(&exclude);
                    if receivers.is_empty() {
                        // no eligible interface (or no perms) — retry later, don't spin
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }
                    tracing::info!(
                        "proc capture on: {}",
                        receivers
                            .iter()
                            .map(|(n, _, _)| n.clone())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
                let locals: Vec<IpAddr> = receivers
                    .iter()
                    .flat_map(|(_, ips, _)| ips.clone())
                    .collect();
                let mut dead = false;
                for (_, _, rx) in receivers.iter_mut() {
                    match rx.next() {
                        Ok(frame) => account(frame, &owners, &locals, &mut cum),
                        Err(e)
                            if e.kind() == std::io::ErrorKind::TimedOut
                                || e.kind() == std::io::ErrorKind::WouldBlock =>
                        {
                            continue;
                        }
                        Err(_) => {
                            dead = true;
                            break;
                        }
                    }
                }
                if dead {
                    receivers.clear();
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                if last_snap.elapsed() > Duration::from_secs(15) {
                    owners = snapshot();
                    last_snap = Instant::now();
                }
                if last_flush.elapsed() > Duration::from_secs(1) || cum.len() > 5000 {
                    // fold pid-keyed scratch into comm totals (PIDs are unstable)
                    if !cum.is_empty() {
                        let mut shared = shared.lock().unwrap();
                        for (_, (comm, rx_b, tx_b)) in cum.drain() {
                            let e = shared.entry(comm).or_insert((0, 0));
                            e.0 += rx_b;
                            e.1 += tx_b;
                        }
                    }
                    last_flush = Instant::now();
                }
            }
        })
    }
}

fn open_all(exclude: &[String]) -> Vec<(String, Vec<IpAddr>, Box<dyn DataLinkReceiver>)> {
    let mut out = vec![];
    for ni in datalink::interfaces() {
        if !ni.is_up() || ni.is_loopback() {
            continue;
        }
        if exclude.iter().any(|p| config::glob_match(p, &ni.name)) {
            continue;
        }
        let ips: Vec<IpAddr> = ni.ips.iter().map(|n| n.ip()).collect();
        let cfg = datalink::Config {
            read_timeout: Some(Duration::from_millis(200)),
            ..Default::default()
        };
        match datalink::channel(&ni, cfg) {
            Ok(Channel::Ethernet(_, rx)) => out.push((ni.name, ips, rx)),
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!("proc capture: cannot open {} ({e})", ni.name);
                continue;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_v4() {
        let (ip, port) = parse_local("0100007F:0035", false).unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(port, 53);
    }

    #[test]
    fn parses_proc_v6_loopback() {
        let (ip, port) = parse_local("00000000000000000000000001000000:01BB", true).unwrap();
        assert_eq!(ip, IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(port, 443);
    }

    #[test]
    fn snapshot_builds() {
        let o = snapshot();
        let _ = (o.by_ep.len(), o.comm.len());
    }

    #[test]
    fn account_counts_frame_bytes() {
        // synthetic Ethernet/IPv4/TCP frame, unknown owner → unknown bucket
        let mut f = vec![0u8; 14 + 20 + 20];
        f[12] = 0x08;
        f[13] = 0x00; // IPv4 ethertype
        f[14] = 0x45; // version+IHL
        f[23] = 6; // TCP
        f[14 + 12..14 + 16].copy_from_slice(&[1, 2, 3, 4]);
        f[14 + 16..14 + 20].copy_from_slice(&[5, 6, 7, 8]);
        let owners = Owners {
            by_ep: HashMap::new(),
            comm: HashMap::new(),
        };
        let mut cum = Cum::new();
        account(&f, &owners, &[], &mut cum);
        let rx = cum.get(&0).map(|(_, rx, _)| *rx).unwrap_or(0);
        assert_eq!(rx, f.len() as u64);
    }
}
