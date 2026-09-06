/// Human byte formatting: binary (KiB/MiB…) default, decimal option.
pub fn fmt_bytes(b: i64, decimal: bool) -> String {
    let b = b.max(0) as f64;
    if decimal {
        let units = ["B", "KB", "MB", "GB", "TB", "PB"];
        let mut v = b;
        let mut u = 0;
        while v >= 1000.0 && u < units.len() - 1 {
            v /= 1000.0;
            u += 1;
        }
        if u == 0 {
            format!("{v:.0} {}", units[u])
        } else {
            format!("{v:.1} {}", units[u])
        }
    } else {
        let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
        let mut v = b;
        let mut u = 0;
        while v >= 1024.0 && u < units.len() - 1 {
            v /= 1024.0;
            u += 1;
        }
        if u == 0 {
            format!("{v:.0} {}", units[u])
        } else {
            format!("{v:.1} {}", units[u])
        }
    }
}

pub fn fmt_date(ts: i64, local: bool, with_time: bool) -> String {
    let z = if local {
        jiff::Timestamp::from_second(ts)
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::system())
    } else {
        jiff::Timestamp::from_second(ts)
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC)
    };
    let dt = z.datetime();
    if with_time {
        format!(
            "{:04}-{:02}-{:02} {:02}:00",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour()
        )
    } else {
        format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
    }
}

pub fn bar(frac: f64, w: usize) -> String {
    let f = frac.clamp(0.0, 1.0);
    let fill = (f * w as f64).round() as usize;
    "█".repeat(fill) + &"░".repeat(w - fill)
}
