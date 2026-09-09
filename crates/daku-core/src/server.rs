use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use daku_protocol::{
    ClientMessage, Command, DASHBOARD_CHUNK_BUDGET_BYTES, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION,
    Request, ResponseOutcome, ResponsePayload, RpcError, ServerMessage,
};
use parking_lot::Mutex as ParkingMutex;
use subtle::ConstantTimeEq as _;
use tungstenite::handshake::server::{
    ErrorResponse, Request as HandshakeRequest, Response as HandshakeResponse,
};
use tungstenite::http::{StatusCode, header::ORIGIN};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket, accept_hdr_with_config};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_HANDSHAKE_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;

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
    fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload>;

    fn shutdown(&self) {}
}

#[derive(Default)]
struct HubState {
    next_subscriber_id: u64,
    subscribers: HashMap<u64, Sender<ServerMessage>>,
    dashboard: BTreeMap<String, ServerMessage>,
    dropped: u64,
    next_transfer_id: u64,
}

/// Bound on per-subscriber queued dashboard messages. A stalled client stops
/// growing memory here; further publishes drop with a counter instead.
pub const SUBSCRIBER_QUEUE_BOUND: usize = 512;

#[derive(Default)]
struct Hub {
    state: ParkingMutex<HubState>,
}

impl Hub {
    /// Broadcasts a dashboard message and remembers the whole message for
    /// late subscribers. The cache key space is fixed (one entry per
    /// environment and signal series), so caching whole messages cannot
    /// grow memory with payload size; chunking bounds the wire instead.
    /// Slow-subscriber drops are counted and logged, never silent.
    fn publish_dashboard(&self, message: ServerMessage) {
        let mut state = self.state.lock();
        if let Some(key) = message.dashboard_cache_key() {
            state.dashboard.insert(key, message.clone());
        }
        // Snapshot senders so the drop counter mutation never aliases the
        // subscriber borrow.
        let senders: Vec<(u64, Sender<ServerMessage>)> = state
            .subscribers
            .iter()
            .map(|(&id, sender)| (id, sender.clone()))
            .collect();
        let mut disconnected = Vec::new();
        let mut drops = 0u64;
        for (id, subscriber) in senders {
            let transfer_id = state.next_transfer_id;
            state.next_transfer_id = state.next_transfer_id.saturating_add(1);
            let mut failed = false;
            let mut gone = false;
            for piece in split_for_wire(&message, transfer_id) {
                match subscriber.try_send(piece) {
                    Ok(()) => {}
                    Err(crossbeam_channel::TrySendError::Full(_)) => {
                        failed = true;
                        break;
                    }
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        gone = true;
                        break;
                    }
                }
            }
            if gone {
                disconnected.push(id);
            } else if failed {
                // One drop per message: a partial transfer can never
                // assemble, so its queued pieces are dead weight the next
                // transfer for the same series evicts.
                drops = drops.saturating_add(1);
            }
        }
        if drops > 0 {
            state.dropped = state.dropped.saturating_add(drops);
            if state.dropped % 20 == 1 || drops > 1 {
                eprintln!(
                    "daku-daemon dropped {} dashboard messages for slow subscribers",
                    state.dropped,
                );
            }
        }
        let removed = disconnected.len();
        for id in disconnected {
            state.subscribers.remove(&id);
        }
        if removed > 0 {
            eprintln!("daku-daemon removed {removed} disconnected dashboard subscribers");
        }
    }

    /// Replays the full cached state to one sender, chunked like live
    /// delivery. Returns the subscriber id and the count of cached
    /// messages that did not fit, so the connection can say so.
    fn subscribe(&self, sender: Sender<ServerMessage>) -> (u64, u64) {
        let mut state = self.state.lock();
        // Non-blocking replay: a subscriber that cannot keep up gets what
        // fits plus a drop count instead of deadlocking the handshake
        // before its read loop starts.
        let replay_drops = replay_cached(&mut state, &sender);
        if replay_drops > 0 {
            state.dropped = state.dropped.saturating_add(replay_drops);
            eprintln!(
                "daku-daemon dropped {replay_drops} replay messages for a slow subscriber (total {})",
                state.dropped,
            );
        }
        let id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.saturating_add(1);
        state.subscribers.insert(id, sender);
        (id, replay_drops)
    }

    /// Replays the full cached state to one sender outside of subscribe,
    /// for an explicit resync request. Drops are counted and logged like
    /// any other slow-subscriber loss; the next live tick converges.
    fn resync_to(&self, sender: &Sender<ServerMessage>) {
        let mut state = self.state.lock();
        let drops = replay_cached(&mut state, sender);
        if drops > 0 {
            state.dropped = state.dropped.saturating_add(drops);
            eprintln!("daku-daemon dropped {drops} resync messages for a slow subscriber");
        }
    }

    fn unsubscribe(&self, subscriber_id: u64) {
        self.state.lock().subscribers.remove(&subscriber_id);
    }
}

/// Sends every cached message to one sender, chunked for the wire.
/// Returns the count of messages that did not fit.
fn replay_cached(state: &mut HubState, sender: &Sender<ServerMessage>) -> u64 {
    let mut drops = 0u64;
    for message in state.dashboard.values().cloned().collect::<Vec<_>>() {
        let transfer_id = state.next_transfer_id;
        state.next_transfer_id = state.next_transfer_id.saturating_add(1);
        let mut failed = false;
        for piece in split_for_wire(&message, transfer_id) {
            if sender.try_send(piece).is_err() {
                failed = true;
                break;
            }
        }
        if failed {
            drops = drops.saturating_add(1);
        }
    }
    drops
}

/// Serialized size of one wire message.
fn wire_size(message: &ServerMessage) -> usize {
    serde_json::to_vec(message)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
}

/// Splits a dashboard message for the wire: whole when it fits the chunk
/// budget, sequenced chunks for the three large series variants. Anything
/// else oversize crosses whole with a log line, since no chunk shape
/// exists for it.
fn split_for_wire(message: &ServerMessage, transfer_id: u64) -> Vec<ServerMessage> {
    if wire_size(message) <= DASHBOARD_CHUNK_BUDGET_BYTES {
        return vec![message.clone()];
    }
    match message {
        ServerMessage::SignalSamplesUpdated {
            environment_id,
            signal_id,
            points,
        } => split_items(points, |group, index, count| {
            ServerMessage::SignalSamplesChunk {
                environment_id: environment_id.clone(),
                signal_id: signal_id.clone(),
                transfer_id,
                chunk_index: index,
                chunk_count: count,
                points: group,
            }
        }),
        ServerMessage::SignalRollupsUpdated {
            environment_id,
            signal_id,
            points,
        } => split_items(points, |group, index, count| {
            ServerMessage::SignalRollupsChunk {
                environment_id: environment_id.clone(),
                signal_id: signal_id.clone(),
                transfer_id,
                chunk_index: index,
                chunk_count: count,
                points: group,
            }
        }),
        ServerMessage::SignalSnapshotsUpdated {
            environment_id,
            snapshots,
        } => split_items(snapshots, |group, index, count| {
            ServerMessage::SignalSnapshotsChunk {
                environment_id: environment_id.clone(),
                transfer_id,
                chunk_index: index,
                chunk_count: count,
                snapshots: group,
            }
        }),
        _ => {
            eprintln!(
                "daku-daemon publish above chunk budget with no chunk shape: {} bytes for {:?}",
                wire_size(message),
                message
                    .dashboard_cache_key()
                    .as_deref()
                    .unwrap_or("uncached"),
            );
            vec![message.clone()]
        }
    }
}

/// Splits `items` into groups whose serialized chunk messages fit the
/// budget. Sizes up from the whole-message average, then keeps halving
/// any grouping that still overflows, so uneven item sizes converge.
fn split_items<T: Clone + serde::Serialize>(
    items: &[T],
    build: impl Fn(Vec<T>, u32, u32) -> ServerMessage,
) -> Vec<ServerMessage> {
    if items.is_empty() {
        return vec![build(Vec::new(), 0, 1)];
    }
    let total = wire_size(&build(items.to_vec(), 0, 1));
    let per_item = (total / items.len()).max(1);
    let mut group_len = (DASHBOARD_CHUNK_BUDGET_BYTES / per_item).max(1);
    for _ in 0..16 {
        let groups = chunk_vec(items, group_len);
        let count = groups.len() as u32;
        let fits = groups.iter().enumerate().all(|(index, group)| {
            wire_size(&build(group.clone(), index as u32, count)) <= DASHBOARD_CHUNK_BUDGET_BYTES
        });
        if fits {
            return groups
                .into_iter()
                .enumerate()
                .map(|(index, group)| build(group, index as u32, count))
                .collect();
        }
        if group_len == 1 {
            break;
        }
        group_len = (group_len / 2).max(1);
    }
    // Deeper pathology than halving fixes (one giant item): single-item
    // groups, and the wire cap decides.
    let groups = chunk_vec(items, 1);
    let count = groups.len() as u32;
    groups
        .into_iter()
        .enumerate()
        .map(|(index, group)| build(group, index as u32, count))
        .collect()
}

/// Consecutive groups of at most `len` items.
fn chunk_vec<T: Clone>(items: &[T], len: usize) -> Vec<Vec<T>> {
    items.chunks(len.max(1)).map(<[T]>::to_vec).collect()
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
                        Ok(message) => hub.publish_dashboard(message),
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

// The handshake closure below returns tungstenite's ErrorResponse; boxing it buys nothing here.
#[allow(clippy::result_large_err)]
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
    match hello {
        ClientMessage::Hello {
            protocol_version,
            token,
            ..
        } if protocol_version == PROTOCOL_VERSION && token_matches(expected_token, &token) => {}
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

    let (outgoing, outgoing_rx) = bounded(SUBSCRIBER_QUEUE_BOUND);
    let (subscriber_id, replay_drops) = hub.subscribe(outgoing.clone());
    // A lossy replay must be learnable: retry the notice after each drain
    // until it queues, since the same full queue that caused the drops
    // would also drop a single best-effort send.
    let mut pending_sync_drops = replay_drops;

    'connection: while !shutdown.load(Ordering::Acquire) {
        while let Ok(message) = outgoing_rx.try_recv() {
            if write_json(&mut socket, &message).is_err() {
                break 'connection;
            }
        }
        if pending_sync_drops > 0
            && outgoing
                .try_send(ServerMessage::DashboardSyncNeeded {
                    dropped: pending_sync_drops,
                })
                .is_ok()
        {
            pending_sync_drops = 0;
        }
        match socket.read() {
            Ok(Message::Text(text)) => match serde_json::from_str(text.as_ref()) {
                Ok(ClientMessage::Request(request)) => {
                    // Resync replays Hub state, which no backend owns, so
                    // the serve loop answers it directly.
                    if matches!(request.command, Command::RequestDashboardSync) {
                        dispatch_resync(request, outgoing.clone(), hub.clone());
                    } else {
                        dispatch_request(request, outgoing.clone(), backend.clone());
                    }
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

fn dispatch_resync(request: Request, outgoing: Sender<ServerMessage>, hub: Arc<Hub>) {
    std::thread::Builder::new()
        .name("daku-daemon-resync".into())
        .spawn(move || {
            hub.resync_to(&outgoing);
            let _ = outgoing.send(ServerMessage::Response {
                request_id: request.request_id,
                outcome: ResponseOutcome::Ok {
                    payload: ResponsePayload::Ack,
                },
            });
        })
        .ok();
}

fn dispatch_request(request: Request, outgoing: Sender<ServerMessage>, backend: Arc<dyn Backend>) {
    std::thread::Builder::new()
        .name("daku-daemon-request".into())
        .spawn(move || {
            let outcome = match backend.handle(request.command) {
                Ok(payload) => ResponseOutcome::Ok { payload },
                Err(error) => ResponseOutcome::Error {
                    error: RpcError::from(error),
                },
            };
            let _ = outgoing.send(ServerMessage::Response {
                request_id: request.request_id,
                outcome,
            });
        })
        .ok();
}

// tungstenite's ErrorResponse is a full http::Response; boxing it buys nothing here.
#[allow(clippy::result_large_err)]
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
    use crossbeam_channel::unbounded;

    use daku_protocol::ServerMessage;

    #[test]
    fn token_matches_is_exact() {
        assert!(token_matches("secret", "secret"));
        assert!(!token_matches("secret", "other"));
    }

    #[test]
    fn hub_publishes_to_existing_subscribers() {
        let hub = Hub::default();
        let (tx, rx) = unbounded();
        let (_, replay_drops) = hub.subscribe(tx);
        assert_eq!(replay_drops, 0);
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        match rx.try_recv().unwrap() {
            ServerMessage::EnvironmentsUpdated { environments } => {
                assert!(environments.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn hub_replays_latest_dashboard_state_to_late_subscriber() {
        let hub = Hub::default();
        hub.publish_dashboard(ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![],
        });
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        let (tx, rx) = unbounded();
        let (_, replay_drops) = hub.subscribe(tx);
        assert_eq!(replay_drops, 0);
        let replayed: Vec<ServerMessage> = rx.try_iter().collect();
        assert_eq!(replayed.len(), 2);
        assert!(matches!(
            replayed[0],
            ServerMessage::EnvironmentsUpdated { .. }
        ));
        assert!(matches!(
            replayed[1],
            ServerMessage::SignalSnapshotsUpdated { .. }
        ));
    }

    #[test]
    fn hub_bounds_slow_subscribers_with_drop_counter() {
        let hub = Hub::default();
        let (tx, rx) = bounded(1);
        let _ = hub.subscribe(tx);
        for _ in 0..5 {
            hub.publish_dashboard(ServerMessage::EnvironmentsUpdated {
                environments: vec![],
            });
        }
        // Bounded queue keeps the first message; the rest drop with a counter.
        assert!(rx.try_iter().count() >= 1);
        assert!(hub.state.lock().dropped >= 4);
    }

    #[test]
    fn hub_removes_disconnected_subscribers() {
        let hub = Hub::default();
        let (tx, rx) = bounded::<ServerMessage>(8);
        let _ = hub.subscribe(tx);
        drop(rx);
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        assert!(hub.state.lock().subscribers.is_empty());
    }

    #[test]
    fn hub_replay_never_blocks_on_a_full_queue() {
        let hub = Hub::default();
        // Fill the cache beyond the subscriber bound with distinct keys.
        for i in 0..(SUBSCRIBER_QUEUE_BOUND + 16) {
            hub.publish_dashboard(ServerMessage::SignalSnapshotsUpdated {
                environment_id: format!("env-{i}"),
                snapshots: vec![],
            });
        }
        let (tx, _rx) = bounded::<ServerMessage>(1);
        // Must return without blocking even though replay exceeds capacity.
        let (_, replay_drops) = hub.subscribe(tx);
        assert!(replay_drops >= 1);
        assert!(hub.state.lock().dropped >= 1);
    }

    #[test]
    fn hub_chunks_oversize_rollups_within_budget() {
        use daku_protocol::{DASHBOARD_CHUNK_BUDGET_BYTES, RollupPoint};
        let hub = Hub::default();
        let points: Vec<RollupPoint> = (0..6000)
            .map(|hour| RollupPoint {
                hour_start: 1_700_000_000 - hour * 3600,
                avg_real: Some(2.5),
                max_real: Some(9.0),
                sample_count: 30,
            })
            .collect();
        let whole = ServerMessage::SignalRollupsUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points,
        };
        assert!(
            serde_json::to_vec(&whole).unwrap().len() > DASHBOARD_CHUNK_BUDGET_BYTES,
            "fixture must actually exceed the chunk budget"
        );
        let (tx, rx) = unbounded();
        hub.subscribe(tx);
        hub.publish_dashboard(whole);
        let mut chunks = Vec::new();
        for message in rx.try_iter() {
            match message {
                ServerMessage::SignalRollupsChunk {
                    chunk_index,
                    chunk_count,
                    points,
                    ..
                } => chunks.push((chunk_index, chunk_count, points.len())),
                other => panic!("oversize publish must arrive chunked, got {other:?}"),
            }
        }
        assert!(chunks.len() > 1);
        let count = chunks[0].1;
        let total: usize = chunks.iter().map(|(_, _, len)| len).sum();
        assert_eq!(total, 6000);
        let mut indexes: Vec<u32> = chunks.iter().map(|(index, _, _)| *index).collect();
        indexes.sort_unstable();
        assert_eq!(
            indexes,
            (0..count).collect::<Vec<_>>(),
            "chunks carry a complete 0..count sequence"
        );
    }

    #[test]
    fn hub_small_messages_cross_whole() {
        let hub = Hub::default();
        let (tx, rx) = unbounded();
        hub.subscribe(tx);
        hub.publish_dashboard(ServerMessage::SignalRollupsUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: vec![],
        });
        assert!(matches!(
            rx.try_recv().unwrap(),
            ServerMessage::SignalRollupsUpdated { .. }
        ));
    }

    #[test]
    fn split_for_wire_keeps_every_piece_within_budget() {
        use daku_protocol::{DASHBOARD_CHUNK_BUDGET_BYTES, SamplePoint};
        let points: Vec<SamplePoint> = (0..20000)
            .map(|at| SamplePoint {
                observed_at: at,
                value_real: Some(1.5),
            })
            .collect();
        let whole = ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points,
        };
        assert!(
            serde_json::to_vec(&whole).unwrap().len() > DASHBOARD_CHUNK_BUDGET_BYTES,
            "fixture must actually exceed the chunk budget"
        );
        let pieces = split_for_wire(&whole, 3);
        assert!(pieces.len() > 1);
        for piece in &pieces {
            assert!(
                serde_json::to_vec(piece).unwrap().len() <= DASHBOARD_CHUNK_BUDGET_BYTES,
                "every piece must fit the budget"
            );
        }
    }

    #[test]
    fn hub_resync_replays_cache_to_one_sender() {
        let hub = Hub::default();
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        hub.publish_dashboard(ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![],
        });
        let (tx, rx) = bounded::<ServerMessage>(16);
        hub.resync_to(&tx);
        let replayed: Vec<ServerMessage> = rx.try_iter().collect();
        assert_eq!(replayed.len(), 2);
        // Resync never registers the sender as a live subscriber.
        assert!(hub.state.lock().subscribers.is_empty());
    }
}
