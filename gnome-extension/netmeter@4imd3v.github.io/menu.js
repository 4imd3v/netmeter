// Shell-only helpers for building popup menus (NOT imported by prefs.js).
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {formatBytes} from './dataSource.js';

export class MenuBuilder {
    constructor(menu) {
        this._menu = menu;
        this._items = [];
    }

    clear() {
        for (const item of this._items)
            item.destroy();
        this._items = [];
    }

    _add(item) {
        this._items.push(item);
        this._menu.addMenuItem(item);
        return item;
    }

    row(text) {
        return this._add(new PopupMenu.PopupMenuItem(text, {reactive: false}));
    }

    action(text, callback) {
        const item = this._add(new PopupMenu.PopupMenuItem(text));
        item.connect('activate', callback);
        return item;
    }

    separator() {
        return this._add(new PopupMenu.PopupSeparatorMenuItem());
    }
}

function usageRows(builder, title, totals, split, decimal) {
    builder.row(`${title}: ${formatBytes(totals.total, decimal)}`);
    builder.row(`  ↓ ${formatBytes(totals.rx, decimal)}     ↑ ${formatBytes(totals.tx, decimal)}`);
    if (split) {
        builder.row(`  LAN  ↓ ${formatBytes(totals.lan_rx, decimal)}     ↑ ${formatBytes(totals.lan_tx, decimal)}`);
        builder.row(`  WAN  ↓ ${formatBytes(totals.wan_rx, decimal)}     ↑ ${formatBytes(totals.wan_tx, decimal)}`);
    } else {
        builder.row('  TOTAL-only (no LAN/WAN split)');
    }
}

export function subtitleFor(model, units) {
    if (!model.installed)
        return _('netmeter not installed');
    if (!model.daemonActive)
        return _('daemon not running');
    if (!model.day)
        return _('no data yet');
    return `${_('24h')} ${formatBytes(model.day.total, units === 'decimal')}`;
}

export function fillFromModel(builder, model, units, extensionObject) {
    builder.clear();
    const decimal = units === 'decimal';

    if (!model.installed) {
        builder.row(_('netmeter not installed'));
        builder.action(_('Preferences'), () => extensionObject.openPreferences());
        return;
    }

    if (!model.daemonActive) {
        builder.row(_('Daemon not running'));
        builder.row('  sudo netmeter daemon install');
        builder.action(_('Preferences'), () => extensionObject.openPreferences());
        return;
    }

    if (model.day)
        usageRows(builder, _('Last 24 hours'), model.day, model.split, decimal);
    else
        builder.row(_('No data yet'));

    if (model.month)
        usageRows(builder, _('This month'), model.month, model.split, decimal);

    if (model.topApps.length) {
        builder.separator();
        builder.row(_('Top apps · today'));
        const limit = extensionObject.getSettings().get_int('apps-limit');
        for (const app of model.topApps.slice(0, limit))
            builder.row(`  ${app.app}  ${formatBytes(app.total, decimal)}`);
    }

    builder.separator();
    const prefsItem = builder.action(_('Preferences'), () => extensionObject.openPreferences());
    prefsItem.visible = Main.sessionMode.allowSettings;
}
