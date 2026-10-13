// Quick Settings tile (system menu) for GNOME 45+.
import GObject from 'gi://GObject';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import {MenuBuilder, fillFromModel, subtitleFor} from './menu.js';

const NetMeterToggle = GObject.registerClass(
class NetMeterToggle extends QuickSettings.QuickMenuToggle {
    constructor(extensionObject) {
        super({
            title: _('NetMeter'),
            subtitle: '…',
            iconName: 'network-transmit-receive-symbolic',
            toggleMode: false,
        });
        this._extensionObject = extensionObject;
        this.menu.setHeader('network-transmit-receive-symbolic', _('NetMeter'), '');
        this._builder = new MenuBuilder(this.menu);
    }

    update(model, units) {
        this.subtitle = subtitleFor(model, units);
        fillFromModel(this._builder, model, units, this._extensionObject);
    }
});

export const NetMeterTile = GObject.registerClass(
class NetMeterTile extends QuickSettings.SystemIndicator {
    constructor(extensionObject) {
        super();
        this._toggle = new NetMeterToggle(extensionObject);
        this.quickSettingsItems.push(this._toggle);
    }

    update(model, units) {
        this._toggle.update(model, units);
    }

    destroy() {
        this._toggle?.destroy();
        this._toggle = null;
        super.destroy();
    }
});
