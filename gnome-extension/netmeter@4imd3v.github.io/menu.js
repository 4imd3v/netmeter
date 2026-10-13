// Shell-only helpers for building popup menus (NOT imported by prefs.js).
import St from 'gi://St';
import Clutter from 'gi://Clutter';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {formatBytes} from './dataSource.js';

const ARROW_DOWN = '\u2193'; // ↓
const ARROW_UP = '\u2191'; // ↑

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

    section(text) {
        return this._add(new PopupMenu.PopupSeparatorMenuItem(text));
    }

    separator() {
        return this._add(new PopupMenu.PopupSeparatorMenuItem());
    }

    // A two-column row: optional icon + label on the left, value right-aligned.
    stat({icon = null, label, value, valueClass = 'netmeter-value', indent = false} = {}) {
        const item = new PopupMenu.PopupBaseMenuItem({
            reactive: false,
            can_focus: false,
            style_class: 'netmeter-row',
        });
        if (indent)
            item.add_style_class_name('netmeter-indent');
        if (icon)
            item.add_child(new St.Icon({icon_name: icon, style_class: 'netmeter-icon', icon_size: 16}));
        item.add_child(new St.Label({
            text: label,
            y_align: Clutter.ActorAlign.CENTER,
        }));
        item.add_child(new St.Label({
            text: value,
            x_expand: true,
            x_align: Clutter.ActorAlign.END,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: valueClass,
        }));
        return this._add(item);
    }

    row(text) {
        return this._add(new PopupMenu.PopupMenuItem(text, {reactive: false}));
    }

    action(text, callback) {
        const item = this._add(new PopupMenu.PopupMenuItem(text));
        item.connect('activate', callback);
        return item;
    }
}

function splitValue(rx, tx, decimal) {
    return `${ARROW_DOWN} ${formatBytes(rx, decimal)}   ${ARROW_UP} ${formatBytes(tx, decimal)}`;
}

export function subtitleFor(model, units) {
    if (!model.installed)
        return _('netmeter not installed');
    if (!model.daemonActive)
        return _('daemon not running');
    if (!model.day)
        return _('no data yet');
    return `${_('Today')} ${formatBytes(model.day.total, units === 'decimal')}`;
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

    if (model.day) {
        builder.section(_('Today'));
        builder.stat({icon: 'go-down-symbolic', label: _('Download'), value: formatBytes(model.day.rx, decimal)});
        builder.stat({icon: 'go-up-symbolic', label: _('Upload'), value: formatBytes(model.day.tx, decimal)});
        builder.stat({label: _('Total'), value: formatBytes(model.day.total, decimal), valueClass: 'netmeter-total'});
        if (model.split) {
            builder.stat({label: _('LAN'), value: splitValue(model.day.lan_rx, model.day.lan_tx, decimal), indent: true});
            builder.stat({label: _('WAN'), value: splitValue(model.day.wan_rx, model.day.wan_tx, decimal), indent: true});
        } else {
            builder.row(`  ${_('TOTAL-only (no LAN/WAN split)')}`);
        }
    } else {
        builder.row(_('No data yet'));
    }

    if (model.month) {
        builder.section(_('This month'));
        builder.stat({label: _('Total'), value: formatBytes(model.month.total, decimal), valueClass: 'netmeter-total'});
        if (model.split)
            builder.stat({label: _('WAN'), value: splitValue(model.month.wan_rx, model.month.wan_tx, decimal), indent: true});
    }

    if (model.topApps.length) {
        builder.section(_('Top apps · today'));
        const limit = extensionObject.getSettings().get_int('apps-limit');
        for (const app of model.topApps.slice(0, limit))
            builder.stat({label: app.app, value: formatBytes(app.total, decimal), indent: true});
    }

    builder.separator();
    const prefsItem = builder.action(_('Preferences'), () => extensionObject.openPreferences());
    prefsItem.visible = Main.sessionMode.allowSettings;
}
