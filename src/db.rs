use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::Path;

pub const SCHEMA_VERSION: i32 = 1;
pub const API_VERSION: i32 = 1;

pub fn open(path: &Path, readonly: bool) -> Result<Connection> {
    if !readonly {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display()))?;
        }
    }
    let conn = if readonly {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open ro {}", path.display()))?
    } else {
        Connection::open(path).with_context(|| format!("open {}", path.display()))?
    };
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;
         PRAGMA cache_size=-64000;
         PRAGMA temp_store=MEMORY;
         PRAGMA foreign_keys=ON;",
    )?;
    if !readonly {
        init(&conn)?;
    }
    Ok(conn)
}

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS samples(
           ts INTEGER NOT NULL, iface TEXT NOT NULL,
           rx_total INTEGER NOT NULL, tx_total INTEGER NOT NULL,
           lan_rx INTEGER NOT NULL, lan_tx INTEGER NOT NULL,
           PRIMARY KEY(ts, iface)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS hourly(
           ts_hour INTEGER NOT NULL, iface TEXT NOT NULL,
           rx INTEGER NOT NULL, tx INTEGER NOT NULL,
           lan_rx INTEGER NOT NULL, lan_tx INTEGER NOT NULL,
           PRIMARY KEY(ts_hour, iface)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS daily(
           ts_day INTEGER NOT NULL, iface TEXT NOT NULL,
           rx INTEGER NOT NULL, tx INTEGER NOT NULL,
           lan_rx INTEGER NOT NULL, lan_tx INTEGER NOT NULL,
           PRIMARY KEY(ts_day, iface)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS monthly(
           ts_mon INTEGER NOT NULL, iface TEXT NOT NULL,
           rx INTEGER NOT NULL, tx INTEGER NOT NULL,
           lan_rx INTEGER NOT NULL, lan_tx INTEGER NOT NULL,
           PRIMARY KEY(ts_mon, iface)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS budget_events(
           month TEXT PRIMARY KEY, crossed80 INTEGER NOT NULL DEFAULT 0,
           crossed100 INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS proc_hourly(
           ts_hour INTEGER NOT NULL, comm TEXT NOT NULL,
           rx INTEGER NOT NULL, tx INTEGER NOT NULL,
           PRIMARY KEY(ts_hour, comm)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;",
    )?;
    conn.execute(
        "INSERT INTO meta(k,v) VALUES('schema_version',?1)
         ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub ts: i64,
    pub iface: String,
    pub rx: i64,
    pub tx: i64,
    pub lan_rx: i64,
    pub lan_tx: i64,
}
impl Row {
    pub fn total(&self) -> i64 {
        self.rx + self.tx
    }
    pub fn wan_rx(&self) -> i64 {
        (self.rx - self.lan_rx).max(0)
    }
    pub fn wan_tx(&self) -> i64 {
        (self.tx - self.lan_tx).max(0)
    }
}

fn sample_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, String, i64, i64, i64, i64)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

/// Newest sample ts already folded into the rollup tables (`meta.last_rollup_ts`).
fn rollup_marker(conn: &Connection) -> Result<Option<i64>> {
    let v: Option<String> = conn
        .query_row("SELECT v FROM meta WHERE k='last_rollup_ts'", [], |r| {
            r.get(0)
        })
        .ok();
    Ok(v.and_then(|s| s.parse().ok()))
}

/// Roll raw samples into hourly/daily/monthly buckets (local-time boundaries).
///
/// Incremental: only samples newer than `meta.last_rollup_ts` are read and the
/// buckets they land in are *added to* (upsert `+=`). Rebuilding the whole
/// table from `samples` — the old v1 approach — silently truncated every
/// rollup to `retention_days_raw`, because `samples` itself is pruned to that
/// window while `retention_days_hourly` is 365 days (PRD §6).
///
/// A full rebuild still runs once, when the marker is absent (fresh DB or
/// upgrade from the full-refresh version), and seeds the marker.
///
/// shortcut: a sample that lands with `ts <= marker` (wall clock stepped
/// backwards) is never folded in; the daemon timestamps inserts with the
/// current clock, so this needs an NTP step backwards to matter.
pub fn rollup(conn: &Connection, tz_local: bool) -> Result<()> {
    let last = rollup_marker(conn)?;
    let args: Vec<i64> = last.into_iter().collect();
    let mut stmt = conn.prepare(if args.is_empty() {
        "SELECT ts, iface, rx_total, tx_total, lan_rx, lan_tx FROM samples"
    } else {
        "SELECT ts, iface, rx_total, tx_total, lan_rx, lan_tx FROM samples WHERE ts > ?1"
    })?;
    let rows: Vec<(i64, String, i64, i64, i64, i64)> = stmt
        .query_map(rusqlite::params_from_iter(args), sample_row)?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    if rows.is_empty() {
        return Ok(());
    }
    let max_ts = rows.iter().map(|r| r.0).max().unwrap_or(0);
    use std::collections::HashMap;
    let mut h: HashMap<(i64, String), (i64, i64, i64, i64)> = HashMap::new();
    let mut d: HashMap<(i64, String), (i64, i64, i64, i64)> = HashMap::new();
    let mut m: HashMap<(i64, String), (i64, i64, i64, i64)> = HashMap::new();
    for (ts, iface, rx, tx, lrx, ltx) in rows {
        let hb = floor_hour(ts, tz_local);
        let db = floor_day(ts, tz_local);
        let mb = floor_month(ts, tz_local);
        for (map, b) in [(&mut h, hb), (&mut d, db), (&mut m, mb)] {
            let e = map.entry((b, iface.clone())).or_insert((0, 0, 0, 0));
            e.0 += rx;
            e.1 += tx;
            e.2 += lrx;
            e.3 += ltx;
        }
    }
    let tx = conn.unchecked_transaction()?;
    if last.is_none() {
        // no marker: rollups may hold buckets from the old full-refresh code,
        // which rebuilt everything from whatever `samples` still held.
        for t in ["hourly", "daily", "monthly"] {
            tx.execute(&format!("DELETE FROM {t}"), [])?;
        }
    }
    for (table, col, map) in [
        ("hourly", "ts_hour", &h),
        ("daily", "ts_day", &d),
        ("monthly", "ts_mon", &m),
    ] {
        let mut ins = tx.prepare(&format!(
            "INSERT INTO {table}({col},iface,rx,tx,lan_rx,lan_tx) VALUES(?,?,?,?,?,?) \
             ON CONFLICT({col},iface) DO UPDATE SET rx=rx+excluded.rx, tx=tx+excluded.tx, \
             lan_rx=lan_rx+excluded.lan_rx, lan_tx=lan_tx+excluded.lan_tx"
        ))?;
        for ((b, i), (rx, txx, lrx, ltx)) in map {
            ins.execute(params![b, i, rx, txx, lrx, ltx])?;
        }
    }
    tx.execute(
        "INSERT INTO meta(k,v) VALUES('last_rollup_ts',?1) \
         ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![max_ts.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn query_range(
    conn: &Connection,
    table: &str,
    from: i64,
    to: i64,
    iface: Option<&str>,
) -> Result<Vec<Row>> {
    let col = match table {
        "hourly" => "ts_hour",
        "daily" => "ts_day",
        "monthly" => "ts_mon",
        _ => "ts",
    };
    let sql = if table == "samples" && iface.is_some() {
        "SELECT ts, iface, rx_total, tx_total, lan_rx, lan_tx FROM samples WHERE ts>=?1 AND ts<?2 AND iface=?3 ORDER BY ts".to_string()
    } else if table == "samples" {
        "SELECT ts, '_all', SUM(rx_total), SUM(tx_total), SUM(lan_rx), SUM(lan_tx) FROM samples WHERE ts>=?1 AND ts<?2 GROUP BY ts ORDER BY ts".to_string()
    } else if iface.is_some() {
        let txc = "tx";
        format!(
            "SELECT {col}, iface, rx, {txc}, lan_rx, lan_tx FROM {table} WHERE {col}>=?1 AND {col}<?2 AND iface=?3 ORDER BY {col}"
        )
    } else {
        format!(
            "SELECT {col}, '_all', SUM(rx), SUM(tx), SUM(lan_rx), SUM(lan_tx) FROM {table} WHERE {col}>=?1 AND {col}<?2 GROUP BY {col} ORDER BY {col}"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    if let Some(f) = iface {
        let rows = stmt.query_map(params![from, to, f], |r| {
            Ok(Row {
                ts: r.get(0)?,
                iface: r.get(1)?,
                rx: r.get(2)?,
                tx: r.get(3)?,
                lan_rx: r.get(4)?,
                lan_tx: r.get(5)?,
            })
        })?;
        return Ok(rows.filter_map(|r| r.ok()).collect());
    }
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(Row {
            ts: r.get(0)?,
            iface: r.get(1)?,
            rx: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            tx: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            lan_rx: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            lan_tx: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ---- time floors (UTC or local) ----
fn tz(local: bool) -> jiff::tz::TimeZone {
    if local {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::UTC
    }
}
fn zoned(ts: i64, local: bool) -> jiff::Zoned {
    jiff::Timestamp::from_second(ts)
        .unwrap()
        .to_zoned(tz(local))
}
/// Shared midnight-floor tail: resolve the zone once, rebuild at picked y/mo/d/h.
fn floor_dt(ts: i64, local: bool, pick: fn(jiff::civil::DateTime) -> (i16, i8, i8, i8)) -> i64 {
    let tzone = tz(local);
    let dt = jiff::Timestamp::from_second(ts)
        .unwrap()
        .to_zoned(tzone.clone())
        .datetime();
    let (y, mo, d, h) = pick(dt);
    jiff::civil::DateTime::new(y, mo, d, h, 0, 0, 0)
        .unwrap()
        .to_zoned(tzone)
        .unwrap()
        .timestamp()
        .as_second()
}
pub fn floor_hour(ts: i64, local: bool) -> i64 {
    floor_dt(ts, local, |dt| (dt.year(), dt.month(), dt.day(), dt.hour()))
}
pub fn floor_day(ts: i64, local: bool) -> i64 {
    floor_dt(ts, local, |dt| (dt.year(), dt.month(), dt.day(), 0))
}
pub fn floor_month(ts: i64, local: bool) -> i64 {
    floor_dt(ts, local, |dt| (dt.year(), dt.month(), 1, 0))
}
/// Per-app totals in a window, biggest first (for `top-apps`).
pub fn query_proc(conn: &Connection, from: i64, limit: usize) -> Result<Vec<(String, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT comm, SUM(rx), SUM(tx) FROM proc_hourly WHERE ts_hour>=?1 \
         GROUP BY comm ORDER BY SUM(rx)+SUM(tx) DESC LIMIT ?2",
    )?;
    let rows: Vec<(String, i64, i64)> = stmt
        .query_map(params![from, limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Monday 00:00 of the week containing ts.
pub fn floor_week(ts: i64, local: bool) -> i64 {
    let tzone = tz(local);
    let dt = jiff::Timestamp::from_second(ts)
        .unwrap()
        .to_zoned(tzone.clone())
        .datetime();
    let off = dt.date().weekday().to_monday_zero_offset() as i32;
    let monday = dt.date().checked_sub(jiff::ToSpan::days(off)).unwrap();
    jiff::civil::DateTime::new(monday.year(), monday.month(), monday.day(), 0, 0, 0, 0)
        .unwrap()
        .to_zoned(tzone)
        .unwrap()
        .timestamp()
        .as_second()
}
/// Shared day/week/month window (proc views + budget start). One enum for CLI, TUI keys, and floors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Window {
    Day,
    Week,
    Month,
}

/// Midnight/Monday/month-start floor for a [`Window`].
pub fn window_floor(w: Window, ts: i64, local: bool) -> i64 {
    match w {
        Window::Day => floor_day(ts, local),
        Window::Week => floor_week(ts, local),
        Window::Month => floor_month(ts, local),
    }
}

/// Window start for a budget period ("day" | "week" | "month").
pub fn budget_window_start(ts: i64, local: bool, period: &str) -> i64 {
    match period {
        "day" => floor_day(ts, local),
        "week" => floor_week(ts, local),
        _ => floor_month(ts, local),
    }
}
/// Stable per-window key for budget_events ("day:2026-09-06", "week:2026-09-01", ...).
pub fn budget_key(ts: i64, local: bool, period: &str) -> String {
    let start = budget_window_start(ts, local, period);
    let dt = zoned(start, local).datetime();
    format!(
        "{period}:{:04}-{:02}-{:02}",
        dt.year(),
        dt.month(),
        dt.day()
    )
}
/// day → daily, week → weekly, month → monthly.
pub fn period_adverb(period: &str) -> &'static str {
    match period {
        "day" => "daily",
        "week" => "weekly",
        _ => "monthly",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(conn: &Connection, ts: i64, iface: &str, rx: i64, tx: i64) {
        conn.execute(
            "INSERT INTO samples(ts,iface,rx_total,tx_total,lan_rx,lan_tx) VALUES(?,?,?,?,?,?)
             ON CONFLICT(ts,iface) DO UPDATE SET rx_total=excluded.rx_total, tx_total=excluded.tx_total,
             lan_rx=excluded.lan_rx, lan_tx=excluded.lan_tx",
            params![ts, iface, rx, tx, 0, 0],
        )
        .unwrap();
    }

    fn bucket(conn: &Connection, table: &str, col: &str, at: i64) -> i64 {
        conn.query_row(
            &format!("SELECT COALESCE(rx,0) FROM {table} WHERE {col}=?1 AND iface='eth0'"),
            params![at],
            |r| r.get(0),
        )
        .unwrap_or(-1)
    }

    #[test]
    fn first_rollup_seeds_marker() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        let t = 1_800_000_000;
        put(&conn, t, "eth0", 100, 10);
        rollup(&conn, false).unwrap();
        assert_eq!(
            bucket(&conn, "hourly", "ts_hour", floor_hour(t, false)),
            100
        );
        assert_eq!(bucket(&conn, "daily", "ts_day", floor_day(t, false)), 100);
        let m: String = conn
            .query_row("SELECT v FROM meta WHERE k='last_rollup_ts'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(m, t.to_string());
    }

    #[test]
    fn rollup_accumulates_only_new_samples() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        let t = 1_800_000_000;
        put(&conn, t, "eth0", 100, 10);
        rollup(&conn, false).unwrap();
        // more traffic in the same hour → same bucket, accumulated
        put(&conn, t + 60, "eth0", 50, 5);
        rollup(&conn, false).unwrap();
        assert_eq!(
            bucket(&conn, "hourly", "ts_hour", floor_hour(t, false)),
            150
        );
        // a later hour opens a new bucket and leaves the old one intact
        let t2 = t + 7200;
        put(&conn, t2, "eth0", 7, 1);
        rollup(&conn, false).unwrap();
        assert_eq!(
            bucket(&conn, "hourly", "ts_hour", floor_hour(t, false)),
            150
        );
        assert_eq!(bucket(&conn, "hourly", "ts_hour", floor_hour(t2, false)), 7);
    }

    /// Regression: the full-refresh rollup rebuilt hourly/daily/monthly from
    /// `samples`, which retention prunes to `retention_days_raw` (30d) while
    /// hourly is meant to keep 365d. Every hourly rollup therefore deleted all
    /// history older than 30 days, so `retention_days_hourly` did nothing.
    #[test]
    fn rollup_keeps_history_older_than_raw_retention() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        let old = 1_700_000_000; // ~9 months back
        put(&conn, old, "eth0", 4096, 0);
        rollup(&conn, false).unwrap();
        let old_hour = floor_hour(old, false);
        assert_eq!(bucket(&conn, "hourly", "ts_hour", old_hour), 4096);
        // retention pruned the raw samples that fed that bucket
        conn.execute("DELETE FROM samples WHERE ts < ?1", params![old + 1])
            .unwrap();
        // fresh traffic arrives; the old bucket must survive the rollup
        let now = 1_800_000_000;
        put(&conn, now, "eth0", 20, 0);
        rollup(&conn, false).unwrap();
        assert_eq!(bucket(&conn, "hourly", "ts_hour", old_hour), 4096);
        assert_eq!(
            bucket(&conn, "daily", "ts_day", floor_day(old, false)),
            4096
        );
        assert_eq!(
            bucket(&conn, "hourly", "ts_hour", floor_hour(now, false)),
            20
        );
    }
}
