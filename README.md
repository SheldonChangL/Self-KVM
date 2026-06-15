# Self-KVM

A software KVM — share **one keyboard and mouse across multiple computers** over
the network, moving the cursor between machines by pushing it off a screen edge.
A from-scratch, independent re-implementation of the Synergy/Barrier idea in
**Rust + Tauri**, modelled on Barrier's wire protocol but written without
referencing its source.

> Move your cursor off the right edge of your desktop and it appears on your
> laptop — keyboard, mouse, and clipboard follow.

---

## What works today

| Capability | Status |
|---|---|
| Wire protocol (framing, handshake, key/mouse/clipboard messages) | ✅ implemented + unit tested |
| Length-framed TCP transport | ✅ tested over localhost |
| Screen layout, edge-crossing & coordinate mapping | ✅ tested |
| Server/client state machines (enter/leave, input forwarding, keep-alive) | ✅ tested |
| **End-to-end forwarding** (server → client, mock injection) | ✅ tested over real localhost TCP |
| TLS with trust-on-first-use SHA-256 fingerprints | ✅ tested over localhost |
| Config persistence, screen-lock hotkey | ✅ tested |
| Real input capture (`rdev` grab) + injection (`enigo`) | ✅ compiles; needs OS permissions to run |
| Clipboard sync (`arboard`) | ✅ compiles + mock tested |
| **Fast file transfer** (dedicated bulk channel, FTP-style `send`/`recv`) | ✅ tested end-to-end over real localhost TCP + TLS |
| Tauri GUI (control-deck UI, tray, live status) | ✅ builds; visually verified in demo mode |

**57 tests pass; the workspace builds with zero warnings.** The parts that need a
display or OS input permissions (real capture/injection, the GUI window) are
implemented and compile, but cannot be exercised headlessly — see
[Verification](#verification).

---

## Architecture

Pure logic is isolated from all I/O, which is what makes the protocol and
switching behaviour exhaustively testable without a display or permissions.

```
                 ┌──────────────────────────────────────────────┐
   keyboard ───▶ │  SERVER (primary)                            │
   + mouse       │  rdev grab ─▶ ServerMachine ─▶ messages ──────┼──┐
                 │  (suppress local input while on a remote)     │  │  TCP (+TLS)
                 └──────────────────────────────────────────────┘  │
                                                                     ▼
                 ┌──────────────────────────────────────────────┐
                 │  CLIENT (secondary)                           │
                 │  messages ─▶ ClientMachine ─▶ enigo injection │ ─▶ keyboard + mouse
                 └──────────────────────────────────────────────┘
```

### Crate map

| Crate | Responsibility | I/O? |
|---|---|---|
| `kvm-proto` | Message types, big-endian codec, key tables | none (pure) |
| `kvm-core` | Screen layout, edge detection, server/client state machines, config | none (pure) |
| `kvm-net` | Async length-framed transport; TLS + TOFU fingerprints (`tls` feature) | tokio |
| `kvm-input` | Capture/injection/clipboard traits + mock; `rdev`/`enigo`/`arboard` (`backends`) | OS |
| `kvm-daemon` | Wires it all into runnable server/client + headless CLI | all |
| `src-tauri` + `ui` | Tauri 2 desktop app — the "Control Deck" GUI | all |

### How cursor switching works

1. The server holds a layout of every screen (its own + each client's) and where
   they border one another.
2. While the cursor is on the **local** screen the OS owns it; the server only
   watches for it reaching an edge with a neighbour.
3. On crossing, the server **grabs** local input (so this machine stops
   reacting), sends `Enter` to the target client, and starts forwarding events
   against a *virtual* cursor it tracks from relative motion.
4. When the virtual cursor crosses back, the server releases the grab and warps
   the real cursor home.

Enter messages carry a **sequence number** so stale/out-of-order replies are
discarded during fast switching, and a `CIAK` gate makes the client ignore moves
until it has acknowledged its geometry — both lifted from the reference design.

---

## Fast file transfer

Beyond keyboard/mouse, Self-KVM can push whole files between machines — think
`scp`/FTP-put, not a mounted share. The design choice that matters: file bytes
travel on their **own dedicated TCP (and optionally TLS) connection**, entirely
separate from the keyboard/mouse control connection. A single TCP stream is one
ordered byte sequence, so a multi-gigabyte file sent over the control link would
head-of-line-block the input frames queued behind it and make the cursor
stutter. A second connection lets a transfer saturate the link while input keeps
flowing untouched.

Files are split into chunks (`FileOffer → FileAccept → N×FileChunk → FileEnd`),
streamed straight off disk, and reassembled with strict validation: in-order
chunks, a declared-size ceiling, and a final chunk count — a truncated,
reordered, or oversized transfer is rejected, never written as a corrupt file.

```bash
# On the receiver — saves into ./inbox (binds loopback by default)
cargo run -p kvm-daemon -- recv --dir ./inbox

# On the sender — push a file (optionally rename it on arrival)
cargo run -p kvm-daemon -- send --to 192.168.1.10:24801 --file ./movie.mkv --name clip.mkv

# Encrypted, with the same TOFU fingerprint model as the control channel
cargo run -p kvm-daemon --features tls -- recv --dir ./inbox --tls
cargo run -p kvm-daemon --features tls -- send --to 192.168.1.10:24801 --file ./secret.pdf --tls
```

The bulk channel defaults to the control port + 1 (`24801`). The receiver writes
to a temp file and atomically renames it into place only after the whole
transfer validates, so it never clobbers or follows a symlink at the
destination, and a failed transfer leaves nothing behind.

> **Trust model:** the receive path is *unauthenticated* — anyone who can reach
> the port may push a (size-limited, name-sanitised) file. It therefore binds
> **loopback by default**; only bind a public interface on a trusted network.
> `--tls` encrypts the channel and pins the receiver's fingerprint (TOFU) but
> does not yet authenticate the *sender*. See [Security notes](#security-notes).

## Build & run

Prerequisites: Rust ≥ 1.80, Node ≥ 18 (for the GUI).

### Headless CLI (the engine, no GUI)

```bash
# Core library + protocol tests
cargo test

# Run a server (shares this machine). Needs --features real-input to actually
# capture input (and Accessibility / Input Monitoring permission on macOS).
cargo run -p kvm-daemon --features real-input -- server --port 24800

# Run a client on another machine
cargo run -p kvm-daemon --features real-input -- \
    client --server 192.168.1.10:24800 --name laptop --width 1280 --height 800
```

Feature flags on `kvm-daemon`: `real-input` (rdev+enigo), `tls`, `clipboard`.

### Desktop app (the Control Deck)

```bash
cd ui && npm install && npm run build   # build the frontend
cd ../src-tauri && cargo build           # build the app (real-input + tls + clipboard)
# or, for live development:
cargo tauri dev                          # if tauri-cli is installed
```

The frontend also runs standalone in a browser (`cd ui && npm run dev`) in a
**demo mode** that simulates the backend — handy for previewing the UI.

---

## Verification

This was built test-first with a deliberate separation so that the meaningful
logic is provable without hardware:

- `kvm-proto`, `kvm-core`, `kvm-net` are pure/tokio and **fully unit + integration
  tested** (codec round-trips, edge crossings, state machines, framed transport,
  TLS handshake + fingerprint pinning).
- The **end-to-end test** (`kvm-daemon`) starts a real server and client over
  localhost TCP and asserts that a mock injector receives exactly the right
  commands as the cursor crosses, clicks, and types — proving the full pipeline.
- Input capture/injection and the GUI window need a display and OS permissions,
  so they are implemented and **compile-verified** but not run in CI. The GUI was
  visually verified in browser demo mode.

```
$ cargo test
   kvm-proto   12 passed
   kvm-net      3 passed  (+2 with --features tls)
   kvm-core    32 passed  (incl. file chunking/reassembly + filename sanitisation)
   kvm-input    1 passed  (+1 with --features backends)
   kvm-daemon   8 passed  (end-to-end input forwarding + file transfer over real sockets)
```

The file-transfer tests start a real receiver over localhost TCP and assert the
received bytes match the source exactly, including a multi-chunk file, a hostile
`../../` filename contained to the download dir, an over-limit offer being
declined, an empty file, a mid-stream disconnect leaving no partial file, and an
out-of-order chunk being rejected.

---

## Security notes

TLS is layered *under* the message protocol: the handshake and fingerprint check
complete before any protocol traffic is accepted. The trust model is TOFU — the
server presents a self-signed certificate and the client pins its SHA-256
fingerprint on first connection, rejecting any later change. There is no CA chain.

**File transfer** hardens the receive path against a hostile sender: filenames
are reduced to a safe base name (path traversal, drive prefixes, dotfiles,
control characters and reserved device names are rejected), a declared-size
ceiling (default 10 GiB) and per-chunk accounting bound disk use, empty chunks
are refused, an idle timeout aborts stalled transfers, concurrent transfers are
capped, and files are published via a non-clobbering atomic rename. Known
limitations (see roadmap): the receive path is **not sender-authenticated**
(bind loopback, or a trusted network), and the receiver's TLS certificate is
**regenerated per run**, so a pinned fingerprint goes stale across restarts.

---

## Roadmap toward full Barrier parity

- Wire the server→client *and* client→server clipboard polling end-to-end (the
  bridge + protocol exist; server broadcast + client apply are wired).
- File transfer: drag-and-drop in the GUI (the `send_file` command exists; the
  drop-zone UI is the remaining piece), sender authentication (mutual-TLS /
  shared token), a persisted receiver certificate so TOFU pins survive restarts,
  and an optional content hash in `FileEnd` for end-to-end integrity.
- Per-platform key-map refinement (dead keys, IME, media keys).
- Persisted server certificate + a GUI fingerprint-trust prompt.
- Wayland capture/inject hardening (evdev/libei paths).

## License

MIT.
