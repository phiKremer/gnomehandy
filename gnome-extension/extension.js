import St from 'gi://St';

import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';

import { BluetoothStatus } from './bluetooth-status.js';
import { addDialer } from './dialer.js';
import { IncomingCallOverlay } from './incoming-call-overlay.js';
import { subscribeToRustSignal, unsubscribeFromRustSignal } from './rust-service.js';

export default class HandyExtension extends Extension {
    enable() {
        console.log('=== [Handy Extension] AKTIVIERT ===');

        this._indicator = new PanelMenu.Button(0.0, this.metadata.name);
        this._indicator.add_child(new St.Icon({
            icon_name: 'call-start-symbolic',
            style_class: 'system-status-icon',
        }));

        this._bluetoothStatus = new BluetoothStatus(this._indicator.menu);
        this._bluetoothStatus.start();

        this._callOverlay = new IncomingCallOverlay(this.getSettings());
        addDialer(this._indicator.menu, () => this.openPreferences(), (callId, number) => {
            this._callOverlay.showOutgoingCall(callId, number);
        });
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        this._incomingCallSignalId = subscribeToRustSignal('IncomingCall',
            (_connection, _sender, _path, _interface, _signal, parameters) => {
                this._callOverlay.show(parameters);
            });
        this._outgoingCallSignalId = subscribeToRustSignal('OutgoingCall',
            (_connection, _sender, _path, _interface, _signal, parameters) => {
                this._callOverlay.showOutgoing(parameters);
            });
        this._callEndedSignalId = subscribeToRustSignal('CallEnded',
            (_connection, _sender, _path, _interface, _signal, parameters) => {
                this._callOverlay.hide(parameters);
            });
        this._callActiveSignalId = subscribeToRustSignal('CallActive',
            (_connection, _sender, _path, _interface, _signal, parameters) => {
                this._callOverlay.setActive(parameters);
            });
        this._audioStateSignalId = subscribeToRustSignal('AudioState',
            (_connection, _sender, _path, _interface, _signal, parameters) => {
                const [callId, status] = parameters.deepUnpack();
                this._callOverlay.setAudioStatus(callId, status);
            });
        this._stageSizeIds = ['notify::width', 'notify::height'].map(signal =>
            global.stage.connect(signal, () => this._callOverlay.resize()));
    }

    disable() {
        console.log('=== [Handy Extension] DEAKTIVIERT ===');

        for (const signalId of this._stageSizeIds ?? [])
            global.stage.disconnect(signalId);
        this._stageSizeIds = [];

        unsubscribeFromRustSignal(this._incomingCallSignalId);
        unsubscribeFromRustSignal(this._outgoingCallSignalId);
        unsubscribeFromRustSignal(this._callEndedSignalId);
        unsubscribeFromRustSignal(this._callActiveSignalId);
        unsubscribeFromRustSignal(this._audioStateSignalId);
        this._incomingCallSignalId = 0;
        this._outgoingCallSignalId = 0;
        this._callEndedSignalId = 0;
        this._callActiveSignalId = 0;
        this._audioStateSignalId = 0;

        this._callOverlay?.destroy();
        this._callOverlay = null;
        this._bluetoothStatus?.destroy();
        this._bluetoothStatus = null;
        this._indicator?.destroy();
        this._indicator = null;
    }
}
