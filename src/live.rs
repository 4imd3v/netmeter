use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Row, Sparkline, Table},
};
use std::time::{Duration, Instant};

use crate::{capture, config::Config, db, fmtx, top_proc::ProcSampler};

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

fn live_loop(
    terminal: &mut ratatui::DefaultTerminal,
    cfg: &Config,
    iface_filter: Option<String>,
) -> Result<()> {
    let mut dec = cfg.units == "decimal";
    let mut interval_ms: u64 = 1000;
    let tz_local = cfg.timezone != "utc";
    let started = Instant::now();

    // network-speed histories (B/s per tick, 120 samples)
    let mut hist_down: Vec<u64> = vec![0; 120];
    let mut hist_up: Vec<u64> = vec![0; 120];
    let mut prev = capture::read_proc().unwrap_or_default();
    let mut prev_lan = capture::read_nft();
    let mut lan_rate = String::from("…");
    let mut session: std::collections::HashMap<String, (u64, u64)> = Default::default();
    let mut tick_n: u64 = 0;
    // cached slow queries
    let mut day_tot = (0i64, 0i64);
    let mut week_tot = (0i64, 0i64);
    let mut month_used: i64 = 0;
    let mut month_split = (0i64, 0i64, 0i64, 0i64); // rx, tx, lan_rx, lan_tx
                                                    // top-apps sampler shares the iface filter; None when unpermitted (no CAP_NET_RAW)
    let mut sampler = ProcSampler::start(iface_filter.clone()).ok();
    let mut app_rates: Vec<crate::top_proc::ProcRate> = vec![];

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
        hist_down.push(tot_down);
        hist_up.push(tot_up);
        if hist_down.len() > 120 {
            hist_down.remove(0);
            hist_up.remove(0);
        }
        let max_row = rows.iter().map(|r| r.down + r.up).max().unwrap_or(1);

        if tick_n % 5 == 1 {
            day_tot = query_since(cfg, db::floor_day(now_ts(), tz_local));
            week_tot = query_week_total(cfg, tz_local);
            let (used, split) = query_month(cfg, tz_local);
            month_used = used;
            month_split = split;
        }
        if let Some(sm) = sampler.as_mut() {
            if sm.poll() {
                app_rates = sm.rates();
                app_rates.truncate(12);
            } else {
                sampler = None; // channel died; fall back to hint panel
            }
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
        let hd = hist_down.clone();
        let hu = hist_up.clone();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // stats header
                    Constraint::Min(8),    // interfaces | usage
                    Constraint::Length(7), // network speed sparks
                    Constraint::Length(3), // budget / month strip
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

            // ---- middle: left stacks interfaces+usage, right top apps ----
            let mid = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[1]);
            let left = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(mid[0]);

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
                left[0],
            );

            let usage_rows = vec![
                usage_row("TODAY", day_tot, dec),
                usage_row("WEEK", week_tot, dec),
                usage_row(
                    "MONTH",
                    (month_split.0, month_split.1),
                    dec,
                ),
                usage_row("LAN-M", (month_split.2, month_split.3), dec),
                usage_row(
                    "WAN-M",
                    (
                        (month_split.0 - month_split.2).max(0),
                        (month_split.1 - month_split.3).max(0),
                    ),
                    dec,
                ),
            ];
            f.render_widget(
                Table::new(
                    usage_rows,
                    [
                        Constraint::Length(7),
                        Constraint::Percentage(30),
                        Constraint::Percentage(30),
                        Constraint::Percentage(30),
                    ],
                )
                .header(
                    Row::new(vec!["", "DOWN", "UP", "TOTAL"])
                        .style(Style::default().fg(Color::Yellow)),
                )
                .block(Block::default().borders(Borders::ALL).title(" usage ")),
                left[1],
            );

            // ---- top apps (right column) ----
            if sampler.is_some() {
                let app_rows: Vec<Row> = app_rates
                    .iter()
                    .map(|r| {
                        let name = if r.pid == 0 {
                            r.comm.clone()
                        } else {
                            format!("{} [{}]", r.comm, r.pid)
                        };
                        Row::new(vec![
                            name,
                            format!("{}/s", fmtx::fmt_bytes(r.rx as i64, dec)),
                            format!("{}/s", fmtx::fmt_bytes(r.tx as i64, dec)),
                        ])
                    })
                    .collect();
                f.render_widget(
                    Table::new(
                        app_rows,
                        [
                            Constraint::Percentage(48),
                            Constraint::Percentage(26),
                            Constraint::Percentage(26),
                        ],
                    )
                    .header(
                        Row::new(vec!["APP", "DOWN/s", "UP/s"])
                            .style(Style::default().fg(Color::Yellow)),
                    )
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" top apps "),
                    ),
                    mid[1],
                );
            } else {
                f.render_widget(
                    Paragraph::new(
                        "top apps need packet capture\nrun `sudo netmeter live`\nor `sudo netmeter top-proc` standalone",
                    )
                    .block(Block::default().borders(Borders::ALL).title(" top apps ")),
                    mid[1],
                );
            }

            // ---- network speed ----
            let speeds = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[2]);
            f.render_widget(
                Sparkline::default()
                    .block(
                        Block::default().borders(Borders::ALL).title(format!(
                            " ▼ speed {} ",
                            fmt_rate(tot_down, 1.0, dec)
                        )),
                    )
                    .data(&hd)
                    .style(Style::default().fg(Color::Green)),
                speeds[0],
            );
            f.render_widget(
                Sparkline::default()
                    .block(
                        Block::default().borders(Borders::ALL).title(format!(
                            " ▲ speed {} ",
                            fmt_rate(tot_up, 1.0, dec)
                        )),
                    )
                    .data(&hu)
                    .style(Style::default().fg(Color::Blue)),
                speeds[1],
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
                        " month {} (▼{} ▲{}) │ LAN {}/{} │ WAN {}/{} │ `netmeter config set monthly_budget_gb 50` for a budget bar",
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

fn usage_row(label: &str, dt: (i64, i64), dec: bool) -> Row<'static> {
    Row::new(vec![
        label.to_string(),
        fmtx::fmt_bytes(dt.0, dec),
        fmtx::fmt_bytes(dt.1, dec),
        fmtx::fmt_bytes(dt.0 + dt.1, dec),
    ])
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

fn query_since(cfg: &Config, from: i64) -> (i64, i64) {
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return (0, 0);
    };
    conn.query_row(
        "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0) FROM samples WHERE ts>=?1",
        rusqlite::params![from],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, 0))
}

/// Monday-00:00 → now (local or UTC week).
fn query_week_total(cfg: &Config, local: bool) -> (i64, i64) {
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return (0, 0);
    };
    let now = now_ts();
    let tzone = if local {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::UTC
    };
    let dt = jiff::Timestamp::from_second(now)
        .unwrap()
        .to_zoned(tzone.clone())
        .datetime();
    let off = dt.date().weekday().to_monday_zero_offset() as i32;
    let monday = dt.date().checked_sub(jiff::ToSpan::days(off)).unwrap();
    let m0 = jiff::civil::DateTime::new(monday.year(), monday.month(), monday.day(), 0, 0, 0, 0)
        .unwrap()
        .to_zoned(tzone)
        .unwrap()
        .timestamp()
        .as_second();
    conn.query_row(
        "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0) FROM samples WHERE ts>=?1",
        rusqlite::params![m0],
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
