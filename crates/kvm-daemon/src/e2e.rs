//! End-to-end forwarding test over real localhost TCP with a mock injector.
//!
//! This is the proof that the whole pipeline works: a server crosses the cursor
//! onto a client, then forwards motion / clicks / keystrokes; the client's
//! state machine injects exactly the expected commands — verified without a
//! display or input permissions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kvm_core::{
    ClientConfig, Edge, InputCommand, KeyAction, LocalEvent, ScreenLayout, ScreenSize, ServerConfig,
};
use kvm_input::{Injector, MockInjector};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

use crate::{ClientRuntime, ClientStatus, RecordingHooks, ServerRuntime, ServerStatus};

async fn wait_server(rx: &mut mpsc::Receiver<ServerStatus>, want: ServerStatus) {
    timeout(Duration::from_secs(5), async {
        while let Some(s) = rx.recv().await {
            if s == want {
                return;
            }
        }
        panic!("server status stream ended before {want:?}");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for server status {want:?}"));
}

async fn wait_client(rx: &mut mpsc::Receiver<ClientStatus>, want: ClientStatus) {
    timeout(Duration::from_secs(5), async {
        while let Some(s) = rx.recv().await {
            if s == want {
                return;
            }
        }
        panic!("client status stream ended before {want:?}");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for client status {want:?}"));
}

async fn poll_until_len(
    log: &Arc<Mutex<Vec<InputCommand>>>,
    n: usize,
    dur: Duration,
) -> Vec<InputCommand> {
    timeout(dur, async {
        loop {
            {
                let g = log.lock().unwrap();
                if g.len() >= n {
                    return g.clone();
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for injected commands")
}

#[tokio::test]
async fn server_forwards_input_to_client_end_to_end() {
    // Layout: srv(1920x1080) --right--> lap(1280x800).
    let mut layout = ScreenLayout::new();
    layout.add_screen("srv", ScreenSize::new(1920, 1080));
    layout.add_screen("lap", ScreenSize::new(1280, 800));
    layout.link("srv", Edge::Right, "lap");

    let server_cfg = ServerConfig {
        bind: "127.0.0.1".into(),
        port: 0,
        local_screen: "srv".into(),
        layout,
        tls: false,
    };
    let server = ServerRuntime::bind(server_cfg).await.unwrap();
    let addr = server.local_addr();

    // Capture source + recording hooks.
    let (ev_tx, ev_rx) = mpsc::channel::<LocalEvent>(64);
    let (sstat_tx, mut sstat_rx) = mpsc::channel::<ServerStatus>(64);
    let hooks = RecordingHooks::new();
    let hooks_log = hooks.log();
    tokio::spawn(async move {
        server.run(ev_rx, Box::new(hooks), sstat_tx).await.unwrap();
    });

    // Client "lap" with a mock injector recording everything.
    let injector = MockInjector::new();
    let inj_log = injector.log();
    let client_cfg = ClientConfig {
        server_addr: addr.to_string(),
        name: "lap".into(),
        screen: ScreenSize::new(1280, 800),
        tls: false,
    };
    let (cstat_tx, mut cstat_rx) = mpsc::channel::<ClientStatus>(64);
    tokio::spawn(async move {
        ClientRuntime::run(client_cfg, Box::new(injector) as Box<dyn Injector>, cstat_tx)
            .await
            .unwrap();
    });

    // Barrier: the client must be acked (Connected) AND registered in the
    // server's controller before we feed input, so nothing races/drops.
    wait_client(&mut cstat_rx, ClientStatus::Connected).await;
    wait_server(&mut sstat_rx, ServerStatus::ClientConnected("lap".into())).await;

    // Drive the server: cross the right edge into lap, move, click, type.
    ev_tx
        .send(LocalEvent::MotionAbs { x: 1920, y: 540 })
        .await
        .unwrap(); // enter lap at (0, 400)
    ev_tx
        .send(LocalEvent::MotionRel { dx: 100, dy: 50 })
        .await
        .unwrap(); // -> (100, 450)
    ev_tx
        .send(LocalEvent::Button {
            button: 1,
            pressed: true,
        })
        .await
        .unwrap();
    ev_tx
        .send(LocalEvent::Button {
            button: 1,
            pressed: false,
        })
        .await
        .unwrap();
    ev_tx
        .send(LocalEvent::Key {
            id: 0x41, // 'A'
            mask: 0,
            button: 30,
            action: KeyAction::Down,
        })
        .await
        .unwrap();

    let recorded = poll_until_len(&inj_log, 5, Duration::from_secs(5)).await;
    assert_eq!(
        recorded,
        vec![
            InputCommand::MouseMoveAbs { x: 0, y: 400 }, // from Enter
            InputCommand::MouseMoveAbs { x: 100, y: 450 },
            InputCommand::MouseButton {
                button: 1,
                pressed: true
            },
            InputCommand::MouseButton {
                button: 1,
                pressed: false
            },
            InputCommand::Key {
                id: 0x41,
                mask: 0,
                button: 30,
                action: KeyAction::Down
            },
        ]
    );

    // The server grabbed local input exactly once, on entering the client.
    assert_eq!(hooks_log.lock().unwrap().grabs, vec![true]);

    // Keep the capture channel alive until the assertions are done.
    drop(ev_tx);
}

#[tokio::test]
async fn client_with_unknown_name_is_rejected() {
    let mut layout = ScreenLayout::new();
    layout.add_screen("srv", ScreenSize::new(1920, 1080));
    let server_cfg = ServerConfig {
        bind: "127.0.0.1".into(),
        port: 0,
        local_screen: "srv".into(),
        layout,
        tls: false,
    };
    let server = ServerRuntime::bind(server_cfg).await.unwrap();
    let addr = server.local_addr();

    let (ev_tx, ev_rx) = mpsc::channel::<LocalEvent>(8);
    let (sstat_tx, _sstat_rx) = mpsc::channel::<ServerStatus>(8);
    tokio::spawn(async move {
        server
            .run(ev_rx, Box::new(crate::NoopHooks), sstat_tx)
            .await
            .unwrap();
    });

    let client_cfg = ClientConfig {
        server_addr: addr.to_string(),
        name: "stranger".into(), // not in the layout
        screen: ScreenSize::new(1024, 768),
        tls: false,
    };
    let (cstat_tx, mut cstat_rx) = mpsc::channel::<ClientStatus>(8);
    tokio::spawn(async move {
        let _ = ClientRuntime::run(
            client_cfg,
            Box::new(kvm_input::NoopInjector) as Box<dyn Injector>,
            cstat_tx,
        )
        .await;
    });

    // The client should be told it is unknown and disconnect.
    let disconnected = timeout(Duration::from_secs(5), async {
        while let Some(s) = cstat_rx.recv().await {
            if let ClientStatus::Disconnected(reason) = s {
                return reason;
            }
        }
        panic!("client never disconnected");
    })
    .await
    .expect("timed out");
    assert!(
        disconnected.contains("UnknownToServer"),
        "unexpected reason: {disconnected}"
    );
    drop(ev_tx);
}
