// Generated with AI for personal use.
// Do NOT upload to extensions.gnome.org (EGO) unless you understand JavaScript
// and can maintain this code.
import GLib from 'gi://GLib';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {NetMeterIndicator} from './indicator.js';
import {NetMeterTile} from './tile.js';
import {fetchAll, buildModel, formatBytes} from './dataSource.js';

export default class NetMeter extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._model = null;

        this._tile = new NetMeterTile(this);
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._tile);

        this._indicator = new NetMeterIndicator();
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        this._settingsChangedId = this._settings.connect('changed', () => this._startTimer());
        this._panelLabelChangedId = this._settings.connect('changed::show-panel-label', () => this._updatePanelLabel(this._model));
        this._startTimer();
        this._poll();
    }

    disable() {
        if (this._timeoutId) {
            GLib.Source.remove(this._timeoutId);
            this._timeoutId = 0;
        }
        if (this._settingsChangedId) {
            this._settings.disconnect(this._settingsChangedId);
            this._settingsChangedId = 0;
        }
        if (this._panelLabelChangedId) {
            this._settings.disconnect(this._panelLabelChangedId);
            this._panelLabelChangedId = 0;
        }
        this._indicator?.destroy();
        this._indicator = null;
        this._tile?.destroy();
        this._tile = null;
        this._settings = null;
        this._model = null;
    }

    _startTimer() {
        if (this._timeoutId) {
            GLib.Source.remove(this._timeoutId);
            this._timeoutId = 0;
        }
        const seconds = Math.max(5, this._settings.get_int('poll-interval'));
        this._timeoutId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, seconds, () => {
            this._poll();
            return GLib.SOURCE_CONTINUE;
        });
    }

    async _poll() {
        const binary = this._settings.get_string('binary-path') || 'netmeter';
        const units = this._settings.get_string('units');
        const data = await fetchAll(binary);
        this._model = buildModel(binary, data);
        this._tile?.update(this._model, units);
        this._indicator?.update(this._model, units, this);
        this._updatePanelLabel(this._model);
    }

    // Keep the panel usage number in sync with the model and the user toggle.
    _updatePanelLabel(model) {
        if (!this._indicator)
            return;
        const show = this._settings.get_boolean('show-panel-label');
        const decimal = this._settings.get_string('units') === 'decimal';
        const ready = model && model.installed && model.daemonActive && model.day;
        const text = ready ? formatBytes(model.day.total, decimal) : '';
        this._indicator.setPanelLabel(text, show);
    }
}
