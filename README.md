# GNOME Handy

GNOME Handy verbindet einen Linux-Rechner mit einem Android- oder anderen Bluetooth-Handy und ergänzt GNOME um eine einfache Freisprechlösung. Eine GNOME-Shell-Erweiterung zeigt den Verbindungsstatus und den Anrufbildschirm; ein Rust-Dienst verwaltet Bluetooth, Audio und Telefonfunktionen.

> **Projektstatus:** frühe Entwicklung. Der aktuelle Stand wurde vor allem mit GNOME 48, BlueZ und einem Android-Handy erprobt. Andere GNOME-, BlueZ-, PipeWire- und Telefonversionen können sich anders verhalten. Beiträge und Tests mit weiteren Geräten sind willkommen.

## Funktionen

- Zeigt in der GNOME-Leiste, ob der Rust-Dienst und ein HFP-Handy verbunden sind.
- Zeigt einen Bildschirm für eingehende und ausgehende Anrufe mit Annehmen, Ablehnen beziehungsweise Auflegen.
- Leitet Gesprächston und Mikrofon über Bluetooth HFP und die ausgewählten Audio-Geräte.
- Unterstützt Wählen, DTMF-Töne und Rufnummern aus `tel:`-Links.
- Spielt einen wählbaren GNOME-Systemton oder eigene Audiodateien als Klingelton ab. Eigene Töne lassen sich in den Erweiterungseinstellungen hinzufügen und probehören.
- Liest SMS über Bluetooth MAP und zeigt neue Nachrichten als GNOME-Benachrichtigung an, sofern das Handy MAP unterstützt und den Zugriff freigibt.
- Startet den Rust-Dienst über systemd neu, wenn er unerwartet endet. Bluetooth-Profile und Handyverbindungen werden erneut aufgebaut.

## Voraussetzungen

- Debian 13 oder eine vergleichbare Linux-Distribution mit systemd im Benutzerkontext
- GNOME Shell 48 (die Erweiterungsmetadaten nennen derzeit Version 48)
- BlueZ mit Bluetooth-Unterstützung und ein gekoppeltes Handy mit HFP
- PipeWire mit `pipewire-pulse` oder PulseAudio
- Rust 1.85 oder neuer, Cargo, `pkg-config`, ein C-Compiler und PulseAudio-Entwicklungsdateien zum Bauen
- `gnome-extensions`, `systemctl`, `xdg-mime` und `update-desktop-database` (letzteres ist optional)

Auf Debian lassen sich die üblichen Build-Abhängigkeiten so installieren:

```sh
sudo apt install build-essential pkg-config libpulse-dev rustc cargo
```

Installiere außerdem GNOME Shell, BlueZ, PipeWire mit PulseAudio-Kompatibilität und die GNOME-Erweiterungswerkzeuge, falls sie noch fehlen.

## Installation

Klone oder entpacke das Projekt und führe im Projektverzeichnis aus:

```sh
./install.sh
```

Das Skript baut den Dienst im Release-Modus, installiert die GNOME-Erweiterung in dein Benutzerkonto, richtet einen systemd-Benutzerdienst ein, registriert den `tel:`-Handler und aktiviert die Erweiterung. Es meldet einen Fehler, falls die Erweiterung danach nicht aktiv ist.

Kopple dein Handy anschließend in den GNOME-Bluetooth-Einstellungen. Bestätige am Handy die HFP-Verbindung. Für SMS musst du gegebenenfalls zusätzlich den Nachrichten- beziehungsweise MAP-Zugriff erlauben. Einige Telefone erlauben HFP und MAP getrennt.

## Bedienung

Öffne die GNOME-Handy-Erweiterung in der oberen Leiste. Dort siehst du den Dienst- und Handystatus und kannst eine Nummer über das Tastenfeld wählen. Über **Einstellungen** legst du Klingelton, Klingel-Ausgänge, Gesprächsausgabe und Mikrofon fest.

`tel:`-Links können den Handler direkt aufrufen, zum Beispiel:

```sh
xdg-open 'tel:+4930123456'
```

GNOME Kontakte 48 zeigt selbst keinen Anrufknopf neben Telefonnummern. Der Handler verarbeitet `tel:`-Links, die von Anwendungen tatsächlich geöffnet werden; die Registrierung allein fügt in Kontakte keinen Knopf hinzu.

## Dienst und Fehlerdiagnose

```sh
systemctl --user status pkremer-gnomehandy.service
journalctl --user -u pkremer-gnomehandy.service -f
```

Den Status der Erweiterung kannst du so prüfen:

```sh
gnome-extensions info handy@pkremer.de
```

Wenn ein Anrufbildschirm erscheint, aber kein Ton zu hören ist, prüfe in den Erweiterungseinstellungen die Gesprächsausgabe und den Mikrofoneingang. Prüfe außerdem, ob das Handy die Audioverbindung während des Gesprächs an den Computer übergibt.

## Entwicklung

Für eine isolierte GNOME-Shell-Testumgebung:

```sh
./scripts/run-gnome-extension-dev.sh
```

Für Formatierung, Clippy, Tests, Release-Build, Shell-Syntax, systemd-Unit-Prüfung und GNOME-Erweiterungspaket:

```sh
./scripts/check.sh
```

## Bekannte Grenzen

- Die GNOME-Kontakte-App bietet in Version 48 keinen eingebauten Anrufknopf. Das kann GNOME Handy nicht allein durch die Registrierung eines URI-Handlers ändern.
- Bluetooth-Funktionen hängen von den Profilen und Berechtigungen des jeweiligen Handys ab. MAP-Unterstützung und SMS-Zugriff sind nicht bei allen Geräten verfügbar.
- Das Audioverhalten hängt vom Handy, Bluetooth-Adapter und Audio-Stack ab. Nicht jede Geräte- und Codec-Kombination wurde getestet.
- Derzeit ist kein Demo-Klingelton im Projekt enthalten. In den Erweiterungseinstellungen kannst du eigene Audiodateien hinzufügen.
- Das Projekt steht unter GNU GPL Version 3. Die Lizenzbedingungen stehen in [`LICENSE`](LICENSE).

## Lizenz

Dieses Projekt steht unter der **GNU General Public License v3.0**. Details stehen in der Datei [`LICENSE`](LICENSE).
