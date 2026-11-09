import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import { callRust } from './rust-service.js';

export function addDialer(menu, openSettings, onDialStarted) {
    const section = new PopupMenu.PopupMenuSection();
    const entryItem = new PopupMenu.PopupBaseMenuItem({ can_focus: true });
    const entryRow = new St.BoxLayout({ x_expand: true, style_class: 'handy-dialer-row' });
    const entry = new St.Entry({
        hint_text: 'Telefonnummer',
        can_focus: true,
        x_expand: true,
        style_class: 'handy-dialer-entry',
    });
    const clear = new St.Button({ label: '⌫', can_focus: true, style_class: 'button' });
    clear.connect('clicked', () => entry.set_text(entry.get_text().slice(0, -1)));
    entryRow.add_child(entry);
    entryRow.add_child(clear);
    entryItem.add_child(entryRow);
    section.addMenuItem(entryItem);

    for (const rowKeys of [['1', '2', '3'], ['4', '5', '6'], ['7', '8', '9'], ['*', '0', '#']]) {
        const rowItem = new PopupMenu.PopupBaseMenuItem({ can_focus: false });
        const row = new St.BoxLayout({ x_expand: true, style_class: 'handy-dialer-row' });
        for (const key of rowKeys) {
            const button = new St.Button({ label: key, x_expand: true, can_focus: true, style_class: 'button' });
            button.connect('clicked', () => entry.set_text(`${entry.get_text()}${key}`));
            row.add_child(button);
        }
        rowItem.add_child(row);
        section.addMenuItem(rowItem);
    }

    const actions = new PopupMenu.PopupBaseMenuItem({ can_focus: false });
    const actionRow = new St.BoxLayout({ x_expand: true, style_class: 'handy-dialer-row' });
    const dial = new St.Button({ label: 'Wählen', x_expand: true, can_focus: true, style_class: 'button suggested-action' });
    dial.connect('clicked', () => dialNumber(entry, dial, onDialStarted));
    const settings = new St.Button({ label: 'Einstellungen', x_expand: true, can_focus: true, style_class: 'button' });
    settings.connect('clicked', openSettings);
    actionRow.add_child(dial);
    actionRow.add_child(settings);
    actions.add_child(actionRow);
    section.addMenuItem(actions);
    menu.addMenuItem(section);
    menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
}

async function dialNumber(entry, button, onDialStarted) {
    const number = entry.get_text().trim();
    if (!number) {
        Main.notify('GNOME Handy', 'Bitte eine Telefonnummer eingeben.');
        return;
    }

    button.reactive = false;
    try {
        const reply = await callRust('DialNumber', new GLib.Variant('(s)', [number]));
        const [callId] = reply.deepUnpack();
        onDialStarted?.(callId, number);
        Main.notify('GNOME Handy', `Wähle ${number} …`);
    } catch (error) {
        console.error(`[GNOME Handy] Anruf konnte nicht gestartet werden: ${error.message}`);
        Main.notify('GNOME Handy', `Anruf konnte nicht gestartet werden: ${error.message}`);
    } finally {
        button.reactive = true;
    }
}
