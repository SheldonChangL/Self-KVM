//! Self-KVM Tauri application: a thin control surface over `kvm-daemon`.
//!
//! The GUI builds a [`ServerConfig`]/[`ClientConfig`] from a grid-based layout
//! editor and starts the matching runtime. Status flows back to the frontend as
//! `kvm://status` / `kvm://log` events.
//!
//! Threading notes:
//! * `rdev` capture runs on its own OS thread (it blocks); a shared sink slot
//!   lets a single capture thread feed whichever server runtime is current, so
//!   restarting/switching modes works.
//! * `enigo` injection handles are not `Send`, so injection runs on a dedicated
//!   thread reached through a channel ([`ThreadedInjector`]).

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use kvm_core::{ClientConfig, Edge, InputCommand, LocalEvent, ScreenLayout, ScreenSize, ServerConfig};
use kvm_daemon::hooks::LiveHooks;
use kvm_daemon::{ClientRuntime, ClientStatus, ServerRuntime, ServerStatus};
use kvm_input::enigo_backend::EnigoInjector;
use kvm_input::rdev_backend::RdevCapture;
use kvm_input::{EventSink, GrabSwitch, InputCapture, InputError, Injector};

type SinkSlot = Arc<Mutex<Option<mpsc::Sender<LocalEvent>>>>;

#[derive(Default)]
struct RunState {
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    capture_started: bool,
    mode: Option<String>,
}

struct AppState {
    inner: Mutex<RunState>,
    sink: SinkSlot,
    grab: GrabSwitch,
}

// --- frontend DTOs ----------------------------------------------------------

#[derive(Deserialize)]
struct ScreenDto {
    name: String,
    w: i32,
    h: i32,
    col: i32,
    row: i32,
}

#[derive(Deserialize)]
struct ServerSetup {
    bind: String,
    port: u16,
    local_screen: String,
    screens: Vec<ScreenDto>,
    #[serde(default)]
    tls: bool,
}

#[derive(Deserialize)]
struct ClientSetup {
    server_addr: String,
    name: String,
    width: i32,
    height: i32,
    #[serde(default)]
    tls: bool,
}

#[derive(Serialize, Clone)]
struct StatusEvent {
    kind: String,
    detail: String,
}

#[derive(Serialize)]
struct StateDto {
    running: bool,
    mode: Option<String>,
}

/// Build a [`ScreenLayout`] from grid cell positions: screens in adjacent cells
/// become left/right or top/bottom neighbours.
fn build_layout(screens: &[ScreenDto]) -> ScreenLayout {
    let mut l = ScreenLayout::new();
    for s in screens {
        l.add_screen(&s.name, ScreenSize::new(s.w.max(1), s.h.max(1)));
    }
    for a in screens {
        for b in screens {
            if a.name == b.name {
                continue;
            }
            if a.row == b.row && b.col == a.col + 1 {
                l.link(&a.name, Edge::Right, &b.name);
            }
            if a.col == b.col && b.row == a.row + 1 {
                l.link(&a.name, Edge::Bottom, &b.name);
            }
        }
    }
    l
}

/// Injector proxy that forwards commands to a dedicated thread owning the real
/// (non-`Send`) enigo handle.
struct ThreadedInjector {
    tx: std::sync::mpsc::Sender<InputCommand>,
}

impl ThreadedInjector {
    fn spawn() -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel::<InputCommand>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("kvm-inject".into())
            .spawn(move || {
                let mut enigo = match EnigoInjector::new() {
                    Ok(e) => {
                        let _ = ready_tx.send(Ok(()));
                        e
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                while let Ok(cmd) = rx.recv() {
                    if let Err(e) = enigo.inject(cmd) {
                        tracing::warn!("injection failed: {e}");
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Self { tx })
    }
}

impl Injector for ThreadedInjector {
    fn inject(&mut self, cmd: InputCommand) -> Result<(), InputError> {
        self.tx
            .send(cmd)
            .map_err(|e| InputError::Backend(e.to_string()))
    }
}

// --- commands ---------------------------------------------------------------

#[tauri::command]
async fn start_server(app: AppHandle, setup: ServerSetup) -> Result<(), String> {
    stop_inner(&app);

    let layout = build_layout(&setup.screens);
    if !layout.contains(&setup.local_screen) {
        return Err(format!(
            "this machine's screen {:?} is not in the layout",
            setup.local_screen
        ));
    }
    let config = ServerConfig {
        bind: setup.bind,
        port: setup.port,
        local_screen: setup.local_screen,
        layout,
        tls: setup.tls,
    };

    let runtime = ServerRuntime::bind(config).await.map_err(|e| e.to_string())?;
    let addr = runtime.local_addr();

    ensure_capture(&app)?;
    let (ev_tx, ev_rx) = mpsc::channel::<LocalEvent>(512);
    {
        let state = app.state::<AppState>();
        *state.sink.lock().unwrap() = Some(ev_tx);
    }
    let grab = app.state::<AppState>().grab.clone();
    let hooks = Box::new(LiveHooks::new(grab, Box::new(|_x, _y| {})));

    let (status_tx, status_rx) = mpsc::channel::<ServerStatus>(64);
    spawn_server_status_forwarder(app.clone(), status_rx);

    let handle = tauri::async_runtime::spawn(async move {
        if let Err(e) = runtime.run(ev_rx, hooks, status_tx).await {
            tracing::error!("server runtime ended: {e:#}");
        }
    });
    {
        let state = app.state::<AppState>();
        let mut rs = state.inner.lock().unwrap();
        rs.task = Some(handle);
        rs.mode = Some("server".into());
    }
    let _ = app.emit("kvm://log", format!("server listening on {addr}"));
    Ok(())
}

#[tauri::command]
async fn start_client(app: AppHandle, setup: ClientSetup) -> Result<(), String> {
    stop_inner(&app);

    let config = ClientConfig {
        server_addr: setup.server_addr,
        name: setup.name,
        screen: ScreenSize::new(setup.width.max(1), setup.height.max(1)),
        tls: setup.tls,
    };

    let injector: Box<dyn Injector> = match ThreadedInjector::spawn() {
        Ok(i) => Box::new(i),
        Err(e) => {
            let _ = app.emit(
                "kvm://log",
                format!("injector unavailable ({e}); grant Accessibility — injecting nothing"),
            );
            Box::new(kvm_input::NoopInjector)
        }
    };

    let (status_tx, status_rx) = mpsc::channel::<ClientStatus>(64);
    spawn_client_status_forwarder(app.clone(), status_rx);

    let handle = tauri::async_runtime::spawn(async move {
        if let Err(e) = ClientRuntime::run(config, injector, status_tx).await {
            tracing::error!("client runtime ended: {e:#}");
        }
    });
    {
        let state = app.state::<AppState>();
        let mut rs = state.inner.lock().unwrap();
        rs.task = Some(handle);
        rs.mode = Some("client".into());
    }
    Ok(())
}

#[tauri::command]
fn stop(app: AppHandle) {
    stop_inner(&app);
}

#[tauri::command]
fn get_state(app: AppHandle) -> StateDto {
    let state = app.state::<AppState>();
    let rs = state.inner.lock().unwrap();
    StateDto {
        running: rs.task.is_some(),
        mode: rs.mode.clone(),
    }
}

#[derive(Deserialize)]
struct FileSendSetup {
    /// Receiver address, `host:port`.
    to: String,
    /// Absolute path of the file to send.
    path: String,
    /// Optional basename override advertised to the receiver.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tls: bool,
}

/// Push a file to a peer over the dedicated bulk channel. Runs independently of
/// any active KVM session, so it never blocks input forwarding.
#[tauri::command]
async fn send_file(app: AppHandle, setup: FileSendSetup) -> Result<(), String> {
    let _ = app.emit("kvm://log", format!("sending {} → {}", setup.path, setup.to));
    kvm_daemon::send_file(
        &setup.to,
        std::path::Path::new(&setup.path),
        setup.name.as_deref(),
        kvm_daemon::SendOptions {
            chunk_size: kvm_core::file_transfer::DEFAULT_CHUNK_SIZE,
            tls: setup.tls,
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = app.emit("kvm://log", format!("sent {} → {}", setup.path, setup.to));
    Ok(())
}

// --- helpers ----------------------------------------------------------------

fn stop_inner(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut rs = state.inner.lock().unwrap();
        if let Some(h) = rs.task.take() {
            h.abort();
        }
        rs.mode = None;
    }
    *state.sink.lock().unwrap() = None;
    state.grab.set(false);
    let _ = app.emit(
        "kvm://status",
        StatusEvent {
            kind: "stopped".into(),
            detail: String::new(),
        },
    );
}

/// Start the (singleton) rdev capture thread if it isn't already running. It
/// feeds whichever server runtime currently owns the shared sink.
fn ensure_capture(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut rs = state.inner.lock().unwrap();
    if rs.capture_started {
        return Ok(());
    }
    let sink_slot = state.sink.clone();
    let grab = state.grab.clone();
    let sink: EventSink = Box::new(move |ev| {
        if let Some(tx) = sink_slot.lock().unwrap().as_ref() {
            let _ = tx.try_send(ev);
        }
    });
    RdevCapture::new()
        .start(sink, grab)
        .map_err(|e| e.to_string())?;
    rs.capture_started = true;
    Ok(())
}

fn spawn_server_status_forwarder(app: AppHandle, mut rx: mpsc::Receiver<ServerStatus>) {
    tauri::async_runtime::spawn(async move {
        while let Some(s) = rx.recv().await {
            let ev = match s {
                ServerStatus::Listening(a) => mk("listening", a.to_string()),
                ServerStatus::ClientConnected(n) => mk("client_connected", n),
                ServerStatus::ClientDisconnected(n) => mk("client_disconnected", n),
                ServerStatus::ActiveScreen(n) => mk("active_screen", n),
                ServerStatus::Grab(b) => mk("grab", b.to_string()),
            };
            let _ = app.emit("kvm://status", ev);
        }
    });
}

fn spawn_client_status_forwarder(app: AppHandle, mut rx: mpsc::Receiver<ClientStatus>) {
    tauri::async_runtime::spawn(async move {
        while let Some(s) = rx.recv().await {
            let ev = match s {
                ClientStatus::Connecting => mk("connecting", String::new()),
                ClientStatus::Connected => mk("connected", String::new()),
                ClientStatus::Entered => mk("entered", String::new()),
                ClientStatus::Left => mk("left", String::new()),
                ClientStatus::Disconnected(r) => mk("disconnected", r),
            };
            let _ = app.emit("kvm://status", ev);
        }
    });
}

fn mk(kind: &str, detail: String) -> StatusEvent {
    StatusEvent {
        kind: kind.to_string(),
        detail,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app_state = AppState {
        inner: Mutex::new(RunState::default()),
        sink: Arc::new(Mutex::new(None)),
        grab: GrabSwitch::new(),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            start_server,
            start_client,
            stop,
            get_state,
            send_file
        ])
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "Open Control Deck", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Self-KVM", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => reveal_main(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        reveal_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Self-KVM");
}

fn reveal_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}
