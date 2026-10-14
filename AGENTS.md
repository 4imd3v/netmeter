# AGENTS.md — AI agent guide for netmeter

## Memory (NWron MCP)

- Before answering questions about this project's history, decisions, or setup, call `remember` first and ground your answer in what it returns.
- After reaching a decision, changing an architecture choice, or learning a fact worth keeping, call `memorize` to persist it.
- On session start, call `recent` to scan what's already known.

Read this before changing code. Goal: correct diffs without hallucinated APIs or broken invariants.

## Source of truth (in order)

1. `PRD.md` — product decisions, privilege model, schema. If code contradicts it, ask.
2. This file — repo working agreements.
3. Code + local crate sources (`~/.cargo/registry/src/...`) — when unsure about an API, read the source. Never guess signatures.

## Workflow

- `make check` (fmt + clippy `-D warnings` + tests) must pass. No exceptions.
- Verify behavior, not just compilation: `netmeter status`, `show`, `top-apps` against a dev DB (`NETMETER_DATABASE_PATH=/tmp/x.db`, 1s poll).
- **`cargo build` before running the binary by hand.** `make check`/`cargo test` build a *test harness* (`netmeter-<hash>`) and leave the plain binary stale — testing it "confirms" bugs that are already fixed. This bit twice in one session. On this box the target dir is shared (`~/.cargo/config.toml` → `/run/media/cody/DATA/.cargo-shared-target`), so `./target/debug/netmeter` never exists at all.
- Two `make check` traps on rustc 1.99: a `#[cfg(test)] mod tests` must be the **last** item in its file (`items_after_test_module`), and clippy bans `let mut c = Config::default(); c.x = …` (`field_reassign_with_default`) — use a struct literal with `..Default::default()`.
- Makefile targets that share a name with a file *or directory* are silently skipped unless `.PHONY`. That is how `make man` became a no-op for five releases.
- Small diffs. Fewest files. No speculative abstractions, no new crates without a stated reason.
- Don't touch `/etc`, `/usr/bin`, systemd, or nft rules on the dev machine unless asked. Never `sudo` in scripts.

## Architecture invariants (do not break)

- **Single writer**: only the daemon writes the DB. Every CLI command opens read-only; the GNOME extension is a read-only viewer too — it shells out to the CLI and never opens the DB for write.
- **WAN is derived**: `WAN = TOTAL − LAN`, clamped ≥ 0. TOTAL comes from `/proc/net/dev`, LAN from the nft table. Never store WAN.
- **Time**: store UTC epoch seconds; bucket and display in local time unless `timezone = "utc"`.
- **LAN/WAN honesty**: per-interface LAN split is impossible from kernel counters — LAN is global (nft) pro-rated. TOTAL-only fallback (no nft/perms) must keep working, surfaced via `status` (the daemon publishes `meta.nft_ok` each time the state changes, because an unprivileged `status` cannot run `nft` itself — the old "any LAN bytes ever recorded" guess reported a split that had been flushed away).
- **Rollups are incremental — never rebuild them from `samples`.** `db::rollup` reads only samples newer than `meta.last_rollup_ts` and accumulates buckets (`ON CONFLICT … rx=rx+excluded.rx`). The previous full-refresh version deleted `hourly`/`daily`/`monthly` and rebuilt from `samples`, which retention prunes to 30 days — so every hourly rollup truncated history to 30 days and `retention_days_hourly=365` did nothing. A first run without the marker does one full rebuild, then seeds it.
- **Retention windows are deliberately unequal**: `samples` 30 d, `hourly` 365 d, `daily`/`monthly` unbounded (PRD §6). Any rollup that depends on rows outside its own window destroys data.
- **Measurement ceiling — document, never "fix"**: TOTAL is exact, but the kernel offloads large TCP sends (GSO) *before* netfilter accounting, so LAN reads slightly low and the derived WAN slightly high on bulk traffic (systemd#34775, openwrt#10399). Recorded in README + PRD §7. No clamping, and eBPF/XDP does not bypass it either.
- **Counter hygiene** (`daemon.rs::delta`): distinguish wrap vs reset vs spike. Never remove the sanity filter.
- **Privacy**: local-only. No network calls, no telemetry. DB holds byte counts + iface/comm names only — never payloads, hosts, URLs.
- **Privileges**: daemon runs as `netmeter` user + ambient caps (see `packaging/netmeter.service`). Viewers must work unprivileged.
- **systemd hardening has a trap list.** `packaging/netmeter.service` documents the directives that must *not* be set, and a test enforces it. Each looks like hardening and silently breaks something: `RestrictAddressFamilies=AF_UNIX` (needs AF_PACKET + AF_NETLINK), `ProcSubset=pid` (reads `/proc/net/dev`, `/proc/net/{tcp,udp}`), `ProtectProc=invisible` (reads `/proc/<pid>/fd`), `UMask=0077` (the DB must stay world-readable for viewers), `PrivateNetwork=yes`.
- **Three budget readers must agree.** `show`'s budget bar, `daemon::check_budget` and `live::query_budget` all derive the window from `db::budget_window_start` and must limit the sum to it — `show` used to sum every rendered row (30 daily rows against a *daily* budget) and reported 2313%.

## Config & schema discipline

- Layers: flags > `NETMETER_*` env > user `~/.config` > `/etc/netmeter/config.toml` > defaults. Daemon-visible keys live in `/etc`; `config set` as root writes there.
- Enum-ish string keys (`units`, `timezone`, `budget_basis`, `budget_period`) are lowercased on load by `Config::normalize`, because every consumer compares lowercase literals — without it `timezone = "UTC"` silently means local. Normalize new keys in that class too.
- JSON output (`--json`) carries `api_version` — bump it only on breaking field changes.
- Schema changes are additive (`CREATE TABLE IF NOT EXISTS`, new nullable columns/keys). Old DBs must keep opening. `meta` keys are additive too: `schema_version`, `discarded_samples`, `nft_ok`, `last_rollup_ts`.

## TUI rules (`live.rs`, `top_proc.rs`)

- Keep the single-screen layout and existing keys (`q` quit, `u` units, `+`/`−` refresh, `1/2/3` period). Don't split into tabs.
- No blocking work inside draw closures. New visible colors must read on dark terminals (no `DarkGray` + dim).
- Daemon remains the only recorder; TUIs read.

## GNOME Shell extension (`gnome-extension/`)

- **Read-only viewer, same contract as the CLI.** It never opens the DB for write; it runs `netmeter <cmd> --json` (`status`, `today`, `show`, `top-apps`) via `Gio.Subprocess`. GJS has no SQLite binding — do not reimplement interface selection or totals in JS; reuse the CLI's `api_version` output. The privacy/local-only invariant applies (no network from the extension).
- **Never block the shell thread.** Fetch async (`Gio.Subprocess.communicate_utf8_async`, or async D-Bus if distributing). A synchronous read inside gnome-shell freezes the whole UI.
- **Lifecycle (gjs.guide best-practices):** remove the old timeout before creating a new one; `destroy()` order = sources → signals → children → `super.destroy()`; no `_destroyed` flags. A `PanelMenu.Button` paints one child — wrap icon + label in a single `St.BoxLayout`.
- **Target GNOME 45+ (ESM), developed against 50.** `metadata.json` sets `shell-version` + `settings-schema`. Shared modules (e.g. `dataSource.js`) must not import `St`/`Gtk` (prefs runs in a separate GTK process). Additive UI only (low breakage risk) — no monkey-patching shell internals.
- **Reload/deploy loop (this box):** code installs to `~/.local/share/gnome-shell/extensions/<uuid>/`. **JS-module changes need a full re-login** (disable/enable does NOT reload the module). **GSettings schema changes:** `glib-compile-schemas` then `gnome-extensions disable <uuid> && enable <uuid>` reloads the schema (no re-login) — a stale schema throws `schema does not contain a key named '…'` and silently freezes polling. Talk to the live shell with `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus gnome-extensions …`; errors via `journalctl --user -b -o cat | grep -i netmeter`.
- **One binary, `/usr/bin/netmeter`.** The shell, the GNOME extension (it spawns the bare name `netmeter` via `Gio.Subprocess`, so `PATH` decides — its `binary-path` GSettings default is that bare name, so it follows the binary wherever it moves) and the daemon (`ExecStart` is pinned) all resolve the same file. `make deploy` copies the cargo build there and **deletes** `~/.cargo/bin/netmeter` — that copy used to precede `/usr/bin` in `PATH`, which silently kept the shell and the extension on an older build while the daemon ran the new one (a curl install looked like it had failed). Consequence: testing a new CLI flag for the extension needs `make deploy` (sudo), not `make install`. When the extension uses a new CLI flag, keep a fallback for older binaries.

## Release & distribution

- Primary channel is `install.sh` (curl|sh) against GitHub Releases; tags `v*` trigger `.github/workflows/release.yml` (musl tarballs + `SHA256SUMS` + `.deb`/`.rpm`; `cargo publish` only when `CARGO_REGISTRY_TOKEN` is set). PRs run `make check` + `shellcheck install.sh` (`.github/workflows/ci.yml`).
- **Version bump** = `Cargo.toml` + `Cargo.lock` (synced by `cargo build`) + `make man`. `status`/`--version` derive from `CARGO_PKG_VERSION`. The workflow refuses a tag whose `Cargo.toml` version does not match, **or that has no `CHANGELOG.md` entry**.
- **Release notes live in `CHANGELOG.md`** (Keep a Changelog). Write into `[Unreleased]` as you go; the workflow fails a tag with no matching entry, so the file cannot rot.
- **Upgrades**: `daemon install` now `enable`s for boot *and* `restart`s. `enable --now` is a no-op on an already-active service, which used to leave the daemon running the old binary while the installer printed "enabled+started". `install.sh` additionally warns when another `netmeter` earlier in `PATH` shadows the one it just installed.
- `src/main.rs::INSTALL_BIN` (`/usr/bin/netmeter`) must match the units' `ExecStart` — the test enforces it. `daemon install` copies the running binary there so cargo/curl installs work; don't change one without the other.

## Docs

- Update `README.md` for user-facing changes, `man/netmeter.1` after CLI changes (`make man` — the raw `netmeter manpage > man/netmeter.1` in older docs does not work on this box), `CHANGELOG.md` for release notes, `PRD.md` for decision changes.
