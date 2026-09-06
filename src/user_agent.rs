use crate::{config::Config, db};
use anyhow::Result;

/// User-session agent: polls budget_events for the configured window, notifies once per threshold.
/// Designed to run as systemd --user unit; safe headless no-op.
pub fn run(cfg: Config, once: bool) -> Result<()> {
    loop {
        let _ = check_once(&cfg);
        if once {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    Ok(())
}

fn check_once(cfg: &Config) -> Result<()> {
    let Some((period, _)) = cfg.effective_budget() else {
        return Ok(());
    };
    if !cfg.notify_on_budget {
        return Ok(());
    }
    if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_err() && !cfg_path_has_display() {
        return Ok(()); // headless: log only, daemon already journaled
    }
    let conn = db::open(&cfg.db_path_expanded(), true)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let tz_local = cfg.timezone != "utc";
    let key = db::budget_key(now, tz_local, period);
    let adv = db::period_adverb(period);
    let (c80, c100): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(crossed80,0), COALESCE(crossed100,0) FROM budget_events WHERE month=?1",
            rusqlite::params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    // notify on fresh crossing only if a sentinel file hasn't recorded it
    let sent80 = sentinel(&key, 80);
    let sent100 = sentinel(&key, 100);
    if c80 == 1 && !sent80 {
        notify(&format!("NetMeter: 80% of {adv} budget used ({key})"));
        mark_sent(&key, 80);
    }
    if c100 == 1 && !sent100 {
        notify(&format!("NetMeter: 100% of {adv} budget used ({key})"));
        mark_sent(&key, 100);
    }
    Ok(())
}

fn cfg_path_has_display() -> bool {
    std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
}

fn sentinel_file(month: &str, pct: u8) -> std::path::PathBuf {
    let base = directories::ProjectDirs::from("", "", "netmeter")
        .map(|p| p.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join(format!("budget-{month}-{pct}.sent"))
}
fn sentinel(month: &str, pct: u8) -> bool {
    sentinel_file(month, pct).exists()
}
fn mark_sent(month: &str, pct: u8) {
    let f = sentinel_file(month, pct);
    if let Some(p) = f.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(f, "1");
}

fn notify(msg: &str) {
    // notify-rust first (holds handle briefly for GNOME/Wayland quirk), fallback notify-send
    let ok = notify_rust::Notification::new()
        .summary("NetMeter")
        .body(msg)
        .appname("netmeter")
        .timeout(5000)
        .show()
        .map(|h| {
            // keep handle alive momentarily so compositor doesn't drop it (issue #218)
            std::thread::sleep(std::time::Duration::from_millis(500));
            drop(h);
        })
        .is_ok();
    if !ok {
        let _ = std::process::Command::new("notify-send")
            .args(["NetMeter", msg])
            .output();
    }
    tracing::warn!("{msg}");
}
