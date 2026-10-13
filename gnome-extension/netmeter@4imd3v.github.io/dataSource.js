// Shared by extension.js (shell process) and prefs.js (GTK process).
// Process isolation: MUST NOT import St/Clutter/Gtk/Gdk/Adw here.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

export function binaryExists(binary) {
    if (!binary)
        return false;
    if (binary.includes('/'))
        return GLib.file_test(binary, GLib.FileTest.EXISTS);
    return GLib.find_program_in_path(binary) !== null;
}

// Run `netmeter <args> --json` and resolve the parsed object. Async: never
// blocks the shell main loop. Rejects on spawn failure / non-zero exit.
function runJson(binary, args) {
    return new Promise((resolve, reject) => {
        let proc;
        try {
            proc = Gio.Subprocess.new(
                [binary, ...args],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_SILENCE
            );
        } catch (error) {
            reject(new Error(`spawn: ${error.message}`));
            return;
        }
        proc.communicate_utf8_async(null, null, (process, result) => {
            try {
                const [ok, stdout] = process.communicate_utf8_finish(result);
                if (!ok) {
                    reject(new Error('netmeter exited non-zero'));
                    return;
                }
                resolve(JSON.parse(stdout));
            } catch (error) {
                reject(new Error(`read: ${error.message}`));
            }
        });
    });
}

const safe = (binary, args) => runJson(binary, args).catch(() => null);

// Concurrent read-only queries; each resolves to parsed JSON or null.
export function fetchAll(binary) {
    return Promise.all([
        safe(binary, ['status', '--json']),
        safe(binary, ['today', '--json']),
        safe(binary, ['show', '--period', 'hour', '--json']),
        safe(binary, ['show', '--period', 'month', '--json']),
        safe(binary, ['top-apps', '--period', 'day', '--json']),
    ]).then(([status, today, hourly, monthly, apps]) => ({status, today, hourly, monthly, apps}));
}

export function sumRows(rows) {
    const totals = {rx: 0, tx: 0, total: 0, lan_rx: 0, lan_tx: 0, wan_rx: 0, wan_tx: 0};
    for (const row of rows || []) {
        totals.rx += row.rx || 0;
        totals.tx += row.tx || 0;
        totals.total += row.total || 0;
        totals.lan_rx += row.lan_rx || 0;
        totals.lan_tx += row.lan_tx || 0;
        totals.wan_rx += row.wan_rx || 0;
        totals.wan_tx += row.wan_tx || 0;
    }
    return totals;
}

export function lastRow(rows) {
    return rows && rows.length ? rows[rows.length - 1] : null;
}

// Today's date in the shell's local timezone (matches netmeter's local-time
// bucket labels). Assumes netmeter's timezone is "local" (the default).
function todayLocalStr() {
    const d = new Date();
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

export function buildModel(binary, {status, today, hourly, monthly, apps}) {
    // Prefer the fresh server-side `today` sum; fall back to filtering the
    // hourly rows by today's local date when the command is unavailable.
    let day = null;
    if (today && typeof today.total === 'number') {
        day = {
            rx: today.rx, tx: today.tx, total: today.total,
            lan_rx: today.lan_rx, lan_tx: today.lan_tx,
            wan_rx: today.wan_rx, wan_tx: today.wan_tx,
        };
    } else if (hourly && hourly.rows) {
        day = sumRows(hourly.rows.filter(r => (r.date || '').startsWith(todayLocalStr())));
    }
    return {
        installed: binaryExists(binary),
        daemonActive: Boolean(status && status.daemon_active),
        split: Boolean(status && status.nft_split),
        version: status ? status.version : null,
        day,
        month: monthly && monthly.rows ? lastRow(monthly.rows) : null,
        topApps: apps && apps.rows ? apps.rows : [],
    };
}

export function formatBytes(bytes, decimal = false) {
    if (bytes == null || Number.isNaN(bytes))
        return '—';
    const base = decimal ? 1000 : 1024;
    const units = decimal
        ? ['B', 'kB', 'MB', 'GB', 'TB']
        : ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
    let value = Math.abs(bytes);
    let index = 0;
    while (value >= base && index < units.length - 1) {
        value /= base;
        index++;
    }
    const decimals = index === 0 ? 0 : (value < 10 ? 2 : 1);
    const sign = bytes < 0 ? '-' : '';
    return `${sign}${value.toFixed(decimals)} ${units[index]}`;
}
