#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${HANDY_BINARY:-$project_dir/target/release/pkremer-gnomehandy}"
desktop_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
desktop_file="$desktop_dir/gnomehandy-tel-handler.desktop"

if [[ ! -x "$binary" ]]; then
    cargo build --release --manifest-path "$project_dir/Cargo.toml" --bin pkremer-gnomehandy
fi
install -d "$desktop_dir"
cat > "$desktop_file" <<EOF
[Desktop Entry]
Type=Application
Name=GNOME Handy Telefonie
Comment=Telefonnummer über das verbundene Mobiltelefon wählen
# Show this handler in GNOME's Default Applications settings.
Terminal=false
Exec="$binary" --tel %u
MimeType=x-scheme-handler/tel;
EOF

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$desktop_dir"
fi
xdg-mime default gnomehandy-tel-handler.desktop x-scheme-handler/tel
printf 'tel:-Handler registriert: %s\n' "$desktop_file"
