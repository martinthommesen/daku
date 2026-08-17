use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use daku_protocol::{
    ClientMessage, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, ReplayCursor, Request,
    ResponseOutcome, ResponsePayload, RpcError, SequencedEvent, ServerMessage, WireDriverEvent,
};
use parking_lot::Mutex as ParkingMutex;
use subtle::ConstantTimeEq as _;
use tungstenite::handshake::server::{
    ErrorResponse, Request as HandshakeRequest, Response as HandshakeResponse,
};
use tungstenite::http::{StatusCode, header::ORIGIN};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket, accept_hdr_with_config};
use uuid::Uuid;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_HANDSHAKE_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;
const MAX_REPLAY_EVENTS_PER_SESSION: usize = 4096;
const MAX_CACHED_RESPONSES: usize = 2048;

#[derive(Clone, Debug, Default)]
pub struct ServerOptions {
    pub allowed_origins: HashSet<String>,
    pub allow_shutdown: bool,
}

struct ConnectionPermit(Arc<AtomicUsize>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub trait Backend: Send + Sync + 'static {
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload>;

    fn shutdown(&self) {}
}

#[derive(Clone)]
pub struct EventSink {
    session_id: Uuid,
    runtime_id: Uuid,
    hub: Arc<Hub>,
}

impl EventSink {
    pub fn send(&self, event: WireDriverEvent) -> anyhow::Result<()> {
        self.hub.emit(self.session_id, self.runtime_id, event, true);
        Ok(())
    }

    pub fn send_ephemeral(&self, event: WireDriverEvent) -> anyhow::Result<()> {
        self.hub
            .emit(self.session_id, self.runtime_id, event, false);
        Ok(())
    }
}

#[derive(Default)]
struct HubState {
    next_subscriber_id: u64,
    task_state_revision: u64,
    subscribers: HashMap<u64, Sender<ServerMessage>>,
    active_runtimes: HashMap<Uuid, Uuid>,
    next_sequences: HashMap<(Uuid, Uuid), u64>,
    journal: HashMap<(Uuid, Uuid), VecDeque<SequencedEvent>>,
    responses: VecDeque<(Uuid, ResponseOutcome)>,
}

struct Hub {
    epoch: Uuid,
    state: ParkingMutex<HubState>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            epoch: Uuid::new_v4(),
            state: ParkingMutex::new(HubState::default()),
        }
    }
}

impl Hub {
    fn event_sink(self: &Arc<Self>, session_id: Uuid, runtime_id: Uuid) -> EventSink {
        EventSink {
            session_id,
            runtime_id,
            hub: self.clone(),
        }
    }

    fn emit(&self, session_id: Uuid, runtime_id: Uuid, event: WireDriverEvent, replayable: bool) {
        let mut state = self.state.lock();
        if state.active_runtimes.get(&session_id) != Some(&runtime_id) {
            return;
        }
        let sequence = state
            .next_sequences
            .entry((session_id, runtime_id))
            .or_default();
        *sequence = sequence.saturating_add(1);
        let event = SequencedEvent {
            session_id,
            runtime_id,
            epoch: self.epoch,
            sequence: *sequence,
            event,
        };
        if replayable {
            let journal = state.journal.entry((session_id, runtime_id)).or_default();
            journal.push_back(event.clone());
            while journal.len() > MAX_REPLAY_EVENTS_PER_SESSION {
                journal.pop_front();
            }
        }
        let message = ServerMessage::Event(event);
        state
            .subscribers
            .retain(|_, subscriber| subscriber.send(message.clone()).is_ok());
    }

    fn broadcast(&self, message: ServerMessage) {
        let mut state = self.state.lock();
        state
            .subscribers
            .retain(|_, subscriber| subscriber.send(message.clone()).is_ok());
    }

    fn subscribe(&self, resume_from: &[ReplayCursor], sender: Sender<ServerMessage>) -> u64 {
        let mut state = self.state.lock();
        for (&(session_id, runtime_id), events) in &state.journal {
            let sequence = resume_from
                .iter()
                .find(|cursor| {
                    cursor.session_id == session_id
                        && cursor.runtime_id == runtime_id
                        && cursor.epoch == self.epoch
                })
                .map(|cursor| cursor.sequence)
                .unwrap_or_default();
            for event in events.iter().filter(|event| event.sequence > sequence) {
                let _ = sender.send(ServerMessage::Event(event.clone()));
            }
        }
        let id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.saturating_add(1);
        state.subscribers.insert(id, sender);
        id
    }

    fn unsubscribe(&self, subscriber_id: u64) {
        self.state.lock().subscribers.remove(&subscriber_id);
    }

    fn task_state_changed(&self, source_subscriber_id: u64) {
        let mut state = self.state.lock();
        state.task_state_revision = state.task_state_revision.saturating_add(1);
        let message = ServerMessage::TaskStateChanged {
            revision: state.task_state_revision,
        };
        state.subscribers.retain(|subscriber_id, subscriber| {
            *subscriber_id == source_subscriber_id || subscriber.send(message.clone()).is_ok()
        });
    }

    fn cached_response(&self, request_id: Uuid) -> Option<ResponseOutcome> {
        self.state
            .lock()
            .responses
            .iter()
            .rev()
            .find_map(|(cached_id, outcome)| (*cached_id == request_id).then(|| outcome.clone()))
    }

    fn cache_response(&self, request_id: Uuid, outcome: ResponseOutcome) {
        let mut state = self.state.lock();
        state.responses.push_back((request_id, outcome));
        while state.responses.len() > MAX_CACHED_RESPONSES {
            state.responses.pop_front();
        }
    }
}

pub fn serve(
    listener: TcpListener,
    auth: String,
    backend: Arc<dyn Backend>,
    shutdown: Arc<AtomicBool>,
    options: ServerOptions,
    dashboard_events: Option<Receiver<ServerMessage>>,
) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("could not configure daku daemon listener")?;
    let hub = Arc::new(Hub::default());
    if let Some(dashboard_events) = dashboard_events {
        let hub = hub.clone();
        let shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("daku-dashboard".into())
            .spawn(move || {
                while !shutdown.load(Ordering::Acquire) {
                    match dashboard_events.recv_timeout(Duration::from_millis(25)) {
                        Ok(message) => hub.broadcast(message),
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .context("could not start daku dashboard thread")?;
    }
    let options = Arc::new(options);
    let active_connections = Arc::new(AtomicUsize::new(0));
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if active_connections
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                        (active < MAX_CONNECTIONS).then_some(active + 1)
                    })
                    .is_err()
                {
                    continue;
                }
                let connection_permit = ConnectionPermit(active_connections.clone());
                let auth = auth.clone();
                let backend = backend.clone();
                let hub = hub.clone();
                let shutdown = shutdown.clone();
                let options = options.clone();
                std::thread::Builder::new()
                    .name("daku-daemon-connection".into())
                    .spawn(move || {
                        let _connection_permit = connection_permit;
                        if let Err(error) =
                            handle_connection(stream, &auth, backend, hub, shutdown, &options)
                        {
                            eprintln!("daku-daemon connection ended: {error:#}");
                        }
                    })
                    .context("could not start daku daemon connection thread")?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("daku daemon listener failed"),
        }
    }
    backend.shutdown();
    Ok(())
}

fn handle_connection(
    stream: TcpStream,
    expected_token: &str,
    backend: Arc<dyn Backend>,
    hub: Arc<Hub>,
    shutdown: Arc<AtomicBool>,
    options: &ServerOptions,
) -> anyhow::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_HANDSHAKE_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_HANDSHAKE_MESSAGE_BYTES));
    let allowed_origins = options.allowed_origins.clone();
    let mut socket = accept_hdr_with_config(
        stream,
        move |request: &HandshakeRequest, response: HandshakeResponse| {
            validate_handshake(request, response, &allowed_origins)
        },
        Some(config),
    )
    .context("WebSocket handshake failed")?;
    let hello = read_client_message(&mut socket)?;
    let resume_from = match hello {
        ClientMessage::Hello {
            protocol_version,
            token,
            resume_from,
            ..
        } if protocol_version == PROTOCOL_VERSION && token_matches(expected_token, &token) => {
            resume_from
        }
        ClientMessage::Hello {
            protocol_version, ..
        } if protocol_version != PROTOCOL_VERSION => {
            write_json(
                &mut socket,
                &ServerMessage::Rejected {
                    message: format!(
                        "protocol {protocol_version} is unsupported; expected {PROTOCOL_VERSION}"
                    ),
                },
            )?;
            return Ok(());
        }
        ClientMessage::Hello { .. } => {
            write_json(
                &mut socket,
                &ServerMessage::Rejected {
                    message: "authentication failed".into(),
                },
            )?;
            return Ok(());
        }
        _ => bail!("first daemon message was not a hello"),
    };
    write_json(
        &mut socket,
        &ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").into(),
        },
    )?;
    socket.set_config(|config| {
        config.max_message_size = Some(MAX_WIRE_MESSAGE_BYTES);
        config.max_frame_size = Some(MAX_WIRE_MESSAGE_BYTES);
    });
    socket
        .get_mut()
        .set_read_timeout(Some(SOCKET_POLL_INTERVAL))?;

    let (outgoing, outgoing_rx) = unbounded();
    let subscriber_id = hub.subscribe(&resume_from, outgoing.clone());

    'connection: while !shutdown.load(Ordering::Acquire) {
        while let Ok(message) = outgoing_rx.try_recv() {
            if write_json(&mut socket, &message).is_err() {
                break 'connection;
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => match serde_json::from_str(text.as_ref()) {
                Ok(ClientMessage::Request(request)) => {
                    dispatch_request(
                        request,
                        outgoing.clone(),
                        subscriber_id,
                        backend.clone(),
                        hub.clone(),
                    );
                }
                Ok(ClientMessage::Shutdown) => {
                    if options.allow_shutdown {
                        write_json(&mut socket, &ServerMessage::ShuttingDown)?;
                        shutdown.store(true, Ordering::Release);
                        break;
                    }
                    write_json(
                        &mut socket,
                        &ServerMessage::Rejected {
                            message: "daemon shutdown is managed by its service owner".into(),
                        },
                    )?;
                }
                Ok(ClientMessage::Hello { .. }) => {}
                Err(error) => {
                    eprintln!("daku-daemon ignored invalid message: {error}");
                }
            },
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if retryable_io(&error) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break,
            Err(error) => return Err(error).context("daku daemon WebSocket failed"),
        }
    }
    hub.unsubscribe(subscriber_id);
    Ok(())
}

fn dispatch_request(
    request: Request,
    outgoing: Sender<ServerMessage>,
    source_subscriber_id: u64,
    backend: Arc<dyn Backend>,
    hub: Arc<Hub>,
) {
    std::thread::Builder::new()
        .name("daku-daemon-request".into())
        .spawn(move || {
            let request_id = request.request_id;
            let notification = request_id.is_nil();
            let session_id = request.session_id;
            let runtime_id = request.runtime_id;
            let (outcome, executed) = if !notification
                && let Some(cached) = hub.cached_response(request_id)
            {
                (cached, false)
            } else {
                let outcome = match backend.handle(
                    request,
                    hub.event_sink(session_id, runtime_id),
                ) {
                    Ok(payload) => ResponseOutcome::Ok { payload },
                    Err(error) => ResponseOutcome::Error {
                        error: RpcError::from(error),
                    },
                };
                if !notification {
                    hub.cache_response(request_id, outcome.clone());
                }
                (outcome, true)
            };
            if executed && matches!(&outcome, ResponseOutcome::Ok { .. }) {
                if matches!(
                    outcome,
                    ResponseOutcome::Ok {
                        payload: ResponsePayload::TaskState { .. }
                    }
                ) {
                    hub.task_state_changed(source_subscriber_id);
                }
            }
            if !notification {
                let _ = outgoing.send(ServerMessage::Response {
                    request_id,
                    outcome,
                });
            }
        })
        .ok();
}

fn validate_handshake(
    request: &HandshakeRequest,
    response: HandshakeResponse,
    allowed_origins: &HashSet<String>,
) -> Result<HandshakeResponse, ErrorResponse> {
    if request.uri().path() != "/v1" {
        return Err(handshake_error(
            StatusCode::NOT_FOUND,
            "unknown daemon endpoint",
        ));
    }
    if let Some(origin) = request.headers().get(ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .is_some_and(|origin| allowed_origins.contains(origin));
        if !allowed {
            return Err(handshake_error(
                StatusCode::FORBIDDEN,
                "WebSocket origin is not allowed",
            ));
        }
    }
    Ok(response)
}

fn handshake_error(status: StatusCode, message: &str) -> ErrorResponse {
    tungstenite::http::Response::builder()
        .status(status)
        .body(Some(message.to_owned()))
        .expect("static WebSocket rejection is valid")
}

fn token_matches(expected: &str, candidate: &str) -> bool {
    expected.as_bytes().ct_eq(candidate.as_bytes()).into()
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

fn read_client_message(socket: &mut WebSocket<TcpStream>) -> anyhow::Result<ClientMessage> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(serde_json::from_str(text.as_ref())?),
            Message::Ping(_) => socket.flush()?,
            Message::Close(_) => bail!("client closed during handshake"),
            _ => {}
        }
    }
}

fn write_json<S: io::Read + io::Write, T: serde::Serialize>(
    socket: &mut WebSocket<S>,
    value: &T,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(value)?;
    socket.send(Message::Text(payload.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use daku_protocol::{Command, ServerMessage};

    struct TestBackend;

    impl Backend for TestBackend {
        fn handle(&self, request: Request, _: EventSink) -> anyhow::Result<ResponsePayload> {
            match request.command {
                Command::Ping => Ok(ResponsePayload::Ack),
                Command::GetSettings => Ok(ResponsePayload::Settings {
                    settings: Default::default(),
                }),
                _ => bail!("unexpected command"),
            }
        }
    }

    #[test]
    fn handshake_rejects_wrong_protocol_version() {
        assert_eq!(PROTOCOL_VERSION, 1);
        assert!(token_matches("secret", "secret"));
        assert!(!token_matches("secret", "other"));
    }

    #[test]
    fn hub_broadcasts_environments_updated() {
        let hub = Hub::default();
        let (tx, rx) = unbounded();
        hub.subscribe(&[], tx);
        hub.broadcast(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        match rx.try_recv().unwrap() {
            ServerMessage::EnvironmentsUpdated { environments } => {
                assert!(environments.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
