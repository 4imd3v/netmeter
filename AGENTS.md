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
- Small diffs. Fewest files. No speculative abstractions, no new crates without a stated reason.
- Don't touch `/etc`, `/usr/bin`, systemd, or nft rules on the dev machine unless asked. Never `sudo` in scripts.

## Architecture invariants (do not break)

- **Single writer**: only the daemon writes the DB. Every CLI command opens read-only; the GNOME extension is a read-only viewer too — it shells out to the CLI and never opens the DB for write.
- **WAN is derived**: `WAN = TOTAL − LAN`, clamped ≥ 0. TOTAL comes from `/proc/net/dev`, LAN from the nft table. Never store WAN.
- **Time**: store UTC epoch seconds; bucket and display in local time unless `timezone = "utc"`.
- **LAN/WAN honesty**: per-interface LAN split is impossible from kernel counters — LAN is global (nft) pro-rated. TOTAL-only fallback (no nft/perms) must keep working, surfaced via `status`.
- **Counter hygiene** (`daemon.rs::delta`): distinguish wrap vs reset vs spike. Never remove the sanity filter.
- **Privacy**: local-only. No network calls, no telemetry. DB holds byte counts + iface/comm names only — never payloads, hosts, URLs.
- **Privileges**: daemon runs as `netmeter` user + ambient caps (see `packaging/netmeter.service`). Viewers must work unprivileged.

## Config & schema discipline

- Layers: flags > `NETMETER_*` env > user `~/.config` > `/etc/netmeter/config.toml` > defaults. Daemon-visible keys live in `/etc`; `config set` as root writes there.
- JSON output (`--json`) carries `api_version` — bump it only on breaking field changes.
- Schema changes are additive (`CREATE TABLE IF NOT EXISTS`, new nullable columns/keys). Old DBs must keep opening.

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
- **Binary coupling:** the extension resolves `netmeter` through the shell's PATH (`~/.cargo/bin` precedes `/usr/bin`), so `make install` (no sudo) is enough for the extension to pick up new CLI flags; `make deploy` (sudo) is only for `/usr/bin`/daemon. When the extension uses a new CLI flag, keep a fallback for older binaries.

## Release & distribution

- Primary channel is `install.sh` (curl|sh) against GitHub Releases; tags `v*` trigger `.github/workflows/release.yml` (musl tarballs + `SHA256SUMS` + `.deb`/`.rpm`; `cargo publish` only when `CARGO_REGISTRY_TOKEN` is set). PRs run `make check` + `shellcheck install.sh` (`.github/workflows/ci.yml`).
- `src/main.rs::INSTALL_BIN` (`/usr/bin/netmeter`) must match the units' `ExecStart` — the test enforces it. `daemon install` copies the running binary there so cargo/curl installs work; don't change one without the other.

## Docs

- Update `README.md` for user-facing changes, `man/netmeter.1` after CLI changes (`netmeter manpage > man/netmeter.1`), `PRD.md` for decision changes.
