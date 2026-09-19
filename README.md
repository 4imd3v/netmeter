# NetMeter - local bandwidth usage for Linux

NetMeter is a local-first bandwidth meter for Linux. A background daemon records
usage in a local SQLite database, and the CLI shows TOTAL traffic, an nftables
LAN estimate, derived WAN usage, and best-effort per-app attribution. There is
no cloud service, telemetry, payload capture, or URL/host logging.

```text
$ netmeter show --period day
DATE                   DOWN         UP      TOTAL          LAN down/up       WAN down/up
2026-09-19          11.7 KiB    14.8 KiB    26.4 KiB       0 B/0 B        11.7 KiB/14.8 KiB
```

## What NetMeter measures

- **TOTAL** comes from Linux interface counters in `/proc/net/dev`.
- **LAN** is a global nftables counter estimate. It is not an exact per-interface
  kernel split; NetMeter pro-rates the global LAN total across interfaces when a
  view needs a per-interface breakdown.
- **WAN** is derived as `TOTAL - LAN`, clamped at zero.
- **Per-app history** is best effort. The daemon uses AF_PACKET and `/proc`
  metadata; short or unattributed flows may appear as `unknown`.
- `top`, `top-apps`, and `top-proc` are TOTAL-only. `show` includes LAN/WAN
  columns when split data is available. `live` shows current per-interface TOTAL
  rates plus global LAN information and historical summaries.

If nftables or the required capabilities are unavailable, NetMeter continues in
TOTAL-only mode. `netmeter status` reports the split state, including historical
evidence when a current unprivileged nft query is unavailable.

## Install

The primary distribution channel is the release installer. It downloads a
static musl binary for x86_64 or aarch64, verifies `SHA256SUMS`, installs the
binary and man page, and then attempts system setup.

```sh
curl -fsSL https://raw.githubusercontent.com/4imd3v/netmeter/main/install.sh | sh
```

Read the installer first if preferred:

```sh
curl -fsSLO https://raw.githubusercontent.com/4imd3v/netmeter/main/install.sh
sh install.sh --help
sh install.sh
```

System mode needs root (or `sudo`). It attempts to install the systemd service,
create the `netmeter` user, install the nftables table, and start the daemon.
Failures to create the nft table or user are reported and the daemon can still
run in TOTAL-only mode. The installer does not silently download a replacement
asset when a release is missing.

For a per-user install without a system daemon:

```sh
sh install.sh --user --no-daemon
```

This installs to `~/.local/bin` and uses TOTAL-only recording when you run the
daemon yourself. The user-agent notifier is a separate systemd user unit; it is
not installed or enabled by `--user`.

Other channels:

```sh
cargo install netmeter             # crates.io; requires Rust
# .deb / .rpm assets are attached to GitHub Releases.
```

From a source checkout:

```sh
make install   # cargo install --locked --path .
make deploy    # copy the cargo-installed binary to /usr/bin and restart the service
make check     # fmt + clippy -D warnings + tests
```

`make deploy` assumes the service already exists. It does not create the user,
nftables table, systemd unit, or user-agent unit; use `sudo netmeter daemon
install` or the release installer for initial system setup.

## Quick start

```sh
# Initial system setup
sudo netmeter daemon install
netmeter status

# Live view (q or Esc quits, u changes units, +/- changes refresh rate)
netmeter live

# Historical usage
netmeter show --period day
netmeter show --period month --from 2026-09-01 --json

# Rankings and per-app views (TOTAL-only)
netmeter top --by iface
netmeter top-apps --period week
netmeter top-proc --period day
```

`show --from` is an inclusive end-date anchor, not a start date. Use `--iface`
to filter an interface and `--json` for the versioned usage response where
supported.

## Commands

```text
netmeter live [--iface IFACE]
netmeter show --period hour|day|week|month [--from YYYY-MM-DD] [--iface IFACE] [--json]
netmeter top --by iface|day [--limit N]
netmeter status [--json]

netmeter config path
netmeter config get [KEY]
netmeter config set KEY VALUE
netmeter config reset

netmeter export --from YYYY-MM-DD --to YYYY-MM-DD --format csv|json
netmeter import [--from vnstat] [--db VNSTAT_DB]
netmeter completions bash|elvish|fish|powershell|zsh
netmeter manpage

netmeter daemon run|start|stop|restart|install|uninstall
netmeter top-apps --period day|week|month [--limit N] [--json]
netmeter top-proc --period day|week|month
netmeter user-agent [--once]
```

### Configuration

`config set` accepts exactly one `<KEY> <VALUE>` pair per invocation. It does
not accept multiple pairs, and a shell pipeline does not combine commands. These
are invalid:

```sh
netmeter config set budget_gb 2 budget_period day
netmeter config get | set budget_gb 2
```

Use separate commands:

```sh
sudo netmeter config set budget_gb 2
sudo netmeter config set budget_period month
```

The effective configuration is merged in this order:

1. Supported environment variables
2. User config (`~/.config/netmeter/config.toml`)
3. System config (`/etc/netmeter/config.toml`)
4. Built-in defaults

Run as root to write the daemon-visible system config. Run as a normal user to
write the user overlay. `config reset` removes only the current user's overlay.
List-valued keys are easiest to edit directly in TOML; `config set` is a scalar
key/value operation and does not validate every key or enum.

Common keys and defaults:

| Key | Default |
| --- | --- |
| `poll_interval_sec` | `5` |
| `database_path` | `/var/lib/netmeter/netmeter.db` |
| `units` | `binary` |
| `timezone` | `local` |
| `exclude_ifaces` | `lo`, `docker*`, `veth*`, `br-*`, `virbr*` |
| `lan_subnets` | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `fe80::/10`, `fc00::/7` |
| `force_lan_ifaces` | `tailscale0*`, `zt*` |
| `retention_days_raw` | `30` |
| `retention_days_hourly` | `365` |
| `proc_retention_days` | `30` |
| `proc_recording` | `true` |
| `max_rate_mbit` | `10000` |
| `budget_period` | `month` (`day`, `week`, or `month`) |
| `budget_gb` | `0` (disabled) |
| `budget_basis` | `total` (`total` or `wan`) |
| `notify_on_budget` | `true` |

The supported environment overrides are `NETMETER_DATABASE_PATH`,
`NETMETER_POLL_INTERVAL_SEC`, `NETMETER_UNITS`, `NETMETER_BUDGET_GB`, and
`NETMETER_BUDGET_PERIOD`.

### Budgets and notifications

Set a non-zero `budget_gb` and a supported `budget_period` for the daemon-visible
configuration. The separate user-agent process checks budget events every 60
seconds and sends a desktop notification at 80% and 100% when a graphical
session is available.

After installing the user unit, enable it for each desktop user:

```sh
install -Dm644 /usr/share/netmeter/user/netmeter-agent.service \
  ~/.config/systemd/user/netmeter-agent.service
systemctl --user daemon-reload
systemctl --user enable --now netmeter-agent
```

Use `netmeter user-agent --once` for a single check. The unit assumes the
system binary at `/usr/bin/netmeter`; a `--user` installation in
`~/.local/bin` needs a copied or adjusted unit.

### Export and import

`export` requires both dates. It exports raw samples, not rollups, and `--to` is
inclusive. CSV uses this header:

```text
ts,rx,tx,lan_rx,lan_tx,wan_rx,wan_tx
```

`--format json` writes a bare JSON array with those fields. It is not the
versioned `show`, `status`, or `top-apps` API and does not include `api_version`.

`import --from vnstat` runs `vnstat --json` itself; it does not read JSON from
stdin or a JSON file. `--db PATH` is passed to vnStat as its database path. The
importer currently reads daily rows only, stores LAN as zero, and therefore
reports WAN as TOTAL for imported history.

```sh
netmeter export --from 2026-09-01 --to 2026-09-07 --format csv
netmeter import --from vnstat --db /var/lib/vnstat
```

## LAN/WAN and privileges

The daemon runs as the `netmeter` user with `CAP_NET_ADMIN`, `CAP_NET_RAW`,
`CAP_DAC_READ_SEARCH`, and `CAP_SYS_PTRACE`. These support nftables counters,
best-effort packet attribution, and reading the process information needed to
label flows. The systemd unit remains local-only and does not send traffic data
elsewhere.

The nftables table is named `inet netmeter`. If it cannot be read or installed,
the daemon keeps recording TOTAL and marks the split unavailable. Containers,
restricted hosts, and user-mode installs commonly use this fallback.

## Services and packages

The `.deb` and `.rpm` packages contain the binary, systemd units, example config,
and the root man page. Package post-install scripts create the system user but
do not automatically enable the user-agent unit. The release installer performs
the initial system setup and starts `netmeter.service`.

The repository ships one root manual page, `man/netmeter.1`. It documents the
top-level command and its subcommands; there are no separate
`netmeter-live(1)`, `netmeter-show(1)`, or similar pages.

## Development and releases

```sh
make check
./target/debug/netmeter manpage > man/netmeter.1
```

Pushing a `v*` tag runs `.github/workflows/release.yml`, which builds static
musl binaries for x86_64 and aarch64, verifies the archives, publishes the
GitHub Release assets, and optionally publishes to crates.io when
`CARGO_REGISTRY_TOKEN` is set. `.deb` and `.rpm` packages are attached after the
binary release.

To backfill an existing tag, open **Actions -> release -> Run workflow**, select
`main`, and enter the exact existing tag (for example, `v0.2.0`) in the **tag**
field. The workflow validates the tag before building or publishing.

Local static builds need `musl-tools`:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
ldd target/x86_64-unknown-linux-musl/release/netmeter
```
