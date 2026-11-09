#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
extension_dir="$HOME/.local/share/gnome-shell/extensions/handy@pkremer.de"
log_dir="${XDG_STATE_HOME:-$HOME/.local/state}/pkremer-gnomehandy"
log_file="$log_dir/extension-dev.log"

install -d "$log_dir"
: > "$log_file"
exec > >(tee -a "$log_file") 2>&1

install -d "$extension_dir"
install -d "$extension_dir/schemas"
install -m 644 "$project_dir/gnome-extension/metadata.json" "$extension_dir/metadata.json"
install -m 644 "$project_dir"/gnome-extension/*.js "$extension_dir/"
install -m 644 "$project_dir/gnome-extension/stylesheet.css" "$extension_dir/stylesheet.css"
install -m 644 "$project_dir"/gnome-extension/schemas/*.gschema.xml "$extension_dir/schemas/"
glib-compile-schemas "$extension_dir/schemas"

cargo build --manifest-path "$project_dir/Cargo.toml" --bin pkremer-gnomehandy

host_session_bus_address="${DBUS_SESSION_BUS_ADDRESS:-}"
exec env HOST_SESSION_BUS_ADDRESS="$host_session_bus_address" dbus-run-session -- bash -c '
    set -euo pipefail
    "$1/target/debug/pkremer-gnomehandy" &
    service_pid=$!
    shell_pid=""
    bus_address_file="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pkremer-gnomehandy-dev-session-bus"
    printf "%s\n" "$DBUS_SESSION_BUS_ADDRESS" > "$bus_address_file"
    chmod 600 "$bus_address_file"
    cleanup() {
        rm -f "$bus_address_file"
        if [[ -n "$shell_pid" ]]; then
            kill "$shell_pid" 2>/dev/null || true
            wait "$shell_pid" 2>/dev/null || true
        fi
        kill "$service_pid" 2>/dev/null || true
        wait "$service_pid" 2>/dev/null || true
    }
    trap cleanup EXIT

    service_ready=false
    for _ in {1..50}; do
        if gdbus call --session \
            --dest org.freedesktop.DBus \
            --object-path /org/freedesktop/DBus \
            --method org.freedesktop.DBus.NameHasOwner \
            de.pkremer.GnomeHandy 2>/dev/null | grep -q true; then
            service_ready=true
            break
        fi
        sleep 0.1
    done

    if [[ "$service_ready" != true ]]; then
        echo "Rust-Dienst ist nicht auf dem Session-Bus gestartet." >&2
        exit 1
    fi

    env MUTTER_DEBUG_DUMMY_MODE_SPECS=1600x1000 gnome-shell --nested --wayland &
    shell_pid=$!

    shell_ready=false
    for _ in {1..50}; do
        if gdbus call --session \
            --dest org.freedesktop.DBus \
            --object-path /org/freedesktop/DBus \
            --method org.freedesktop.DBus.NameHasOwner \
            org.gnome.Shell 2>/dev/null | grep -q true; then
            shell_ready=true
            break
        fi
        sleep 0.1
    done

    if [[ "$shell_ready" != true ]]; then
        echo "GNOME Shell wurde nicht auf dem Dev-Session-Bus gestartet." >&2
        exit 1
    fi

    gnome-extensions enable handy@pkremer.de
    wait "$shell_pid"
' _ "$project_dir"
