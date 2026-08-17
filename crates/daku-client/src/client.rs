use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use parking_lot::Mutex;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use daku_protocol::MAX_WIRE_MESSAGE_BYTES;
use daku_protocol::{
    ClientMessage, Command, PROTOCOL_VERSION, Request, ResponseOutcome, ResponsePayload, RpcError,
    ServerMessage,
};

const READ_POLL_INTERVAL: Duration = Duration::from_millis(25);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

enum Outgoing {
    Message(ClientMessage),
    Shutdown,
}

struct ClientInner {
    outgoing: Sender<Outgoing>,
    pending: Mutex<HashMap<Uuid, Sender<Result<ResponsePayload, RpcError>>>>,
    dashboard: Mutex<Vec<Sender<ServerMessage>>>,
    dashboard_cache: Mutex<BTreeMap<String, ServerMessage>>,
    disconnected: AtomicBool,
}

#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<ClientInner>,
}

impl DaemonClient {
    pub fn connect(address: &str, token: String) -> anyhow::Result<Self> {
        let url = daemon_url(address)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WIRE_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_WIRE_MESSAGE_BYTES));
        let (mut socket, _) =
            tungstenite::client::connect_with_config(url.as_str(), Some(config), 3)
                .context("could not connect to daku daemon")?;
        set_client_read_timeout(&mut socket, Some(Duration::from_secs(5)))?;
        write_json(
            &mut socket,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                token,
                client_id: Uuid::new_v4(),
            },
        )?;
        let hello = read_server_message(&mut socket)?;
        match hello {
            ServerMessage::Hello {
                protocol_version, ..
            } if protocol_version == PROTOCOL_VERSION => {}
            ServerMessage::Hello {
                protocol_version, ..
            } => bail!(
                "daemon protocol {protocol_version} does not match desktop protocol {PROTOCOL_VERSION}"
            ),
            ServerMessage::Rejected { message } => bail!("daemon rejected connection: {message}"),
            other => bail!("daemon sent an invalid handshake response: {other:?}"),
        }
        set_client_read_timeout(&mut socket, Some(READ_POLL_INTERVAL))?;

        let (outgoing, outgoing_rx) = unbounded();
        let inner = Arc::new(ClientInner {
            outgoing,
            pending: Mutex::new(HashMap::new()),
            dashboard: Mutex::new(Vec::new()),
            dashboard_cache: Mutex::new(BTreeMap::new()),
            disconnected: AtomicBool::new(false),
        });
        let thread_inner = inner.clone();
        std::thread::Builder::new()
            .name("daku-daemon-client".into())
            .spawn(move || run_client(socket, outgoing_rx, thread_inner))
            .context("could not start daku daemon client thread")?;
        Ok(Self { inner })
    }

    /// True once the reader thread has ended — the socket closed, the daemon
    /// shut down, or the connection broke. Supervisors poll this to reconnect.
    pub fn is_disconnected(&self) -> bool {
        self.inner.disconnected.load(Ordering::Acquire)
    }

    pub fn subscribe_dashboard(&self) -> Receiver<ServerMessage> {
        let (events, receiver) = unbounded();
        for message in self.inner.dashboard_cache.lock().values() {
            let _ = events.send(message.clone());
        }
        let mut dashboard = self.inner.dashboard.lock();
        // The reader thread flips `disconnected` and then clears this list
        // exactly once; a sender registered after that would never be dropped
        // and `recv()` would block forever instead of letting the caller move
        // to the next client. Checking under the lock closes both orderings.
        if !self.inner.disconnected.load(Ordering::Acquire) {
            dashboard.push(events);
        }
        receiver
    }

    pub fn request(&self, command: Command) -> anyhow::Result<ResponsePayload> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            bail!("daku daemon is disconnected");
        }
        let request_id = Uuid::new_v4();
        let (response, response_rx) = bounded(1);
        self.inner.pending.lock().insert(request_id, response);
        let message = ClientMessage::Request(Request {
            request_id,
            command,
        });
        if self
            .inner
            .outgoing
            .send(Outgoing::Message(message))
            .is_err()
        {
            self.inner.pending.lock().remove(&request_id);
            bail!("daku daemon connection is closed");
        }
        match response_rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(error)) => Err(anyhow!(error.message)),
            Err(error) => {
                self.inner.pending.lock().remove(&request_id);
                Err(anyhow!("timed out waiting for daku daemon: {error}"))
            }
        }
    }

    pub fn shutdown(&self) {
        let _ = self.inner.outgoing.send(Outgoing::Shutdown);
    }
}

fn daemon_url(address: &str) -> anyhow::Result<String> {
    let normalized = if address.starts_with("ws://") || address.starts_with("wss://") {
        address.to_owned()
    } else {
        format!("ws://{address}")
    };
    let mut url = url::Url::parse(&normalized).context("daku daemon address is invalid")?;
    url.set_path("/v1");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn run_client(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    outgoing: Receiver<Outgoing>,
    inner: Arc<ClientInner>,
) {
    'connection: loop {
        while let Ok(message) = outgoing.try_recv() {
            match message {
                Outgoing::Message(message) => {
                    if write_json(&mut socket, &message).is_err() {
                        break 'connection;
                    }
                }
                Outgoing::Shutdown => {
                    let _ = write_json(&mut socket, &ClientMessage::Shutdown);
                    let _ = socket.flush();
                    break 'connection;
                }
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(message) = serde_json::from_str::<ServerMessage>(text.as_ref()) else {
                    continue;
                };
                match message {
                    ServerMessage::Response {
                        request_id,
                        outcome,
                    } => {
                        if let Some(pending) = inner.pending.lock().remove(&request_id) {
                            let result = match outcome {
                                ResponseOutcome::Ok { payload } => Ok(payload),
                                ResponseOutcome::Error { error } => Err(error),
                            };
                            let _ = pending.send(result);
                        }
                    }
                    ServerMessage::EnvironmentsUpdated { .. }
                    | ServerMessage::SignalSnapshotsUpdated { .. }
                    | ServerMessage::SignalSamplesUpdated { .. } => {
                        if let Some(key) = message.dashboard_cache_key() {
                            inner.dashboard_cache.lock().insert(key, message.clone());
                        }
                        inner
                            .dashboard
                            .lock()
                            .retain(|subscriber| subscriber.send(message.clone()).is_ok());
                    }
                    ServerMessage::ShuttingDown => break,
                    ServerMessage::Hello { .. } | ServerMessage::Rejected { .. } => {}
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if retryable_io(&error) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break,
            Err(_) => break,
        }
    }

    inner.disconnected.store(true, Ordering::Release);
    let pending = std::mem::take(&mut *inner.pending.lock());
    for (_, response) in pending {
        let _ = response.send(Err(RpcError {
            message: "daku daemon disconnected".into(),
        }));
    }
    inner.dashboard.lock().clear();
    inner.dashboard_cache.lock().clear();
}

fn set_client_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        #[allow(unreachable_patterns)]
        _ => Ok(()),
    }
}

fn retryable_io(error: &io::Error) -> bool {
    retryable_error(error)
}

fn retryable_error(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
        ) {
            return true;
        }
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::EAGAIN)
            || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        {
            return true;
        }
    }
    error.source().is_some_and(retryable_error)
}

fn write_json<S: io::Read + io::Write, T: serde::Serialize>(
    socket: &mut WebSocket<S>,
    value: &T,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(value)?;
    socket.send(Message::Text(payload.into()))?;
    Ok(())
}

fn read_server_message(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) -> anyhow::Result<ServerMessage> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(serde_json::from_str(text.as_ref())?),
            Message::Ping(_) => socket.flush()?,
            Message::Close(_) => bail!("daku daemon closed during handshake"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_dashboard_after_disconnect_closes_immediately() {
        // A sender registered after the reader thread's cleanup would never be
        // dropped and the desktop would block on `recv()` forever.
        let (outgoing, _rx) = unbounded();
        let client = DaemonClient {
            inner: Arc::new(ClientInner {
                outgoing,
                pending: Mutex::new(HashMap::new()),
                dashboard: Mutex::new(Vec::new()),
                dashboard_cache: Mutex::new(BTreeMap::new()),
                disconnected: AtomicBool::new(true),
            }),
        };
        let receiver = client.subscribe_dashboard();
        assert!(receiver.recv().is_err());
        assert!(client.inner.dashboard.lock().is_empty());
    }

    #[test]
    fn daemon_endpoint_accepts_addresses_and_secure_urls() {
        assert_eq!(
            daemon_url("127.0.0.1:4312").unwrap(),
            "ws://127.0.0.1:4312/v1"
        );
        assert_eq!(
            daemon_url("wss://daku.example.test/old?ignored=1").unwrap(),
            "wss://daku.example.test/v1"
        );
    }

    #[test]
    fn protocol_dashboard_decodes_environments_updated() {
        let json = serde_json::json!({
            "type": "environmentsUpdated",
            "environments": [{
                "id": "prod",
                "label": "Production",
                "instanceUrl": "https://prod.example.service-now.com",
                "platformId": "servicenow",
                "health": "healthy",
                "reachability": "asleep",
                "lastObservedAt": 1_700_000_000
            }]
        });
        match serde_json::from_value::<ServerMessage>(json).unwrap() {
            ServerMessage::EnvironmentsUpdated { environments } => {
                assert_eq!(environments[0].id, "prod");
                assert_eq!(
                    environments[0].health,
                    daku_protocol::EnvironmentHealth::Healthy
                );
                assert_eq!(
                    environments[0].reachability,
                    daku_protocol::Reachability::Asleep
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn protocol_dashboard_decodes_signal_samples_updated() {
        let json = serde_json::json!({
            "type": "signalSamplesUpdated",
            "environmentId": "prod",
            "signalId": "jobs",
            "points": [
                { "observedAt": 10, "valueReal": 2.0 },
                { "observedAt": 20, "valueReal": null }
            ]
        });
        match serde_json::from_value::<ServerMessage>(json).unwrap() {
            ServerMessage::SignalSamplesUpdated {
                environment_id,
                signal_id,
                points,
            } => {
                assert_eq!(environment_id, "prod");
                assert_eq!(signal_id, "jobs");
                assert_eq!(points.len(), 2);
                assert_eq!(points[0].value_real, Some(2.0));
                assert_eq!(points[1].value_real, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
