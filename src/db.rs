use anyhow::{Context, Result};
use rusqlite::{params, Connection};
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

/// Roll raw samples into hourly/daily/monthly buckets (local-time boundaries).
/// v1: aggregate from `samples` grouped by hour/day/month floors computed in Rust
/// (avoids SQLite timezone pitfalls), upsert into rollup tables.
pub fn rollup(conn: &Connection, tz_local: bool) -> Result<()> {
    let mut stmt =
        conn.prepare("SELECT ts, iface, rx_total, tx_total, lan_rx, lan_tx FROM samples")?;
    let rows: Vec<(i64, String, i64, i64, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
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
    // cheap full-refresh (sample counts are small in v1: 17k rows/day/iface max)
    tx.execute("DELETE FROM hourly", [])?;
    tx.execute("DELETE FROM daily", [])?;
    tx.execute("DELETE FROM monthly", [])?;
    {
        let mut ih = tx
            .prepare("INSERT INTO hourly(ts_hour,iface,rx,tx,lan_rx,lan_tx) VALUES(?,?,?,?,?,?)")?;
        for ((b, i), (rx, txx, lrx, ltx)) in &h {
            ih.execute(params![b, i, rx, txx, lrx, ltx])?;
        }
    }
    {
        let mut id =
            tx.prepare("INSERT INTO daily(ts_day,iface,rx,tx,lan_rx,lan_tx) VALUES(?,?,?,?,?,?)")?;
        for ((b, i), (rx, txx, lrx, ltx)) in &d {
            id.execute(params![b, i, rx, txx, lrx, ltx])?;
        }
    }
    {
        let mut im = tx
            .prepare("INSERT INTO monthly(ts_mon,iface,rx,tx,lan_rx,lan_tx) VALUES(?,?,?,?,?,?)")?;
        for ((b, i), (rx, txx, lrx, ltx)) in &m {
            im.execute(params![b, i, rx, txx, lrx, ltx])?;
        }
    }
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
        format!("SELECT {col}, iface, rx, {txc}, lan_rx, lan_tx FROM {table} WHERE {col}>=?1 AND {col}<?2 AND iface=?3 ORDER BY {col}")
    } else {
        format!("SELECT {col}, '_all', SUM(rx), SUM(tx), SUM(lan_rx), SUM(lan_tx) FROM {table} WHERE {col}>=?1 AND {col}<?2 GROUP BY {col} ORDER BY {col}")
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
pub fn floor_hour(ts: i64, local: bool) -> i64 {
    let z = zoned(ts, local);
    let dt = z.datetime();
    jiff::civil::DateTime::new(dt.year(), dt.month(), dt.day(), dt.hour(), 0, 0, 0)
        .unwrap()
        .to_zoned(z.time_zone().clone())
        .unwrap()
        .timestamp()
        .as_second()
}
pub fn floor_day(ts: i64, local: bool) -> i64 {
    let z = zoned(ts, local);
    let dt = z.datetime();
    jiff::civil::DateTime::new(dt.year(), dt.month(), dt.day(), 0, 0, 0, 0)
        .unwrap()
        .to_zoned(z.time_zone().clone())
        .unwrap()
        .timestamp()
        .as_second()
}
pub fn floor_month(ts: i64, local: bool) -> i64 {
    let z = zoned(ts, local);
    let dt = z.datetime();
    jiff::civil::DateTime::new(dt.year(), dt.month(), 1, 0, 0, 0, 0)
        .unwrap()
        .to_zoned(z.time_zone().clone())
        .unwrap()
        .timestamp()
        .as_second()
}
#[allow(dead_code)]
pub fn month_key(ts: i64, local: bool) -> String {
    let dt = zoned(ts, local).datetime();
    format!("{:04}-{:02}", dt.year(), dt.month())
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
