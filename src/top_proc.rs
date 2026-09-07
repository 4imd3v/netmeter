//! Per-app usage viewer (reads daemon-recorded `proc_hourly`, no privileges needed).
//!
//! Always-on recording happens in the daemon (see `proc_cap::ProcRecorder`);
//! this is just an auto-refreshing window onto it. No capture here, so no
//! sudo required — unlike the old live-sniffing `top-proc`.

use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
};
use std::time::{Duration, Instant};

use crate::{config::Config, db, fmtx};

pub fn run(cfg: Config, period: db::Window) -> Result<()> {
    ratatui::run(|term| view_loop(term, &cfg, period))?;
    Ok(())
}

/// Today totals for the shared live/viewer top-apps column (one query; n=12 live, 25 viewer).
pub fn today_top_apps(
    conn: &rusqlite::Connection,
    cfg: &Config,
    n: usize,
) -> Vec<(String, i64, i64)> {
    db::query_proc(
        conn,
        window_start(db::Window::Day, cfg.timezone != "utc"),
        n,
    )
    .unwrap_or_default()
}

fn window_start(period: db::Window, local: bool) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    db::window_floor(period, now, local)
}

fn view_loop(
    term: &mut ratatui::DefaultTerminal,
    cfg: &Config,
    mut period: db::Window,
) -> Result<()> {
    let mut dec = cfg.units == "decimal";
    let local = cfg.timezone != "utc";
    let mut rows: Vec<(String, i64, i64)> = vec![];
    let mut tick = Instant::now();

    loop {
        // 2s refresh cadence, keys responsive throughout
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(remain) = deadline.checked_duration_since(Instant::now()) {
            if crossterm::event::poll(remain.min(Duration::from_millis(100)))? {
                if let crossterm::event::Event::Key(k) = crossterm::event::read()? {
                    match k.code {
                        crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Esc => {
                            return Ok(())
                        }
                        crossterm::event::KeyCode::Char('u') => dec = !dec,
                        crossterm::event::KeyCode::Char('1') => period = db::Window::Day,
                        crossterm::event::KeyCode::Char('2') => period = db::Window::Week,
                        crossterm::event::KeyCode::Char('3') => period = db::Window::Month,
                        _ => {}
                    }
                }
            }
        }
        let _ = tick;
        tick = Instant::now();
        let label = match period {
            db::Window::Day => "today",
            db::Window::Week => "since Monday",
            db::Window::Month => "this month",
        };
        rows = db::open(&cfg.db_path_expanded(), true)
            .ok()
            .map(|c| match period {
                db::Window::Day => today_top_apps(&c, cfg, 25),
                _ => db::query_proc(&c, window_start(period, local), 25).unwrap_or_default(),
            })
            .unwrap_or_default();
        let max_tot = rows.first().map(|(_, rx, tx)| rx + tx).unwrap_or(1);
        let tot: i64 = rows.iter().map(|(_, rx, tx)| rx + tx).sum();

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
                    Span::raw(format!("{} ", fmtx::fmt_bytes(tot, dec))),
                    Span::styled(
                        format!("│ {label} │ 1 day │ 2 week │ 3 month "),
                        Style::default().fg(Color::Yellow),
                    ),
                ]))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" top apps · daemon-recorded "),
                ),
                chunks[0],
            );
            let body: Vec<Row> = rows
                .iter()
                .map(|(comm, rx, tx)| {
                    let share = ((rx + tx) as f64 / max_tot as f64 * 10.0).round() as usize;
                    Row::new(vec![
                        comm.clone(),
                        fmtx::fmt_bytes(*rx, dec),
                        fmtx::fmt_bytes(*tx, dec),
                        fmtx::fmt_bytes(rx + tx, dec),
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
                    Row::new(vec!["APP", "DOWN", "UP", "TOTAL", "SHARE"])
                        .style(Style::default().fg(Color::Yellow)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" per-app usage (short flows → unknown) "),
                ),
                chunks[1],
            );
            f.render_widget(
                Paragraph::new(" q quit │ u units │ 1/2/3 period ")
                    .style(Style::default().fg(Color::Gray)),
                chunks[2],
            );
        })?;
    }
}
