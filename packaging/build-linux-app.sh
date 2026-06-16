#!/usr/bin/env bash
#
# Build the Self-KVM desktop app (the "Control Deck" GUI) on Ubuntu/Debian.
# The SAME app runs as either server or client — on the client machine you open
# it, switch to client mode, point it at the server, and start. Produces an
# installable .deb and a portable .AppImage.
#
#   bash packaging/build-linux-app.sh
#
# Requires Ubuntu 24.04+ (Tauri 2 needs webkit2gtk-4.1). On 22.04 the
# libwebkit2gtk-4.1-dev package is unavailable and this will fail — use the
# headless client (packaging/install-linux-client.sh) there instead.
#
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> Installing build dependencies (sudo)…"
sudo apt update
sudo apt install -y \
  build-essential curl wget file pkg-config \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev libssl-dev \
  libx11-dev libxi-dev libxtst-dev libxdo-dev libxkbcommon-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes-dev

if ! command -v cargo >/dev/null 2>&1; then
  echo "!!  Rust not found. Install it: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi

if ! cargo tauri --version >/dev/null 2>&1; then
  echo "==> Installing tauri-cli (one-time)…"
  cargo install tauri-cli --version '^2.0' --locked
fi

echo "==> Installing frontend deps + building the app bundle…"
( cd "$REPO/ui" && npm install )
( cd "$REPO/src-tauri" && cargo tauri build --bundles deb appimage )

echo
echo "Done. Bundles are in:"
ls -1 "$REPO/src-tauri/target/release/bundle/deb/"*.deb 2>/dev/null || true
ls -1 "$REPO/src-tauri/target/release/bundle/appimage/"*.AppImage 2>/dev/null || true

cat <<DONE

Install / run:
  * .deb:       sudo dpkg -i src-tauri/target/release/bundle/deb/*.deb   (then launch "Self-KVM" from the app menu)
  * AppImage:   chmod +x src-tauri/target/release/bundle/appimage/*.AppImage && ./...AppImage

In the app: switch to CLIENT mode, set server = <mac-ip>:24800, name = ubuntu, Start.

Reminders (same as the headless client):
  * Use an Xorg session, not Wayland (echo \$XDG_SESSION_TYPE → x11) or input won't inject.
  * Stop Barrier on this machine first.
DONE
