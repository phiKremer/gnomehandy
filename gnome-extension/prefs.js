import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import { ExtensionPreferences } from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';
import { callRust } from './rust-service.js';

const SYSTEM_TONE = 'theme:bell-window-system';
const SILENT = 'none';
const RINGTONE_DIRECTORY = GLib.build_filenamev([
    GLib.get_user_data_dir(),
    'pkremer-gnomehandy',
    'ringtones',
]);
const AUDIO_SUFFIXES = ['oga', 'ogg', 'wav', 'mp3', 'flac'];

export default class HandyPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        this._settings = this.getSettings();
        this._preview = null;
        this._previewTimeout = 0;
        this._customToneRows = [];

        const page = new Adw.PreferencesPage({
            title: 'Klingelton',
            icon_name: 'audio-volume-high-symbolic',
        });
        window.add(page);

        this._addAudioPage(window);

        const selectedGroup = new Adw.PreferencesGroup({
            title: 'Eingehender Anruf',
            description: 'Der ausgewählte Ton wird während des Anrufs wiederholt.',
        });
        page.add(selectedGroup);
        this._selectedRow = new Adw.ActionRow({ title: 'Aktueller Klingelton' });
        selectedGroup.add(this._selectedRow);

        const systemRow = new Adw.ActionRow({
            title: 'GNOME-Systemton',
            subtitle: 'bell-window-system aus dem aktiven Soundtheme',
        });
        systemRow.add_suffix(this._button('Vorschau', () => this._previewSystemTone()));
        systemRow.add_suffix(this._button('Auswählen', () => this._setTone(SYSTEM_TONE)));
        selectedGroup.add(systemRow);

        const silentRow = new Adw.ActionRow({
            title: 'Stumm',
            subtitle: 'Nur den Anruf-Layer anzeigen',
        });
        silentRow.add_suffix(this._button('Auswählen', () => this._setTone(SILENT)));
        selectedGroup.add(silentRow);

        this._customGroup = new Adw.PreferencesGroup({
            title: 'Eigene Klingeltöne',
            description: 'Audiodateien werden in deinen Benutzerordner kopiert.',
        });
        page.add(this._customGroup);
        const addRow = new Adw.ActionRow({
            title: 'Eigenen Ton hinzufügen',
            subtitle: 'OGA, OGG, WAV, MP3 oder FLAC',
        });
        addRow.add_suffix(this._button('Datei hinzufügen…', () => this._chooseTone(window)));
        this._customGroup.add(addRow);

        this._settings.connect('changed::ringtone-file', () => this._refreshSelection());
        window.connect('close-request', () => {
            this._stopPreview();
            return false;
        });
        this._refreshSelection();
        this._refreshCustomTones();
    }

    _addAudioPage(window) {
        const page = new Adw.PreferencesPage({
            title: 'Audio',
            icon_name: 'audio-speakers-symbolic',
        });
        window.add(page);

        const ringingGroup = new Adw.PreferencesGroup({ title: 'Klingeln auf' });
        page.add(ringingGroup);
        this._addAudioChoice(ringingGroup, 'Klingeln auf', 'ListAudioOutputs',
            'GetAudioOutputSelection', 'SetAudioOutputSelection', true);

        const outputGroup = new Adw.PreferencesGroup({ title: 'Gesprächsausgabe' });
        page.add(outputGroup);
        this._addAudioChoice(outputGroup, 'Ausgabegerät', 'ListAudioOutputs',
            'GetConversationOutputSelection', 'SetConversationOutputSelection');

        const inputGroup = new Adw.PreferencesGroup({ title: 'Mikrofon' });
        page.add(inputGroup);
        this._addAudioChoice(inputGroup, 'Mikrofon', 'ListAudioInputs',
            'GetMicrophoneSelection', 'SetMicrophoneSelection');

    }

    async _addAudioChoice(group, title, listMethod, getMethod, setMethod, includeAll = false) {
        const row = new Adw.ComboRow({ title, sensitive: false });
        group.add(row);
        try {
            const [deviceReply, selectionReply] = await Promise.all([
                callRust(listMethod),
                callRust(getMethod),
            ]);
            const [devices] = deviceReply.deepUnpack();
            const [selection] = selectionReply.deepUnpack();
            const values = includeAll ? ['all', ...devices.map(([name]) => name)] : ['', ...devices.map(([name]) => name)];
            const labels = includeAll
                ? ['Auf allen Geräten', ...devices.map(([, description]) => description)]
                : ['Systemstandard', ...devices.map(([, description]) => description)];
            row.model = Gtk.StringList.new(labels);
            row._handyValues = values;
            row._handySaving = false;
            row.selected = Math.max(0, values.indexOf(selection));
            row.sensitive = true;
            row.connect('notify::selected', async () => {
                if (row._handySaving)
                    return;
                const value = row._handyValues[row.selected];
                if (value === undefined)
                    return;
                row._handySaving = true;
                row.sensitive = false;
                try {
                    await callRust(setMethod, new GLib.Variant('(s)', [value]));
                } catch (error) {
                    console.error(`[GNOME Handy] Audio-Auswahl konnte nicht gespeichert werden: ${error.message}`);
                    row.selected = Math.max(0, row._handyValues.indexOf(selection));
                } finally {
                    row.sensitive = true;
                    row._handySaving = false;
                }
            });
        } catch (error) {
            row.subtitle = 'Audio-Geräte konnten nicht geladen werden';
            console.error(`[GNOME Handy] Audio-Geräte konnten nicht geladen werden: ${error.message}`);
        }
    }

    _button(label, callback) {
        const button = new Gtk.Button({ label, valign: Gtk.Align.CENTER });
        button.connect('clicked', callback);
        return button;
    }

    _setTone(value) {
        this._stopPreview();
        this._settings.set_string('ringtone-file', value);
    }

    _refreshSelection() {
        const value = this._settings.get_string('ringtone-file');
        if (value === SYSTEM_TONE)
            this._selectedRow.subtitle = 'GNOME-Systemton';
        else if (value === SILENT)
            this._selectedRow.subtitle = 'Stumm';
        else
            this._selectedRow.subtitle = GLib.path_get_basename(value) || 'Kein Ton ausgewählt';
        this._refreshCustomTones();
    }

    _refreshCustomTones() {
        for (const row of this._customToneRows)
            this._customGroup.remove(row);
        this._customToneRows = [];

        for (const path of listCustomTones()) {
            const selected = this._settings.get_string('ringtone-file') === path;
            const row = new Adw.ActionRow({
                title: GLib.path_get_basename(path),
                subtitle: selected ? 'Aktuell ausgewählt' : '',
            });
            row.add_suffix(this._button('Vorschau', () => this._previewFile(path)));
            row.add_suffix(this._button(selected ? 'Ausgewählt' : 'Verwenden', () => this._setTone(path)));
            row.add_suffix(this._button('Entfernen', () => this._removeTone(path)));
            this._customGroup.add(row);
            this._customToneRows.push(row);
        }
    }

    _chooseTone(window) {
        const dialog = new Gtk.FileDialog({ title: 'Eigenen Klingelton hinzufügen' });
        const filter = new Gtk.FileFilter();
        filter.set_name('Audiodateien');
        for (const suffix of AUDIO_SUFFIXES)
            filter.add_suffix(suffix);
        const filters = Gio.ListStore.new(Gtk.FileFilter);
        filters.append(filter);
        dialog.set_filters(filters);
        dialog.set_default_filter(filter);
        dialog.open(window, null, (source, result) => {
            try {
                const sourceFile = source.open_finish(result);
                const destination = copyToneToLibrary(sourceFile);
                this._setTone(destination.get_path());
                this._refreshCustomTones();
            } catch (error) {
                if (error.matches?.(Gtk.DialogError, Gtk.DialogError.DISMISSED))
                    return;
                console.error(`[GNOME Handy] Klingelton konnte nicht hinzugefügt werden: ${error.message}`);
            }
        });
    }

    _removeTone(path) {
        try {
            if (this._settings.get_string('ringtone-file') === path)
                this._settings.set_string('ringtone-file', SYSTEM_TONE);
            Gio.File.new_for_path(path).delete(null);
            this._refreshCustomTones();
        } catch (error) {
            console.error(`[GNOME Handy] Klingelton konnte nicht entfernt werden: ${error.message}`);
        }
    }

    _previewSystemTone() {
        const themeName = Gio.Settings.new('org.gnome.desktop.sound').get_string('theme-name');
        const dataDirs = [GLib.get_user_data_dir(), ...GLib.get_system_data_dirs()];
        const themes = [...new Set([themeName, 'freedesktop'])];
        const candidates = [];
        for (const dataDir of dataDirs) {
            for (const theme of themes) {
                for (const filename of ['bell-window-system.oga', 'bell-window-system.wav', 'bell.oga'])
                    candidates.push(GLib.build_filenamev([dataDir, 'sounds', theme, 'stereo', filename]));
            }
        }
        const path = candidates.find(candidate => Gio.File.new_for_path(candidate).query_exists(null));
        if (path)
            this._previewFile(path);
        else
            console.warn('[GNOME Handy] Keine lokale Vorschau für den GNOME-Systemton gefunden.');
    }

    _previewFile(path) {
        this._stopPreview();
        try {
            this._preview = Gtk.MediaFile.new_for_filename(path);
            this._preview.play();
            this._previewTimeout = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
                this._stopPreview();
                return GLib.SOURCE_REMOVE;
            });
        } catch (error) {
            console.error(`[GNOME Handy] Vorschau konnte nicht abgespielt werden: ${error.message}`);
        }
    }

    _stopPreview() {
        if (this._previewTimeout) {
            GLib.source_remove(this._previewTimeout);
            this._previewTimeout = 0;
        }
        this._preview?.pause();
        this._preview = null;
    }
}

function listCustomTones() {
    const directory = Gio.File.new_for_path(RINGTONE_DIRECTORY);
    try {
        const enumerator = directory.enumerate_children(
            'standard::name,standard::type',
            Gio.FileQueryInfoFlags.NONE,
            null,
        );
        const paths = [];
        let info;
        while ((info = enumerator.next_file(null)) !== null) {
            const name = info.get_name();
            const suffix = name.split('.').pop().toLowerCase();
            if (info.get_file_type() === Gio.FileType.REGULAR && AUDIO_SUFFIXES.includes(suffix))
                paths.push(GLib.build_filenamev([RINGTONE_DIRECTORY, name]));
        }
        enumerator.close(null);
        return paths.sort((left, right) => left.localeCompare(right));
    } catch (_error) {
        return [];
    }
}

function copyToneToLibrary(sourceFile) {
    const directory = Gio.File.new_for_path(RINGTONE_DIRECTORY);
    if (!directory.query_exists(null))
        directory.make_directory_with_parents(null);

    const basename = sourceFile.get_basename();
    const dot = basename.lastIndexOf('.');
    const stem = dot > 0 ? basename.slice(0, dot) : basename;
    const suffix = dot > 0 ? basename.slice(dot) : '';
    let destination;
    for (let index = 1; ; index++) {
        const candidate = index === 1 ? basename : `${stem} (${index})${suffix}`;
        destination = directory.get_child(candidate);
        if (!destination.query_exists(null))
            break;
    }

    sourceFile.copy(destination, Gio.FileCopyFlags.NONE, null, null);
    return destination;
}
