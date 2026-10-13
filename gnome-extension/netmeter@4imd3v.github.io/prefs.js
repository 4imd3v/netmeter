// Runs in the GTK4/Adwaita preferences process.
import Gtk from 'gi://Gtk';
import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

export default class NetMeterPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        const page = new Adw.PreferencesPage({
            title: 'General',
            icon_name: 'preferences-system-symbolic',
        });
        const group = new Adw.PreferencesGroup({title: 'NetMeter'});

        const pollRow = new Adw.SpinRow({
            title: 'Poll interval (seconds)',
            subtitle: 'How often to query netmeter',
            adjustment: new Gtk.Adjustment({
                lower: 5, upper: 600, step_increment: 1, page_increment: 10,
                value: settings.get_int('poll-interval'),
            }),
        });
        pollRow.connect('notify::value', () => settings.set_int('poll-interval', pollRow.value));
        group.add(pollRow);

        const binaryRow = new Adw.EntryRow({title: 'netmeter binary'});
        binaryRow.text = settings.get_string('binary-path');
        binaryRow.connect('changed', () => settings.set_string('binary-path', binaryRow.text));
        group.add(binaryRow);

        const appsRow = new Adw.SpinRow({
            title: 'Top apps to show',
            adjustment: new Gtk.Adjustment({
                lower: 1, upper: 20, step_increment: 1,
                value: settings.get_int('apps-limit'),
            }),
        });
        appsRow.connect('notify::value', () => settings.set_int('apps-limit', appsRow.value));
        group.add(appsRow);

        const unitsRow = new Adw.ComboRow({
            title: 'Units',
            model: new Gtk.StringList({strings: ['binary', 'decimal']}),
        });
        unitsRow.selected = settings.get_string('units') === 'decimal' ? 1 : 0;
        unitsRow.connect('notify::selected', () =>
            settings.set_string('units', unitsRow.selected === 1 ? 'decimal' : 'binary'));
        group.add(unitsRow);

        page.add(group);
        window.add(page);
    }
}
