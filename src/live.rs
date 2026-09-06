use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{BarChart, Block, Borders, Gauge, Paragraph, Row, Table},
};
use std::time::{Duration, Instant};

use crate::{capture, config::Config, db, fmtx};

pub fn run(cfg: Config, iface_filter: Option<String>) -> Result<()> {
    ratatui::run(|terminal| live_loop(terminal, &cfg, iface_filter.clone()))?;
    Ok(())
}

struct IfRow {
    iface: String,
    down: u64,
    up: u64,
    session_rx: u64,
    session_tx: u64,
}

/// One bar-bucket series of history.
struct Hist {
    labels: Vec<String>,
    down: Vec<u64>,
    up: Vec<u64>,
}

fn live_loop(
    terminal: &mut ratatui::DefaultTerminal,
    cfg: &Config,
    iface_filter: Option<String>,
) -> Result<()> {
    let mut dec = cfg.units == "decimal";
    let mut interval_ms: u64 = 1000;
    let tz_local = cfg.timezone != "utc";
    let started = Instant::now();

    let mut prev = capture::read_proc().unwrap_or_default();
    let mut prev_lan = capture::read_nft();
    let mut lan_rate = String::from("…");
    let mut session: std::collections::HashMap<String, (u64, u64)> = Default::default();
    let mut tick_n: u64 = 0;
    // cached slow queries
    let mut day_tot = (0i64, 0i64);
    let mut month_used: i64 = 0;
    let mut month_split = (0i64, 0i64, 0i64, 0i64); // rx, tx, lan_rx, lan_tx
    let mut h_day = Hist {
        labels: vec![],
        down: vec![],
        up: vec![],
    };
    let mut h_month = Hist {
        labels: vec![],
        down: vec![],
        up: vec![],
    };

    loop {
        std::thread::sleep(Duration::from_millis(interval_ms));
        if crossterm::event::poll(Duration::from_millis(0))? {
            if let crossterm::event::Event::Key(k) = crossterm::event::read()? {
                match k.code {
                    crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Esc => break,
                    crossterm::event::KeyCode::Char('u') => dec = !dec,
                    crossterm::event::KeyCode::Char('+') | crossterm::event::KeyCode::Char('=') => {
                        interval_ms = (interval_ms - 250).max(250)
                    }
                    crossterm::event::KeyCode::Char('-') => {
                        interval_ms = (interval_ms + 250).min(5000)
                    }
                    _ => {}
                }
            }
        }
        tick_n += 1;
        let per_sec = 1000.0 / interval_ms as f64;

        let cur = capture::read_proc().unwrap_or_default();
        let lan_now = capture::read_nft();
        if let (Some((pr, pt)), Some((nr, nt))) = (prev_lan, lan_now) {
            lan_rate = format!(
                "+{}/+{}B",
                fmt_rate(nr.saturating_sub(pr), per_sec, dec),
                fmt_rate(nt.saturating_sub(pt), per_sec, dec)
            );
        } else if !capture::nft_table_exists() {
            lan_rate = String::from("n/a TOTAL-only");
        }
        prev_lan = lan_now;

        let mut rows: Vec<IfRow> = vec![];
        for (iface, (rx, tx)) in &cur {
            if cfg.excluded(iface) {
                continue;
            }
            if let Some(f) = &iface_filter {
                if iface != f {
                    continue;
                }
            }
            if let Some((orx, otx)) = prev.get(iface) {
                let drx = rx.saturating_sub(*orx);
                let dtx = tx.saturating_sub(*otx);
                let e = session.entry(iface.clone()).or_insert((0, 0));
                e.0 += drx;
                e.1 += dtx;
                rows.push(IfRow {
                    iface: iface.clone(),
                    down: (drx as f64 * per_sec) as u64,
                    up: (dtx as f64 * per_sec) as u64,
                    session_rx: e.0,
                    session_tx: e.1,
                });
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.down + r.up));
        prev = cur;

        let tot_down: u64 = rows.iter().map(|r| r.down).sum();
        let tot_up: u64 = rows.iter().map(|r| r.up).sum();
        let max_row = rows.iter().map(|r| r.down + r.up).max().unwrap_or(1);

        if tick_n % 5 == 1 {
            day_tot = query_day_total(cfg, tz_local);
            let (used, split) = query_month(cfg, tz_local);
            month_used = used;
            month_split = split;
        }
        if tick_n % 15 == 1 {
            h_day = load_history(cfg, tz_local, 24, false, iface_filter.as_deref());
            h_month = load_history(cfg, tz_local, 30, true, iface_filter.as_deref());
        }
        let uptime = started.elapsed().as_secs();
        let sess_rx: u64 = session.values().map(|v| v.0).sum();
        let sess_tx: u64 = session.values().map(|v| v.1).sum();

        let budget_frac = if cfg.monthly_budget_gb > 0.0 {
            month_used as f64 / (cfg.monthly_budget_gb * 1_073_741_824.0)
        } else {
            -1.0
        };
        let (mrx, mtx, mlrx, mltx) = month_split;
        let mwan = (mrx - mlrx).max(0) + (mtx - mltx).max(0);

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // stats header
                    Constraint::Min(6),    // interfaces table
                    Constraint::Length(8), // 24h | 30d bars
                    Constraint::Length(3), // budget gauge / month strip
                    Constraint::Length(1), // keys
                ])
                .split(f.area());

            // ---- stats header ----
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " ▼ ",
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{} ", fmt_rate(tot_down, 1.0, dec))),
                    Span::styled(
                        " ▲ ",
                        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{} ", fmt_rate(tot_up, 1.0, dec))),
                    Span::styled(" │ LAN ", Style::default().fg(Color::Yellow)),
                    Span::raw(format!("{lan_rate} ")),
                    Span::styled("│ sess ", Style::default().fg(Color::Yellow)),
                    Span::raw(format!(
                        "▼{} ▲{} ",
                        fmtx::fmt_bytes(sess_rx as i64, dec),
                        fmtx::fmt_bytes(sess_tx as i64, dec)
                    )),
                    Span::styled("│ today ", Style::default().fg(Color::Yellow)),
                    Span::raw(format!(
                        "▼{} ▲{} ",
                        fmtx::fmt_bytes(day_tot.0, dec),
                        fmtx::fmt_bytes(day_tot.1, dec)
                    )),
                    Span::styled("│ month ", Style::default().fg(Color::Yellow)),
                    Span::raw(fmtx::fmt_bytes(month_used, dec)),
                ]))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" netmeter ({}ms) ", interval_ms)),
                ),
                chunks[0],
            );

            // ---- interfaces table ----
            let table_rows: Vec<Row> = rows
                .iter()
                .map(|r| {
                    let share = ((r.down + r.up) as f64 / max_row as f64 * 10.0).round() as usize;
                    Row::new(vec![
                        r.iface.clone(),
                        format!("{}/s", fmtx::fmt_bytes(r.down as i64, dec)),
                        format!("{}/s", fmtx::fmt_bytes(r.up as i64, dec)),
                        fmtx::bar(share as f64 / 10.0, 10),
                        fmtx::fmt_bytes((r.session_rx + r.session_tx) as i64, dec),
                    ])
                })
                .collect();
            f.render_widget(
                Table::new(
                    table_rows,
                    [
                        Constraint::Percentage(28),
                        Constraint::Percentage(18),
                        Constraint::Percentage(18),
                        Constraint::Percentage(16),
                        Constraint::Percentage(20),
                    ],
                )
                .header(
                    Row::new(vec!["IFACE", "DOWN/s", "UP/s", "SHARE", "SESSION"])
                        .style(Style::default().fg(Color::Yellow)),
                )
                .block(Block::default().borders(Borders::ALL).title(format!(
                    " interfaces ({}) — up {}s ",
                    rows.len(),
                    uptime
                ))),
                chunks[1],
            );

            // ---- history bars side by side ----
            let hist = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[2]);
            render_bars(
                f,
                hist[0],
                &format!(" 24h {} ", fmtx::fmt_bytes(sum(&h_day) as i64, dec)),
                &h_day,
                Color::Green,
            );
            render_bars(
                f,
                hist[1],
                &format!(" 30d {} ", fmtx::fmt_bytes(sum(&h_month) as i64, dec)),
                &h_month,
                Color::Blue,
            );

            // ---- month / budget strip ----
            if budget_frac >= 0.0 {
                let pct = (budget_frac.clamp(0.0, 1.0) * 100.0) as u16;
                f.render_widget(
                    Gauge::default()
                        .block(Block::default().borders(Borders::ALL).title(format!(
                            " month {:.1}/{:.0} GiB │ WAN {} ",
                            month_used as f64 / 1_073_741_824.0,
                            cfg.monthly_budget_gb,
                            fmtx::fmt_bytes(mwan, dec),
                        )))
                        .gauge_style(Style::default().fg(if pct >= 100 {
                            Color::Red
                        } else if pct >= 80 {
                            Color::Yellow
                        } else {
                            Color::Green
                        }))
                        .percent(pct),
                    chunks[3],
                );
            } else {
                f.render_widget(
                    Paragraph::new(format!(
                        " month {} (▼{} ▲{}) │ LAN {}/{} │ WAN {}/{} │ set a budget: `netmeter config set monthly_budget_gb 50`",
                        fmtx::fmt_bytes(month_used, dec),
                        fmtx::fmt_bytes(mrx, dec),
                        fmtx::fmt_bytes(mtx, dec),
                        fmtx::fmt_bytes(mlrx, dec),
                        fmtx::fmt_bytes(mltx, dec),
                        fmtx::fmt_bytes((mrx - mlrx).max(0), dec),
                        fmtx::fmt_bytes((mtx - mltx).max(0), dec),
                    ))
                    .block(Block::default().borders(Borders::ALL).title(" month ")),
                    chunks[3],
                );
            }

            f.render_widget(
                Paragraph::new(" q quit │ u units │ +/− refresh ")
                    .style(Style::default().fg(Color::Gray)),
                chunks[4],
            );
        })?;
    }
    Ok(())
}

fn sum(h: &Hist) -> u64 {
    h.down.iter().sum::<u64>() + h.up.iter().sum::<u64>()
}

fn render_bars(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    title: &str,
    h: &Hist,
    color: Color,
) {
    if h.labels.is_empty() {
        f.render_widget(
            Paragraph::new("…collecting")
                .block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
        return;
    }
    let data: Vec<(&str, u64)> = h
        .labels
        .iter()
        .zip(h.down.iter().zip(h.up.iter()))
        .map(|(l, (d, u))| (l.as_str(), d + u))
        .collect();
    let n = data.len();
    let (bw, gap) = if n > 40 {
        (1, 0)
    } else if n > 24 {
        (2, 0)
    } else {
        (3, 1)
    };
    f.render_widget(
        BarChart::default()
            .block(Block::default().borders(Borders::ALL).title(title))
            .data(&data)
            .bar_width(bw)
            .bar_gap(gap)
            .bar_style(Style::default().fg(color))
            .value_style(Style::default().fg(Color::Black).bg(color)),
        area,
    );
}

// ---------- queries ----------

fn fmt_rate(bps: u64, per_sec: f64, dec: bool) -> String {
    format!("{}/s", fmtx::fmt_bytes((bps as f64 * per_sec) as i64, dec))
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Load last N buckets (hours or days) — rollup table first, raw samples fallback.
fn load_history(cfg: &Config, local: bool, n: usize, daily: bool, iface: Option<&str>) -> Hist {
    let now = now_ts();
    let empty = Hist {
        labels: vec![],
        down: vec![],
        up: vec![],
    };
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return empty;
    };
    let (table, span, floor): (&str, i64, fn(i64, bool) -> i64) = if daily {
        ("daily", n as i64 * 86400, db::floor_day)
    } else {
        ("hourly", n as i64 * 3600, db::floor_hour)
    };
    let fetched: Vec<db::Row> =
        db::query_range(&conn, table, now - span, now + 1, iface).unwrap_or_default();
    let mut rows: Vec<db::Row> = fetched;
    if rows.is_empty() {
        // fallback: bucket raw 5s samples (daemon rolls up hourly)
        let s = db::query_range(&conn, "samples", now - span, now + 1, iface).unwrap_or_default();
        use std::collections::BTreeMap;
        let mut m: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
        for r in s {
            let b = floor(r.ts, local);
            let e = m.entry(b).or_insert((0, 0));
            e.0 += r.rx;
            e.1 += r.tx;
        }
        rows = m
            .into_iter()
            .map(|(ts, (rx, tx))| db::Row {
                ts,
                iface: "_all".into(),
                rx,
                tx,
                lan_rx: 0,
                lan_tx: 0,
            })
            .collect();
    }
    rows.sort_by_key(|r| r.ts);
    if rows.len() > n {
        rows = rows.split_off(rows.len() - n);
    }
    let mut h = Hist {
        labels: vec![],
        down: vec![],
        up: vec![],
    };
    for r in rows {
        let dt = if local {
            jiff::Timestamp::from_second(r.ts)
                .unwrap()
                .to_zoned(jiff::tz::TimeZone::system())
                .datetime()
        } else {
            jiff::Timestamp::from_second(r.ts)
                .unwrap()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .datetime()
        };
        h.labels.push(if daily {
            format!("{:02}", dt.day())
        } else {
            format!("{:02}", dt.hour())
        });
        h.down.push(r.rx.max(0) as u64);
        h.up.push(r.tx.max(0) as u64);
    }
    h
}

fn query_day_total(cfg: &Config, local: bool) -> (i64, i64) {
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return (0, 0);
    };
    let d0 = db::floor_day(now_ts(), local);
    conn.query_row(
        "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0) FROM samples WHERE ts>=?1",
        rusqlite::params![d0],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, 0))
}

/// Returns (basis_used, (rx, tx, lan_rx, lan_tx)) for current month.
fn query_month(cfg: &Config, local: bool) -> (i64, (i64, i64, i64, i64)) {
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return (0, (0, 0, 0, 0));
    };
    let m0 = db::floor_month(now_ts(), local);
    let (rx, tx, lrx, ltx): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0), COALESCE(SUM(lan_rx),0), COALESCE(SUM(lan_tx),0) FROM samples WHERE ts>=?1",
            rusqlite::params![m0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((0, 0, 0, 0));
    let used = if cfg.budget_basis == "wan" {
        (rx - lrx).max(0) + (tx - ltx).max(0)
    } else {
        rx + tx
    };
    (used, (rx, tx, lrx, ltx))
}
