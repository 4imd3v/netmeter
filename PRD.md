# PRD — NetMeter CLI

> Status: v2.0 IMPLEMENTED — M1 MVP built (see README.md). Per-process (M2/Aya) still deferred.
> Name: `netmeter` · License: MIT · Scope: Linux-only MVP · Language: Rust (AI-built)
> Previous: v1.0 → corrections below from deep research (vnStat 2.13/2.14, nftables 1.0.9+, ratatui 0.30, rusqlite 0.40, Aya eBPF, systemd caps, notify-rust 4.18, musl packaging)

## 0. What changed in v2.0 (read first)

1. **Privilege model corrected → least-privilege system service.** `AmbientCapabilities=CAP_NET_ADMIN` + `CapabilityBoundingSet=CAP_NET_ADMIN`, `User=netmeter`. User-unit `AmbientCapabilities` fails with `218/CAPABILITIES` (verified Sept 2025 report). File-`setcap` rejected (sticky-binary risk). See §6.
2. **nft design corrected → named counters in private table, accept-only.** Anonymous counters hit reset bug (#1401); `flush ruleset` would nuke Docker/firewalld. We use `table inet netmeter` + named counters + `list counters` JSON. Coexists with firewalld/Docker/uFW at `priority filter`. See §5.
3. **Notification design corrected → daemon CANNOT notify.** System service (root) has no user-session D-Bus; `notify-send` from daemon silently fails on Wayland/GNOME46+. New split: daemon records `budget_events` in DB+journal; `netmeter user-agent` (systemd **user** unit) sends the popup via `notify-rust` (zbus) with `notify-send` fallback. Headless → log only. See §9.
4. **Sanity filter added (vnStat lesson).** Counter resets/PPPoE flaps + 32/64-bit wrap confusion cause phantom GBs (vnStat issues #40/#224/#234). We add `max_rate_mbit` per-iface sanity + counter-width detection + RTC/TimeSyncWait on boot. See §7.
5. **Subnet defaults corrected for 2026 reality.** Added Tailscale/CGNAT rule: `100.64.0.0/10` NOT in default LAN (ISP CGNAT = WAN); `tailscale0` interface forced to LAN + `classify_tailscale_as_lan=true`. Docker `172.17/16`, k8s, ULA `fc00::/7`, link-local included. Multicast/broadcast → LAN (documented). See §5.
6. **Storage PRAGMA stack pinned.** `WAL + synchronous=NORMAL + busy_timeout=5000 + cache_size=-64000 + temp_store=MEMORY + foreign_keys=ON`, single writer, `BEGIN IMMEDIATE`, short txns. Backup via `VACUUM INTO`, never file copy. See §8.
7. **M2 per-process path chosen: Aya eBPF (deferred), not ss-polling.** `/proc` inode→PID mapping is racy (misses short flows, `unknown TCP` bucket — nethogs/bandwhich caveat). Precise path is Aya `cgroup_skb`/TC hooks (production-proven 2026: bpfman, Pulsar, mitmproxy_rs) but needs nightly + BTF + extra crates. Stays M2 opt-in. See §12.
8. **Packaging pinned: musl static + deb/rpm + cargo.** `x86_64-unknown-linux-musl`, `rusqlite bundled` (SQLite 3.53.2), `rustls` (no openssl), `cargo-deb`/`cargo-generate-rpm` pattern (stgit precedent). Verify with `ldd → not a dynamic executable`. See §11.
9. **TUI/CLI stack pinned.** `ratatui 0.30` (modular workspace, `run()` helper, crossterm 0.29 backend) + `clap 4 derive` + `tracing` logs + `clap_complete` completions. No async in render path; tokio only for tick/event stream. See §10.

## 1. Problem & differentiation

No simple local-first CLI answers: "how much did THIS machine use — LAN vs WAN vs TOTAL — by hour/day/week/month?" Scoreboard Sept 2026:

| Tool | Totals | LAN/WAN split | History | Live TUI | Daemon | Verdict |
|---|---|---|---|---|---|---|
| vnStat 2.13 (+2.14 dev) | ✅ per-iface | ❌ none | ✅ 5min/hr/day/mo/yr, JSON v2 API, `--alert` exit-2 | ⚠️ `--live` line only | ✅ vnstatd, no-root, XDG conf | closest cousin; no split, no TUI |
| bandwhich / nethogs | ✅ live rate | ❌ | ❌ | ✅ | ❌ needs `CAP_NET_RAW+ADMIN+SYS_PTRACE` | answers "which proc NOW", not history |
| ayaFlow (2026, Aya+TC+SQLite) | ✅ +SNI/DNS | ⚠️ | ✅ SQLite+Prom | ✅ web | ✅ | heavy, server-oriented, not personal CLI |
| procflow (design-stage) | per-Identity payload | n/a | DuckDB tiers | planned | eBPF daemon+protobuf IPC | validates our M2 direction, not shippable |

NetMeter = vnStat's trust (kernel counters, light, no sniff) + accurate LAN/WAN via nft accounting + ratatui dashboard + Rust single binary. Non-goals hold: no DPI/SNI, no pcap, no firewall drops, no cloud, no Win/macOS in v1.

## 2. Goals (unchanged, tightened)

- G1: Every view shows three numbers: **LAN + WAN + TOTAL**, `TOTAL == LAN + WAN` (WAN derived as `TOTAL − LAN`, clamped ≥0).
- G2: Headless daemon, autostart on boot, survives reboot/sleep/interface flap without phantom traffic.
- G3: Both UX modes: one-shot tables (`show`) + live dashboard (`live`).
- G4: Configurable: units, ifaces, subnets, retention, budgets, timezone.
- G5: Local-first offline, <1% CPU, <50MB RAM daemon, <100MB disk/year typical single-NIC.
- G6: Perfect-tool bar: ±5% vs ISP meter monthly; install→reboot→data intact; `cargo install` or `.deb/.rpm` in one step.

## 3. Non-goals (v1)

NG1 no per-process/per-domain (M2 opt-in `top-proc` via Aya). NG2 no pcap/DPI/blocking (count-only nft rules, always `accept`). NG3 no cloud/multi-device/mobile. NG4 no Windows/macOS GUI or backend (M3 TOTAL-only via host counters, no split).

## 4. Users & success criteria

Single Linux power-user/dev. Acceptance:
- A1 install→`daemon install`→reboot→`status` green, data continuous.
- A2 `show --period month` vs ISP within ±5% (loopback/docker/veth excluded).
- A3 idle overhead verified: `systemd-cgtop` <1% CPU, RSS <50MB; 30-day query <200ms.
- A4 `show --json` stable schema (`api_version` bump only on breaking change, vnStat `jsonversion` precedent).
- A5 corrupt DB → auto-backup+recreate+warn, ≤1 poll window lost on `kill -9`.

## 5. LAN vs WAN design (Q1 LOCKED)

### Why counters alone can't split
`/proc/net/dev` gives per-NIC totals. Home PCs carry LAN+WAN on one NIC → split needs kernel accounting. Decision: **nftables named counters** (kernel counts, ~zero overhead), not conntrack polling, not `ss` sampling.

### Ruleset spec (private table, accept-only, Docker-safe)
- Table: `inet netmeter` (family `inet` covers v4+v6 in one place). NEVER `flush ruleset`; install = `add table` + `add chain` + `add counter` + `add rule`, uninstall = `delete table inet netmeter` only.
- Named counters (avoid anonymous-reset bug): `lan_rx_bytes`, `lan_tx_bytes` (+ `lan_rx_pkts`, `lan_tx_pkts` for diagnostics).
- Chains (base, `type filter hook {input,output} priority filter; policy accept`):
  - `input`: `ip daddr @lan_nets counter name lan_rx_bytes accept` + `ip6 daddr @lan_nets counter name lan_rx_bytes accept` (ingress classified by *source*? careful) — canonical: **egress classified by daddr, ingress by saddr**:
    - `chain acct_in { type filter hook input priority filter; policy accept; ip saddr @lan4 counter name lan_rx_bytes accept; ip6 saddr @lan6 counter name lan_rx_bytes accept; }`
    - `chain acct_out { type filter hook output priority filter; policy accept; ip daddr @lan4 counter name lan_tx_bytes accept; ip6 daddr @lan6 counter name lan_tx_bytes accept; }`
  - Sets: `set lan4 { type ipv4_addr; flags interval; elements = { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 } }`, `set lan6 { type ipv6_addr; flags interval; elements = { fe80::/10, fc00::/7 } }`. Elements regenerated from config on `daemon install`/`restart` (no live element churn in v1).
- Priority note: compat `iptables-nft` lives in `ip filter` @ priority 0 too; accept-only rules at `filter` never shadow Docker DNAT (nat dstnat precedes filter) nor firewalld drops — but document: if user runs strict DROP forward policy, our chains still only count+accept within our table, verdict stays with the dropping table per hook order. Test matrix must include Docker-active host.
- Requirements: kernel ≥3.13 (practically ≥5.10 for counter comments), `nftables ≥1.0.9` (Trixie default). `iptables` 1.8.13 (Mar 2026) is maintenance-only shim — do NOT depend on it.
- Read path: `nft --json list counters table inet netmeter` (schema-check `json_schema_version`), parse `packets`/`bytes` per named counter. Daemon polls alongside `/proc/net/dev` each tick: `TOTAL` from proc, `LAN` from nft, `WAN = max(0, TOTAL − LAN)` per iface aggregate (nft counters are global; per-iface LAN split is NOT available in v1 — report LAN/WAN global + TOTAL per-iface; document honestly).
- Degrade: nft missing / no `CAP_NET_ADMIN` / container without netlink → TOTAL-only mode + `status` warns `lan_split: unavailable (reason)`. Never crash.

### LAN defaults (2026-correct)
```toml
lan_subnets = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fe80::/10", "fc00::/7"]
# 100.64.0.0/10 (CGNAT) deliberately EXCLUDED: ISP CGNAT = WAN.
classify_tailscale_as_lan = true   # traffic on iface tailscale0* counted LAN regardless of 100.x
force_lan_ifaces = ["tailscale0*", "zt*"]  # zerotier etc.
exclude_ifaces = ["lo", "docker*", "veth*", "br-*", "virbr*"]
# multicast 224.0.0.0/4 + broadcast + lo → counted LAN/not-WAN, documented; loopback excluded from TOTAL by default
```

## 6. Daemon & privilege (systemd)

- Unit: system service `/etc/systemd/system/netmeter.service`, `WantedBy=multi-user.target`, `After=network.target time-sync.target`, `Restart=on-failure`.
```ini
[Service]
Type=simple
User=netmeter
Group=netmeter
AmbientCapabilities=CAP_NET_ADMIN
CapabilityBoundingSet=CAP_NET_ADMIN
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/netmeter
ExecStart=/usr/bin/netmeter daemon run
```
- Rationale: `CAP_NET_ADMIN` = netlink nft ops (capabilities(7)). Non-root + ambient = least privilege; `User=netmeter` owns DB dir. User units CANNOT acquire ambient caps (systemd `218/CAPABILITIES`) — system unit mandatory. No `CAP_NET_RAW` (we never sniff), no `SYS_PTRACE` (M2 only).
- Boot ordering: `After=time-sync.target` + `TimeSyncWait`-style gate (wait ≤60s for NTP/RTC sane; vnStat RTC lesson) before writing timestamped samples; while unsynced, accumulate in memory, backfill on sync.
- Tick: default 5s (range 1–60). Each tick: read `/proc/net/dev` + `nft --json` counters → compute deltas → sanity-filter (§7) → single SQLite txn (`BEGIN IMMEDIATE`, prepared statements) → enforce retention hourly.
- Sleep/suspend: detect `now - last_tick > 2× interval` → carry forward, never fabricate rate; log gap in journal.
- No-systemd fallback: `daemon run --foreground` + pidfile (documented, no autostart, no nft auto-install — TOTAL-only unless root runs `daemon install-nft`).

## 7. Correctness: counters, wraps, resets, sanity (vnStat lessons)

- Sources: `/proc/net/dev` primary (fallback `/sys/class/net/*/statistics/{rx,tx}_bytes` if proc missing), per-iface `rx_bytes/tx_bytes` u64.
- Width detection: assume 64-bit; if `value < 2^32` after seeing `>4G` history treat as 64-bit persistent (vnStat a76c9065 lesson). 32-bit wrap: `delta = (new >= old) ? new-old : (2^32 - old + new)`; 64-bit wrap practically impossible but handle identically with 2^64.
- Reset vs wrap vs spike: interface down/up, PPPoE flap, driver reset → counters drop to 0/small. Rule: if `new < old` AND `new < 1MiB` → treat as reset (delta = new, log). If implied rate `delta/interval > max_rate` → discard sample, keep `old` baseline, count `discarded_samples` metric surfaced in `status`.
- `max_rate_mbit`: default from `BandwidthDetection` equivalent — read `/sys/class/net/<if>/speed` (Mb/s) when present; fallback global `max_rate_mbit = 10000` (10G) + per-iface override `max_rate_mbit_<if>`. Re-detect every 5 min (`BandwidthDetectionInterval` precedent). Prevents 4GB-phantom bug (issues #40/#224/#234).
- Overflow timing bound: 32-bit @1Gbps wraps in ~34s → 5s default poll safe; if user sets interval >30s with 32-bit iface, warn at install.
- nft resets: ruleset flush/reboot zeroes named counters → same reset rule; daemon re-installs table on start if missing (`nft list table inet netmeter` fails → recreate from config sets).
- Clock: monotonic `CLOCK_MONOTONIC` for deltas, wall-clock UTC epoch for storage; refuse to write samples with wall time < last committed (NTP step back) — hold in mem until time catches up.

## 8. Storage & aggregation

- Path: `/var/lib/netmeter/netmeter.db` (0600 dir 0755? DB 0640 `netmeter:netmeter` + ACL/read group for CLI? Simplest: DB 0644 byte-counts only, no hosts/URLs — privacy §13. LOCKED 0644 for CLI readability as non-root user).
- Engine: `rusqlite 0.40` + `bundled` (SQLite 3.53.2). Single writer (daemon). CLI read-only (`SQLITE_OPEN_READ_ONLY`).
- Per-connection PRAGMAs (every open): `journal_mode=WAL; synchronous=NORMAL; busy_timeout=5000; cache_size=-64000; temp_store=MEMORY; foreign_keys=ON; mmap_size=268435456`.
- Txn discipline: `BEGIN IMMEDIATE`, one txn/poll, `prepare_cached` inserts, keep txn <5ms. Never hold txn across `nft` subprocess call (read first, then txn).
- DDL (v1):
```sql
CREATE TABLE IF NOT EXISTS samples(
  ts INTEGER NOT NULL,            -- UTC epoch sec, poll-aligned
  iface TEXT NOT NULL,
  rx_total INTEGER NOT NULL, tx_total INTEGER NOT NULL,
  lan_rx INTEGER NOT NULL, lan_tx INTEGER NOT NULL,  -- global LAN pro-rated? v1: global counters stored on iface='*lan'
  PRIMARY KEY(ts, iface)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hourly(ts_hour INTEGER, iface TEXT, rx INTEGER, tx INTEGER, lan_rx INTEGER, lan_tx INTEGER, PRIMARY KEY(ts_hour, iface)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS daily(ts_day INTEGER, iface TEXT, rx INTEGER, tx INTEGER, lan_rx INTEGER, lan_tx INTEGER, PRIMARY KEY(ts_day, iface)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS monthly(ts_mon INTEGER, iface TEXT, rx INTEGER, tx INTEGER, lan_rx INTEGER, lan_tx INTEGER, PRIMARY KEY(ts_mon, iface)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS budget_events(month TEXT PRIMARY KEY, crossed80 INTEGER, crossed100 INTEGER);
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);  -- schema_version, nft_ok, discarded_samples
```
- Rollups: computed in-daemon every hour (hours→days at midnight local, months on 1st) + on-demand backfill in CLI if gap detected. Boundaries in **local time** (procflow ADR-0003 precedent), week Monday ISO, month calendar, `timezone = local|utc` config.
- Retention: `retention_days_raw=30` (5s rows ≈ 17k/day/iface ≈ ~2MB/day → 30d ≈ 60MB worst case, vacuum weekly), `retention_days_hourly=365`, daily/monthly unbounded (tiny). `VACUUM INTO backup` for export/backup; never raw file copy while daemon runs.
- Size math documented in `status` (`db_size`, `wal_size`, checkpoint health; warn on WAL > 50MB = checkpoint starvation from pinned reader).

## 9. Budgets & notifications (Q6 LOCKED, design corrected)

- Config: `monthly_budget_gb = 0` (off) else e.g. 50. `show --period month` renders `used/budget bar %`.
- Daemon: evaluates month-to-date WAN+LAN TOTAL at each tick; on crossing 80%/100% inserts `budget_events` + journal log. Sends NOTHING itself (no session bus).
- User agent: `netmeter user-agent` under systemd **user** unit (`--user`, `After=graphical-session.target`, restart on-failure, 60s poll): reads DB `budget_events`, fires `notify-rust 4.x` (zbus default) summary `NetMeter: 80% of Sep budget (40/50 GiB)`; fallback `notify-send` if D-Bus absent; hold `NotificationHandle` briefly + `on_close` for GNOME/Wayland persistence quirk (issue #218). Headless (no `DBUS_SESSION_BUS_ADDRESS`) → skip silently, `status` shows `notified80/100`.
- `notify_on_budget=true` default; `budget_basis = total|wan` (default `total`; metered-ISP users set `wan`).

## 10. CLI & TUI spec (Q4 LOCKED: both)

- Framework: `clap 4 derive` (`Parser` root, `Subcommand` enum, `ValueEnum` for period/units/format). Global `--output auto|text|json`, `--no-color`, `--limit/--offset` on lists (CLI-spec 2026 pattern). `clap_complete` `completions <shell>` subcommand. `Command::debug_assert` test.
```
netmeter live [--iface eth0] [--unit auto|bin|dec]
netmeter show --period hour|day|week|month [--from YYYY-MM-DD] [--iface eth0] [--json|--no-color]
netmeter top --by iface|day [--limit N]
netmeter status [--json]          # daemon alive, nft ok, db size, discarded, version, api_version
netmeter config get|set|path|reset
netmeter export --from X --to Y --format csv|json
netmeter import --from vnstat [--db PATH]   # vnStat JSON in, ours out
netmeter completions <bash|zsh|fish|elvish>
netmeter daemon run|start|stop|restart|install|uninstall|install-nft
netmeter user-agent [--once]      # normally via user unit
```
- Periods: `hour`=last 24 hourly bins, `day`=last 30 daily, `week`=last 12 Mon–Sun, `month`=last 12 calendar months. `--from` anchors end date. All values bytes internally, rendered `binary` (KiB/MiB…) default or `decimal` (KB/MB) per config; `--json` always bytes + `api_version: 1`.
- `live`: `ratatui 0.30` + `crossterm 0.29` backend via `ratatui::run()` helper; 1s redraw, immediate-mode, layout `Table` per-iface + `Sparkline` total + LAN/WAN/TOTAL header + budget gauge. Panic hook restores terminal. Resize → redraw from backend size (no cached-size assumption). Render to stdout; logs via `tracing` to file only (never stdout in TUI). No tokio in draw path; tick via `crossterm::event::EventStream` + interval.
- `show` table sketch (kept):
```
DATE        DOWN      UP      TOTAL   LAN ↓/↑      WAN ↓/↑
2026-09-05  2.1 GiB   310 MiB 2.4 GiB 800/120M     1.3G/190M
Budget Sep: 2.9/50 GiB ████░░░░░░ 6%
```
- `import --from vnstat`: parse `vnstat --json` (`jsonversion` check, modes d/m), map per-iface day/month entries into `daily/monthly` (source=`vnstat`), ~30 lines + test fixture. Preserves history (Q7 LOCKED include in M1).

## 11. Config, packaging, distribution

- Precedence: CLI flags > `NETMETER_*` env > user `~/.config/netmeter/config.toml` > system `/etc/netmeter/config.toml` > defaults. `config set` edits user file. TOML (v1 keys):
```toml
poll_interval_sec = 5
database_path = "/var/lib/netmeter/netmeter.db"
units = "binary"            # binary | decimal
timezone = "local"          # local | utc
week_start = "monday"
exclude_ifaces = ["lo", "docker*", "veth*", "br-*", "virbr*"]
lan_subnets = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fe80::/10", "fc00::/7"]
classify_tailscale_as_lan = true
force_lan_ifaces = ["tailscale0*", "zt*"]
retention_days_raw = 30
retention_days_hourly = 365
max_rate_mbit = 10000
# max_rate_mbit_eth0 = 1000
monthly_budget_gb = 0
budget_basis = "total"      # total | wan
notify_on_budget = true
```
- Build: Rust stable, `rusqlite{bundled}`, `rustls` (never openssl-vendored), `ratatui/crossterm`, `clap{derive}`, `serde/serde_json`, `notify-rust`, `tracing`, `directories`, `jiff` (dates). MSRV = clap's (1.74+) floor.
- Release: `x86_64-unknown-linux-musl` static (`rustup target add`, `linker=musl-gcc`, `strip`, LTO), verify `ldd → not a dynamic executable`. Artifacts: musl tarball + `.deb` (`cargo-deb`) + `.rpm` (`cargo-generate-rpm`) + `cargo install --locked netmeter`. AUR recipe M2. x86_64 + aarch64 (via `cross`/BlackDex images in CI).
- Install footprint: binary ~5–10MB stripped, deb/rpm ship binary + system unit + user-agent unit + default configs. `daemon install` creates `netmeter` user, table, units, enables both (`--system` + `--user` note: user unit enabled per-user on first login).

## 12. Roadmap (no build yet)

- M1 MVP (ship): §§5–11 Linux, systemd system+user units, nft split (TOTAL-only fallback), show+live+top+status+config+export+import+completions, budgets+user-agent notify, man pages.
- M2 (partly SHIPPED v0.2.0): `top-proc` live per-process view via AF_PACKET capture + `/proc` inode→PID attribution (nethogs/bandwhich method, best-effort: short flows → `unknown`, needs `sudo` for CAP_NET_RAW + full fd visibility). UPGRADE PATH: Aya eBPF `cgroup_skb`/TC hooks + Identity keying + persisted rollups (needs nightly, `bpf-linker`, BTF kernel — deferred until requested). Remaining: TUI graphs (SHIPPED: history bars in `live`), `install --user` TOTAL-only mode.
- M3: cross-OS TOTAL-only (no split documented), Prometheus endpoint (vnStat `vnstat-metrics.cgi` precedent).

## 13. Security & privacy

Local-only: no sockets except nft netlink (root, least-cap) + user-session D-Bus notify; no telemetry, no network calls, ever. DB 0644 holds byte counts + iface names only (no URLs/hosts/SNI — explicitly out of scope even though ayaFlow does it). Systemd hardening on (§6) + `ProtectKernelModules=true`, `SystemCallFilter=@system-service`. Supply chain: `--locked` builds, `cargo audit/deny` in CI.

## 14. Test plan (AI-built, must pass before "done")

- Unit: delta math (wrap/reset/sanity), subnet classifier (incl. CGNAT/Tailscale/Docker cases), rollup boundaries (DST change, month edge), JSON schema golden.
- Integration (Linux VM/CI): fake `/proc/net/dev` fixtures + `nft -c` ruleset check; reboot-sim (drop counters → no phantom); clock jump; Docker-active coexistence (`nft list ruleset` still shows docker table); user-agent notify dry-run (`--once` with fake bus).
- Perf: 30d query <200ms on 500k-row fixture; poll+write p99 <50ms; `systemd-cgtop` idle gate.
- Compat: kernels 5.15/6.x, nftables 1.0.9+, GNOME/Wayland + headless, x86_64+aarch64 musl smoke.

## 15. Decisions log (all LOCKED)

Q1 LAN+WAN+TOTAL via nft named counters + proc totals. Q2 Linux-only MVP. Q3 per-process DEFER M2/Aya. Q4 both table+TUI. Q5 Rust single static binary (C/Python/Go rejected §0.10 history). Q6 notify via user-agent (daemon cannot). Q7 vnStat import IN M1. Q8 `netmeter` + MIT.

## 16. Research sources (Sept 2026 cut)

vnStat 2.13/2.14 (humdi.net, manpages, CHANGES, issues #40/#224/#234, ifinfo.c readproc); nftables docs (Counters wiki, nft(8), libnftables-json, OneUptime counters 2026-03, Debian13 migration, ZeonEdge/pbxscience/serverside 2026 status, iptables 1.8.13 maintenance); systemd caps (AmbientCapabilities+BoundingSet guides, NFTables.Port CAP_NET_ADMIN, capabilities(7), 218/CAPABILITIES user-unit failure); ratatui 0.30/0.30.2 changelog+ARCHITECTURE+FAQ+best-practices; rusqlite 0.40.1/bundled + SQLite WAL docs + 2026 prod stacks (busy_timeout/IMMEDIATE/single-writer); bandwhich/nethogs/proc-bandwidth + netring /proc-attribution caveats + procflow ADRs; Aya (aya-rs.dev, FOSDEM26, InnerWarden 40-hooks, ayaFlow, BTF/CO-RE); notify-rust 4.18 + GNOME46/Wayland #218 + Arch desktop-notifications (systemd-run --machine); musl static (rustfaq, RHEL musl, stgit deb/rpm precedent, BlackDex images); Tailscale CGNAT/100.64/10 + reserved IPs.
