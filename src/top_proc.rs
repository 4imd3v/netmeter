//! Live per-process bandwidth (M2, capture backend).
//!
//! Sniffs one interface via AF_PACKET (no libpcap needed), attributes each
//! frame's bytes to the owning PID by snapshotting `/proc/net/{tcp,tcp6,udp,udp6}`
//! (socket → inode) + `/proc/<pid>/fd/*` (inode → pid), same approach as
//! nethogs/bandwhich. Best-effort + racy by nature: short flows that open and
//! close between 5s snapshots land in `unknown`; UDP mapping is weaker.
//! Needs CAP_NET_RAW (+ ptrace/dac for other users' sockets): run with sudo.
//! Future upgrade path: Aya eBPF sock hooks (precise, always-on).

use anyhow::{Context, Result};
use pnet::datalink::{self, Channel, DataLinkReceiver, NetworkInterface};
use pnet::packet::ethernet::{EtherTypes, EthernetPacket};
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::ipv4::Ipv4Packet;
use pnet::packet::ipv6::Ipv6Packet;
use pnet::packet::tcp::TcpPacket;
use pnet::packet::udp::UdpPacket;
use pnet::packet::Packet;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use crate::{config::Config, fmtx};

pub fn run(cfg: Config, iface: Option<String>) -> Result<()> {
    let mut sampler = ProcSampler::start(iface)?;
    let ifname = sampler.ifname().to_string();
    ratatui::run(|term| view_loop(term, &mut sampler, &ifname, &cfg))?;
    Ok(())
}

/// Reusable packet→process sampler (shared by `top-proc` and the `live` dashboard).
/// [`poll`] drains frames for a short budget; [`rates`] returns per-tick B/s deltas.
///
/// [`poll`]: ProcSampler::poll
/// [`rates`]: ProcSampler::rates
pub struct ProcSampler {
    ifname: String,
    rx: Box<dyn DataLinkReceiver>,
    local: Vec<IpAddr>,
    owners: Owners,
    last_snap: Instant,
    cum: Cum,
    prev: Cum,
}

/// pid, comm, rx B/s, tx B/s since last [`rates`](ProcSampler::rates) call.
pub struct ProcRate {
    pub pid: u32,
    pub comm: String,
    pub rx: u64,
    pub tx: u64,
}

impl ProcSampler {
    pub fn start(iface: Option<String>) -> Result<Self> {
        let ifaces = datalink::interfaces();
        let ni = pick_iface(&ifaces, iface.as_deref())?;
        let local_ips: Vec<IpAddr> = ni.ips.iter().map(|n| n.ip()).collect();
        let dl_cfg = datalink::Config {
            read_timeout: Some(Duration::from_millis(200)),
            ..Default::default()
        };
        let rx: Box<dyn DataLinkReceiver> = match datalink::channel(&ni, dl_cfg) {
            Ok(Channel::Ethernet(_, rx)) => rx,
            Ok(_) => anyhow::bail!("unsupported channel type on {}", ni.name),
            Err(e) => anyhow::bail!(
                "cannot capture on {} ({e}). Need CAP_NET_RAW — run with sudo.",
                ni.name
            ),
        };
        Ok(Self {
            ifname: ni.name,
            rx,
            local: local_ips,
            owners: snapshot(),
            last_snap: Instant::now(),
            cum: HashMap::new(),
            prev: HashMap::new(),
        })
    }

    pub fn ifname(&self) -> &str {
        &self.ifname
    }

    /// Drain frames for up to ~300ms. Returns false on fatal channel error.
    pub fn poll(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_millis(300);
        loop {
            match self.rx.next() {
                Ok(frame) => account(frame, &self.owners, &self.local, &mut self.cum),
                Err(e)
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(_) => return false,
            }
            if Instant::now() >= deadline {
                break;
            }
            if self.last_snap.elapsed() > Duration::from_secs(5) {
                self.owners = snapshot();
                self.last_snap = Instant::now();
            }
        }
        true
    }

    /// Per-process B/s since last call, sorted desc. First call primes baselines.
    pub fn rates(&mut self) -> Vec<ProcRate> {
        let mut out = vec![];
        for (pid, (comm, rx_b, tx_b)) in &self.cum {
            let (prx, ptx) = self
                .prev
                .get(pid)
                .map(|(_, a, b)| (*a, *b))
                .unwrap_or((0, 0));
            out.push(ProcRate {
                pid: *pid,
                comm: comm.clone(),
                rx: rx_b.saturating_sub(prx),
                tx: tx_b.saturating_sub(ptx),
            });
        }
        self.prev = self.cum.clone();
        out.sort_by_key(|r| std::cmp::Reverse(r.rx + r.tx));
        out
    }
}

fn pick_iface(ifaces: &[NetworkInterface], want: Option<&str>) -> Result<NetworkInterface> {
    if let Some(name) = want {
        return ifaces
            .iter()
            .find(|i| i.name == name)
            .cloned()
            .with_context(|| format!("interface {name} not found"));
    }
    ifaces
        .iter()
        .find(|i| i.is_up() && !i.is_loopback() && !i.ips.is_empty())
        .or_else(|| ifaces.iter().find(|i| i.is_up() && !i.is_loopback()))
        .cloned()
        .context("no usable interface (try --iface)")
}

// ---------- owner snapshot ----------

struct Owners {
    by_ep: HashMap<(IpAddr, u16), u32>,
    comm: HashMap<u32, String>,
}

fn snapshot() -> Owners {
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

fn parse_local(s: &str, v6: bool) -> Option<(IpAddr, u16)> {
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

// ---------- capture + attribute ----------

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

/// pid → (comm, rx_bytes, tx_bytes), cumulative since start.
type Cum = HashMap<u32, (String, u64, u64)>;

fn account(frame: &[u8], owners: &Owners, local: &[IpAddr], cum: &mut Cum) {
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

// ---------- TUI ----------

fn view_loop(
    term: &mut ratatui::DefaultTerminal,
    sampler: &mut ProcSampler,
    ifname: &str,
    cfg: &Config,
) -> Result<()> {
    let mut dec = cfg.units == "decimal";
    let mut rates: Vec<ProcRate> = vec![];
    let mut tick = Instant::now();
    let started = Instant::now();

    loop {
        // capture until 1s tick elapsed, polling keys between reads
        while tick.elapsed() < Duration::from_secs(1) {
            if crossterm::event::poll(Duration::from_millis(0))? {
                if let crossterm::event::Event::Key(k) = crossterm::event::read()? {
                    match k.code {
                        crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Esc => {
                            return Ok(())
                        }
                        crossterm::event::KeyCode::Char('u') => dec = !dec,
                        _ => {}
                    }
                }
            }
            if !sampler.poll() {
                anyhow::bail!("capture channel died");
            }
        }
        tick = Instant::now();
        rates = sampler.rates();
        rates.truncate(25);

        let tot_rx: u64 = rates.iter().map(|r| r.rx).sum();
        let tot_tx: u64 = rates.iter().map(|r| r.tx).sum();
        let max_r = rates.first().map(|r| r.rx + r.tx).unwrap_or(1);
        let up_s = started.elapsed().as_secs();

        term.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(1),
                ])
                .split(f.area());
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " ▼ ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{}/s ", fmtx::fmt_bytes(tot_rx as i64, dec))),
                    Span::styled(
                        " ▲ ",
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{}/s ", fmtx::fmt_bytes(tot_tx as i64, dec))),
                    Span::styled("│ sorting by rate ", Style::default().fg(Color::Yellow)),
                    Span::raw(format!("│ up {up_s}s ")),
                ]))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" top-proc on {ifname} ")),
                ),
                chunks[0],
            );
            let body: Vec<Row> = rates
                .iter()
                .map(|r| {
                    let share = ((r.rx + r.tx) as f64 / max_r as f64 * 10.0).round() as usize;
                    let name = if r.pid == 0 {
                        r.comm.clone()
                    } else {
                        format!("{} [{}]", r.comm, r.pid)
                    };
                    Row::new(vec![
                        name,
                        format!("{}/s", fmtx::fmt_bytes(r.rx as i64, dec)),
                        format!("{}/s", fmtx::fmt_bytes(r.tx as i64, dec)),
                        format!("{}/s", fmtx::fmt_bytes((r.rx + r.tx) as i64, dec)),
                        fmtx::bar(share as f64 / 10.0, 10),
                    ])
                })
                .collect();
            f.render_widget(
                Table::new(
                    body,
                    [
                        Constraint::Percentage(34),
                        Constraint::Percentage(16),
                        Constraint::Percentage(16),
                        Constraint::Percentage(16),
                        Constraint::Percentage(18),
                    ],
                )
                .header(
                    Row::new(vec!["PROCESS", "DOWN/s", "UP/s", "TOTAL/s", "SHARE"])
                        .style(Style::default().fg(Color::Yellow)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" per-process live (5s owner refresh; short flows → unknown) "),
                ),
                chunks[1],
            );
            f.render_widget(
                Paragraph::new(
                    " q quit │ u units │ needs CAP_NET_RAW: run with sudo for full view ",
                )
                .style(Style::default().fg(Color::Gray)),
                chunks[2],
            );
        })?;
    }
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
        // must not panic, even unprivileged (sees at least own sockets)
        let o = snapshot();
        let _ = (o.by_ep.len(), o.comm.len());
    }

    #[test]
    fn iface_pick_explicit_missing_errors() {
        let ifaces = datalink::interfaces();
        assert!(pick_iface(&ifaces, Some("no-such-iface-xyz")).is_err());
    }
}
