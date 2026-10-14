use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;

/// Parse a `/proc/net/dev` body.
///
/// `rx_bytes` is column 0 and `tx_bytes` column 8 — the order of the kernel's
/// `struct rtnl_link_stats64` (Documentation/networking/statistics.rst). Lines
/// with fewer than 16 columns, or no `:` at all, are skipped.
pub fn parse_proc(text: &str) -> HashMap<String, (u64, u64)> {
    let mut map = HashMap::new();
    for line in text.lines().skip(2) {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let iface = line[..colon].trim().to_string();
        let rest: Vec<&str> = line[colon + 1..].split_whitespace().collect();
        if rest.len() < 16 {
            continue;
        }
        let rx: u64 = rest[0].parse().unwrap_or(0);
        let tx: u64 = rest[8].parse().unwrap_or(0);
        map.insert(iface, (rx, tx));
    }
    map
}

/// Per-iface totals from /proc/net/dev (fallback /sys/class/net).
pub fn read_proc() -> Result<HashMap<String, (u64, u64)>> {
    if let Ok(text) = std::fs::read_to_string("/proc/net/dev") {
        let map = parse_proc(&text);
        if !map.is_empty() {
            return Ok(map);
        }
    }
    // fallback sysfs
    let mut map = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/sys/class/net") {
        for e in dir.flatten() {
            let iface = e.file_name().to_string_lossy().into_owned();
            let rx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_bytes"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            let tx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/tx_bytes"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            map.insert(iface, (rx, tx));
        }
    }
    Ok(map)
}

/// LAN byte counters from our nft table (global, not per-iface in v1).
/// Returns (lan_rx_bytes, lan_tx_bytes) or None when the table is missing,
/// unreadable, or either named counter is absent.
pub fn read_nft() -> Option<(u64, u64)> {
    let out = Command::new("nft")
        .args(["--json", "list", "counters", "table", "inet", "netmeter"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nft_counters(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `nft --json list counters table inet netmeter`.
pub fn parse_nft_counters(text: &str) -> Option<(u64, u64)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut lan_rx = None;
    let mut lan_tx = None;
    if let Some(arr) = v.get("nftables").and_then(|a| a.as_array()) {
        for obj in arr {
            if let Some(c) = obj.get("counter") {
                let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let bytes = c.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0);
                match name {
                    "lan_rx_bytes" => lan_rx = Some(bytes),
                    "lan_tx_bytes" => lan_tx = Some(bytes),
                    _ => {}
                }
            }
        }
    }
    match (lan_rx, lan_tx) {
        (Some(r), Some(t)) => Some((r, t)),
        _ => None,
    }
}

pub fn nft_table_exists() -> bool {
    Command::new("nft")
        .args(["list", "table", "inet", "netmeter"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate nft ruleset text from lan subnets (v4/v6 split).
pub fn render_ruleset(lan_subnets: &[String]) -> String {
    let mut v4 = vec![];
    let mut v6 = vec![];
    for s in lan_subnets {
        if s.contains(':') {
            v6.push(s.clone());
        } else {
            v4.push(s.clone());
        }
    }
    let v4e = if v4.is_empty() {
        "10.0.0.0/8".to_string()
    } else {
        v4.join(", ")
    };
    let v6e = if v6.is_empty() {
        "fe80::/10".to_string()
    } else {
        v6.join(", ")
    };
    format!(
        r#"table inet netmeter {{
    set lan4 {{ type ipv4_addr; flags interval; elements = {{ {v4e} }} }}
    set lan6 {{ type ipv6_addr; flags interval; elements = {{ {v6e} }} }}
    counter lan_rx_bytes {{ }}
    counter lan_tx_bytes {{ }}
    counter lan_rx_pkts {{ }}
    counter lan_tx_pkts {{ }}
    chain acct_in {{ type filter hook input priority filter; policy accept;
        ip saddr @lan4 counter name lan_rx_bytes accept
        ip6 saddr @lan6 counter name lan_rx_bytes accept
    }}
    chain acct_out {{ type filter hook output priority filter; policy accept;
        ip daddr @lan4 counter name lan_tx_bytes accept
        ip6 daddr @lan6 counter name lan_tx_bytes accept
    }}
}}
"#
    )
}

pub fn install_nft(lan_subnets: &[String]) -> Result<()> {
    let rs = render_ruleset(lan_subnets);
    // `nft -f` runs the file as one transaction: `add table` on an existing
    // table fails and aborts the whole batch, so a second `daemon install`
    // (or a changed lan_subnets) never took effect. Drop any previous table
    // first; `destroy` is a no-op when absent, and it is run separately with
    // the error ignored so a missing table is not fatal.
    let _ = Command::new("nft")
        .args(["destroy", "table", "inet", "netmeter"])
        .output();
    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(rs.as_bytes())?;
    } // drop stdin so nft sees EOF
    let out = child.wait_with_output()?;
    anyhow::ensure!(
        out.status.success(),
        "nft -f failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim snapshot of this host's `/proc/net/dev` (2026-10-14). Note the
    /// two header lines, the right-aligned `    lo:` and `enp3s0:` with no
    /// space before the colon.
    const PROC: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 159987594  135595    0    0    0     0          0         0 159987594  135595    0    0    0     0       0          0
enp3s0:   17355     226    0    0    0     0          0       210    93880     173    0    0    0     0       0          0
wlp4s0:       0       0       0    0    0     0          0         0        0       0    0    0    0     0       0          0
enxa600b05be45b: 72154591  138505    7    0    0     7          0         0 93598623  122878    0    0    0     0       0          0
";

    #[test]
    fn parses_proc_net_dev_columns() {
        let m = parse_proc(PROC);
        // rx is column 0, tx is column 8 of the kernel's rtnl_link_stats64
        assert_eq!(m.get("lo"), Some(&(159987594, 159987594)));
        assert_eq!(m.get("enp3s0"), Some(&(17355, 93880)));
        assert_eq!(m.get("wlp4s0"), Some(&(0, 0)));
        assert_eq!(m.get("enxa600b05be45b"), Some(&(72154591, 93598623)));
        assert_eq!(m.len(), 4, "header lines must not become interfaces");
    }

    #[test]
    fn parses_proc_skips_malformed_lines() {
        let m = parse_proc(
            "h1\nh2\neth0: 1 2\nshort: 1 2 3\nno_colon_here\neth1: 5 0 0 0 0 0 0 0 7 0 0 0 0 0 0 0\n",
        );
        assert_eq!(m.get("eth0"), None, "fewer than 16 columns must be skipped");
        assert_eq!(m.get("eth1"), Some(&(5, 7)));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn parses_proc_empty_input() {
        assert!(parse_proc("").is_empty());
        assert!(parse_proc("only one header line\n").is_empty());
    }

    #[test]
    fn parses_nft_counters() {
        // Shape of `nft --json list counters table inet netmeter`: the packet
        // counters share the same object array and must be ignored.
        let json = r#"{"nftables":[
            {"counter":{"family":"inet","name":"lan_rx_bytes","bytes":1234,"packets":10}},
            {"counter":{"family":"inet","name":"lan_tx_bytes","bytes":99,"packets":3}},
            {"counter":{"family":"inet","name":"lan_rx_pkts","bytes":7,"packets":7}}
        ]}"#;
        assert_eq!(parse_nft_counters(json), Some((1234, 99)));
    }

    #[test]
    fn nft_counters_need_both_named_counters() {
        // One named counter alone cannot produce a split; treat it as absent so
        // the daemon falls back to TOTAL-only instead of guessing.
        let only_rx = r#"{"nftables":[{"counter":{"name":"lan_rx_bytes","bytes":1}}]}"#;
        assert_eq!(parse_nft_counters(only_rx), None);
        assert_eq!(parse_nft_counters(r#"{"nftables":[]}"#), None);
        assert_eq!(parse_nft_counters("not json"), None);
        assert_eq!(parse_nft_counters(""), None);
        // anonymous counters carry no name and must never be mistaken for ours
        let anon = r#"{"nftables":[{"counter":{"bytes":5}},{"counter":{"bytes":6}}]}"#;
        assert_eq!(parse_nft_counters(anon), None);
    }

    #[test]
    fn ruleset_uses_named_counters_and_both_hooks() {
        // Anonymous counters cannot be reset (nftables #1401) and are invisible
        // to `list counters`, so a regression here silently kills the split.
        let rs = render_ruleset(&["10.0.0.0/8".into(), "fe80::/10".into()]);
        assert!(
            rs.contains("counter lan_rx_bytes"),
            "rx counter must be named"
        );
        assert!(
            rs.contains("counter lan_tx_bytes"),
            "tx counter must be named"
        );
        assert!(rs.contains("chain acct_in"), "ingress hook missing");
        assert!(rs.contains("chain acct_out"), "egress hook missing");
        assert!(rs.contains("10.0.0.0/8"), "v4 subnet missing");
        assert!(rs.contains("fe80::/10"), "v6 subnet missing");
    }

    #[test]
    fn ruleset_defaults_when_no_subnets_configured() {
        let rs = render_ruleset(&[]);
        assert!(rs.contains("10.0.0.0/8"), "v4 default missing");
        assert!(rs.contains("fe80::/10"), "v6 default missing");
    }
}
