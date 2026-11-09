#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"

cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release --bin pkremer-gnomehandy
bash -n install.sh scripts/install-tel-handler.sh scripts/run-gnome-extension-dev.sh
systemd-analyze verify systemd/pkremer-gnomehandy.service

bundle_dir="$project_dir/target/extension-check"
mkdir -p "$bundle_dir"
gnome-extensions pack --force \
    --schema="$project_dir/gnome-extension/schemas/de.pkremer.GnomeHandy.gschema.xml" \
    --extra-source=bluetooth-status.js \
    --extra-source=dialer.js \
    --extra-source=incoming-call-overlay.js \
    --extra-source=rust-service.js \
    --out-dir "$bundle_dir" "$project_dir/gnome-extension"
