# TODO — NetMeter GNOME Shell extension

> Status: **IN PROGRESS** — M1–M3 implemented (skeleton, Quick Settings tile, prefs). M4 (optional `rate --json` + D-Bus) and M5 (packaging) pending. JS syntax-checked with `node --check`, schema compiled, and the data pipeline verified against the live CLI. **Not yet loaded in a nested GNOME session** — that is the next required step.
> guide (gjs.guide/extensions) and the current netmeter code. Target: Debian +
> GNOME 50+. The extension is a **read-only viewer** — a companion to the daemon,
> not a replacement.

## 0. One-line summary

Add a GNOME Shell extension that renders the data the `netmeter` daemon already
records, as a top-bar indicator and a Quick Settings tile. It reuses the CLI's
stable JSON contract, runs unprivileged, makes no network calls, and never writes
the DB. GNOME 50 is a clean target (no breaking changes for GNOME 45+ extensions).

## 1. Why this fits (architecture mapping)

netmeter already separates the **recorder** (Rust daemon, single writer,
`CAP_NET_ADMIN`, system service) from **readers** (CLI + TUIs, read-only,
unprivileged). An extension is just another reader living in the user session.

| netmeter invariant (AGENTS.md) | How the extension honors it |
|---|---|
| Single writer — only the daemon writes | Extension is unprivileged; only calls read-only `netmeter` commands. Never opens the DB for write. |
| WAN = TOTAL − LAN, derived, clamped ≥ 0 | Renders `wan_rx/wan_tx` from `show --json`; never recomputes or stores WAN. |
| LAN/WAN honesty (TOTAL-only fallback) | Reads `status --json`; if the split is unavailable, show TOTAL-only and label it. Never fabricate a split. |
| Privacy: local-only, byte counts only | Reads local DB via CLI (or D-Bus). **No network calls, no telemetry.** |
| Viewers work unprivileged | Extension = viewer; nothing privileged required. |

## 2. Key decisions (resolve before coding)

- **D1 — Data access.** GJS has **no SQLite binding**, so the extension cannot
  open `netmeter.db` directly. Two options:
  - **A (start here): shell out to the CLI** via `Gio.Subprocess` →
    `netmeter show --period day --json`, parse the JSON. Zero daemon changes,
    reuses the `api_version` contract. *Caveat:* the guide's Best Practices say
    "avoid spawning external shell commands… use D-Bus… offload heavy tasks to a
    separate app communicating via D-Bus." Polling a tiny Rust binary every few
    seconds is fine for personal use but not the blessed pattern.
  - **B (if distributing): read-only D-Bus on the daemon.** Daemon takes a
    well-known name on the **system bus**; add a `dbus-1/system.d` policy letting
    the user call *query* methods only (daemon still writes only from its own
    loop). Extension calls via async `Gio.DBusProxy`. Keeps SQLite work out of the
    shell process — the "correct" GNOME architecture.
  - **Recommendation:** start with A; migrate to B only for distribution.
- **D2 — Surfaces.** Panel indicator (`PanelMenu.Button`) **and** Quick Settings
  tile (`QuickSettings.SystemIndicator` + `QuickMenuToggle`). Both are cheap and
  purely *additive* (low breakage risk vs. monkey-patching). Recommend both.
- **D3 — What the panel/tile shows.** There is **no one-shot "current speed" JSON
  command** (`live` is a TUI; `status --json` is daemon/DB state, not a rate).
  Options:
  - **Totals-only first** (recommended): today / this-month totals + budget bar,
    polled every ~30s via `show --json` / `top-apps --json`. Cheap, honest.
  - **Live rate later:** add a small daemon-free `netmeter rate --json` (reuse
    `live.rs` logic: read `/proc/net/dev` + nft, honor `exclude_ifaces`, print
    current ↓/↑ per iface + split availability, unprivileged). Do **not**
    re-implement interface selection in JS — it would drift from `config.rs`.
    (CLI read-only addition → needs the updated binary in PATH; no daemon
    restart.)
- **D4 — Poll interval default:** 30s for totals (configurable via GSettings).
- **D5 — Distribution scope:** personal install vs. ship in the `.deb` vs. EGO
  (EGO requires maintainership + removing the AI-use notice comment).

## 3. File layout

New top-level dir `gnome-extension/` (ships as a zip; the `.deb` can install it
system-wide):

```
gnome-extension/
  netmeter@4imd3v.github.io/
    metadata.json          # uuid, shell-version, settings-schema, session-modes
    extension.js           # entry point: enable()/disable(); wires indicator + tile
    indicator.js           # PanelMenu.Button (top bar) module
    tile.js                # QuickSettings SystemIndicator + MenuToggle module
    dataSource.js          # SHARED fetch+parse (subprocess now, D-Bus later)
    prefs.js               # GTK4/Adw preferences window (separate process)
    stylesheet.css
    schemas/
      org.gnome.shell.extensions.netmeter.gschema.xml
      gschemas.compiled
    locale/                # later (gettext)
```

**Process-isolation rule (from Best Practices):** `dataSource.js` is imported by
both `extension.js` (shell process, uses `St`) and `prefs.js` (GTK process), so it
must **not** import `St`/`Clutter`/`Gtk`/`Gdk`/`Adw`. Keep it pure
`Gio`/`GLib`/JSON.

`metadata.json`:
```json
{
  "uuid": "netmeter@4imd3v.github.io",
  "name": "NetMeter",
  "description": "Local LAN/WAN/TOTAL bandwidth usage in the top bar and system menu.",
  "shell-version": ["50"],
  "url": "https://github.com/4imd3v/netmeter",
  "settings-schema": "org.gnome.shell.extensions.netmeter",
  "session-modes": ["user"]
}
```
(`shell-version` may add `"49"`/`"51"` once tested; ESM is required — GNOME 45+.)

## 4. Data contract (already implemented — do not change the wire format)

Read-only commands the extension calls (all unprivileged, all carry `api_version`):

- `netmeter status --json` → daemon activity, DB location/size, discarded
  samples, and **whether the LAN/WAN split is currently available**. Drives the
  honesty label and the "daemon down" state.
- `netmeter show --period {day|week|month} --json` →
  `{api_version, period, rows:[{ts,date,iface,rx,tx,total,lan_rx,lan_tx,wan_rx,wan_tx}]}`.
- `netmeter top-apps --period {day|week|month} --limit N --json` → per-app totals
  (TOTAL-only; short flows may be `unknown`).

Edge cases the parser must handle: empty DB (`show` returns `rows: []`), missing
`netmeter` binary (subprocess spawn fails → "netmeter not installed"), daemon not
running (`status` reflects it → degrade to a neutral tile). The extension needs
only the `netmeter` binary on `PATH`; config/DB-path resolution stays in the CLI.

## 5. UI

- **Quick Settings tile** (primary, idiomatic on 50): `SystemIndicator` with a
  `QuickMenuToggle`. Title "NetMeter"; subtitle = live figure (e.g. "Today 4.1 GB"
  or, after D3, "↓ 1.2 MB/s ↑ 0.3 MB/s"). `menu.setHeader(...)` + a section with
  LAN / WAN / TOTAL rows and a "Top apps" submenu; a separator + "Preferences"
  action gated by `Main.sessionMode.allowSettings`.
- **Panel indicator** (secondary): `PanelMenu.Button` + `St.Icon`
  (`network-transmit-receive-symbolic` or similar) with a `PopupMenu` mirroring
  the tile.
- Icons via `St.Icon`/`icon_name` — **no emoji** (Best Practices). Budget
  progress via `ui.BarLevel` or an `St.Bin`, not ASCII bars.

## 6. Lifecycle rules (Best-Practices checklist — non-negotiable)

- **Never block the shell thread.** All fetches are async (subprocess
  `communicate` callback, or async D-Bus). A synchronous read inside gnome-shell
  freezes the whole UI — the #1 failure mode.
- Remove the previous timeout source **before** creating a new one, and keep the
  removal adjacent to the creation. Destroy the source in `disable()`.
- `destroy()` order: sources → disconnect signals → release children →
  `super.destroy()` last. No `_destroyed`/`_enabled` boolean guards; null the
  instance after destroy.
- Settings schema id lives in `metadata.json`; read with `this.getSettings()`.
- Keep `enable()`/`disable()` adjacent; keep the entry point small; split logic
  into single-responsibility modules.
- If generated with AI for personal use, keep the required notice comment; remove
  it only if you understand the JS and intend to publish.

## 7. Testing (Debian, GNOME 50)

- Nested Wayland session: `dbus-run-session gnome-shell --devkit --wayland`
  (GNOME 49+ needs the mutter devkit — **confirm the Debian package name**; the
  guide lists Ubuntu `mutter-dev-bin`). Then inside the nested session:
  `gnome-extensions enable netmeter@4imd3v.github.io`, and watch
  `journalctl -f -o cat /usr/bin/gnome-shell`.
- JS engine caches modules — repeat enable (or restart the nested shell) after
  every code change.
- Scenarios to verify: (a) daemon running → totals render; (b) daemon stopped →
  neutral tile, no crash; (c) `netmeter` binary absent → "not installed" state;
  (d) split unavailable → TOTAL-only + honesty label; (e) empty DB → `rows: []`.
- `make check` stays green (no Rust changes unless D3 adds `rate --json`, which
  needs its own small test).

## 8. Distribution

- Zip the `netmeter@4imd3v.github.io/` dir.
- Personal: `~/.local/share/gnome-shell/extensions/`.
- `.deb`: add the dir under `/usr/share/gnome-shell/extensions/` and compile the
  schema in `packaging/debian/postinst`; note the dependency on the `netmeter`
  binary + daemon. Additive — does **not** touch `INSTALL_BIN`/units.
- EGO: optional; requires maintainership and removing the AI notice.

## 9. Milestones

- **M1 — skeleton:** ✅ metadata + `extension.js` + `dataSource.js` + `indicator.js` (panel indicator, today/24h TOTAL), 30s poll, full lifecycle discipline.
- **M2 — Quick Settings tile:** ✅ `tile.js` (`SystemIndicator` + `QuickMenuToggle`), menu with LAN/WAN/TOTAL + top apps; `status`-driven honesty + daemon-down handling (`menu.js`).
- **M3 — prefs:** ✅ GSettings schema + `prefs.js` (poll interval, binary path, top-apps count, units).
- **M4 — optional live rate:** ⬜ `netmeter rate --json` (reuse `live.rs`) + fast poll; migrate `dataSource.js` to D-Bus if distributing.
- **M5 — packaging + docs:** ⬜ deb asset + postinst schema compile, zip release. (README section + PRD Q9 done.)

### Files written (`gnome-extension/netmeter@4imd3v.github.io/`)
`metadata.json`, `extension.js`, `dataSource.js` (shared, no St/Gtk), `menu.js`,
`indicator.js`, `tile.js`, `prefs.js`, `stylesheet.css`,
`schemas/org.gnome.shell.extensions.netmeter.gschema.xml` (+ compiled), and
`gnome-extension/install-dev.sh`.

### Verified here (headless)
- `node --check` passes on all `.js` (ESM).
- `glib-compile-schemas` succeeds; `metadata.json` + schema XML well-formed.
- Parser field access matches real `netmeter status/show/top-apps --json` output.
- `buildModel` + `formatBytes` + aggregation exercised in Node against live data
  (24h total, WAN split, month total, top apps, empty-DB + missing-binary cases).

### Design pass (2026-10-14)
Replaced the plain text dump in `menu.js` with aligned two-column rows: directional
icons (`go-down/up-symbolic`), sectioned headers (`PopupSeparatorMenuItem`),
label-left / value-right layout, bold Total, and indented LAN/WAN + per-app
rows. Shared by the panel menu and the Quick Settings tile. Styling in
`stylesheet.css` (`.netmeter-row`, `.netmeter-total`, `.netmeter-indent`).
Pushed to the live install; loads ACTIVE with no JS errors. Needs a shell
re-login to guarantee the visual refresh (module cache).

New menu layout:
```
── Last 24 hours ──
[↓] Download         407.9 MiB
[↑] Upload            19.3 MiB
    Total            427.3 MiB   (bold)
      LAN     ↓ 407.9 MiB  ↑ 19.3 MiB
      WAN     ↓ 407.9 MiB  ↑ 19.3 MiB
── This month ──
    Total             45.0 GiB   (bold)
── Top apps · today ──
      curl            79.7 MiB
      helium          21.0 MiB
──────────────────
Preferences
```

### Panel usage label (2026-10-14) — ✅ verified working
New GSettings key `show-panel-label` (bool, default **true**). When on, the top-bar
`PanelMenu.Button` shows the last-24h total as a `St.Label` beside the icon
(`indicator.js::setPanelLabel`); `extension.js` keeps it in sync on every poll and
reacts instantly to the toggle via `changed::show-panel-label`. Prefs row:
"Show usage in panel" (`Adw.SwitchRow`).

**Fix that made it render:** a `PanelMenu.Button` paints effectively one child, so
the icon + label must live inside a single horizontal `St.BoxLayout` (added first
as the button's only child). Confirmed live after re-login.

### Still to do
- **Recognize + load in GNOME (BLOCKED on shell restart).** Attempted live against the running GNOME 50.5 shell: `gnome-extensions install` of a root-files zip succeeds (rc=0, schema auto-compiled) and files land in `~/.local/share/gnome-shell/extensions/<uuid>/`, but the running shell does **not** rescan the dir — netmeter is absent from the shell's authoritative `ListExtensions` with **no** load error. ext4 (inotify OK), valid metadata/schema ⇒ it's a rescan/monitor gap, not a bug in the extension. **Fix: log out/in (Wayland) to restart the shell**, then `gnome-extensions enable netmeter@4imd3v.github.io`, then check the panel/tile + `journalctl --user -o cat | grep -i netmeter`.
  - Gotcha: raw `cp` (`install-dev.sh`) also needs a restart to be recognized.
  - Gotcha: the zip for `gnome-extensions install` must have the files at the zip **root** (uuid comes from `metadata.json`), not wrapped in a `<uuid>/` subdir.
- Confirm the Debian mutter-devkit package name for nested testing (§10).
- Watch-items that only a live session can catch: `this.subtitle` updates on the
  QuickToggle, `menu.setHeader`, `addExternalIndicator`, `toggleMode:false`
  click-to-open, and `communicate_utf8_finish` returning `[ok, stdout]`.

## 10. Open questions

- Confirm the Debian GNOME-50 mutter devkit package for nested testing.
- Decide D1 (subprocess vs. D-Bus) and D3 (totals-only vs. add `rate --json`)
  before M1.
- Decide D5 distribution scope (drives whether the D-Bus migration is in-scope).
