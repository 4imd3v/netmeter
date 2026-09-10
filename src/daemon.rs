use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::{capture, config::Config, db, proc_cap::ProcRecorder};

/// Classic counter-delta with wrap/reset/sanity handling (vnStat lessons).
/// Returns accepted delta or 0 when discarded.
fn delta(old: u64, new: u64, max_bytes_per_tick: u64) -> (u64, bool) {
    const U32: u64 = 1 << 32;
    // 32-bit wrap looks identical to a reset (big old → tiny new). Prefer wrap
    // when old sits within one tick of the 32-bit boundary (plausible overflow).
    let near_wrap = old >= U32.saturating_sub(max_bytes_per_tick) && old < U32;
    // reset: counter dropped to ~zero (iface flap / reboot)
    if new < old && new < 1024 * 1024 && !near_wrap {
        return (new, false); // delta = new (count since reset), not a wrap
    }
    let d = if new >= old {
        new - old
    } else {
        // 32-bit wrap preferred (most phantom-GB bugs are 32-bit);
        // 64-bit wrap practically impossible but handled.
        const U32: u64 = 1 << 32;
        if old > U32 {
            new.wrapping_sub(old)
        } else {
            U32 - old + new
        }
    };
    if d > max_bytes_per_tick {
        return (0, true); // discarded spike
    }
    (d, false)
}

pub fn max_bytes_per_tick(cfg: &Config, iface: &str) -> u64 {
    let mbit = std::fs::read_to_string(format!("/sys/class/net/{iface}/speed"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(cfg.max_rate_mbit);
    let mbit = mbit.max(1);
    // bytes = mbit*1e6/8 * interval * slack(1.25)
    ((mbit as f64 * 1_000_000.0 / 8.0) * cfg.poll_interval_sec as f64 * 1.25) as u64
}

pub fn run(cfg: Config) -> Result<()> {
    tracing::info!("netmeter daemon start, poll={}s", cfg.poll_interval_sec);
    // NTP/RTC gate: wait up to 60s for clock after 2020 (vnStat TimeSyncWait lesson)
    for _ in 0..12 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if now > 1_577_836_800 {
            break;
        }
        tracing::warn!("clock unsynced, waiting 5s");
        std::thread::sleep(Duration::from_secs(5));
    }
    if !capture::nft_table_exists() {
        tracing::warn!("nft table inet netmeter missing → TOTAL-only mode (run `netmeter daemon install` as root)");
    }

    let db_path = cfg.db_path_expanded();
    let conn = db::open(&db_path, false)?;
    let tz_local = cfg.timezone != "utc";

    let mut prev: HashMap<String, (u64, u64)> = HashMap::new();
    let mut prev_lan: Option<(u64, u64)> = None;
    let mut proc_rec = if cfg.proc_recording {
        tracing::info!("proc recording on (30d cap, see proc_retention_days)");
        Some(ProcRecorder::spawn(&cfg.exclude_ifaces))
    } else {
        None
    };
    let mut last_tick = Instant::now();
    let mut last_rollup = Instant::now() - Duration::from_secs(3600);
    let mut last_retain = Instant::now();

    loop {
        let interval = cfg.poll_interval_sec.max(1);
        std::thread::sleep(Duration::from_secs(interval));
        let now_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // sleep/suspend gap: don't fabricate
        let gap = last_tick.elapsed().as_secs();
        last_tick = Instant::now();
        if gap > interval * 2 + 5 {
            tracing::warn!("time gap {gap}s (sleep/suspend?) — carrying forward, no sample");
            prev.clear(); // force re-baseline, skip one sample
            prev_lan = capture::read_nft();
            continue;
        }

        let totals = capture::read_proc().unwrap_or_default();
        let lan_now = capture::read_nft();
        let (lan_rx_d, lan_tx_d) = match (prev_lan, lan_now) {
            (Some((pr, pt)), Some((nr, nt))) => {
                let maxb = max_bytes_per_tick(&cfg, "lan");
                let (dr, _) = delta(pr, nr, maxb);
                let (dt, _) = delta(pt, nt, maxb);
                (dr as i64, dt as i64)
            }
            _ => (0, 0),
        };
        prev_lan = lan_now;

        // Distribute global LAN delta across ifaces proportional to their TOTAL delta
        // (v1 honest approximation; per-iface LAN impossible without per-iface nft rules).
        struct PerIf {
            iface: String,
            rx: i64,
            tx: i64,
        }
        let mut per: Vec<PerIf> = vec![];
        let mut discarded = 0u64;
        for (iface, (rx, tx)) in &totals {
            if cfg.excluded(iface) {
                continue;
            }
            let maxb = max_bytes_per_tick(&cfg, iface);
            if let Some((orx, otx)) = prev.get(iface) {
                let (drx, dx1) = delta(*orx, *rx, maxb);
                let (dtx, dx2) = delta(*otx, *tx, maxb);
                if dx1 || dx2 {
                    discarded += 1;
                }
                if drx > 0 || dtx > 0 {
                    per.push(PerIf {
                        iface: iface.clone(),
                        rx: drx as i64,
                        tx: dtx as i64,
                    });
                }
            }
        }
        // re-baseline
        prev = totals
            .into_iter()
            .filter(|(i, _)| !cfg.excluded(i))
            .collect();

        if discarded > 0 {
            let c: i64 = conn
                .query_row(
                    "SELECT COALESCE((SELECT v FROM meta WHERE k='discarded_samples'),'0')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or("0".to_string())
                .parse()
                .unwrap_or(0);
            let _ = conn.execute(
                "INSERT INTO meta(k,v) VALUES('discarded_samples',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![(c + discarded as i64).to_string()],
            );
            tracing::warn!("discarded {discarded} spike sample(s) (sanity filter)");
        }

        if !per.is_empty() {
            let tot_rx: i64 = per.iter().map(|p| p.rx).sum();
            let tot_tx: i64 = per.iter().map(|p| p.tx).sum();
            let tx = conn.unchecked_transaction()?;
            {
                let mut ins = tx.prepare(
                    "INSERT INTO samples(ts,iface,rx_total,tx_total,lan_rx,lan_tx) VALUES(?,?,?,?,?,?)
                     ON CONFLICT(ts,iface) DO UPDATE SET rx_total=excluded.rx_total, tx_total=excluded.tx_total,
                     lan_rx=excluded.lan_rx, lan_tx=excluded.lan_tx",
                )?;
                for p in &per {
                    // pro-rate LAN; forced-LAN ifaces (tailscale) claim LAN first
                    let (lrx, ltx) = if cfg.forced_lan(&p.iface) {
                        (p.rx, p.tx)
                    } else if tot_rx > 0 || tot_tx > 0 {
                        let frx = if tot_rx > 0 {
                            p.rx as f64 / tot_rx as f64
                        } else {
                            0.0
                        };
                        let ftx = if tot_tx > 0 {
                            p.tx as f64 / tot_tx as f64
                        } else {
                            0.0
                        };
                        (
                            (lan_rx_d as f64 * frx) as i64,
                            (lan_tx_d as f64 * ftx) as i64,
                        )
                    } else {
                        (0, 0)
                    };
                    // never attribute more LAN than total on an iface
                    let lrx = lrx.min(p.rx);
                    let ltx = ltx.min(p.tx);
                    ins.execute(params![now_wall, p.iface, p.rx, p.tx, lrx, ltx])?;
                }
            }
            tx.commit()?;
            check_budget(&conn, &cfg, tz_local)?;
        }

        // Always-on per-app recording: `drain_deltas()` returns bytes since the
        // last tick (capture keeps appending to a fresh map), so insert directly.
        // Runs even when `per` is empty — idle TOTAL ticks must not starve it.
        if let Some(rec) = proc_rec.as_mut() {
            rec.ensure_running(&cfg.exclude_ifaces);
            let cur = rec.drain_deltas();
            if !cur.is_empty() {
                let hour = db::floor_hour(now_wall, tz_local);
                let tx = conn.unchecked_transaction()?;
                {
                    let mut pins = tx.prepare(
                        "INSERT INTO proc_hourly(ts_hour,comm,rx,tx) VALUES(?,?,?,?)
                         ON CONFLICT(ts_hour,comm) DO UPDATE SET rx=rx+excluded.rx, tx=tx+excluded.tx",
                    )?;
                    for (comm, (rx, tx)) in &cur {
                        let (dr, dt) = (*rx as i64, *tx as i64);
                        if dr > 0 || dt > 0 {
                            pins.execute(params![hour, comm, dr, dt])?;
                        }
                    }
                }
                tx.commit()?;
            }
        }

        if last_rollup.elapsed() > Duration::from_secs(3600) {
            let _ = db::rollup(&conn, tz_local);
            last_rollup = Instant::now();
        }
        if last_retain.elapsed() > Duration::from_secs(6 * 3600) {
            enforce_retention(&conn, &cfg)?;
            last_retain = Instant::now();
        }
    }
}

fn enforce_retention(conn: &rusqlite::Connection, cfg: &Config) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cut_raw = now - cfg.retention_days_raw * 86400;
    let cut_hr = now - cfg.retention_days_hourly * 86400;
    conn.execute("DELETE FROM samples WHERE ts < ?1", params![cut_raw])?;
    conn.execute("DELETE FROM hourly WHERE ts_hour < ?1", params![cut_hr])?;
    let cut_proc = now - cfg.proc_retention_days.max(1) * 86400;
    conn.execute(
        "DELETE FROM proc_hourly WHERE ts_hour < ?1",
        params![cut_proc],
    )?;
    // weekly vacuum (cheap enough at this size)
    let _ = conn.execute("PRAGMA wal_checkpoint(PASSIVE)", []);
    Ok(())
}

fn check_budget(conn: &rusqlite::Connection, cfg: &Config, tz_local: bool) -> Result<()> {
    let Some((period, gb)) = cfg.effective_budget() else {
        return Ok(());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let start = db::budget_window_start(now, tz_local, period);
    let key = db::budget_key(now, tz_local, period);
    let (rx, tx, lrx, ltx): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0), COALESCE(SUM(lan_rx),0), COALESCE(SUM(lan_tx),0) FROM samples WHERE ts>=?1",
            params![start],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((0, 0, 0, 0));
    let used = if cfg.budget_basis == "wan" {
        (rx - lrx).max(0) + (tx - ltx).max(0)
    } else {
        rx + tx
    };
    let budget = (gb * 1_073_741_824.0) as i64;
    if budget <= 0 {
        return Ok(());
    }
    let adv = db::period_adverb(period);
    let frac = used as f64 / budget as f64;
    let (c80, c100): (i64, i64) = conn
        .query_row("SELECT COALESCE(crossed80,0), COALESCE(crossed100,0) FROM budget_events WHERE month=?1", params![key], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, 0));
    if frac >= 0.8 && c80 == 0 {
        conn.execute("INSERT INTO budget_events(month,crossed80,crossed100) VALUES(?1,1,0) ON CONFLICT(month) DO UPDATE SET crossed80=1", params![key])?;
        tracing::warn!("NetMeter: 80% of {adv} budget used ({used}/{budget} bytes)");
    }
    if frac >= 1.0 && c100 == 0 {
        conn.execute("INSERT INTO budget_events(month,crossed80,crossed100) VALUES(?1,1,1) ON CONFLICT(month) DO UPDATE SET crossed80=1, crossed100=1", params![key])?;
        tracing::warn!("NetMeter: 100% of {adv} budget used ({used}/{budget} bytes)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::delta;
    #[test]
    fn wrap_32bit_accepted() {
        let old = u32::MAX as u64 - 100;
        let (d, disc) = delta(old, 200, u64::MAX);
        assert!(!disc);
        assert_eq!(d, 301);
    }
    #[test]
    fn reset_detected() {
        let (d, _) = delta(5_000_000_000, 1234, u64::MAX);
        assert_eq!(d, 1234);
    }
    #[test]
    fn spike_discarded() {
        let (_, disc) = delta(1000, 1000 + 10_000_000_000, 1_000_000);
        assert!(disc);
    }
}
