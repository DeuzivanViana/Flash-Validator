#!/bin/sh
# Installs FlashValidator from an extracted release archive.
#
#   sudo ./install.sh            # installs into /usr/local
#   sudo PREFIX=/usr ./install.sh
#   sudo ./install.sh --uninstall
set -eu

PREFIX="${PREFIX:-/usr/local}"
HERE="$(cd "$(dirname "$0")" && pwd)"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$PREFIX/bin/flashvalidator" "$PREFIX/bin/flashvalidator-gui" \
          "$PREFIX/share/applications/flashvalidator.desktop" \
          "$PREFIX/share/icons/hicolor/scalable/apps/flashvalidator.svg"
    echo "FlashValidator removed from $PREFIX"
    exit 0
fi

install -Dm755 "$HERE/flashvalidator"      "$PREFIX/bin/flashvalidator"
install -Dm755 "$HERE/flashvalidator-gui"  "$PREFIX/bin/flashvalidator-gui"
install -Dm644 "$HERE/flashvalidator.desktop" "$PREFIX/share/applications/flashvalidator.desktop"
install -Dm644 "$HERE/flashvalidator.svg"  "$PREFIX/share/icons/hicolor/scalable/apps/flashvalidator.svg"

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$PREFIX/share/applications" || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache -q "$PREFIX/share/icons/hicolor" || true

echo "FlashValidator installed into $PREFIX"
