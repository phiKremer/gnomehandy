#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
binary_dir="$HOME/.local/bin"
binary="$binary_dir/pkremer-gnomehandy"
extension_id="handy@pkremer.de"
service_dir="$config_home/systemd/user"

for command in cargo gnome-extensions systemctl; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'Benötigtes Programm fehlt: %s\n' "$command" >&2
        exit 1
    }
done

printf 'Baue GNOME Handy im Release-Modus …\n'
cargo build --release --manifest-path "$project_dir/Cargo.toml" --bin pkremer-gnomehandy

install -d "$binary_dir" "$service_dir"
install -m 755 "$project_dir/target/release/pkremer-gnomehandy" "$binary"

bundle_dir="$(mktemp -d "${TMPDIR:-/tmp}/gnomehandy-install.XXXXXX")"
trap 'rm -rf "$bundle_dir"' EXIT
gnome-extensions pack --force \
    --schema="$project_dir/gnome-extension/schemas/de.pkremer.GnomeHandy.gschema.xml" \
    --extra-source="$project_dir/gnome-extension/bluetooth-status.js" \
    --extra-source="$project_dir/gnome-extension/dialer.js" \
    --extra-source="$project_dir/gnome-extension/incoming-call-overlay.js" \
    --extra-source="$project_dir/gnome-extension/rust-service.js" \
    --out-dir "$bundle_dir" "$project_dir/gnome-extension"
gnome-extensions install --force "$bundle_dir/$extension_id.shell-extension.zip"

install -m 644 "$project_dir/systemd/pkremer-gnomehandy.service" \
    "$service_dir/pkremer-gnomehandy.service"
systemctl --user daemon-reload
systemctl --user enable pkremer-gnomehandy.service
systemctl --user restart pkremer-gnomehandy.service

HANDY_BINARY="$binary" "$project_dir/scripts/install-tel-handler.sh"

extension_active=false
for _ in {1..10}; do
    gnome-extensions enable "$extension_id" || true
    extension_state="$(gnome-extensions info "$extension_id" 2>/dev/null \
        | awk -F': ' '/^[[:space:]]*State:/{print $2}')"
    if [[ "$extension_state" == "ACTIVE" ]]; then
        extension_active=true
        break
    fi
    sleep 1
done

if [[ "$extension_active" != true ]]; then
    printf 'Die GNOME-Erweiterung wurde installiert, ist aber nicht aktiv.\n' >&2
    printf 'Aktueller Zustand: %s\n' "${extension_state:-unbekannt}" >&2
    printf 'Bitte prüfen: gnome-extensions info %s\n' "$extension_id" >&2
    exit 1
fi

printf '\nGNOME Handy ist installiert.\n'
printf 'Dienststatus: systemctl --user status pkremer-gnomehandy.service\n'
printf 'Dienstlogs:   journalctl --user -u pkremer-gnomehandy.service -f\n'
