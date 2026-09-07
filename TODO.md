# NetMeter Ponytail-Audit TODO

Source: repo-wide `ponytail-audit` 2026-09-07.
Scope: over-engineering / complexity only. No correctness, security, perf fixes here.
Rules from `AGENTS.md`: `make check` green (fmt + clippy `-D warnings` + tests), verify with dev DB (`NETMETER_DATABASE_PATH=/tmp/x.db`, 1s poll) via `status`/`show`/`top-apps`, small diffs, single writer (daemon only), additive schema only, no `sudo` in scripts, update `README.md`/`man`/`PRD.md` if user-facing.

Net estimate: ~-280 lines, -1 dep (`notify-rust` OR `notify-send` fallback, keep one — see research §R3: decision revised to KEEP both, so dep cut deferred).

## R: research notes — Sept 2026 maturation pass (no code changed)

Version matrix (local `Cargo.lock` vs crates.io latest 2026-09-07):
- `clap` 4.6.6 = latest (2026-08-06). `ValueEnum` kebab-case default unchanged. Item 2/12 safe as planned.
- `jiff` 0.2.35 local > docs 0.2.32; tzdb 2026c (2026-07-08). Jiff 1.0 still unreleased (slipped past Summer 2025, no date Apr 2026). Stay on 0.2.x, get tz updates free. Behaviour change since 0.1: `TimeZone::system()` falls back to `unknown` (`Etc/Unknown`, UTC-like) + WARN log, not silent UTC. Code calls `system()` per-row/per-tick (items 3/4/17) → cache one `TimeZone` per call-site, fixes log spam + syscalls. `TZ` env overrides heuristics on all platforms — tests should set `TZ=UTC`.
- `rusqlite` 0.40.2 / `libsqlite3-sys` 0.38.2 bundles SQLite 3.53.2 → already past WAL-reset fix (3.51.3, 2026-03-13; 3.52.0 withdrawn over float expr-index, re-released 3.53.0). No upgrade needed. See §R1.
- `notify-rust` 4.18.0 = latest (2026-06-16). Issue #218 (GNOME 46 Wayland drop) STILL OPEN (updated 2025-11-06). Upstream workaround = hold `NotificationHandle` open — current `sleep(500ms)+drop` matches it. Community fallback = shell `notify-send` (djmaze branch). Deleting fallback (item 21 lazy) removes the only path that works when zbus/dbus fails. Revised: KEEP both, -1 dep deferred.
- `pnet` 0.35.0 = latest release but from 2024-05-30 (147 open issues, last push 2026-05-01). Successors exist (`ferranet` 0.2.0 Jul 2026 beta, `netring` 0.29.0 Jul 2026) but both unstable/async-first/XDP-focused — migration is anti-ponytail. KEEP pnet, no action.
- `ratatui` 0.30.2 = latest docs. `Sparkline::data()` takes `IntoIterator<Item: Into<SparklineBar>>`, accepts `&[u64]`/`Vec`; `&VecDeque<u64>` yields `&u64` — use `make_contiguous()` or `iter().copied()` collect (item 11 as planned).
- `directories` 6.0.0 = current. `ProjectDirs::from("","","netmeter")` works on Linux (XDG) but empty qualifier/org is off-spec; keep (no behaviour change in this pass). For item 13 (SUDO_USER home) do NOT add `homedir`/`uzers`/`nix` deps — ladder says `libc::getpwuid_r` (already dep) + `/home` fallback. `homedir 0.3.6`/`uzers 0.12`/`nix user` all wrap the same `getpwuid_r`; new dep buys nothing.
- `toml` 0.8.23 local vs 0.9.12 latest (2026-02-10). 0.9 is near-rewrite (serde/std features split, `Serializer` takes `&mut Buffer`, `FromStr for Value` parses values not docs, order needs `preserve_order`). Defer upgrade — orthogonal to shrink, do NOT bundle with item 16.
- `shellexpand` 3.1.2 (2026-02-23, dirs 6 compat). Exists for item 18 but adding a dep for ~8 lines of `~/` strip violates ladder. Keep hand-rolled dedupe as planned.
- `nft --json` counter schema stable: libnftables-json 1.1.6-1 man 2026-04-29 still `{counter:{family,table,name,handle,packets,bytes}}`. No parser change. `list counters table inet netmeter` (plural) is the documented form — current `list counters table` call already matches.
- `include_str!` unit pattern validated 2026: innisfree (`include_str!("../files/*.service")` + tera `{{executable_path}}`), repovec (`include_str!(concat!(env!("CARGO_MANIFEST_DIR"),...))` + static validators + tests). Item 1: plain `include_str!` needs no tera (no substitution — ExecStart fixed `/usr/bin/netmeter`); add `#[test]` asserting `!contains("{{")` + contains `ExecStart=` like repovec.

### R1. SQLite WAL-reset bug upgrades item 9 from hygiene to correctness-adjacent
Tailscale post-mortem 2026-08-12 + sqlite.org wal.html §11 (updated 2026-04-13): race between checkpointer and writer resetting WAL, present since 3.7.0, fixed 3.51.3. Needs 2+ connections writing/checkpointing same file at same instant — rare (never reproduced organically, needs test hooks) but corrupts (skipped pages, torn index). Local bundled 3.53.2 already fixed, so NOT an emergency. But item 9 (`show` calling `rollup` = DELETE+INSERT full refresh on a second connection, even best-effort RO) is exactly the second-writer shape the bug needs + violates single-writer invariant. Keep item 9 in Batch A, first or second. Also note: `PRAGMA wal_checkpoint(PASSIVE)` in `enforce_retention` runs on the single writer — safe, keep. RO opens rely on sqlite ≥3.22 WAL-RO relax (`-shm`/`-wal` readable or dir writable) — already satisfied by bundled version, no code change.

### R2. jiff `system()` cost → amend items 3/4/17
Each `floor_*`/`rebucket` call does `TimeZone::system()` (reads `/etc/localtime`, TZ env, may WARN). Called per-sample in `rollup`/`rebucket_week`. When implementing shared `floor()` helper (item 17), take `tz: &TimeZone` param or resolve once per batch (`let tz = if local {system()} else {UTC}`), pass down. Same for new `window_sum` (item 4). Add test with `TZ=UTC` for determinism.

### R3. Notifier decision revised (item 21)
Do NOT cut either path in this pass. Keep `notify-rust` primary + `notify-send` fallback (5 lines). Rationale: #218 open, fallback is the documented community workaround, headless (no DBUS) path depends on graceful fail anyway. If a dep must go later, gate fallback behind `notify_send` feature like djmaze branch — separate change, needs Wayland GNOME test bench. Remove `-1 dep` from net estimate for this pass.

### R4. Out of scope confirmations (no new TODO items)
- No `pnet`→`ferranet`/`netring` migration (unstable APIs, async-first, XDP scope creep).
- No `toml` 0.8→0.9 upgrade in this pass (breaking serde/std split).
- No `directories`→`etcetera`/`dirs` swap (6.0.0 current, works).
- No new dep (`homedir`, `uzers`, `nix`, `shellexpand`, `byte-unit`/`human_bytes`): every case is 5–10 lines on existing deps (`libc`, std). Ladder holds.

Order = biggest cut first. Each item: problem → replacement → files → steps → check.

---

## 1. `delete:` inline systemd units in `daemon_install` [src/main.rs]

Research (Sept 2026): pattern validated — innisfree embeds via `include_str!("../files/*.service")` + renders `{{executable_path}}` with tera; repovec embeds via `include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/..."))` + static layout validators + tests. Netmeter needs NO substitution (ExecStart fixed `/usr/bin/netmeter`), so plain `include_str!` with zero new deps. Add repovec-style `#[test]`: embedded == file on disk (or contains `ExecStart=/usr/bin/netmeter`, no `{{`).

Problem: `let unit = r#"[Unit]...` (~25 lines) + `let user_unit = ...` (~15 lines) duplicate `packaging/netmeter.service` + `packaging/netmeter-agent.service`. Drift risk, two truths.
Replacement: `include_str!("../packaging/netmeter.service")` and same for agent unit. Or read installed file at install time.
Files: `src/main.rs` (`daemon_install`), `packaging/netmeter.service`, `packaging/netmeter-agent.service`.
Steps:
1. Replace both `r#"` literals with `include_str!`.
2. Keep `SUDO_USER` home-write branch unchanged, only string source changes.
3. Confirm `cargo deb` / rpm asset list still references `packaging/` files (no path break).
Check: `make check`, `cargo build`, inspect `cargo expand` or `grep -n include_str src/main.rs`. No behavior change, no man update.

## 2. `yagni:` 4 period enums for same concept [src/main.rs, src/top_proc.rs]

Problem: `Period {Hour,Day,Week,Month}` (clap) + `ProcPeriod {Day,Week,Month}` (clap) + `top_proc::PPeriod {Day,Week,Month}` + `top_proc::PPeriodArg {Day,Week,Month}` + 2 manual mappings in `main.rs::main` and `top_proc::run`. 4 types, ~30 lines, one concept.
Replacement: single shared enum, e.g. `db::Window {Day,Week,Month}` (or keep one clap enum in `main.rs`, re-export). `Hour` stays only on `Show` path: either separate `ShowPeriod = Hour + Window` or keep `Period` and reuse for proc paths (proc never uses `Hour`, clap `possible_values` still fine if shared enum has `Hour`? Better: one `CommonPeriod {Day,Week,Month}` + `ShowPeriod {Hour,Day,Week,Month}` wrapping it, or just accept `Hour` hidden from proc help).
Files: `src/main.rs` (`Period`, `ProcPeriod`, mapping arms), `src/top_proc.rs` (`PPeriod`, `PPeriodArg`, `run`, `window_start`), `src/live.rs` if touched.
Steps:
1. Define one enum in `db.rs` or new tiny `period.rs` (prefer `db.rs`, zero new file): `pub enum Window { Day, Week, Month }` + `floor(Window,ts,local)` helper (feeds item 18).
2. `main.rs`: `ProcPeriod` deleted, `TopApps`/`TopProc` use `Window` (derive `ValueEnum, Clone, Copy`).
3. `top_proc.rs`: delete `PPeriod`+`PPeriodArg`, `run(cfg, Window)`, `window_start(Window,local)` calls `db::floor_*`.
4. Fix 2 mapping arms in `main()` (deleted, direct pass-through).
Check: `make check`, `netmeter top-apps --help`, `top-proc` keys `1/2/3` still switch, `NETMETER_DATABASE_PATH=/tmp/x.db netmeter top-apps --period week`.

## 3. `shrink:` `rebucket_week` duplicates Monday logic [src/main.rs → src/db.rs]

Problem: `main.rs::rebucket_week` (~25 lines: jiff tz, `to_monday_zero_offset`, monday midnight ts, BTreeMap sum) duplicates `db::floor_week` + `db::budget_window_start`. Third copy with `live.rs::query_week_total` (item 4).
Replacement: `rows.into_iter().group_by(|r| db::floor_week(r.ts,local))` style fold reusing `floor_week`. Keep BTreeMap sum, drop jiff block.
Files: `src/main.rs` (`rebucket_week`, `bucket_samples` week tail).
Steps:
1. Replace monday-math body with `let monday_ts = db::floor_week(r.ts, local);`.
2. Keep `iface: "_all"` insert, rx/tx/lan sums unchanged.
3. `bucket_samples` week tail calls same fn (no change needed after).
Check: `make check`, compare `show --period week` output before/after on dev DB (same Monday buckets).

## 4. `shrink:` triple open+SUM in live queries [src/live.rs]

Problem: `query_since` + `query_week_total` (hand Monday math again) + `query_budget` each `db::open(...,true)` + `SELECT COALESCE(SUM...) FROM samples WHERE ts>=?`. ~45 lines, 3 opens per 5 ticks.
Replacement: one `fn window_sum(cfg:&Config, from:i64)->(i64,i64,i64,i64)` returning `(rx,tx,lan_rx,lan_tx)`; `query_week_total` start = `db::floor_week(now,local)`; `query_budget` derives `used` from same tuple via `budget_basis`. Open once per refresh, pass `&Connection` to three callers.
Files: `src/live.rs` (`query_since`, `query_week_total`, `query_budget`, `live_loop` 5-tick block).
Steps:
1. Add `window_sum`.
2. Rewrite three fns as thin wrappers (or delete, call `window_sum` directly).
3. Hoist `db::open` out of the three into the `tick_n % 5` block.
Check: `make check`, run `live` 10s on dev DB, confirm TODAY/WEEK/MONTH strips match pre-change.

## 5. `delete:` sentinel files duplicate `budget_events` [src/user_agent.rs]

Problem: `sentinel_file`+`sentinel`+`mark_sent` (~20 lines) write `budget-*.sent` cache files to dedupe notifies, while `budget_events(crossed80,crossed100)` in DB already dedupes. Two truths, stale cache on DB wipe.
Replacement: rely on DB flags only. `check_once` notifies when `c80==1` etc.? Needs edge-trigger: currently DB flag latched, sentinel gives once-per-process. Alternative without files: track `last_key+flags` in-memory in `run()` loop (loop already sleeps 60s), or add `notified80/100` cols (schema additive allowed). Simplest lazy: in-memory `HashSet<key+pct>` in `run()`, drop file fns.
Files: `src/user_agent.rs` (`sentinel_file`, `sentinel`, `mark_sent`, `check_once`, `run`).
Steps:
1. Delete 3 sentinel fns + `directories` use here (feeds dep cut discussion).
2. `run()`: `let mut seen = HashSet::new(); check_once(&cfg,&mut seen)`.
3. `check_once`: `if c80==1 && seen.insert((key,80)) { notify }` same for 100.
Check: `make check`, `netmeter user-agent --once` twice against dev DB with crossed flags, second run silent. No new migration needed.

## 6. `delete:` legacy `monthly_budget_gb` fallback [src/config.rs, src/main.rs, src/live.rs, src/daemon.rs]

Problem: two budget knobs (`budget_gb`+`budget_period` new, `monthly_budget_gb` legacy) + `effective_budget` fallback branch + `NETMETER_MONTHLY_BUDGET_GB` env + `DAEMON_KEYS` entry + comment in `live.rs`. ~15 lines + docs.
Replacement: `budget_gb`+`budget_period` only. Keep tolerant read: unknown/legacy key in TOML ignored by serde (no crash, old DBs unaffected — config keys additive-tolerant). Decide: hard delete (old `monthly_budget_gb` silently ignored) vs one-release warn-then-ignore. Lazy = hard delete + `README.md` note.
Files: `src/config.rs` (field, Default, load env, `effective_budget`), `src/main.rs` (`DAEMON_KEYS`), `src/live.rs` comment.
Steps:
1. Delete field + `d_` default + env block + fallback branch.
2. Update `config.toml.example`, `README.md` budget section, `man/netmeter.1` via `netmeter manpage > man/netmeter.1` if help text changes.
3. `PRD.md` if it names legacy key.
Check: `make check`, old config with `monthly_budget_gb=50` still loads (ignored), new `budget_gb` path works in `show` budget bar.

## 7. `delete:` `classify_tailscale_as_lan` bool [src/config.rs]

Problem: bool + `if classify && starts_with("tailscale0")` duplicates `force_lan_ifaces` default `["tailscale0*","zt*"]` + glob match. Two ways to say same, ~5 lines + config key + `DAEMON_KEYS` entry.
Replacement: list only. Delete bool field, keep `force_lan_ifaces`. Users wanting old `classify=false` behavior remove `tailscale0*` from list.
Files: `src/config.rs` (field, Default, `forced_lan`), `packaging/config.toml.example`, docs.
Steps:
1. Delete field + Default line + `if` branch, leaving glob check.
2. Remove from `DAEMON_KEYS` in `main.rs`.
3. Docs note: tailnets stay LAN by default via list.
Check: `make check`, `forced_lan("tailscale0") == true` with defaults, false when list cleared.

## 8. `delete:` `DAEMON_KEYS` warning list [src/main.rs]

Problem: `const DAEMON_KEYS: &[&str] = &[...15 keys...]` hard-codes Config field names for a `config set` nudge. Drifts on every Config change, ~20 lines with comment.
Replacement: delete warning block. Or single generic note on every `config set` ("daemon reads /etc…"). Lazy = delete; overlay confusion already covered in `README.md`/PRD layer order.
Files: `src/main.rs` (`cmd_config::Set`).
Steps: delete const + `if DAEMON_KEYS.contains` eprintln. Keep root/user path split.
Check: `make check`, `config set units decimal` prints only `wrote ...`.

## 9. `delete:` `db::rollup` from read-only `cmd_show` [src/main.rs]

Problem: `let _ = db::rollup(&conn, ...)` runs DELETE+re-INSERT full refresh on every `show` (read path), against a `SQLITE_OPEN_READ_ONLY` handle (best-effort, fails silently on locked). Breaks AGENTS.md single-writer invariant, ~1 line + comment but big principle.
Replacement: delete call. Daemon hourly rollup is the writer; `show` already falls back to `bucket_samples` when rollups empty.
Files: `src/main.rs` (`cmd_show` head).
Steps: delete line + comment. No other change (fallback covers fresh data).
Check: `make check`, `NETMETER_DATABASE_PATH=/tmp/x.db netmeter show --period day` as unprivileged user works, no WAL write.

## 10. `delete:` dead `_unused` + `month_key` [src/config.rs, src/db.rs]

Problem: `_unused: Vec<String>` field + `d_empty()` (~5 lines) never read; `month_key` with `#[allow(dead_code)]` (~6 lines) superseded by `budget_key`.
Replacement: delete both.
Files: `src/config.rs`, `src/db.rs`.
Steps: delete field + default fn + Default line; delete `month_key`. Confirm no `toml` unknown-field break: serde ignores missing, old files with `_unused` key still parse (unknown fields ignored by default? verify — if `deny_unknown_fields` absent, fine).
Check: `make check` (clippy would have flagged unused without allow).

## 11. `stdlib:` sliding histories `remove(0)` [src/live.rs]

Problem: `hist_down.push(); if len>120 { remove(0) }` ×2, O(n) memmove per tick.
Replacement: `std::collections::VecDeque<u64>` + `push_back`/`pop_front`, `make_contiguous()` for `Sparkline::data`. ~6 lines changed.
Files: `src/live.rs` (`hist_down`, `hist_up`, draw clones).
Steps: change decls, push/pop, `let hd: Vec<u64> = hist_down.iter().copied().collect()` or `make_contiguous`.
Check: `make check`, 60s `live` run, no visual change.

## 12. `yagni:` `cmd_top by:String` [src/main.rs]

Problem: `#[arg(long, default_value="iface")] by: String` free-form, manual `if by=="day" else iface`, silent misspelling → iface path.
Replacement: `#[derive(ValueEnum)] enum TopBy { Iface, Day }`, `by: TopBy`. Matches `Period`/`OutFmt` pattern already in file.
Files: `src/main.rs` (`Cmd::Top`, `cmd_top`).
Steps: add 6-line enum, change signature, `match by`.
Check: `make check`, `top --by day`, `top --by bogus` errors with help.

## 13. `native:` hand `users_home` [src/main.rs]

Problem: `PathBuf::from(format!("/home/{user}"))` + exists check. Breaks `/root`, custom `HOME`, nsswitch/LDAP. `directories` already in deps, or `libc::getpwnam`.
Replacement: `directories::BaseDirs::new()` for target user? Daemon installs as root for `SUDO_USER`: resolve via `std::env::var("SUDO_UID")` + `libc::getpwuid` (libc already dep), or `ProjectDirs`. Lazy: `getpwuid(SUDO_UID)` fallback to `/home` join (keep current as fallback, +5 lines, still net win via correctness).
Files: `src/main.rs` (`users_home`).
Steps: read `SUDO_UID`, `getpwuid_r` or `getpwnam(SUDO_USER)`, fallback existing.
Check: `make check`, `sudo -E netmeter daemon install` writes agent unit under real home.

## 14. `shrink:` duplicate max-rate formula [src/daemon.rs]

Problem: `max_bytes_per_tick()` fn + inline `(cfg.max_rate_mbit as f64 *1e6/8.0*interval*1.25)` in LAN-delta block. Same math, two sites, drift risk.
Replacement: call `max_bytes_per_tick(&cfg, "lan")` or extract `max_bytes(cfg,max_mbit,interval)`. 1-line change.
Files: `src/daemon.rs` (`run` lan block).
Check: `make check`, spike-discard counts unchanged on dev feed.

## 15. `shrink:` `fmt_bytes` branch duplication [src/fmtx.rs]

Problem: binary vs decimal arms duplicate scale loop + `format!`, differ only in base (1024 vs 1000) + unit table. ~30 lines.
Replacement: `fn fmt_scaled(b:f64, base:f64, units:&[&str])` helper, two 3-line wrappers. Behavior identical (`{v:.0}` for B, `{v:.1}` above).
Files: `src/fmtx.rs`.
Steps: extract helper, keep `fmt_bytes(b,dec)` signature (callers untouched).
Check: `make check`, spot values: 0B, 999B/1000B decimal boundary, 1023/1024 binary boundary.

## 16. `shrink:` env override repetition [src/config.rs]

Problem: 5× `if let Ok(v)=env::var(...) { parse/assign }` blocks (~25 lines).
Replacement: tiny table/macros: `num_env("NETMETER_BUDGET_GB", &mut cfg.budget_gb)` etc. Or 10-line `macro_rules!`. Keep `clamp(1,60)` tail.
Files: `src/config.rs` (`load`).
Steps: add 8-line helper fns (`str_env`, `num_env<T: FromStr>`), replace blocks.
Check: `make check`, `NETMETER_POLL_INTERVAL_SEC=1 NETMETER_UNITS=decimal netmeter config get` reflects overrides.

## 17. `shrink:` floor乡镇 dance ×4 [src/db.rs]

Problem: `floor_hour`/`floor_day`/`floor_month`/`floor_week` each repeat `zoned→datetime→DateTime::new→to_zoned→timestamp` (~10 lines each).
Replacement: `fn floor_to(ts,local,y,mo,d,h)->i64` or compute via `zoned` + rebuild. Each floor becomes 3-line call. `floor_week` keeps weekday-offset pre-step then calls helper.
Files: `src/db.rs`.
Steps: add private helper, rewrite 4 fns as wrappers. Public signatures unchanged (callers in daemon/live/main/top_proc untouched).
Check: `make check`, existing tests + `show --period hour/day/month` boundaries match (DST spot-check if local tz).

## 18. `shrink:` double tilde expansion [src/config.rs]

Problem: `db_path_expanded` strips `~/` then calls `shellexpand_tilde` which strips `~/` again. Two truths, ~12 lines.
Replacement: one `expand_tilde(&str)->PathBuf` used by `db_path_expanded`. Delete the other.
Files: `src/config.rs`.
Steps: keep `shellexpand_tilde`, simplify `db_path_expanded` to `PathBuf::from(shellexpand_tilde(s))`.
Check: `make check`, `NETMETER_DATABASE_PATH=~/x.db` and absolute both resolve.

## 19. `yagni:` `iface_speed_mbit` wrapper [src/capture.rs, src/daemon.rs]

Problem: 6-line `read_to_string→trim→parse→ok` wrapper with one caller (`max_bytes_per_tick`).
Replacement: inline into daemon or keep? Lazy says inline (−6 lines, one less pub fn). Counter: named fn reads well. Audit verdict: inline; `capture::` surface shrinks (`read_proc`, `read_nft`, `nft_table_exists`, `render_ruleset`, `install_nft` remain).
Files: `src/capture.rs` (delete fn), `src/daemon.rs` (inline 4 lines).
Steps: delete, inline `fs::read_to_string(format!(...)).ok()?.trim().parse().ok()` chain at call site.
Check: `make check`.

## 20. `shrink:` live top-apps duplicates viewer [src/live.rs, src/top_proc.rs]

Problem: `live.rs` right column (app `Table` + `query_proc` 12-row fetch every 5 ticks) duplicates `top_proc.rs` viewer logic (~20 lines).
Replacement: share `fn today_top_apps(cfg:&Config, n:usize)->Vec<(String,i64,i64)>` (put in `db.rs` next to `query_proc` or `top_proc.rs` pub fn). Both TUIs call it. No visual change.
Files: `src/live.rs`, `src/top_proc.rs` (or `src/db.rs`).
Steps: extract helper, replace both call sites.
Check: `make check`, `live` right column matches `top-proc` day view on dev DB.

## 21. Deps: KEEP both notifier paths (revised Sept 2026) [src/user_agent.rs, Cargo.toml]

Research: `notify-rust` 4.18.0 latest, issue #218 STILL OPEN. Current `sleep(500ms)` + hold `NotificationHandle` IS the upstream workaround — do not remove. `notify-send` fallback is the community workaround when zbus fails + headless graceful path. Cutting either regresses someone. Decision: NO-OP in this pass (keep ~5-line fallback). Future option (separate change, needs GNOME Wayland bench): feature-gate fallback like djmaze `notify_send` branch. No `Cargo.toml` change here.

Problem: `notify-rust` (dbus, heavy) + `notify-send` subprocess fallback = two notifiers. `libc` also single-use (`geteuid`); keep `libc` (no std replacement), cut notifier dup instead.
Replacement: keep `notify-rust`, delete `notify-send` fallback (~5 lines) OR keep subprocess and drop `notify-rust` dep (−1 dep, bigger build win, loses GNOME handle-keep quirk comment #218 — verify quirk still matters before cutting that direction). Lazy default: delete fallback, keep dep. Bigger cut alternative noted.
Files: `src/user_agent.rs` (`notify`), `Cargo.toml`, `Cargo.lock`.
Steps (lazy): delete `if !ok` subprocess branch. (Aggressive alternative: delete `notify-rust` dep, keep subprocess, `cargo remove notify-rust`.)
Check: `make check`, `user-agent --once` on Wayland + headless (no DBUS) paths.

---

## Execution order (3 batches, each green)

Batch A (safe deletes, no visuals): 9, 10, 8, 12, 14, 18, 19.
Batch B (shared helpers): 3, 4, 15, 16, 17, 20.
Batch C (config surface + units + notifier, needs doc updates): 6, 7, 1, 2, 11, 13, 21.
Each batch: `make check`, dev-DB smoke (`status`, `show --period day/week`, `top-apps`), `git diff --stat` review (target net ~-270 lines; dep cut deferred per §R3).

## Docs to update on land

- `README.md`: budget keys (if 6 lands), tailscale default (if 7 lands).
- `man/netmeter.1`: regenerate via `netmeter manpage > man/netmeter.1` if CLI enums change (2, 12).
- `PRD.md`: only if product decision changes (budget key removal, sentinel removal).
- `packaging/config.toml.example`: mirror Config deletions (6, 7).
