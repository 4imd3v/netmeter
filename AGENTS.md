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

- **Single writer**: only the daemon writes the DB. Every CLI command opens read-only.
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

## Docs

- Update `README.md` for user-facing changes, `man/netmeter.1` after CLI changes (`netmeter manpage > man/netmeter.1`), `PRD.md` for decision changes.
