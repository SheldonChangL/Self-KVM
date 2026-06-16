#!/usr/bin/env bash
#
# Install Self-KVM as an auto-starting systemd --user service (client / secondary).
# Run once on the Ubuntu machine. After this the client auto-starts on login and
# auto-reconnects (Restart=always) — so it survives the server being down,
# starting first, or restarting. You never touch the CLI again.
#
#   bash packaging/install-linux-client.sh [SERVER_ADDR] [SCREEN_NAME]
#   e.g. bash packaging/install-linux-client.sh 192.168.161.44:24800 ubuntu
#
set -euo pipefail

SERVER="${1:-192.168.161.44:24800}"
NAME="${2:-ubuntu}"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/self-kvm-client.service"

if [ "${XDG_SESSION_TYPE:-}" != "x11" ]; then
  echo "!!  XDG_SESSION_TYPE='${XDG_SESSION_TYPE:-unknown}' — input injection needs an Xorg session."
  echo "!!  Log out and pick 'Ubuntu on Xorg' at the login screen, then re-run this."
fi

echo "==> Building release binary (this can take a while the first time)…"
( cd "$REPO" && cargo build -p kvm-daemon --features real-input --release )

echo "==> Installing binary to $BIN_DIR/kvm-daemon"
mkdir -p "$BIN_DIR" "$UNIT_DIR"
install -m 0755 "$REPO/target/release/kvm-daemon" "$BIN_DIR/kvm-daemon"

echo "==> Writing systemd unit (server=$SERVER name=$NAME)"
cat > "$UNIT" <<UNITEOF
[Unit]
Description=Self-KVM client (connects to $SERVER)
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=$BIN_DIR/kvm-daemon client --server $SERVER --name $NAME
Restart=always
RestartSec=2
Environment=RUST_LOG=info
Environment=DISPLAY=:0
# If the cursor connects but nothing is injected, the service can't reach your
# X authority. Find it with \`echo \$XAUTHORITY\` in a desktop terminal and set:
#Environment=XAUTHORITY=%h/.Xauthority

[Install]
WantedBy=default.target
UNITEOF

echo "==> Enabling + starting the service"
systemctl --user daemon-reload
systemctl --user enable --now self-kvm-client.service

cat <<DONE

Done. The client auto-starts on login and keeps retrying until the server is up
(so start order no longer matters), and reconnects if the server restarts.

CHECK / NOTES:
  * Must be an Xorg session:   echo \$XDG_SESSION_TYPE     (want: x11)
  * Stop Barrier here first (it would fight for input).
  * Status:   systemctl --user status self-kvm-client.service
  * Logs:     journalctl --user -u self-kvm-client -f
  * Run even when not logged in:   sudo loginctl enable-linger \$USER

Reconfigure (new server IP/name): re-run this script with new args.
Uninstall:  systemctl --user disable --now self-kvm-client.service ; rm "$UNIT"
DONE
