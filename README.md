# NetMeter — know where your gigabytes went (Linux)

NetMeter is a local-first bandwidth meter for Linux: a headless daemon records
usage in the background, and a single static Rust binary answers "how much did
this machine use — LAN vs WAN vs TOTAL — and which apps used it?" No cloud, no
telemetry, no DPI — just kernel counters plus lightweight per-app accounting,
all stored in a local SQLite database.

```sh
$ netmeter show --period day
DATE                   DOWN         UP      TOTAL          LAN ↓/↑          WAN ↓/↑
2026-09-11          1.8 GiB   83.3 MiB    1.9 GiB          0 B/0 B 1.8 GiB/83.3 MiB
Budget (daily): 1.9 GiB/2.0 GiB █████████░ 96%

$ netmeter top-apps
APP                            DOWN         UP      TOTAL  today
helium                    531.6 KiB  242.3 KiB  773.9 KiB  ██████████
zed-editor                 62.2 KiB   56.3 KiB  118.5 KiB  ██░░░░░░░░
unknown                    90.9 KiB   43.2 KiB  134.1 KiB  ██░░░░░░░░
```

## Features

- **LAN vs WAN vs TOTAL on every view** — TOTAL from `/proc/net/dev`, LAN from
  accept-only nftables counters, `WAN = TOTAL − LAN`. Degrades honestly to
  TOTAL-only where nft isn't available (`status` tells you).
- **Per-app history** — `top-apps --period day|week|month` shows which processes
  used what; `top-proc` is the same data as an auto-refreshing dashboard.
  Recorded by the daemon, so viewers need no privileges (short flows land in
  `unknown` — best effort, documented).
- **Live dashboard** — `live` redraws every second: per-iface rates, sparklines,
  today/week/month strips, budget gauge, and today's top apps side by side.
- **Boring-in-a-good-way daemon** — systemd unit with least-privilege ambient
  capabilities, autostart on boot, survives reboot/sleep/interface flaps without
  phantom gigabytes (wrap/reset/spike sanity filter on every sample).
- **Budgets that nudge you** — configure `budget_gb` + `budget_period`, and a
  per-user agent pops a desktop notification at 80%/100%.
- **Local-first & private** — offline, <1% CPU, tiny disk footprint. The DB holds
  byte counts plus interface/process names only — never payloads, hosts, or URLs.
- **Plays well with others** — one-shot vnStat history import, CSV/JSON export,
  stable `--json` API (`api_version`), shell completions, and a man page.

Spec: `PRD.md`. License: MIT (`LICENSE-MIT`).

## Install (needs root for daemon + LAN/WAN split)

```sh
cargo install --locked --path .
sudo netmeter daemon install   # creates netmeter user, nft table, systemd units, starts service
```

Without root it still works in TOTAL-only mode (no LAN/WAN split):

```sh
cargo run -- status
NETMETER_DATABASE_PATH=~/.local/share/netmeter.db cargo run -- daemon run
```

## Use

```sh
netmeter live                      # 1s dashboard, q to quit
netmeter show --period day         # hour|day|week|month
netmeter show --period month --json
netmeter top --by iface            # or --by day
netmeter top-apps --period week    # per-app history: day|week|month (daemon-recorded)
netmeter top-proc                  # per-app dashboard, auto-refreshing (no sudo needed)
netmeter status
netmeter export --from 2026-09-01 --to 2026-09-07 --format csv
netmeter import --from vnstat       # one-shot vnStat history (needs `vnstat --json`)
netmeter config get | set budget_gb 50
netmeter config set budget_period week   # day | week | month
netmeter completions bash >> ~/.bashrc
```

Budgets: set `budget_gb` + `budget_period`, enable the user agent once per desktop user:

```sh
systemctl --user enable --now netmeter-agent   # after install copies the unit
```

## How LAN/WAN split works

`daemon install` creates accept-only nftables named counters in table
`inet netmeter` (`ip saddr @lan4` in, `ip daddr @lan4` out). TOTAL comes
from `/proc/net/dev`, LAN from nft, `WAN = TOTAL − LAN`. No nft/perms
(e.g. containers) → honest TOTAL-only fallback, `status` tells you.

## Layout

- `src/main.rs` — clap CLI (`show/top/status/config/export/import/completions/daemon/user-agent`)
- `src/daemon.rs` — poll loop, wrap/reset/sanity filter, retention, budgets
- `src/capture.rs` — `/proc/net/dev` + `nft --json` readers, ruleset renderer
- `src/db.rs` — rusqlite/WAL, rollups, local-time buckets
- `src/live.rs` — ratatui dashboard · `src/user_agent.rs` — session notifier
- `packaging/` — systemd units, deb/rpm/AUR scripts, example config · `man/` — generated man page

## Packages (deb/rpm/AUR/man)

```sh
./target/debug/netmeter manpage > man/netmeter.1  # regenerate after CLI changes
cargo install cargo-deb && cargo deb               # → target/debian/*.deb
cargo install cargo-generate-rpm && cargo generate-rpm  # needs rpmbuild
```

- deb: binary + both systemd units + `/etc/netmeter/config.toml` (conffile) +
  man page; postinst creates the `netmeter` user. Depends: systemd.
  Recommends: nftables (LAN/WAN split), libnotify-bin (fallback notify).
- AUR: `packaging/aur/PKGBUILD` (+ `.install` hook). Point `url=` at your repo.
- User-agent unit ships at `/usr/share/netmeter/user/`; enable per desktop user
  (copy/link into `~/.config/systemd/user/` first if needed).

## Release (musl static + deb/rpm)

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
ldd target/x86_64-unknown-linux-musl/release/netmeter  # → not a dynamic executable
cargo install cargo-deb cargo-generate-rpm
cargo deb --target=x86_64-unknown-linux-musl
cargo generate-rpm --target=x86_64-unknown-linux-musl
```
