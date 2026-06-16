# Packaging — run Self-KVM as an auto-starting service

These installers turn Self-KVM from a "type a command every time" tool into a
background service: **set it up once, and both machines auto-connect on boot.**
No CLI, no GUI, no minding the start order.

| Machine | Role | Mechanism | Auto-reconnect |
|---|---|---|---|
| macOS | server (primary) | `launchd` LaunchAgent, `KeepAlive` | restarts on crash / at login |
| Ubuntu | client (secondary) | `systemd --user`, `Restart=always` | retries every 2s until the server is up; reconnects after a drop |

## macOS (server)

```bash
bash packaging/install-macos-server.sh
```

Then, one time:
1. **Quit Barrier** — it owns port 24800: `osascript -e 'quit app "Barrier"'`
2. Grant `~/.local/bin/kvm-daemon` **Accessibility** + **Input Monitoring**
   (System Settings → Privacy & Security).
3. Restart the agent: `launchctl kickstart -k gui/$(id -u)/com.self-kvm.server`

Logs: `tail -f /tmp/self-kvm-server.log` · Uninstall:
`launchctl bootout gui/$(id -u)/com.self-kvm.server ; rm ~/Library/LaunchAgents/com.self-kvm.server.plist`

## Ubuntu (client)

```bash
bash packaging/install-linux-client.sh 192.168.161.44:24800 ubuntu
```

Requirements:
- **Xorg session, not Wayland** (`echo $XDG_SESSION_TYPE` → `x11`). Pick
  "Ubuntu on Xorg" at the login screen.
- Stop Barrier on this machine (it would fight for input).
- Build deps: `build-essential pkg-config libx11-dev libxi-dev libxtst-dev
  libxdo-dev libxkbcommon-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev
  libxcb-xfixes0-dev`.

Status `systemctl --user status self-kvm-client` · Logs
`journalctl --user -u self-kvm-client -f` · Uninstall
`systemctl --user disable --now self-kvm-client ; rm ~/.config/systemd/user/self-kvm-client.service`

To keep the client running when you're logged out: `sudo loginctl enable-linger $USER`.

## Notes

- Resolutions are auto-detected; the layout config only declares adjacency.
- The transport is **plaintext** — use only on a trusted LAN.
- Self-KVM and Barrier are **not** wire-compatible; both ends must run Self-KVM.
