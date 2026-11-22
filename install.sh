#!/bin/sh
# Install Burrow for the current user (no root): the binary next to this script if there is one
# (release download), else a fresh build. ./install.sh --uninstall removes it.
# Your settings in ~/.config/burrow are never touched, and it doesn't become the default by itself.
set -eu
cd "$(dirname "$0")"
BIN="${HOME}/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
ID=world.puppygirl.Burrow
if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/burrow" "$DATA/applications/$ID.desktop" "$DATA/icons/hicolor/scalable/apps/$ID.svg"
    echo "Burrow removed."
    exit 0
fi
if [ -f burrow ]; then
    SRC=burrow
else
    cargo build --release
    SRC=target/release/burrow
fi
install -Dm755 "$SRC" "$BIN/burrow"
install -Dm644 "data/icons/hicolor/scalable/apps/$ID.svg" "$DATA/icons/hicolor/scalable/apps/$ID.svg"
mkdir -p "$DATA/applications"
# xdg-mime and xdg-open don't understand a quoted program, so quote only if the path needs it
case "$BIN" in *[!A-Za-z0-9/._-]*) EXEC="\"$BIN/burrow\"" ;; *) EXEC="$BIN/burrow" ;; esac
sed "s|^Exec=burrow|Exec=$EXEC|" "data/$ID.desktop" > "$DATA/applications/$ID.desktop"
update-desktop-database "$DATA/applications" 2>/dev/null || true
echo "Installed to $BIN/burrow"
echo "Make it the default file manager with:  xdg-mime default $ID.desktop inode/directory"
