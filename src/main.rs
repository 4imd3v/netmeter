mod capture;
mod config;
mod daemon;
mod db;
mod fmtx;
mod live;
mod top_proc;
mod user_agent;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rusqlite::params;

use config::Config;

#[derive(Debug, Clone, ValueEnum)]
enum Period {
    Hour,
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, ValueEnum)]
enum OutFmt {
    Csv,
    Json,
}

#[derive(Parser, Debug)]
#[command(name = "netmeter", version, about = "Local-first LAN/WAN/TOTAL bandwidth meter (Linux)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Live 1s dashboard (q to quit)
    Live {
        #[arg(long)]
        iface: Option<String>,
    },
    /// Show historical usage
    Show {
        #[arg(long, value_enum, default_value = "day")]
        period: Period,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        iface: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        no_color: bool,
    },
    /// Top talkers by iface or day
    Top {
        #[arg(long, default_value = "iface")]
        by: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Daemon + system status
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Config get/set/path/reset
    Config {
        #[command(subcommand)]
        op: ConfigOp,
    },
    /// Export range to csv/json
    Export {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, value_enum, default_value = "csv")]
        format: OutFmt,
    },
    /// One-shot vnStat history import (vnstat --json)
    Import {
        #[arg(long, default_value = "vnstat")]
        from: String,
        #[arg(long)]
        db: Option<String>,
    },
    /// Shell completions
    Completions { shell: clap_complete::Shell },
    /// Print roff man page to stdout (packaging embeds it)
    Manpage,
    /// Background daemon controls
    Daemon {
        #[command(subcommand)]
        op: DaemonOp,
    },
    /// Live per-process bandwidth (needs CAP_NET_RAW: run with sudo)
    TopProc {
        #[arg(long)]
        iface: Option<String>,
    },
    /// Per-user budget notifier (runs as systemd --user unit)
    UserAgent {
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigOp {
    Get { key: Option<String> },
    Set { key: String, value: String },
    Path,
    Reset,
}

#[derive(Subcommand, Debug)]
enum DaemonOp {
    Run,
    Start,
    Stop,
    Restart,
    Install,
    Uninstall,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = Config::load()?;
    match cli.cmd {
        Cmd::Live { iface } => live::run(cfg, iface),
        Cmd::Show {
            period,
            from,
            iface,
            json,
            ..
        } => cmd_show(cfg, period, from, iface, json),
        Cmd::Top { by, limit } => cmd_top(cfg, by, limit),
        Cmd::Status { json } => cmd_status(cfg, json),
        Cmd::Config { op } => cmd_config(cfg, op),
        Cmd::Export { from, to, format } => cmd_export(cfg, from, to, format),
        Cmd::Import { db, .. } => cmd_import(cfg, db),
        Cmd::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "netmeter", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Manpage => {
            use clap::CommandFactory;
            let cmd = Cli::command();
            let man = clap_mangen::Man::new(cmd);
            man.render(&mut std::io::stdout())?;
            Ok(())
        }
        Cmd::Daemon { op } => cmd_daemon(cfg, op),
        Cmd::TopProc { iface } => top_proc::run(cfg, iface),
        Cmd::UserAgent { once } => user_agent::run(cfg, once),
    }
}

// ---------- show ----------
fn cmd_show(
    cfg: Config,
    period: Period,
    from: Option<String>,
    iface: Option<String>,
    as_json: bool,
) -> Result<()> {
    let conn = match db::open(&cfg.db_path_expanded(), true) {
        Ok(c) => c,
        Err(_) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({"api_version": db::API_VERSION, "rows": []})
                );
                return Ok(());
            }
            eprintln!("no data yet — is the daemon running? (`netmeter status`, `sudo netmeter daemon install`)");
            return Ok(());
        }
    };
    let _ = db::rollup(&conn, cfg.timezone != "utc"); // best-effort backfill (read-only may fail on locked? ignore)
    let tz_local = cfg.timezone != "utc";
    let dec = cfg.units == "decimal";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let end = from
        .map(|s| parse_date(&s, tz_local))
        .transpose()?
        .unwrap_or(now);
    let (table, from_ts, bucket): (&str, i64, fn(i64, bool) -> String) = match period {
        Period::Hour => ("hourly", end - 24 * 3600, |t, l| fmtx::fmt_date(t, l, true)),
        Period::Day => ("daily", end - 30 * 86400, |t, l| {
            fmtx::fmt_date(t, l, false)
        }),
        Period::Week => ("daily", end - 12 * 7 * 86400, |t, l| {
            fmtx::fmt_date(t, l, false)
        }),
        Period::Month => ("monthly", end - 365 * 86400, |t, l| {
            fmtx::fmt_date(t, l, false)
        }),
    };
    // week/month need re-bucketing from daily/monthly rows
    let mut rows = db::query_range(&conn, table, from_ts, end + 1, iface.as_deref())?;
    if matches!(period, Period::Week) {
        rows = rebucket_week(rows, tz_local);
    }
    if rows.is_empty() {
        // rollups run hourly in daemon; fall back to live samples so fresh data shows instantly
        let s = db::query_range(&conn, "samples", from_ts, end + 1, iface.as_deref())?;
        rows = bucket_samples(s, &period, tz_local);
    }
    if as_json {
        let arr: Vec<_> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "ts": r.ts, "date": bucket(r.ts, tz_local), "iface": r.iface,
                    "rx": r.rx, "tx": r.tx, "total": r.total(),
                    "lan_rx": r.lan_rx, "lan_tx": r.lan_tx,
                    "wan_rx": r.wan_rx(), "wan_tx": r.wan_tx(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"api_version": db::API_VERSION, "period": format!("{:?}", period).to_lowercase(), "rows": arr})
        );
        return Ok(());
    }
    println!(
        "{:<16} {:>10} {:>10} {:>10} {:>16} {:>16}",
        "DATE", "DOWN", "UP", "TOTAL", "LAN ↓/↑", "WAN ↓/↑"
    );
    for r in &rows {
        println!(
            "{:<16} {:>10} {:>10} {:>10} {:>16} {:>16}",
            bucket(r.ts, tz_local),
            fmtx::fmt_bytes(r.rx, dec),
            fmtx::fmt_bytes(r.tx, dec),
            fmtx::fmt_bytes(r.total(), dec),
            format!(
                "{}/{}",
                fmtx::fmt_bytes(r.lan_rx, dec),
                fmtx::fmt_bytes(r.lan_tx, dec)
            ),
            format!(
                "{}/{}",
                fmtx::fmt_bytes(r.wan_rx(), dec),
                fmtx::fmt_bytes(r.wan_tx(), dec)
            ),
        );
    }
    // budget bar when the view matches the configured budget window
    let view_period = match period {
        Period::Day => "day",
        Period::Week => "week",
        Period::Month => "month",
        Period::Hour => "hour",
    };
    if let Some((bperiod, bgb)) = cfg.effective_budget() {
        if bperiod == view_period {
            let used: i64 = rows
                .iter()
                .map(|r| {
                    if cfg.budget_basis == "wan" {
                        r.wan_rx() + r.wan_tx()
                    } else {
                        r.total()
                    }
                })
                .sum();
            let budget = (bgb * 1_073_741_824.0) as i64;
            let frac = used as f64 / budget as f64;
            println!(
                "Budget ({}): {}/{} {} {:.0}%",
                db::period_adverb(bperiod),
                fmtx::fmt_bytes(used, dec),
                fmtx::fmt_bytes(budget, dec),
                fmtx::bar(frac, 10),
                frac * 100.0
            );
        }
    }
    if rows.is_empty() {
        eprintln!(
            "no data yet — is the daemon running? (`netmeter status`, `netmeter daemon install`)"
        );
    }
    Ok(())
}

fn rebucket_week(rows: Vec<db::Row>, local: bool) -> Vec<db::Row> {
    use std::collections::BTreeMap;
    let tzone = if local {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::UTC
    };
    let mut m: BTreeMap<i64, db::Row> = BTreeMap::new();
    for r in rows {
        let z = jiff::Timestamp::from_second(r.ts)
            .unwrap()
            .to_zoned(tzone.clone());
        let dt = z.datetime();
        let off = dt.date().weekday().to_monday_zero_offset() as i32;
        let monday = dt.date().checked_sub(jiff::ToSpan::days(off)).unwrap();
        let monday_ts =
            jiff::civil::DateTime::new(monday.year(), monday.month(), monday.day(), 0, 0, 0, 0)
                .unwrap()
                .to_zoned(tzone.clone())
                .unwrap()
                .timestamp()
                .as_second();
        let e = m.entry(monday_ts).or_insert(db::Row {
            ts: monday_ts,
            iface: "_all".into(),
            ..Default::default()
        });
        e.rx += r.rx;
        e.tx += r.tx;
        e.lan_rx += r.lan_rx;
        e.lan_tx += r.lan_tx;
    }
    m.into_values().collect()
}

/// Aggregate raw samples into show buckets (fallback before daemon rollup runs).
fn bucket_samples(rows: Vec<db::Row>, period: &Period, local: bool) -> Vec<db::Row> {
    use std::collections::BTreeMap;
    let floor: fn(i64, bool) -> i64 = match period {
        Period::Hour => db::floor_hour,
        Period::Day | Period::Week => db::floor_day,
        Period::Month => db::floor_month,
    };
    let mut m: BTreeMap<i64, db::Row> = BTreeMap::new();
    for r in rows {
        let b = floor(r.ts, local);
        let e = m.entry(b).or_insert(db::Row {
            ts: b,
            iface: "_all".into(),
            ..Default::default()
        });
        e.rx += r.rx;
        e.tx += r.tx;
        e.lan_rx += r.lan_rx;
        e.lan_tx += r.lan_tx;
    }
    let mut out: Vec<_> = m.into_values().collect();
    if matches!(period, Period::Week) {
        out = rebucket_week(out, local);
    }
    out
}

fn parse_date(s: &str, local: bool) -> Result<i64> {
    // YYYY-MM-DD
    let d: jiff::civil::Date = s.parse().context("expected --from YYYY-MM-DD")?;
    let tz = if local {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::UTC
    };
    Ok(
        jiff::civil::DateTime::new(d.year(), d.month(), d.day(), 0, 0, 0, 0)?
            .to_zoned(tz)?
            .timestamp()
            .as_second()
            + 86400,
    )
}

// ---------- top ----------
fn cmd_top(cfg: Config, by: String, limit: usize) -> Result<()> {
    let conn = match db::open(&cfg.db_path_expanded(), true) {
        Ok(c) => c,
        Err(_) => {
            println!(
                "{:<12} {:>10} {:>10} {:>10}",
                "IFACE", "DOWN", "UP", "TOTAL"
            );
            eprintln!("no data yet — start the daemon first");
            return Ok(());
        }
    };
    let dec = cfg.units == "decimal";
    if by == "day" {
        let rows = db::query_range(&conn, "daily", 0, i64::MAX, None)?;
        let mut rows = rows;
        rows.sort_by_key(|r| -(r.total()));
        println!("{:<12} {:>10} {:>10} {:>10}", "DATE", "DOWN", "UP", "TOTAL");
        for r in rows.into_iter().take(limit) {
            println!(
                "{:<12} {:>10} {:>10} {:>10}",
                fmtx::fmt_date(r.ts, cfg.timezone != "utc", false),
                fmtx::fmt_bytes(r.rx, dec),
                fmtx::fmt_bytes(r.tx, dec),
                fmtx::fmt_bytes(r.total(), dec)
            );
        }
    } else {
        // by iface: sum samples all-time per iface
        let mut stmt = conn.prepare("SELECT iface, SUM(rx_total), SUM(tx_total) FROM samples GROUP BY iface ORDER BY SUM(rx_total)+SUM(tx_total) DESC LIMIT ?1")?;
        let rows: Vec<(String, i64, i64)> = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        println!(
            "{:<12} {:>10} {:>10} {:>10}",
            "IFACE", "DOWN", "UP", "TOTAL"
        );
        for (i, rx, tx) in rows {
            println!(
                "{:<12} {:>10} {:>10} {:>10}",
                i,
                fmtx::fmt_bytes(rx, dec),
                fmtx::fmt_bytes(tx, dec),
                fmtx::fmt_bytes(rx + tx, dec)
            );
        }
    }
    Ok(())
}

// ---------- status ----------
fn cmd_status(cfg: Config, as_json: bool) -> Result<()> {
    let db_path = cfg.db_path_expanded();
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let wal_size = std::fs::metadata(format!("{}.wal", db_path.display()))
        .map(|m| m.len())
        .unwrap_or(0);
    let nft_direct = capture::nft_table_exists();
    let db_conn = db::open(&db_path, true).ok();
    // Unprivileged users can't query nft directly; daemon (CAP_NET_ADMIN) can.
    // Fall back to evidence in DB: any LAN bytes ever recorded means split works.
    let lan_seen: i64 = db_conn
        .as_ref()
        .and_then(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(lan_rx)+SUM(lan_tx),0) FROM samples",
                [],
                |r| r.get(0),
            )
            .ok()
        })
        .unwrap_or(0);
    let nft_ok = nft_direct || lan_seen > 0;
    let daemon_alive = std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "netmeter.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let discarded: String = db_conn
        .as_ref()
        .and_then(|c| {
            c.query_row("SELECT v FROM meta WHERE k='discarded_samples'", [], |r| {
                r.get(0)
            })
            .ok()
        })
        .unwrap_or_else(|| "0".into());
    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "api_version": db::API_VERSION, "version": env!("CARGO_PKG_VERSION"),
                "daemon_active": daemon_alive, "nft_split": nft_ok,
                "lan_split": if nft_ok { "available" } else { "unavailable (TOTAL-only)" },
                "db_path": db_path.display().to_string(), "db_bytes": db_size, "wal_bytes": wal_size,
                "discarded_samples": discarded,
            })
        );
        return Ok(());
    }
    println!("netmeter {}", env!("CARGO_PKG_VERSION"));
    println!(
        "daemon:  {}",
        if daemon_alive {
            "active"
        } else {
            "inactive (run `netmeter daemon install`)"
        }
    );
    println!(
        "nft:     {}",
        if nft_ok {
            "split available (LAN/WAN/TOTAL)"
        } else {
            "unavailable → TOTAL-only"
        }
    );
    println!(
        "db:      {} ({} bytes, wal {} bytes)",
        db_path.display(),
        db_size,
        wal_size
    );
    println!("discarded spike samples: {discarded}");
    Ok(())
}

// ---------- config ----------
fn cmd_config(cfg: Config, op: ConfigOp) -> Result<()> {
    match op {
        ConfigOp::Path => {
            println!("{}", Config::system_path().display());
            if let Some(u) = Config::user_path() {
                println!("{}", u.display());
            }
        }
        ConfigOp::Get { key } => {
            let v = serde_json::to_value(&cfg)?;
            if let Some(k) = key {
                println!("{}", v.get(&k).cloned().unwrap_or(serde_json::Value::Null));
            } else {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        }
        ConfigOp::Set { key, value } => {
            // Daemon runs as system user and only reads /etc + its own file:
            // nudge daemon-owned keys toward the system config.
            const DAEMON_KEYS: &[&str] = &[
                "database_path",
                "poll_interval_sec",
                "lan_subnets",
                "exclude_ifaces",
                "force_lan_ifaces",
                "classify_tailscale_as_lan",
                "retention_days_raw",
                "retention_days_hourly",
                "max_rate_mbit",
                "timezone",
                "monthly_budget_gb",
                "budget_gb",
                "budget_period",
                "budget_basis",
                "notify_on_budget",
            ];
            if DAEMON_KEYS.contains(&key.as_str()) {
                eprintln!(
                    "note: `{key}` affects the daemon, which reads /etc/netmeter/config.toml — set it there (sudo) or the service won't see this value"
                );
            }
            let path = Config::user_path().context("no user config dir")?;
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut cur: toml::Value = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| toml::from_str(&s).ok())
                .unwrap_or(toml::Value::Table(toml::map::Map::new()));
            let parsed: toml::Value = if let Ok(n) = value.parse::<i64>() {
                n.into()
            } else if let Ok(n) = value.parse::<f64>() {
                n.into()
            } else if value == "true" || value == "false" {
                (value == "true").into()
            } else {
                value.into()
            };
            cur.as_table_mut().unwrap().insert(key, parsed);
            std::fs::write(&path, toml::to_string_pretty(&cur)?)?;
            println!("wrote {}", path.display());
        }
        ConfigOp::Reset => {
            if let Some(p) = Config::user_path() {
                if p.exists() {
                    std::fs::remove_file(&p)?;
                }
                println!("removed {}", p.display());
            }
        }
    }
    Ok(())
}

// ---------- export ----------
fn cmd_export(cfg: Config, from: String, to: String, format: OutFmt) -> Result<()> {
    let tz_local = cfg.timezone != "utc";
    let f = parse_date(&from, tz_local)?;
    let t = parse_date(&to, tz_local)? + 86400;
    let conn = match db::open(&cfg.db_path_expanded(), true) {
        Ok(c) => c,
        Err(_) => {
            println!("ts,rx,tx,lan_rx,lan_tx,wan_rx,wan_tx");
            return Ok(());
        }
    };
    let rows = db::query_range(&conn, "samples", f, t, None)?;
    match format {
        OutFmt::Json => {
            let arr: Vec<_> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                "ts": r.ts, "rx": r.rx, "tx": r.tx, "lan_rx": r.lan_rx, "lan_tx": r.lan_tx,
                "wan_rx": r.wan_rx(), "wan_tx": r.wan_tx()})
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr)?);
        }
        OutFmt::Csv => {
            println!("ts,rx,tx,lan_rx,lan_tx,wan_rx,wan_tx");
            for r in rows {
                println!(
                    "{},{},{},{},{},{},{}",
                    r.ts,
                    r.rx,
                    r.tx,
                    r.lan_rx,
                    r.lan_tx,
                    r.wan_rx(),
                    r.wan_tx()
                );
            }
        }
    }
    Ok(())
}

// ---------- import vnstat ----------
fn cmd_import(cfg: Config, db_path: Option<String>) -> Result<()> {
    // read `vnstat --json` from stdin or --db file? v1: run vnstat --json live
    let args: Vec<&str> = if let Some(p) = db_path.as_deref() {
        vec!["--json", "--db", p]
    } else {
        vec!["--json"]
    };
    let out = std::process::Command::new("vnstat")
        .args(&args)
        .output()
        .context("run `vnstat --json` (is vnstat installed?)")?;
    anyhow::ensure!(out.status.success(), "vnstat --json failed");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let conn = db::open(&cfg.db_path_expanded(), false)?;
    let tz_local = cfg.timezone != "utc";
    let mut n = 0;
    if let Some(ifaces) = v.get("interfaces").and_then(|a| a.as_array()) {
        for iface in ifaces {
            let name = iface.get("name").and_then(|s| s.as_str()).unwrap_or("?");
            if cfg.excluded(name) {
                continue;
            }
            // daily traffic list: interfaces[].traffic.days[] {date:{year,month,day}, rx, tx}
            if let Some(days) = iface.pointer("/traffic/days").and_then(|a| a.as_array()) {
                for d in days {
                    let (y, mo, da) = (
                        d.pointer("/date/year")
                            .and_then(|x| x.as_i64())
                            .unwrap_or(0) as i32,
                        d.pointer("/date/month")
                            .and_then(|x| x.as_i64())
                            .unwrap_or(1) as i32,
                        d.pointer("/date/day").and_then(|x| x.as_i64()).unwrap_or(1) as i32,
                    );
                    let rx = d.get("rx").and_then(|x| x.as_i64()).unwrap_or(0);
                    let tx = d.get("tx").and_then(|x| x.as_i64()).unwrap_or(0);
                    if y < 2000 {
                        continue;
                    }
                    let tzone = if tz_local {
                        jiff::tz::TimeZone::system()
                    } else {
                        jiff::tz::TimeZone::UTC
                    };
                    let ts = jiff::civil::DateTime::new(y as i16, mo as i8, da as i8, 0, 0, 0, 0)?
                        .to_zoned(tzone)?
                        .timestamp()
                        .as_second();
                    conn.execute(
                        "INSERT INTO daily(ts_day,iface,rx,tx,lan_rx,lan_tx) VALUES(?,?,?,?,0,0)
                         ON CONFLICT(ts_day,iface) DO UPDATE SET rx=excluded.rx, tx=excluded.tx",
                        params![ts, name, rx, tx],
                    )?;
                    n += 1;
                }
            }
        }
    }
    println!("imported {n} daily rows from vnstat (LAN unknown → 0, WAN=TOTAL)");
    Ok(())
}

// ---------- daemon ctl ----------
fn cmd_daemon(cfg: Config, op: DaemonOp) -> Result<()> {
    match op {
        DaemonOp::Run => daemon::run(cfg),
        DaemonOp::Start => run_systemctl(&["start", "netmeter.service"]),
        DaemonOp::Stop => run_systemctl(&["stop", "netmeter.service"]),
        DaemonOp::Restart => run_systemctl(&["restart", "netmeter.service"]),
        DaemonOp::Install => daemon_install(&cfg),
        DaemonOp::Uninstall => run_systemctl(&["disable", "--now", "netmeter.service"]),
    }
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let st = std::process::Command::new("systemctl")
        .args(args)
        .status()?;
    anyhow::ensure!(st.success(), "systemctl {args:?} failed");
    Ok(())
}

fn daemon_install(cfg: &Config) -> Result<()> {
    // nft table (needs root)
    if let Err(e) = capture::install_nft(&cfg.lan_subnets) {
        eprintln!(
            "nft install failed ({e}) — continuing TOTAL-only; re-run as root to enable split"
        );
    }
    let unit = r#"[Unit]
Description=NetMeter bandwidth meter
After=network.target time-sync.target
Wants=time-sync.target

[Service]
Type=simple
User=netmeter
Group=netmeter
AmbientCapabilities=CAP_NET_ADMIN
CapabilityBoundingSet=CAP_NET_ADMIN
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelModules=true
ReadWritePaths=/var/lib/netmeter
ExecStart=/usr/bin/netmeter daemon run
Restart=on-failure

[Install]
WantedBy=multi-user.target
"#;
    let user_unit = r#"[Unit]
Description=NetMeter budget notifier (user session)
After=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/bin/netmeter user-agent
Restart=on-failure

[Install]
WantedBy=default.target
"#;
    std::fs::create_dir_all("/var/lib/netmeter").ok();
    // create netmeter user if missing
    let _ = std::process::Command::new("id")
        .arg("netmeter")
        .output()
        .map(|o| {
            if !o.status.success() {
                let _ = std::process::Command::new("useradd")
                    .args([
                        "-r",
                        "-s",
                        "/usr/sbin/nologin",
                        "-d",
                        "/var/lib/netmeter",
                        "netmeter",
                    ])
                    .status();
            }
        });
    let _ = std::process::Command::new("chown")
        .args(["netmeter:netmeter", "/var/lib/netmeter"])
        .status();
    std::fs::write("/etc/systemd/system/netmeter.service", unit)?;
    println!("wrote /etc/systemd/system/netmeter.service");
    if let Some(mut home) = std::env::var("SUDO_USER").ok().and_then(|u| users_home(&u)) {
        home.push(".config/systemd/user/netmeter-agent.service");
        if let Some(p) = home.parent() {
            std::fs::create_dir_all(p).ok();
        }
        std::fs::write(&home, user_unit).ok();
        println!("wrote {}", home.display());
    } else {
        println!("(user agent unit) save this to ~/.config/systemd/user/netmeter-agent.service:\n{user_unit}");
    }
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", "netmeter.service"])?;
    println!(
        "netmeter.service enabled+started. DB: {}",
        cfg.db_path_expanded().display()
    );
    Ok(())
}

fn users_home(user: &str) -> Option<std::path::PathBuf> {
    // minimal: /home/<user>
    let p = std::path::PathBuf::from(format!("/home/{user}"));
    if p.exists() {
        Some(p)
    } else {
        None
    }
}
