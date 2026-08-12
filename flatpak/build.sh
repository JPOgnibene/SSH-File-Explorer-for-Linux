#!/usr/bin/env bash
#
# Build the SSH File Explorer Flatpak from the Tauri-produced .deb.
#
# One-time prerequisites:
#   sudo apt install flatpak-builder      # or: flatpak install flathub org.flatpak.Builder
#   flatpak install flathub org.gnome.Platform//47 org.gnome.Sdk//47
#
# Usage (run from this flatpak/ directory):
#   ./build.sh            build and install for the current user
#   ./build.sh --bundle   also export a single-file .flatpak you can share
#
set -euo pipefail

APP_ID="io.github.jpognibene.SshFileExplorer"
HERE="$(cd "$(dirname "$0")" && pwd)"
DEB_DIR="$HERE/../ssh-file-explorer/src-tauri/target/release/bundle/deb"

# Locate the most recent .deb. Build it first with `npm run tauri build`
# inside ssh-file-explorer/ if it is missing.
DEB="$(ls -t "$DEB_DIR"/*.deb 2>/dev/null | head -n1 || true)"
if [ -z "$DEB" ]; then
  echo "No .deb found in $DEB_DIR" >&2
  echo "Run 'npm run tauri build' in ssh-file-explorer/ first." >&2
  exit 1
fi

echo "Using $DEB"
cp "$DEB" "$HERE/ssh-file-explorer.deb"

cd "$HERE"
flatpak-builder --force-clean --user --install --repo=repo build "$APP_ID.yml"

if [ "${1:-}" = "--bundle" ]; then
  flatpak build-bundle repo "$APP_ID.flatpak" "$APP_ID"
  echo "Wrote $HERE/$APP_ID.flatpak"
fi

echo
echo "Done. Run it with:"
echo "  flatpak run $APP_ID"
