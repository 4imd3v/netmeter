use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn d_poll() -> u64 {
    5
}
fn d_db() -> String {
    "/var/lib/netmeter/netmeter.db".into()
}
fn d_units() -> String {
    "binary".into()
}
fn d_tz() -> String {
    "local".into()
}
fn d_true() -> bool {
    true
}
fn d_ret_raw() -> i64 {
    30
}
fn d_ret_hr() -> i64 {
    365
}
fn d_max_mbit() -> u64 {
    10_000
}
fn d_basis() -> String {
    "total".into()
}
fn d_budget_period() -> String {
    "month".into()
}
fn d_excl() -> Vec<String> {
    vec![
        "lo".into(),
        "docker*".into(),
        "veth*".into(),
        "br-*".into(),
        "virbr*".into(),
    ]
}
fn d_lan() -> Vec<String> {
    vec![
        "10.0.0.0/8".into(),
        "172.16.0.0/12".into(),
        "192.168.0.0/16".into(),
        "fe80::/10".into(),
        "fc00::/7".into(),
    ]
}
fn d_force_lan() -> Vec<String> {
    vec!["tailscale0*".into(), "zt*".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "d_poll")]
    pub poll_interval_sec: u64,
    #[serde(default = "d_db")]
    pub database_path: String,
    #[serde(default = "d_units")]
    pub units: String, // binary | decimal
    #[serde(default = "d_tz")]
    pub timezone: String,
    #[serde(default = "d_excl")]
    pub exclude_ifaces: Vec<String>,
    #[serde(default = "d_lan")]
    pub lan_subnets: Vec<String>,
    #[serde(default = "d_force_lan")]
    pub force_lan_ifaces: Vec<String>,
    #[serde(default = "d_ret_raw")]
    pub retention_days_raw: i64,
    #[serde(default = "d_ret_hr")]
    pub retention_days_hourly: i64,
    #[serde(default = "d_ret_raw")]
    pub proc_retention_days: i64,
    #[serde(default = "d_true")]
    pub proc_recording: bool,
    #[serde(default = "d_max_mbit")]
    pub max_rate_mbit: u64,
    #[serde(default = "d_budget_period")]
    pub budget_period: String, // day | week | month
    #[serde(default)]
    pub budget_gb: f64,
    #[serde(default = "d_basis")]
    pub budget_basis: String, // total | wan
    #[serde(default = "d_true")]
    pub notify_on_budget: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_sec: d_poll(),
            database_path: d_db(),
            units: d_units(),
            timezone: d_tz(),
            exclude_ifaces: d_excl(),
            lan_subnets: d_lan(),
            force_lan_ifaces: d_force_lan(),
            retention_days_raw: d_ret_raw(),
            retention_days_hourly: d_ret_hr(),
            proc_retention_days: d_ret_raw(),
            proc_recording: true,
            max_rate_mbit: d_max_mbit(),
            budget_period: d_budget_period(),
            budget_gb: 0.0,
            budget_basis: d_basis(),
            notify_on_budget: true,
        }
    }
}

impl Config {
    pub fn system_path() -> PathBuf {
        PathBuf::from("/etc/netmeter/config.toml")
    }
    pub fn user_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "netmeter")
            .map(|p| p.config_dir().to_path_buf().join("config.toml"))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config/netmeter/config.toml"))
            })
    }
    pub fn load() -> Result<Self> {
        let mut val = toml::Value::try_from(Self::default())?;
        for p in [Self::system_path()].into_iter().chain(Self::user_path()) {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(part) = toml::from_str::<toml::Value>(&s) {
                    val = merge_value(val, part);
                }
            }
        }
        let mut cfg: Self = val.try_into()?;
        // env overrides NETMETER_*
        str_env("NETMETER_DATABASE_PATH", &mut cfg.database_path);
        num_env("NETMETER_POLL_INTERVAL_SEC", &mut cfg.poll_interval_sec);
        str_env("NETMETER_UNITS", &mut cfg.units);
        num_env("NETMETER_BUDGET_GB", &mut cfg.budget_gb);
        str_env("NETMETER_BUDGET_PERIOD", &mut cfg.budget_period);
        cfg.poll_interval_sec = cfg.poll_interval_sec.clamp(1, 60);
        Ok(cfg)
    }

    pub fn db_path_expanded(&self) -> PathBuf {
        PathBuf::from(shellexpand_tilde(&self.database_path))
    }

    pub fn excluded(&self, iface: &str) -> bool {
        self.exclude_ifaces.iter().any(|p| glob_match(p, iface))
    }
    pub fn forced_lan(&self, iface: &str) -> bool {
        self.force_lan_ifaces.iter().any(|p| glob_match(p, iface))
    }

    /// Effective budget: (period, budget_gb); None when off.
    pub fn effective_budget(&self) -> Option<(&str, f64)> {
        if self.budget_gb > 0.0 {
            let p = match self.budget_period.as_str() {
                "day" | "week" | "month" => self.budget_period.as_str(),
                _ => "month",
            };
            return Some((p, self.budget_gb));
        }
        None
    }
}

fn str_env(key: &str, dst: &mut String) {
    if let Ok(v) = std::env::var(key) {
        *dst = v;
    }
}

fn num_env<T: std::str::FromStr>(key: &str, dst: &mut T) {
    if let Ok(v) = std::env::var(key) {
        if let Ok(n) = v.parse() {
            *dst = n;
        }
    }
}

fn shellexpand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return format!("{h}/{rest}");
        }
    }
    s.to_string()
}

pub fn glob_match(pat: &str, s: &str) -> bool {
    // tiny glob: supports trailing * only (enough for docker*, veth*, tailscale0*)
    if let Some(pre) = pat.strip_suffix('*') {
        s.starts_with(pre)
    } else {
        pat == s
    }
}

/// Merge raw TOML values (file layers) onto defaults — used by load().
pub fn merge_value(mut base: toml::Value, over: toml::Value) -> toml::Value {
    if let (Some(b), Some(o)) = (base.as_table_mut(), over.as_table()) {
        for (k, v) in o {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}
