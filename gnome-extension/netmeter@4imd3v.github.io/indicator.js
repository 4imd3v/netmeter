// Top-bar panel indicator (icon + optional usage label + popup menu).
import St from 'gi://St';
import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import {MenuBuilder, fillFromModel} from './menu.js';

export const NetMeterIndicator = GObject.registerClass(
class NetMeterIndicator extends PanelMenu.Button {
    constructor() {
        super(0.0, 'NetMeter', false);

        // A panel button paints a single child, so put the icon and the usage
        // label inside one horizontal box.
        this._box = new St.BoxLayout({style_class: 'netmeter-panel-box'});

        this._icon = new St.Icon({
            icon_name: 'network-transmit-receive-symbolic',
            style_class: 'system-status-icon',
        });
        this._box.add_child(this._icon);

        this._label = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'netmeter-panel-label',
        });
        this._label.visible = false;
        this._box.add_child(this._label);

        this.add_child(this._box);
        this._builder = new MenuBuilder(this.menu);
    }

    // Show/hide the usage number beside the icon (user-toggleable).
    setPanelLabel(text, visible) {
        this._label.text = text;
        this._label.visible = visible && text.length > 0;
    }

    update(model, units, extensionObject) {
        fillFromModel(this._builder, model, units, extensionObject);
    }
});
