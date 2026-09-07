use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;

/// Per-iface totals from /proc/net/dev (fallback /sys/class/net).
pub fn read_proc() -> Result<HashMap<String, (u64, u64)>> {
    let mut map = HashMap::new();
    if let Ok(text) = std::fs::read_to_string("/proc/net/dev") {
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
        if !map.is_empty() {
            return Ok(map);
        }
    }
    // fallback sysfs
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
/// Returns (lan_rx_bytes, lan_tx_bytes) or None when table missing / no perms.
pub fn read_nft() -> Option<(u64, u64)> {
    let out = Command::new("nft")
        .args(["--json", "list", "counters", "table", "inet", "netmeter"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
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
    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Write;
    child.stdin.take().unwrap().write_all(rs.as_bytes())?;
    let st = child.wait()?;
    anyhow::ensure!(st.success(), "nft -f failed");
    Ok(())
}
