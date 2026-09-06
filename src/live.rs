use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Row, Sparkline, Table},
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

fn live_loop(
    terminal: &mut ratatui::DefaultTerminal,
    cfg: &Config,
    iface_filter: Option<String>,
) -> Result<()> {
    let mut dec = cfg.units == "decimal";
    let mut interval_ms: u64 = 1000;
    let tz_local = cfg.timezone != "utc";
    let started = Instant::now();

    let mut hist_down: Vec<u64> = vec![0; 60];
    let mut hist_up: Vec<u64> = vec![0; 60];
    let mut prev = capture::read_proc().unwrap_or_default();
    let mut prev_lan = capture::read_nft();
    let mut lan_rate = String::from("…");
    let mut session: std::collections::HashMap<String, (u64, u64)> = Default::default();
    let mut tick_n: u64 = 0;
    // cached slow queries (DB day/month totals), refreshed every ~5 ticks
    let mut day_tot = (0i64, 0i64);
    let mut month_used: i64 = 0;

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
            day_tot = query_day_total(cfg, tz_local);
            month_used = query_month_total(cfg, tz_local);
        }
        let uptime = started.elapsed().as_secs();

        let hd = hist_down.clone();
        let hu = hist_up.clone();
        let budget_frac = if cfg.monthly_budget_gb > 0.0 {
            month_used as f64 / (cfg.monthly_budget_gb * 1_073_741_824.0)
        } else {
            -1.0
        };

        terminal.draw(|f| {
            let area = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // header
                    Constraint::Min(8),    // table
                    Constraint::Length(7), // sparks
                    Constraint::Length(3), // budget/status
                    Constraint::Length(1), // keys
                ])
                .split(area);

            // ---- header ----
            let sess_rx: u64 = session.values().map(|v| v.0).sum();
            let sess_tx: u64 = session.values().map(|v| v.1).sum();
            let header = Paragraph::new(Line::from(vec![
                Span::styled(" ▼ ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} ", fmt_rate(tot_down, 1.0, dec))),
                Span::styled(" ▲ ", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} ", fmt_rate(tot_up, 1.0, dec))),
                Span::styled(" │ LAN ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{lan_rate} ")),
                Span::styled("│ session ", Style::default().fg(Color::Yellow)),
                Span::raw(format!(
                    "▼{} ▲{} ",
                    fmtx::fmt_bytes(sess_rx as i64, dec),
                    fmtx::fmt_bytes(sess_tx as i64, dec)
                )),
                Span::styled("│ today ", Style::default().fg(Color::Yellow)),
                Span::raw(format!(
                    "▼{} ▲{}",
                    fmtx::fmt_bytes(day_tot.0, dec),
                    fmtx::fmt_bytes(day_tot.1, dec)
                )),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" netmeter live ({}ms) ", interval_ms)),
            );
            f.render_widget(header, chunks[0]);

            // ---- table ----
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
            let table = Table::new(
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
                " interfaces ({}) — uptime {}s ",
                rows.len(),
                uptime
            )));
            f.render_widget(table, chunks[1]);

            // ---- sparklines ----
            let spark_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[2]);
            f.render_widget(
                Sparkline::default()
                    .block(Block::default().borders(Borders::ALL).title(" ▼ down B/s "))
                    .data(&hd)
                    .style(Style::default().fg(Color::Green)),
                spark_chunks[0],
            );
            f.render_widget(
                Sparkline::default()
                    .block(Block::default().borders(Borders::ALL).title(" ▲ up B/s "))
                    .data(&hu)
                    .style(Style::default().fg(Color::Blue)),
                spark_chunks[1],
            );

            // ---- budget / status ----
            if budget_frac >= 0.0 {
                let pct = (budget_frac.clamp(0.0, 1.0) * 100.0) as u16;
                let g = Gauge::default()
                    .block(Block::default().borders(Borders::ALL).title(format!(
                        " month {:.1}/{:.0} GiB ",
                        month_used as f64 / 1_073_741_824.0,
                        cfg.monthly_budget_gb
                    )))
                    .gauge_style(Style::default().fg(if pct >= 100 {
                        Color::Red
                    } else if pct >= 80 {
                        Color::Yellow
                    } else {
                        Color::Green
                    }))
                    .percent(pct);
                f.render_widget(g, chunks[3]);
            } else {
                let p = Paragraph::new(format!(
                    " no monthly budget set — `netmeter config set monthly_budget_gb 50` to track one │ nft: {}",
                    if lan_rate == "n/a TOTAL-only" {
                        "TOTAL-only"
                    } else {
                        "LAN/WAN split on"
                    }
                ))
                .block(Block::default().borders(Borders::ALL).title(" budget "));
                f.render_widget(p, chunks[3]);
            }

            f.render_widget(
                Paragraph::new(" q quit │ u units │ +/− refresh │ --iface filter at launch").style(
                    Style::default().fg(Color::Gray),
                ),
                chunks[4],
            );
        })?;
    }
    Ok(())
}

fn fmt_rate(bps: u64, per_sec: f64, dec: bool) -> String {
    format!("{}/s", fmtx::fmt_bytes((bps as f64 * per_sec) as i64, dec))
}

fn query_day_total(cfg: &Config, local: bool) -> (i64, i64) {
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return (0, 0);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let d0 = db::floor_day(now, local);
    conn.query_row(
        "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0) FROM samples WHERE ts>=?1",
        rusqlite::params![d0],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, 0))
}

fn query_month_total(cfg: &Config, local: bool) -> i64 {
    if cfg.monthly_budget_gb <= 0.0 {
        return 0;
    }
    let Ok(conn) = db::open(&cfg.db_path_expanded(), true) else {
        return 0;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let m0 = db::floor_month(now, local);
    let (rx, tx, lrx, ltx): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(rx_total),0), COALESCE(SUM(tx_total),0), COALESCE(SUM(lan_rx),0), COALESCE(SUM(lan_tx),0) FROM samples WHERE ts>=?1",
            rusqlite::params![m0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((0, 0, 0, 0));
    if cfg.budget_basis == "wan" {
        (rx - lrx).max(0) + (tx - ltx).max(0)
    } else {
        rx + tx
    }
}
