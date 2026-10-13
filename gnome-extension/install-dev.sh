#!/usr/bin/env bash
# Dev installer: copy the extension into the user dir and compile its schema.
set -euo pipefail
UUID="netmeter@4imd3v.github.io"
SRC="$(cd "$(dirname "$0")/$UUID" && pwd)"
DEST="$HOME/.local/share/gnome-shell/extensions/$UUID"
mkdir -p "$DEST"
cp -r "$SRC/." "$DEST/"
glib-compile-schemas "$DEST/schemas/"
echo "Installed to $DEST"
echo "Enable with:  gnome-extensions enable $UUID"
