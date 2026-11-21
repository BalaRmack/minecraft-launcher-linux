#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
# Build inputs are staged outside the checkout so Cargo's offline source tree
# is not confused with the vendored mc-launcher-core path dependency.
rm -rf .flatpak-source
mkdir -p .flatpak-source/src-tauri .flatpak-source/packaging/flatpak
cp -a ui .flatpak-source/
cp -a src-tauri/Cargo.toml src-tauri/build.rs src-tauri/tauri.conf.json src-tauri/src src-tauri/vendor src-tauri/icons src-tauri/capabilities .flatpak-source/src-tauri/
cp -a packaging/flatpak/*.desktop packaging/flatpak/*.xml .flatpak-source/packaging/flatpak/
# The committed lockfile makes the dependency resolution reproducible.
test -f src-tauri/Cargo.lock
cp src-tauri/Cargo.lock .flatpak-source/src-tauri/
cargo vendor --locked --manifest-path src-tauri/Cargo.toml .flatpak-source/cargo-vendor >/dev/null
mkdir -p .flatpak-source/src-tauri/.cargo
cat > .flatpak-source/src-tauri/.cargo/config.toml <<'CONFIG'
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "../cargo-vendor"
CONFIG
# Cargo discovers .cargo relative to its current working directory.
cp -a .flatpak-source/src-tauri/.cargo .flatpak-source/.cargo
sed -i 's|../cargo-vendor|cargo-vendor|' .flatpak-source/.cargo/config.toml
flatpak-builder --user --install-deps-from=flathub --force-clean --repo=flatpak-repo flatpak-build packaging/flatpak/io.github.balarmack.horizon.yml
flatpak build-bundle flatpak-repo horizon.flatpak io.github.balarmack.horizon
