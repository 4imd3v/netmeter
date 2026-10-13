// Top-bar panel indicator (icon + popup menu).
import St from 'gi://St';
import GObject from 'gi://GObject';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import {MenuBuilder, fillFromModel} from './menu.js';

export const NetMeterIndicator = GObject.registerClass(
class NetMeterIndicator extends PanelMenu.Button {
    constructor() {
        super(0.0, 'NetMeter', false);
        this._icon = new St.Icon({
            icon_name: 'network-transmit-receive-symbolic',
            style_class: 'system-status-icon',
        });
        this.add_child(this._icon);
        this._builder = new MenuBuilder(this.menu);
    }

    update(model, units, extensionObject) {
        fillFromModel(this._builder, model, units, extensionObject);
    }
});
