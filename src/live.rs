use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Row, Sparkline, Table},
};
use std::time::Duration;

use crate::{capture, config::Config, fmtx};

pub fn run(cfg: Config, iface_filter: Option<String>) -> Result<()> {
    ratatui::run(|terminal| live_loop(terminal, &cfg, iface_filter.clone()))?;
    Ok(())
}

fn live_loop(
    terminal: &mut ratatui::DefaultTerminal,
    cfg: &Config,
    iface_filter: Option<String>,
) -> Result<()> {
    let dec = cfg.units == "decimal";
    let mut hist: Vec<u64> = vec![0; 60];
    let mut prev = capture::read_proc().unwrap_or_default();
    let mut prev_lan = capture::read_nft();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if crossterm::event::poll(Duration::from_millis(0))? {
            if let crossterm::event::Event::Key(k) = crossterm::event::read()? {
                if matches!(
                    k.code,
                    crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Esc
                ) {
                    break;
                }
            }
        }
        let cur = capture::read_proc().unwrap_or_default();
        let lan_now = capture::read_nft();
        let mut rows: Vec<(String, u64, u64)> = vec![];
        let mut tot = 0u64;
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
                tot += drx + dtx;
                rows.push((iface.clone(), drx, dtx));
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.1 + r.2));
        prev = cur;
        let lan_txt = match (prev_lan, lan_now) {
            (Some((pr, pt)), Some((nr, nt))) => {
                format!("LAN +{}/+{}B", nr.saturating_sub(pr), nt.saturating_sub(pt))
            }
            _ => {
                if capture::nft_table_exists() {
                    "LAN …".into()
                } else {
                    "LAN n/a (TOTAL-only)".into()
                }
            }
        };
        prev_lan = lan_now;
        hist.push(tot);
        if hist.len() > 60 {
            hist.remove(0);
        }
        let spark_data = hist.clone();
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(5),
                ])
                .split(f.area());
            let header = Paragraph::new(format!(
                "netmeter live  •  TOTAL {}B/s  •  {}  •  q to quit",
                tot, lan_txt
            ))
            .block(Block::default().borders(Borders::ALL).title("netmeter"));
            f.render_widget(header, chunks[0]);
            let table_rows: Vec<Row> = rows
                .iter()
                .map(|(i, rx, tx)| {
                    Row::new(vec![
                        i.clone(),
                        format!("{}/s", fmtx::fmt_bytes(*rx as i64, dec)),
                        format!("{}/s", fmtx::fmt_bytes(*tx as i64, dec)),
                        fmtx::fmt_bytes((rx + tx) as i64, dec).to_string(),
                    ])
                })
                .collect();
            let table = Table::new(
                table_rows,
                [
                    Constraint::Percentage(40),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                ],
            )
            .header(
                Row::new(vec!["IFACE", "DOWN/s", "UP/s", "TOTAL/s"])
                    .style(Style::default().fg(Color::Yellow)),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("per-interface (1s rate)"),
            );
            f.render_widget(table, chunks[1]);
            let spark = Sparkline::default()
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("total B/s (60s)"),
                )
                .data(&spark_data)
                .style(Style::default().fg(Color::Green));
            f.render_widget(spark, chunks[2]);
        })?;
    }
    Ok(())
}
