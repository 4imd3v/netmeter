# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Tests for the capture parsers, which previously had none because they were
  inseparable from their I/O (`/proc/net/dev` is a hardcoded path, `nft` needs
  `CAP_NET_ADMIN`). Both are now pure functions, covered by a verbatim
  `/proc/net/dev` snapshot and the `nft --json list counters` shape, plus a
  guard that the ruleset keeps its named counters. Mutation-checked.

### Fixed
- `make man` never ran: the target name collided with the `man/` directory, so
  make considered that up-to-date "file" satisfied and skipped the recipe
  (`make: 'man' is up to date.`). Declared all targets `.PHONY` and switched to
  `cargo run`, which also fixes the hardcoded `./target/debug` path when
  `CARGO_TARGET_DIR` points elsewhere.

### Changed
- `/usr/bin/netmeter` is now the only binary. `make deploy` deletes the
  `~/.cargo/bin` copy after installing, because it precedes `/usr/bin` in `PATH`
  and silently kept the shell and the GNOME extension on an older build while
  the daemon ran the new one. Testing a new CLI flag for the extension now
  requires `make deploy` (sudo), not `make install`.

## [0.2.4] - 2026-10-14

### Fixed
- Upgrades left the daemon running the old binary. `netmeter daemon install`
  used `systemctl enable --now`, which is a no-op on an already-active service,
  so the installer copied the new binary, reported success, and kept the
  previous process. It now enables for boot and then restarts.
- `install.sh` now warns when another `netmeter` earlier in `PATH` shadows the
  freshly installed one, naming the version actually in use.

## [0.2.3] - 2026-10-14

### Fixed
- **Rollups destroyed history.** `rollup()` deleted and rebuilt
  `hourly`/`daily`/`monthly` from `samples` on every hourly tick, but retention
  prunes `samples` to `retention_days_raw` (30 days) while `retention_days_hourly`
  is 365 — so every rollup silently truncated all history to 30 days and that
  config key did nothing. Rollups are now incremental: `meta.last_rollup_ts`
  records the newest sample already folded in and buckets are accumulated
  (`+=`) instead of rebuilt.
- The nftables install was not idempotent. `nft -f` runs its input as one
  transaction, so `add table` on an existing table aborted the whole batch and a
  second `daemon install` (or a changed `lan_subnets`) silently kept the old
  ruleset. The previous table is now destroyed first, and nft's stderr is
  surfaced instead of a bare `nft -f failed`.
- `netmeter status` reported the LAN/WAN split as available based on "any LAN
  bytes were ever recorded", so a flushed table stayed "available" forever. The
  daemon now publishes its own verdict in `meta.nft_ok`, and recreates the table
  when it verifiably disappears (flushed ruleset, reboot) instead of recording
  TOTAL-only forever.
- `show --period day` summed all 30 rendered daily rows and compared the total
  against a *daily* budget, reporting 2313% for a day that was 69% used. The
  budget bar is now limited to the current window, matching the daemon and
  `live`.
- `show --period week --iface X --json` reported `"iface":"_all"` because
  rebucketing overwrote the label.
- `--no-color` was accepted and silently ignored on `show` (no command emits
  color). Removed; passing it is now an error.
- Config values are lowercased on load, so `timezone = "UTC"` no longer
  silently means local time and `units = "DECIMAL"` no longer means binary.

### Changed
- Rust 2024 edition, MSRV raised from 1.74 to 1.85. `cargo install` on older
  toolchains now needs a release binary.
- The daemon handles SIGTERM/SIGINT: it flushes a final rollup, runs retention
  and checkpoints the WAL, and notices the signal within ~200ms instead of
  after a whole poll interval.
- `netmeter.service` is hardened further (`systemd-analyze security`: 6.4 MEDIUM
  → 4.2 OK). Deliberately *not* set, because each breaks this daemon or its
  unprivileged viewers: `RestrictAddressFamilies=AF_UNIX` (needs AF_PACKET and
  AF_NETLINK), `ProcSubset=pid` (reads `/proc/net/dev`, `/proc/net/{tcp,udp}`),
  `ProtectProc=invisible` (reads `/proc/<pid>/fd`), `UMask=0077` (the database
  must stay world-readable), `PrivateNetwork=yes`.
- `status` warns when the WAL exceeds 50 MB, which means a long-lived reader is
  pinning it.

### Removed
- Dead code: an unused variable in `top_proc`, and the no-op `--no-color` flag.

## [0.2.2] - 2026-10-14

### Added
- GNOME Shell extension (`gnome-extension/`): a read-only viewer with a panel
  indicator, Quick Settings tile and preferences. It shells out to
  `netmeter <cmd> --json` and never opens the database for write.
- `netmeter today`: today's usage since local midnight, summed from raw samples
  (fresh to within one poll interval). `live` and the extension now show a true
  "today" figure instead of a rolling 24 hours.

## [0.2.1] - 2026-09-19

### Changed
- Rewrote the documentation (README and man page) around current usage and
  configuration, and corrected live-UI key hints.

## [0.2.0] - 2026-09-19

### Added
- First tagged release: LAN/WAN/TOTAL metering from kernel counters
  (`/proc/net/dev` + nftables named counters), a background daemon with systemd
  units, the `live` ratatui dashboard, budgets with desktop notifications,
  history rollups with retention, per-interface and per-app views, vnStat
  import, CSV/JSON export, shell completions and a man page.
- Release and distribution infrastructure: musl static binaries, `SHA256SUMS`,
  `.deb`/`.rpm` packaging, and the `install.sh` installer.

### Fixed
- The per-process capture loop is capped per iteration, so a saturated interface
  can no longer starve the others.
- Capture interfaces are re-resolved periodically, so a tether or VPN interface
  that appears after the daemon starts is picked up.

[Unreleased]: https://github.com/4imd3v/netmeter/compare/v0.2.4...HEAD
[0.2.4]: https://github.com/4imd3v/netmeter/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/4imd3v/netmeter/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/4imd3v/netmeter/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/4imd3v/netmeter/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/4imd3v/netmeter/releases/tag/v0.2.0
