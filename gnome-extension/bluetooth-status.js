import GLib from 'gi://GLib';

import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import { callRust } from './rust-service.js';

export class BluetoothStatus {
    constructor(menu) {
        this._menu = menu;
        this._items = {
            phone: this._addStatusItem(menu, 'Verbundenes Gerät: wird geladen…'),
            ringing: this._addStatusItem(menu, 'Klingeln auf: wird geladen…'),
            output: this._addStatusItem(menu, 'Ausgabe: wird geladen…'),
            microphone: this._addStatusItem(menu, 'Mikrofon: wird geladen…'),
        };
        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._refreshInFlight = false;
        this._errorLogged = false;
        this._destroyed = false;
        this._sourceId = 0;
        this._openSignalId = menu.connect('open-state-changed', (_menu, isOpen) => {
            this._setMenuOpen(isOpen);
        });
    }

    _addStatusItem(menu, text) {
        const item = new PopupMenu.PopupMenuItem(text, { reactive: false, can_focus: false });
        menu.addMenuItem(item);
        return item;
    }

    start() {
        if (this._menu.isOpen)
            this._setMenuOpen(true);
    }

    destroy() {
        this._destroyed = true;
        if (this._openSignalId) {
            this._menu.disconnect(this._openSignalId);
            this._openSignalId = 0;
        }
        if (this._sourceId) {
            GLib.source_remove(this._sourceId);
            this._sourceId = 0;
        }
        this._items = {};
    }

    _setMenuOpen(isOpen) {
        if (this._sourceId) {
            GLib.source_remove(this._sourceId);
            this._sourceId = 0;
        }
        if (!isOpen || this._destroyed)
            return;

        this._refresh();
        this._sourceId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 15, () => {
            if (this._destroyed)
                return GLib.SOURCE_REMOVE;
            this._refresh();
            return GLib.SOURCE_CONTINUE;
        });
    }

    async _refresh() {
        if (this._refreshInFlight || this._destroyed)
            return;
        this._refreshInFlight = true;
        try {
            const replies = await Promise.allSettled([
                callRust('GetBluetoothStatus'),
                callRust('GetAudioOutputSelection'),
                callRust('GetConversationOutputSelection'),
                callRust('GetMicrophoneSelection'),
                callRust('ListAudioOutputs'),
                callRust('ListAudioInputs'),
            ]);
            if (this._destroyed)
                return;

            const [status, ringing, output, microphone, outputs, inputs] = replies;
            if (status.status === 'fulfilled') {
                const [connected, phoneName] = status.value.deepUnpack();
                this._items.phone.label.set_text(connected
                    ? `Verbundenes Gerät: ${phoneName}`
                    : 'Verbundenes Gerät: Nicht verbunden');
                this._errorLogged = false;
            } else {
                this._items.phone.label.set_text('GNOME Handy-Dienst: Nicht erreichbar');
                if (!this._errorLogged) {
                    console.error(`[Handy Extension] Rust-Dienst nicht erreichbar: ${status.reason.message}`);
                    this._errorLogged = true;
                }
            }

            this._setSelectionLabel(
                this._items.ringing,
                'Klingeln auf',
                ringing,
                outputs,
                true,
            );
            this._setSelectionLabel(
                this._items.output,
                'Ausgabe',
                output,
                outputs,
                false,
            );
            this._setSelectionLabel(
                this._items.microphone,
                'Mikrofon',
                microphone,
                inputs,
                false,
            );
        } catch (error) {
            if (this._destroyed)
                return;
            console.error(`[Handy Extension] Gerätestatus konnte nicht angezeigt werden: ${error.message}`);
        } finally {
            this._refreshInFlight = false;
        }
    }

    _setSelectionLabel(item, title, selectionReply, deviceReply, allowAll) {
        if (selectionReply.status !== 'fulfilled' || deviceReply.status !== 'fulfilled') {
            item.label.set_text(`${title}: nicht verfügbar`);
            return;
        }

        const [selection] = selectionReply.value.deepUnpack();
        const [devices] = deviceReply.value.deepUnpack();
        const description = allowAll && selection === 'all'
            ? 'Alle Geräte'
            : this._description(devices, selection, 'Systemstandard');
        item.label.set_text(`${title}: ${description}`);
    }

    _description(devices, selected, fallback) {
        return devices.find(([name]) => name === selected)?.[1] ?? fallback;
    }
}
