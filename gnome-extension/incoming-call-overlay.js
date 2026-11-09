import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import Clutter from 'gi://Clutter';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import { callRust } from './rust-service.js';

export class IncomingCallOverlay {
    constructor(settings) {
        this._settings = settings;
        this._currentCallId = null;
        this._view = null;
        this._actor = null;
        this._addedToChrome = false;
        this._ringtoneGeneration = 0;
        this._ringtoneOperation = Promise.resolve();
        this._declineInFlight = false;
        this._dtmfVisible = false;
        this._isActiveCall = false;
        this._isOutgoingCall = false;
    }

    show(parameters) {
        const [callId, callerName, phoneNumber, details] = parameters.deepUnpack();
        this._showCall(callId, callerName, phoneNumber, details, false);
    }

    showOutgoing(parameters) {
        const [callId, phoneNumber] = parameters.deepUnpack();
        this._showCall(callId, '', phoneNumber, [], true);
    }

    showOutgoingCall(callId, phoneNumber) {
        this._showCall(callId, '', phoneNumber, [], true);
    }

    _showCall(callId, callerName, phoneNumber, details, outgoing) {
        const isNewCall = callId !== this._currentCallId;
        if (isNewCall)
            this._stopRingtone();
        if (!this._view) {
            this._view = createIncomingCallOverlay({
                answer: () => this._answerCall(),
                reject: () => this._rejectCall(),
                hangup: () => this._hangUpCall(),
                toggleDtmf: () => this._toggleDtmf(),
                sendDtmf: tone => this._sendDtmf(tone),
            });
            this._actor = this._view.actor;
        }

        this._currentCallId = callId;
        this._isOutgoingCall = outgoing;
        this._view.name.set_text(callerName || phoneNumber || 'Unbekannte Nummer');
        this._view.number.set_text(phoneNumber);
        this._view.number.visible = Boolean(phoneNumber);

        const detailText = details
            .map(([label, value]) => `${label}: ${value}`)
            .filter(value => value.trim().length > 0)
            .join(' · ');
        this._view.details.set_text(detailText);
        this._view.details.visible = detailText.length > 0;
        if (isNewCall)
            this._view.setAudioStatus('');

        if (!this._addedToChrome) {
            Main.layoutManager.addChrome(this._actor, {
                affectsStruts: false,
                affectsInputRegion: false,
                trackFullscreen: true,
            });
            this._addedToChrome = true;
        }
        this._actor.show();
        if (isNewCall) {
            this._isActiveCall = false;
            this._dtmfVisible = false;
            this._view.setCallState(false, outgoing);
            if (outgoing) {
                this._view.setAudioStatus('Wird gewählt …');
            } else {
                this._startRingtone();
            }
        } else {
            this._view.setCallState(this._isActiveCall, outgoing);
        }
    }

    hide(parameters) {
        const [callId] = parameters.deepUnpack();
        if (callId === this._currentCallId)
            this.destroy();
    }

    setActive(parameters) {
        const [callId] = parameters.deepUnpack();
        if (callId === this._currentCallId)
            this._setActive();
    }

    setAudioStatus(callId, status) {
        if (callId === this._currentCallId)
            this._view?.setAudioStatus(status);
    }

    resize() {
        this._actor?.set_size(global.stage.width, global.stage.height);
    }

    destroy() {
        this._stopRingtone();
        if (this._addedToChrome) {
            Main.layoutManager.removeChrome(this._actor);
            this._addedToChrome = false;
        }
        this._actor?.destroy();
        this._actor = null;
        this._view = null;
        this._currentCallId = null;
    }

    _startRingtone() {
        const ringtone = this._settings.get_string('ringtone-file');
        if (ringtone === 'none')
            return;
        const generation = ++this._ringtoneGeneration;
        const callId = this._currentCallId;
        const path = ringtone.startsWith('theme:')
            ? resolveSoundThemeFile(ringtone.slice('theme:'.length))
            : ringtone;
        if (!path) {
            console.error('[GNOME Handy] Kein passender Pfad für den Klingelton gefunden.');
            return;
        }

        const start = async () => {
            if (generation !== this._ringtoneGeneration || callId !== this._currentCallId)
                return;
            await callRust('StartRingtone', new GLib.Variant('(s)', [path]));
            if (generation !== this._ringtoneGeneration || callId !== this._currentCallId)
                await callRust('StopRingtone');
        };
        this._ringtoneOperation = this._ringtoneOperation.then(start, start).catch(error => {
            console.error(`[GNOME Handy] Klingelton konnte nicht gestartet werden: ${error.message}`);
        });
    }

    _stopRingtone() {
        this._ringtoneGeneration++;
        const stop = () => callRust('StopRingtone');
        this._ringtoneOperation = this._ringtoneOperation.then(stop, stop).catch(error => {
            console.error(`[GNOME Handy] Klingelton konnte nicht gestoppt werden: ${error.message}`);
        });
    }

    _answerCall() {
        if (this._declineInFlight || !this._currentCallId)
            return;

        this._declineInFlight = true;
        this._view.answerButton.reactive = false;
        callRust('AnswerCall').then(() => {
            this._stopRingtone();
            this._setActive();
        }).catch(error => {
            console.error(`[GNOME Handy] Anruf konnte nicht angenommen werden: ${error.message}`);
            Main.notify('GNOME Handy', 'Anruf konnte nicht angenommen werden.');
            if (this._view)
                this._view.answerButton.reactive = true;
        }).finally(() => {
            this._declineInFlight = false;
        });
    }

    _setActive() {
        if (!this._view)
            return;
        this._isActiveCall = true;
        this._stopRingtone();
        this._view.setCallState(true, this._isOutgoingCall);
        this._view.setAudioStatus('Verbinde Gesprächsaudio …');
    }

    _hangUpCall() {
        if (!this._currentCallId || this._declineInFlight)
            return;
        this._declineInFlight = true;
        this._view.hangupButton.reactive = false;
        callRust('HangUpCall').catch(error => {
            console.error(`[GNOME Handy] Anruf konnte nicht beendet werden: ${error.message}`);
            Main.notify('GNOME Handy', 'Anruf konnte nicht beendet werden.');
            if (this._view)
                this._view.hangupButton.reactive = true;
        }).finally(() => {
            this._declineInFlight = false;
        });
    }

    _rejectCall() {
        if (!this._currentCallId || this._declineInFlight)
            return;
        this._declineInFlight = true;
        this._view.rejectButton.reactive = false;
        callRust('RejectCall').then(() => this._stopRingtone()).catch(error => {
            console.error(`[GNOME Handy] Anruf konnte nicht abgelehnt werden: ${error.message}`);
            Main.notify('GNOME Handy', 'Anruf konnte nicht abgelehnt werden.');
            if (this._view)
                this._view.rejectButton.reactive = true;
        }).finally(() => {
            this._declineInFlight = false;
        });
    }

    _toggleDtmf() {
        this._dtmfVisible = !this._dtmfVisible;
        this._view?.setDtmfVisible(this._dtmfVisible);
    }

    _sendDtmf(tone) {
        this._view?.highlightDtmfKey(tone);
        callRust('SendDtmf', new GLib.Variant('(s)', [tone])).catch(error => {
            console.error(`[GNOME Handy] DTMF-Ton ${tone} konnte nicht gesendet werden: ${error.message}`);
            Main.notify('GNOME Handy', `DTMF-Ton ${tone} konnte nicht gesendet werden.`);
        });
    }
}

function resolveSoundThemeFile(eventName) {
    const themeName = Gio.Settings.new('org.gnome.desktop.sound').get_string('theme-name');
    const themes = [...new Set([themeName, 'freedesktop'])];
    const dataDirectories = [GLib.get_user_data_dir(), ...GLib.get_system_data_dirs()];
    for (const directory of dataDirectories) {
        for (const theme of themes) {
            for (const filename of [`${eventName}.oga`, `${eventName}.ogg`, 'bell.oga']) {
                const path = `${directory}/sounds/${theme}/stereo/${filename}`;
                if (Gio.File.new_for_path(path).query_exists(null))
                    return path;
            }
        }
    }
    return null;
}

function createIncomingCallOverlay(actions) {
    const overlay = new St.Widget({
        style_class: 'handy-call-overlay',
        layout_manager: new Clutter.BinLayout(),
        x: 0,
        y: 0,
        width: global.stage.width,
        height: global.stage.height,
        visible: false,
        reactive: false,
    });

    const card = new St.BoxLayout({
        style_class: 'modal-dialog handy-call-card',
        orientation: Clutter.Orientation.VERTICAL,
        x_align: Clutter.ActorAlign.CENTER,
        y_align: Clutter.ActorAlign.CENTER,
        reactive: true,
    });
    overlay.add_child(card);

    const header = new St.BoxLayout({
        style_class: 'handy-call-header',
        x_expand: true,
    });
    header.add_child(new St.Widget({ x_expand: true }));
    header.add_child(new St.Icon({
        icon_name: 'bluetooth-active-symbolic',
        style_class: 'handy-call-bluetooth-icon',
        y_align: Clutter.ActorAlign.CENTER,
    }));
    header.add_child(new St.Label({
        text: 'Bluetooth',
        style_class: 'handy-call-bluetooth-label',
        y_align: Clutter.ActorAlign.CENTER,
    }));
    card.add_child(header);

    const contact = new St.BoxLayout({
        style_class: 'handy-call-contact',
        orientation: Clutter.Orientation.VERTICAL,
        x_align: Clutter.ActorAlign.CENTER,
    });
    contact.add_child(new St.Icon({
        icon_name: 'avatar-default-symbolic',
        style_class: 'handy-call-avatar',
    }));
    const name = new St.Label({ text: '', style_class: 'handy-call-name' });
    contact.add_child(name);
    const number = new St.Label({ text: '', style_class: 'handy-call-number' });
    contact.add_child(number);
    const callStatus = new St.Label({
        text: '',
        style_class: 'handy-call-state',
        visible: false,
    });
    contact.add_child(callStatus);
    card.add_child(contact);

    const details = new St.Label({
        text: '',
        style_class: 'handy-call-details',
        visible: false,
    });
    card.add_child(details);

    const actionButtons = new St.BoxLayout({
        style_class: 'handy-call-actions',
        x_align: Clutter.ActorAlign.CENTER,
    });
    const hangupButton = createCallAction('call-stop-symbolic', 'destructive-action', actions.hangup);
    const rejectButton = createCallAction('call-stop-symbolic', 'destructive-action', actions.reject);
    const answerButton = createCallAction('call-start-symbolic', 'suggested-action', actions.answer);
    const dtmfButton = createCallAction('input-dialpad-symbolic', 'handy-call-keypad-button', actions.toggleDtmf);
    actionButtons.add_child(hangupButton);
    actionButtons.add_child(rejectButton);
    actionButtons.add_child(answerButton);
    actionButtons.add_child(dtmfButton);
    card.add_child(actionButtons);

    const audioStatus = new St.Label({
        text: '',
        style_class: 'handy-call-audio-status',
        visible: false,
    });
    card.add_child(audioStatus);

    const dtmfGrid = new St.Widget({
        style_class: 'handy-call-dtmf-grid',
        layout_manager: new Clutter.GridLayout(),
        visible: false,
    });
    const gridLayout = dtmfGrid.layout_manager;
    const dtmfKeys = new Map();
    for (const [index, tone] of [...'123456789*0#'].entries()) {
        const button = new St.Button({
            style_class: 'handy-call-dtmf-key',
            label: tone,
            can_focus: true,
            reactive: true,
        });
        button.connect('clicked', () => actions.sendDtmf(tone));
        gridLayout.attach(button, index % 3, Math.floor(index / 3), 1, 1);
        dtmfKeys.set(tone, button);
    }
    card.add_child(dtmfGrid);

    return {
        actor: overlay, name, number, details, answerButton, hangupButton, rejectButton,
        setCallState(active, outgoing) {
            answerButton.visible = !active && !outgoing;
            hangupButton.visible = active || outgoing;
            rejectButton.visible = !active && !outgoing;
            dtmfButton.visible = active;
            const status = outgoing ? (active ? 'Verbunden' : 'Wird gewählt …') : '';
            callStatus.set_text(status);
            callStatus.visible = Boolean(status);
            this.setDtmfVisible(false);
        },
        setDtmfVisible(visible) {
            dtmfGrid.visible = visible;
        },
        setAudioStatus(text) {
            audioStatus.set_text(text);
            audioStatus.visible = Boolean(text);
        },
        highlightDtmfKey(tone) {
            const button = dtmfKeys.get(tone);
            if (!button)
                return;
            button.add_style_pseudo_class('active');
            GLib.timeout_add(GLib.PRIORITY_DEFAULT, 250, () => {
                button.remove_style_pseudo_class('active');
                return GLib.SOURCE_REMOVE;
            });
        },
    };
}

function createCallAction(iconName, styleClass, onClicked = null) {
    const button = new St.Button({
        style_class: `modal-dialog-button ${styleClass}`,
        reactive: Boolean(onClicked),
        can_focus: Boolean(onClicked),
    });
    button.set_child(new St.Icon({
        icon_name: iconName,
        style_class: 'handy-call-action-icon',
    }));
    if (onClicked)
        button.connect('clicked', onClicked);
    return button;
}
