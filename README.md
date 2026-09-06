# NetMeter — local-first LAN/WAN/TOTAL bandwidth meter (Linux)

Single static Rust binary. Kernel counters only, no packet sniffing.
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
netmeter status
netmeter export --from 2026-09-01 --to 2026-09-07 --format csv
netmeter import --from vnstat       # one-shot vnStat history (needs `vnstat --json`)
netmeter config get | set monthly_budget_gb 50
netmeter completions bash >> ~/.bashrc
```

Budgets: set `monthly_budget_gb`, enable the user agent once per desktop user:

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
