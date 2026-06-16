#!/usr/bin/env bash
#
# Install Self-KVM as an auto-starting macOS LaunchAgent (server / primary).
# Run once on the Mac. After this the server starts at login, restarts if it
# exits, and you never touch the CLI again.
#
#   bash packaging/install-macos-server.sh
#
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
CFG_DIR="$HOME/.config/self-kvm"
PLIST="$HOME/Library/LaunchAgents/com.self-kvm.server.plist"
LABEL="com.self-kvm.server"
UID_NUM="$(id -u)"

echo "==> Building release binary (this can take a few minutes the first time)…"
( cd "$REPO" && cargo build -p kvm-daemon --features real-input --release )

echo "==> Installing binary to $BIN_DIR/kvm-daemon"
mkdir -p "$BIN_DIR" "$CFG_DIR"
install -m 0755 "$REPO/target/release/kvm-daemon" "$BIN_DIR/kvm-daemon"

if [ ! -f "$CFG_DIR/layout.json" ]; then
  echo "==> Installing default layout to $CFG_DIR/layout.json"
  cp "$REPO/examples/mac-ubuntu.json" "$CFG_DIR/layout.json"
else
  echo "==> Keeping existing $CFG_DIR/layout.json"
fi

echo "==> Writing LaunchAgent $PLIST"
mkdir -p "$(dirname "$PLIST")"
cat > "$PLIST" <<PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$BIN_DIR/kvm-daemon</string>
    <string>server</string>
    <string>--config</string>
    <string>$CFG_DIR/layout.json</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/self-kvm-server.log</string>
  <key>StandardErrorPath</key><string>/tmp/self-kvm-server.log</string>
  <key>EnvironmentVariables</key>
  <dict><key>RUST_LOG</key><string>info</string></dict>
</dict>
</plist>
PLISTEOF

echo "==> (Re)loading LaunchAgent"
launchctl bootout "gui/$UID_NUM/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$UID_NUM" "$PLIST"

if pgrep -x barriers >/dev/null 2>&1; then
  echo "!!  Barrier is running and holds port 24800 — quit it or the server can't bind."
fi

cat <<DONE

Done. The server now starts at login and restarts automatically.

ONE-TIME setup still needed:
  1. Quit Barrier (it owns port 24800):
       osascript -e 'quit app "Barrier"'
  2. Grant permissions to  $BIN_DIR/kvm-daemon  in:
       System Settings → Privacy & Security → Accessibility
       System Settings → Privacy & Security → Input Monitoring
  3. Restart the agent so it picks up the permissions:
       launchctl kickstart -k gui/$UID_NUM/$LABEL
  4. Watch the log:
       tail -f /tmp/self-kvm-server.log

Uninstall:
  launchctl bootout gui/$UID_NUM/$LABEL ; rm "$PLIST"
DONE
