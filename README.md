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

## Install

Prebuilt static (musl) binaries for x86_64 and aarch64 — no Rust toolchain
needed. The installer verifies the release checksum, installs the binary,
and sets up the systemd service (creates the `netmeter` user, nft table,
units; needs root):

```sh
curl -fsSL https://raw.githubusercontent.com/4imd3v/netmeter/main/install.sh | sh
```

Prefer to read before running:

```sh
curl -fsSLO https://raw.githubusercontent.com/4imd3v/netmeter/main/install.sh
sh install.sh --help        # --version, --prefix, --user, --no-daemon
sh install.sh
```

Other channels:

```sh
cargo install netmeter             # crates.io (needs a Rust toolchain)
# .deb / .rpm: attach to GitHub Releases; AUR: packaging/aur/PKGBUILD
```

From a source checkout:

```sh
make install                       # cargo install --locked --path .
make deploy                        # install to /usr/bin + restart the daemon
make check                         # fmt + clippy -D warnings + tests
```

Without root it still works in TOTAL-only mode (no LAN/WAN split, no system
service):

```sh
sh install.sh --user               # ~/.local/bin, run `netmeter daemon run`
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
- AUR: `packaging/aur/PKGBUILD` (+ `.install` hook).
- User-agent unit ships at `/usr/share/netmeter/user/`; enable per desktop user
  (copy/link into `~/.config/systemd/user/` first if needed).

## Release (maintainers)

Push a `v*` tag — `.github/workflows/release.yml` builds static musl binaries
(x86_64 + aarch64), verifies they're not dynamically linked, attaches
`netmeter-<target>.tar.gz` + `SHA256SUMS` to the GitHub Release, publishes to
crates.io when `CARGO_REGISTRY_TOKEN` is set, then attaches `.deb`/`.rpm`.

```sh
git tag v0.2.0 && git push origin v0.2.0
```

To backfill an existing tag (for example, a tag created before this workflow
existed), open **Actions → release → Run workflow**, select the `main` branch,
and enter the exact existing tag (such as `v0.2.0`) in the **tag** field. The
workflow validates the tag before building or publishing.

Local build (needs `musl-tools`):

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
ldd target/x86_64-unknown-linux-musl/release/netmeter  # → not a dynamic executable
```
