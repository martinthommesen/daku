//! Loopback integration tests for the daemon's trust boundary and the
//! desktop→daemon path: Hello auth, protocol version, `Origin`/path checks,
//! request round-trips, dashboard fan-out, and disconnect handling.

use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::bail;
use crossbeam_channel::{Receiver, Sender, unbounded};
use daku_client::DaemonClient;
use daku_core::{Backend, ServerOptions, serve};
use daku_protocol::{ClientMessage, Command, PROTOCOL_VERSION, ResponsePayload, ServerMessage};
use tungstenite::client::ClientRequestBuilder;
use tungstenite::http::StatusCode;
use uuid::Uuid;

const TOKEN: &str = "loopback-test-token";
const WAIT: Duration = Duration::from_secs(5);

struct TestBackend;

impl Backend for TestBackend {
    fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload> {
        match command {
            Command::Ping => Ok(ResponsePayload::Ack),
            Command::GetSettings => Ok(ResponsePayload::Settings {
                settings: Default::default(),
            }),
            _ => bail!("unexpected command"),
        }
    }
}

struct Daemon {
    address: String,
    shutdown: Arc<AtomicBool>,
    dashboard: Sender<ServerMessage>,
    thread: Option<JoinHandle<anyhow::Result<()>>>,
}

impl Daemon {
    fn start(allow_shutdown: bool, allowed_origins: &[&str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (dashboard, dashboard_rx) = unbounded();
        let options = ServerOptions {
            allowed_origins: allowed_origins
                .iter()
                .map(|origin| (*origin).to_owned())
                .collect::<HashSet<_>>(),
            allow_shutdown,
        };
        let thread = {
            let shutdown = shutdown.clone();
            std::thread::spawn(move || {
                serve(
                    listener,
                    TOKEN.to_owned(),
                    Arc::new(TestBackend),
                    shutdown,
                    options,
                    Some(dashboard_rx),
                )
            })
        };
        Self {
            address,
            shutdown,
            dashboard,
            thread: Some(thread),
        }
    }

    fn connect(&self) -> DaemonClient {
        DaemonClient::connect(&self.address, TOKEN.to_owned()).unwrap()
    }

    fn url(&self, path: &str) -> String {
        format!("ws://{}{path}", self.address)
    }

    fn stop(mut self) -> anyhow::Result<()> {
        self.shutdown.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn recv_within(receiver: &Receiver<ServerMessage>, timeout: Duration) -> Option<ServerMessage> {
    receiver.recv_timeout(timeout).ok()
}

#[test]
fn wrong_token_is_rejected_at_hello() {
    let daemon = Daemon::start(false, &[]);
    let Err(error) = DaemonClient::connect(&daemon.address, "nope".to_owned()) else {
        panic!("a wrong token must not connect");
    };
    assert!(
        error.to_string().contains("authentication failed"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn correct_token_gets_ack_and_settings() {
    let daemon = Daemon::start(false, &[]);
    let client = daemon.connect();
    let ack = client.request(Command::Ping).unwrap();
    assert!(matches!(ack, ResponsePayload::Ack), "unexpected {ack:?}");
    let settings = client.request(Command::GetSettings).unwrap();
    assert!(
        matches!(settings, ResponsePayload::Settings { .. }),
        "unexpected {settings:?}"
    );
}

#[test]
fn unknown_path_is_404() {
    let daemon = Daemon::start(false, &[]);
    match tungstenite::connect(daemon.url("/nope")) {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        other => panic!("expected an HTTP 404, got {other:?}"),
    }
}

#[test]
fn disallowed_origin_is_403_and_allowlisted_origin_upgrades() {
    let daemon = Daemon::start(false, &["http://allowed.test"]);
    let evil = ClientRequestBuilder::new(daemon.url("/v1").parse().unwrap())
        .with_header("Origin", "http://evil.test");
    match tungstenite::connect(evil) {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        other => panic!("expected an HTTP 403, got {other:?}"),
    }

    let allowed = ClientRequestBuilder::new(daemon.url("/v1").parse().unwrap())
        .with_header("Origin", "http://allowed.test");
    let (mut socket, response) = tungstenite::connect(allowed).unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let _ = socket.close(None);
}

#[test]
fn shutdown_is_rejected_unless_allowed() {
    let daemon = Daemon::start(false, &[]);
    daemon.connect().shutdown();
    let client = daemon.connect();
    let ack = client.request(Command::Ping).unwrap();
    assert!(matches!(ack, ResponsePayload::Ack), "unexpected {ack:?}");

    let daemon = Daemon::start(true, &[]);
    daemon.connect().shutdown();
    let deadline = Instant::now() + WAIT;
    while !daemon.shutdown.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "daemon ignored an allowed shutdown"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(daemon.stop().is_ok());
}

#[test]
fn dashboard_events_reach_subscribers() {
    let daemon = Daemon::start(false, &[]);
    let client = daemon.connect();
    let events = client.subscribe_dashboard();
    daemon
        .dashboard
        .send(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        })
        .unwrap();
    let message = recv_within(&events, WAIT);
    assert!(
        matches!(message, Some(ServerMessage::EnvironmentsUpdated { .. })),
        "unexpected {message:?}"
    );
}

#[test]
fn daemon_shutdown_disconnects_client() {
    let daemon = Daemon::start(false, &[]);
    let client = daemon.connect();
    let events = client.subscribe_dashboard();
    daemon.stop().unwrap();

    let deadline = Instant::now() + WAIT;
    let error = loop {
        match client.request(Command::Ping) {
            Err(error) => break error,
            Ok(payload) => assert!(
                Instant::now() < deadline,
                "client stayed connected after the daemon stopped; last {payload:?}"
            ),
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let message = error.to_string();
    assert!(
        message.contains("disconnected") || message.contains("closed"),
        "unexpected error: {error:#}"
    );
    assert!(events.recv_timeout(WAIT).is_err());
}

#[test]
fn wrong_protocol_version_is_rejected() {
    let daemon = Daemon::start(false, &[]);
    let (mut socket, _) = tungstenite::connect(daemon.url("/v1")).unwrap();
    let hello = serde_json::to_string(&ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION + 1,
        token: TOKEN.to_owned(),
        client_id: Uuid::new_v4(),
    })
    .unwrap();
    socket
        .send(tungstenite::Message::Text(hello.into()))
        .unwrap();
    let reply = match socket.read().unwrap() {
        tungstenite::Message::Text(text) => {
            serde_json::from_str::<ServerMessage>(text.as_ref()).unwrap()
        }
        other => panic!("expected a text reply, got {other:?}"),
    };
    assert!(
        matches!(reply, ServerMessage::Rejected { ref message } if message.contains("unsupported")),
        "unexpected {reply:?}"
    );
}
