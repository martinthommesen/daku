//! Shared poll loop. Later Signals register here; they do not start timers.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, unbounded};
use daku_protocol::settings::DaemonSettings;
use daku_protocol::{Reachability, ServerMessage, SignalState};
use rusqlite::Connection;

use crate::availability::{AvailabilityCollector, REACHABILITY_REUSE_SECS, recent_reachability};
use crate::config::{
    CredentialStore, EnvironmentConfig, KeychainCredentialStore, load_environments,
};
use crate::drift::DriftCollector;
use crate::health::publish_dashboard;
use crate::jobs::JobsCollector;
use crate::last_clone::LastCloneCollector;
use crate::mid_ecc::MidEccCollector;
use crate::outbound::OutboundCollector;
use crate::persistence::{self, StateStore};
use crate::servicenow::{Clock, ServiceNowClient, SystemClock, UreqTransport};
use crate::syslog::SyslogCollector;

pub use daku_protocol::settings::DEFAULT_POLL_INTERVAL_SECS;

/// Fastest cadence the daemon will poll at, however low the setting is.
pub const MIN_POLL_INTERVAL_SECS: u64 = 30;

pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    match settings.poll_interval_secs {
        0 => DEFAULT_POLL_INTERVAL_SECS,
        secs => secs.max(MIN_POLL_INTERVAL_SECS),
    }
}

pub trait SignalCollector: Send + Sync {
    fn collect(&self) -> anyhow::Result<()>;
}

/// Seconds since the epoch, the timestamp every Signal stamps its snapshot with.
pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// What one probe of one Environment produced. `sample` is appended to the
/// 24 h ring for trend Signals (jobs, syslog).
pub struct Observation {
    pub state: SignalState,
    pub payload: serde_json::Value,
    pub sample: Option<f64>,
}

/// One Signal's per-Environment logic. The loop, `observed_at`, the asleep /
/// unreachable gate, the down snapshot, and sample pruning live in
/// `PerEnvironmentCollector`; implementations only probe.
pub trait Signal: Send + Sync {
    fn id(&self) -> &'static str;

    /// Probes one Environment. `Err` is persisted as a `down` snapshot with the
    /// error text as `detail`; every classified outcome is an `Ok(Observation)`.
    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation>;

    /// Whether to skip probing when Availability reported the Environment
    /// asleep or unreachable this tick. Availability itself returns false.
    fn gated_by_availability(&self) -> bool {
        true
    }

    /// Whether this Signal writes samples (and therefore prunes them).
    fn keeps_samples(&self) -> bool {
        false
    }
}

pub struct PerEnvironmentCollector<S: Signal> {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
    signal: S,
}

impl<S: Signal + Default> PerEnvironmentCollector<S> {
    pub fn new(
        environments: Vec<EnvironmentConfig>,
        credentials: Arc<dyn CredentialStore>,
        client: impl Into<Arc<ServiceNowClient>>,
        store: StateStore,
    ) -> Self {
        Self {
            environments,
            credentials,
            client: client.into(),
            store,
            signal: S::default(),
        }
    }
}

impl<S: Signal> PerEnvironmentCollector<S> {
    pub(crate) fn signal(&self) -> &S {
        &self.signal
    }

    pub(crate) fn client(&self) -> &ServiceNowClient {
        &self.client
    }

    pub(crate) fn credentials(&self) -> &dyn CredentialStore {
        self.credentials.as_ref()
    }

    fn collect_environment(
        &self,
        connection: &Connection,
        environment: &EnvironmentConfig,
        observed_at: i64,
    ) -> anyhow::Result<()> {
        if self.signal.gated_by_availability()
            && let Some(reachability @ (Reachability::Asleep | Reachability::Unreachable)) =
                recent_reachability(
                    connection,
                    &environment.id,
                    observed_at,
                    REACHABILITY_REUSE_SECS,
                )
        {
            return persistence::persist_signal_skipped(
                connection,
                &environment.id,
                self.signal.id(),
                observed_at,
                reachability.as_str(),
            )
            .map_err(anyhow::Error::from);
        }
        match self
            .signal
            .probe(&self.client, self.credentials.as_ref(), environment)
        {
            Ok(observation) => {
                persistence::persist_signal_snapshot(
                    connection,
                    &environment.id,
                    self.signal.id(),
                    observed_at,
                    observation.state,
                    &observation.payload.to_string(),
                )?;
                if observation.sample.is_some() {
                    persistence::persist_signal_sample(
                        connection,
                        &environment.id,
                        self.signal.id(),
                        observed_at,
                        observation.sample,
                        None,
                    )?;
                }
                Ok(())
            }
            Err(error) => persistence::persist_signal_down(
                connection,
                &environment.id,
                self.signal.id(),
                observed_at,
                &error.to_string(),
            )
            .map_err(anyhow::Error::from),
        }
    }
}

impl<S: Signal + 'static> SignalCollector for PerEnvironmentCollector<S> {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = unix_now();
        let mut first_error = None;
        for environment in &self.environments {
            if let Err(error) = self.collect_environment(&connection, environment, observed_at) {
                first_error.get_or_insert(error);
            }
        }
        if self.signal.keeps_samples()
            && let Err(error) = persistence::prune_signal_samples(&connection, observed_at)
        {
            first_error.get_or_insert_with(|| anyhow::Error::from(error));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

pub struct CollectorLoop {
    interval: Duration,
    /// Run concurrently, one scoped thread per group (one group per Environment).
    groups: Vec<Vec<Box<dyn SignalCollector>>>,
    /// Run sequentially after every group has finished (cross-Environment Signals).
    shared: Vec<Box<dyn SignalCollector>>,
}

impl CollectorLoop {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            groups: Vec::new(),
            shared: Vec::new(),
        }
    }

    /// Registers a collector that runs after all groups, on the calling thread.
    pub fn register(&mut self, collector: impl SignalCollector + 'static) {
        self.shared.push(Box::new(collector));
    }

    /// Registers a set of collectors that run in order on their own thread,
    /// concurrently with the other groups.
    pub fn register_group(&mut self, group: Vec<Box<dyn SignalCollector>>) {
        if !group.is_empty() {
            self.groups.push(group);
        }
    }

    pub fn tick(&self) -> anyhow::Result<()> {
        self.tick_timed().0
    }

    /// Runs one tick and reports how long it took, so the caller can sleep the
    /// remainder of the interval instead of a full interval on top of it.
    fn tick_timed(&self) -> (anyhow::Result<()>, Duration) {
        let started = Instant::now();
        let mut errors: Vec<anyhow::Error> = std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .groups
                .iter()
                .map(|group| scope.spawn(move || run_sequential(group)))
                .collect();
            handles
                .into_iter()
                .enumerate()
                .filter_map(|(index, handle)| match handle.join() {
                    Ok(result) => result.err(),
                    Err(_) => Some(anyhow::anyhow!("collector group {index} panicked")),
                })
                .collect()
        });
        // The shared collectors read across every Environment, so they must run
        // after every group has joined — plan 049 needs this tick's availability
        // snapshot committed first. Give them their own joined scope so a panic
        // becomes a tick error instead of killing the collector thread.
        let shared =
            std::thread::scope(|scope| scope.spawn(|| run_sequential(&self.shared)).join());
        match shared {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(error),
            Err(_) => errors.push(anyhow::anyhow!("shared collectors panicked")),
        }
        let elapsed = started.elapsed();
        if elapsed > self.interval {
            eprintln!(
                "daku collector tick took {:.0}s (poll interval {:.0}s)",
                elapsed.as_secs_f64(),
                self.interval.as_secs_f64()
            );
        }
        let result = match errors.into_iter().next() {
            Some(error) => Err(error),
            None => Ok(()),
        };
        (result, elapsed)
    }

    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock, after: &dyn Fn()) {
        // Publish last-known state from SQLite so a fresh subscriber is not blank
        // until the first tick completes.
        publish(after);
        while !shutdown.load(Ordering::Acquire) {
            let (result, elapsed) = self.tick_timed();
            if let Err(error) = result {
                eprintln!("daku collector tick failed: {error}");
            }
            publish(after);
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            // An overrunning tick sleeps zero and ticks again immediately.
            clock.sleep(self.interval.saturating_sub(elapsed));
        }
    }
}

/// Publishes the dashboard, costing one tick's publish rather than the whole
/// loop if it panics.
fn publish(after: &dyn Fn()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(after)).is_err() {
        eprintln!("daku dashboard publish panicked");
    }
}

fn run_sequential(collectors: &[Box<dyn SignalCollector>]) -> anyhow::Result<()> {
    let mut first_error = None;
    for collector in collectors {
        if let Err(error) = collector.collect() {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub fn spawn_collector_loop(
    loop_: CollectorLoop,
    shutdown: Arc<AtomicBool>,
    after_tick: impl Fn() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("daku-collector".into())
        .spawn(move || {
            loop_.run(&shutdown, &SystemClock, &after_tick);
            // Reached only on shutdown or an unwind out of `run`. Either way the
            // daemon stops polling, so say so in ~/.daku/daemon.log rather than
            // leaving the last snapshot to look current forever.
            eprintln!("daku collector loop ended");
        })
        .expect("spawn collector loop")
}

pub fn build_default_loop(
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    store: StateStore,
    interval: Duration,
    client: ServiceNowClient,
) -> CollectorLoop {
    let client = Arc::new(client);
    let mut loop_ = CollectorLoop::new(interval);
    for environment in &environments {
        let one = vec![environment.clone()];
        loop_.register_group(vec![
            Box::new(AvailabilityCollector::new(
                one.clone(),
                credentials.clone(),
                client.clone(),
                store.clone(),
            )),
            Box::new(JobsCollector::new(
                one.clone(),
                credentials.clone(),
                client.clone(),
                store.clone(),
            )),
            Box::new(SyslogCollector::new(
                one.clone(),
                credentials.clone(),
                client.clone(),
                store.clone(),
            )),
            Box::new(MidEccCollector::new(
                one.clone(),
                credentials.clone(),
                client.clone(),
                store.clone(),
            )),
            Box::new(OutboundCollector::new(
                one,
                credentials.clone(),
                client.clone(),
                store.clone(),
            )),
        ]);
    }
    loop_.register(DriftCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
        interval,
    ));
    loop_.register(LastCloneCollector::new(
        environments,
        credentials,
        client,
        store,
    ));
    loop_
}

/// Starts the shared poll loop when `environments.json` is present.
pub fn start_default_loop(
    environments_path: &Path,
    store: StateStore,
    settings: &DaemonSettings,
    shutdown: Arc<AtomicBool>,
) -> Option<Receiver<ServerMessage>> {
    let environments = match load_environments(environments_path) {
        Ok(environments) => environments,
        Err(error) => {
            if is_not_found(&error) {
                eprintln!(
                    "daku collector idle: missing {}",
                    environments_path.display()
                );
                return None;
            }
            eprintln!("daku collector not started: {error}");
            return None;
        }
    };
    if environments.is_empty() {
        return None;
    }
    let (dashboard_tx, dashboard_rx) = unbounded();
    let dashboard_environments = environments.clone();
    let dashboard_store = store.clone();
    let loop_ = build_default_loop(
        environments,
        Arc::new(KeychainCredentialStore),
        store,
        Duration::from_secs(poll_interval_secs(settings)),
        ServiceNowClient::new(UreqTransport::default(), SystemClock),
    );
    spawn_collector_loop(loop_, shutdown, move || {
        let now = unix_now();
        if let Err(error) = publish_dashboard(
            &dashboard_environments,
            &dashboard_store,
            &dashboard_tx,
            now,
        ) {
            eprintln!("daku dashboard publish failed: {error}");
        }
    });
    Some(dashboard_rx)
}

pub fn probe_availability_once(environments_path: &Path, store: StateStore) -> anyhow::Result<()> {
    let environments = load_environments(environments_path)?;
    let loop_ = build_default_loop(
        environments,
        Arc::new(KeychainCredentialStore),
        store,
        Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
        ServiceNowClient::new(UreqTransport::default(), SystemClock),
    );
    loop_.tick()
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|io_error| io_error.kind() == io::ErrorKind::NotFound)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub id: String,
    pub label: String,
    pub credential_present: bool,
    pub credential_error: Option<String>,
    pub reachability: &'static str,
    pub state: &'static str,
    pub build: Option<String>,
    pub error: Option<String>,
    pub rtt_ms: u64,
}

pub struct DoctorReport {
    pub environments_path: PathBuf,
    pub poll_interval_secs: u64,
    pub rows: Vec<DoctorRow>,
}

/// Read-only diagnosis: config, Credential presence (never the value), and a
/// live Availability probe per Environment. Writes nothing to SQLite.
pub fn run_doctor(
    environments_path: &Path,
    settings: &DaemonSettings,
    credentials: Arc<dyn CredentialStore>,
    client: ServiceNowClient,
    store: StateStore,
) -> anyhow::Result<DoctorReport> {
    let environments = load_environments(environments_path)?;
    let probe =
        AvailabilityCollector::new(environments.clone(), credentials.clone(), client, store);
    let rows = environments
        .iter()
        .map(|environment| {
            let (credential_present, credential_error) = match credentials.get(&environment.id) {
                Ok(Some(_)) => (true, None),
                Ok(None) => (false, None),
                Err(error) => (false, Some(error.to_string())),
            };
            let observation = probe.probe(environment);
            DoctorRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                credential_present,
                credential_error,
                reachability: observation.reachability.as_str(),
                state: observation.state.as_str(),
                build: observation.build,
                error: observation.error,
                rtt_ms: observation.rtt_ms,
            }
        })
        .collect();
    Ok(DoctorReport {
        environments_path: environments_path.to_owned(),
        poll_interval_secs: poll_interval_secs(settings),
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{AVAILABILITY_SIGNAL_ID, AvailabilityCollector};
    use crate::config::MemoryCredentialStore;
    use crate::persistence;
    use crate::servicenow::{
        Clock, HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };
    use crate::test_support::{TempDb, prod};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn poll_interval_secs_reads_top_level_json_key() {
        let settings: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs": 30}"#).unwrap();
        assert_eq!(poll_interval_secs(&settings), 30);
    }

    #[test]
    fn poll_interval_secs_zero_means_default() {
        let zero: DaemonSettings = serde_json::from_str(r#"{"poll_interval_secs": 0}"#).unwrap();
        assert_eq!(poll_interval_secs(&zero), DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(
            poll_interval_secs(&DaemonSettings::default()),
            DEFAULT_POLL_INTERVAL_SECS
        );
    }

    #[test]
    fn poll_interval_secs_is_floored_at_30() {
        let interval = |secs: u64| {
            poll_interval_secs(&DaemonSettings {
                poll_interval_secs: secs,
            })
        };
        assert_eq!(interval(5), MIN_POLL_INTERVAL_SECS);
        assert_eq!(interval(30), 30);
        assert_eq!(interval(31), 31);
    }

    #[test]
    fn poll_interval_secs_rejects_non_number() {
        assert!(
            serde_json::from_str::<DaemonSettings>(r#"{"poll_interval_secs":"fast"}"#).is_err()
        );
    }

    struct FixtureTransport;

    impl HttpTransport for FixtureTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                !request.url.contains("oauth_token"),
                "collector tick must not hit the token endpoint in this fixture"
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: include_str!("../tests/fixtures/availability/ok.json").into(),
            })
        }
    }

    #[test]
    fn collector_loop_tick_writes_availability_snapshot() {
        let db = TempDb::new("collector");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = AvailabilityCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(FixtureTransport, SystemClock),
            store,
        );
        assert_eq!(poll_interval_secs(&DaemonSettings::default()), 120);
        let mut loop_ = CollectorLoop::new(Duration::from_secs(poll_interval_secs(
            &DaemonSettings::default(),
        )));
        loop_.register(collector);
        loop_.tick().unwrap();
        loop_.tick().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", AVAILABILITY_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.signal_id, "availability");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "reachable");
        assert_eq!(payload["build"], "glide-zurich-12-18-2025__patch0-hotfix1");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM signal_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn collector_loop_run_publishes_before_and_after_tick() {
        let shutdown = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let loop_ = CollectorLoop::new(Duration::from_millis(1));
        struct StopOnSleep<'a>(&'a AtomicBool);
        impl Clock for StopOnSleep<'_> {
            fn now(&self) -> std::time::SystemTime {
                std::time::SystemTime::now()
            }
            fn sleep(&self, _: Duration) {
                self.0.store(true, Ordering::Release);
            }
        }
        loop_.run(&shutdown, &StopOnSleep(&shutdown), &|| {
            calls.fetch_add(1, Ordering::Release);
        });
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }
    /// Records what `run` asked to sleep for, and stops the loop.
    struct StopRecordingSleep<'a>(&'a AtomicBool, Mutex<Vec<Duration>>);

    impl Clock for StopRecordingSleep<'_> {
        fn now(&self) -> std::time::SystemTime {
            std::time::SystemTime::now()
        }
        fn sleep(&self, duration: Duration) {
            self.1.lock().expect("sleeps").push(duration);
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn run_sleeps_the_remainder_of_the_interval() {
        let interval = Duration::from_secs(10);
        let shutdown = AtomicBool::new(false);
        let clock = StopRecordingSleep(&shutdown, Mutex::new(Vec::new()));
        let mut loop_ = CollectorLoop::new(interval);
        loop_.register(SleepingCollector(
            Duration::from_millis(200),
            Arc::new(AtomicUsize::new(0)),
        ));
        loop_.run(&shutdown, &clock, &|| {});
        let sleeps = clock.1.lock().expect("sleeps").clone();
        assert_eq!(sleeps.len(), 1);
        assert!(
            sleeps[0] < interval && sleeps[0] > interval - Duration::from_secs(1),
            "expected just under the interval, got {:?}",
            sleeps[0]
        );
    }

    #[test]
    fn run_does_not_sleep_after_an_overrunning_tick() {
        let shutdown = AtomicBool::new(false);
        let clock = StopRecordingSleep(&shutdown, Mutex::new(Vec::new()));
        let mut loop_ = CollectorLoop::new(Duration::from_millis(1));
        loop_.register(SleepingCollector(
            Duration::from_millis(50),
            Arc::new(AtomicUsize::new(0)),
        ));
        loop_.run(&shutdown, &clock, &|| {});
        assert_eq!(*clock.1.lock().expect("sleeps"), [Duration::ZERO]);
    }

    #[test]
    fn collector_loop_tick_isolates_failures() {
        struct Failing;
        impl SignalCollector for Failing {
            fn collect(&self) -> anyhow::Result<()> {
                anyhow::bail!("first collector failed")
            }
        }
        struct Recording(Arc<AtomicBool>);
        impl SignalCollector for Recording {
            fn collect(&self) -> anyhow::Result<()> {
                self.0.store(true, Ordering::Release);
                Ok(())
            }
        }
        let ran = Arc::new(AtomicBool::new(false));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(1));
        loop_.register(Failing);
        loop_.register(Recording(ran.clone()));
        let error = loop_.tick().unwrap_err();
        assert!(error.to_string().contains("first collector failed"));
        assert!(
            ran.load(Ordering::Acquire),
            "later collectors must still run"
        );
    }

    struct Panicking;
    impl SignalCollector for Panicking {
        fn collect(&self) -> anyhow::Result<()> {
            panic!("collector exploded")
        }
    }

    /// Silences the panic backtrace the panicking-collector tests provoke.
    fn without_panic_output<T>(body: impl FnOnce() -> T) -> T {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = body();
        std::panic::set_hook(previous);
        result
    }

    #[test]
    fn tick_reports_a_panicking_shared_collector_as_an_error() {
        let mut loop_ = CollectorLoop::new(Duration::from_secs(120));
        loop_.register(Panicking);
        let error = without_panic_output(|| loop_.tick().unwrap_err());
        assert!(error.to_string().contains("shared collectors panicked"));
    }

    #[test]
    fn tick_still_runs_the_other_collectors_when_one_panics() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(120));
        loop_.register_group(vec![Box::new(Panicking)]);
        loop_.register_group(vec![Box::new(SleepingCollector(
            Duration::ZERO,
            calls.clone(),
        ))]);
        loop_.register(SleepingCollector(Duration::ZERO, calls.clone()));
        let error = without_panic_output(|| loop_.tick().unwrap_err());
        assert!(error.to_string().contains("collector group 0 panicked"));
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn run_survives_a_panicking_publish() {
        let shutdown = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let loop_ = CollectorLoop::new(Duration::from_millis(1));
        struct StopAfterTwo<'a>(&'a AtomicBool, &'a AtomicUsize);
        impl Clock for StopAfterTwo<'_> {
            fn now(&self) -> std::time::SystemTime {
                std::time::SystemTime::now()
            }
            fn sleep(&self, _: Duration) {
                if self.1.load(Ordering::Acquire) >= 2 {
                    self.0.store(true, Ordering::Release);
                }
            }
        }
        without_panic_output(|| {
            loop_.run(&shutdown, &StopAfterTwo(&shutdown, &calls), &|| {
                if calls.fetch_add(1, Ordering::AcqRel) == 0 {
                    panic!("publish exploded");
                }
            });
        });
        assert!(
            calls.load(Ordering::Acquire) > 1,
            "a panicking publish must not end the loop"
        );
    }

    struct SleepingCollector(Duration, Arc<AtomicUsize>);

    impl SignalCollector for SleepingCollector {
        fn collect(&self) -> anyhow::Result<()> {
            std::thread::sleep(self.0);
            self.1.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn collector_loop_tick_runs_groups_concurrently() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(120));
        for _ in 0..3 {
            loop_.register_group(vec![Box::new(SleepingCollector(
                Duration::from_millis(200),
                calls.clone(),
            ))]);
        }
        let started = Instant::now();
        loop_.tick().unwrap();
        assert_eq!(calls.load(Ordering::Acquire), 3);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "groups must not run serially"
        );
    }

    #[test]
    fn collector_loop_tick_isolates_failures_and_returns_first_error() {
        struct Failing;
        impl SignalCollector for Failing {
            fn collect(&self) -> anyhow::Result<()> {
                anyhow::bail!("boom")
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(120));
        loop_.register_group(vec![
            Box::new(Failing),
            Box::new(SleepingCollector(Duration::ZERO, calls.clone())),
        ]);
        loop_.register(SleepingCollector(Duration::ZERO, calls.clone()));
        let error = loop_.tick().unwrap_err();
        assert!(error.to_string().contains("boom"));
        assert_eq!(
            calls.load(Ordering::Acquire),
            2,
            "later collectors still run"
        );
    }

    #[test]
    fn build_default_loop_groups_per_environment() {
        let db = TempDb::new("groups");
        let mut second = prod();
        second.id = "test".into();
        let loop_ = build_default_loop(
            vec![prod(), second],
            Arc::new(MemoryCredentialStore::default()),
            db.store(),
            Duration::from_secs(120),
            ServiceNowClient::new(FixtureTransport, SystemClock),
        );
        assert_eq!(loop_.groups.len(), 2);
        assert_eq!(loop_.groups[0].len(), 5);
        assert_eq!(loop_.shared.len(), 2, "drift and last-clone stay shared");
    }

    enum Behaviour {
        Ok(SignalState, Option<f64>),
        Fail,
        Panic,
    }

    struct FakeSignal {
        behaviour: Behaviour,
        keeps_samples: bool,
    }

    impl Signal for FakeSignal {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn keeps_samples(&self) -> bool {
            self.keeps_samples
        }

        fn probe(
            &self,
            _client: &ServiceNowClient,
            _credentials: &dyn CredentialStore,
            _environment: &EnvironmentConfig,
        ) -> anyhow::Result<Observation> {
            match self.behaviour {
                Behaviour::Ok(state, sample) => Ok(Observation {
                    state,
                    payload: serde_json::json!({ "probed": true }),
                    sample,
                }),
                Behaviour::Fail => anyhow::bail!("probe exploded"),
                Behaviour::Panic => panic!("must not probe an asleep Environment"),
            }
        }
    }

    fn fake_collector(
        store: StateStore,
        behaviour: Behaviour,
        keeps_samples: bool,
    ) -> PerEnvironmentCollector<FakeSignal> {
        PerEnvironmentCollector {
            environments: vec![prod()],
            credentials: Arc::new(MemoryCredentialStore::default()),
            client: Arc::new(ServiceNowClient::new(FixtureTransport, SystemClock)),
            store,
            signal: FakeSignal {
                behaviour,
                keeps_samples,
            },
        }
    }

    #[test]
    fn per_environment_collector_persists_ok_observation() {
        let db = TempDb::new("per-env-ok");
        fake_collector(
            db.store(),
            Behaviour::Ok(SignalState::Degraded, Some(3.0)),
            true,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", "fake")
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["probed"], true);
        let samples = persistence::load_signal_samples(&connection, "prod", "fake").unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value_real, Some(3.0));
    }

    #[test]
    fn per_environment_collector_persists_down_on_probe_error() {
        let db = TempDb::new("per-env-down");
        fake_collector(db.store(), Behaviour::Fail, false)
            .collect()
            .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", "fake")
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert_eq!(payload["detail"], "probe exploded");
        assert!(
            persistence::load_signal_samples(&connection, "prod", "fake")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn per_environment_collector_skips_when_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::Reachability;

        let db = TempDb::new("per-env-asleep");
        let observed_at = unix_now();
        {
            let connection = db.store().open().unwrap();
            persist_availability_snapshot(
                &connection,
                "prod",
                &AvailabilityObservation {
                    reachability: Reachability::Asleep,
                    state: SignalState::Healthy,
                    build: None,
                    rtt_ms: 0,
                    error: None,
                },
                observed_at,
            )
            .unwrap();
        }
        fake_collector(db.store(), Behaviour::Panic, false)
            .collect()
            .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", "fake")
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }

    #[test]
    fn per_environment_collector_prunes_only_when_keeps_samples() {
        let stale = |store: &StateStore| {
            let connection = store.open().unwrap();
            persistence::persist_signal_sample(
                &connection,
                "prod",
                "fake",
                unix_now() - 25 * 60 * 60,
                Some(1.0),
                None,
            )
            .unwrap();
        };
        let remaining = |store: &StateStore| {
            let connection = store.open().unwrap();
            persistence::load_signal_samples(&connection, "prod", "fake")
                .unwrap()
                .len()
        };

        let kept = TempDb::new("per-env-noprune");
        stale(&kept.store());
        fake_collector(
            kept.store(),
            Behaviour::Ok(SignalState::Healthy, None),
            false,
        )
        .collect()
        .unwrap();
        assert_eq!(remaining(&kept.store()), 1, "no pruning without samples");

        let pruned = TempDb::new("per-env-prune");
        stale(&pruned.store());
        fake_collector(
            pruned.store(),
            Behaviour::Ok(SignalState::Healthy, None),
            true,
        )
        .collect()
        .unwrap();
        assert_eq!(remaining(&pruned.store()), 0);
    }

    #[test]
    fn doctor_reports_missing_and_present_credential_without_writing() {
        let db = TempDb::new("doctor");
        let environments_path =
            std::env::temp_dir().join(format!("daku-doctor-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &environments_path,
            serde_json::to_vec(&[serde_json::json!({
                "id": "prod",
                "label": "Production",
                "instance_url": prod().instance_url,
                "auth_method": "basic",
                "sort_order": 0,
            })])
            .unwrap(),
        )
        .unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let doctor = || {
            run_doctor(
                &environments_path,
                &DaemonSettings::default(),
                credentials.clone(),
                ServiceNowClient::new(FixtureTransport, SystemClock),
                db.store(),
            )
            .unwrap()
        };

        let report = doctor();
        assert_eq!(report.poll_interval_secs, DEFAULT_POLL_INTERVAL_SECS);
        let row = &report.rows[0];
        assert_eq!(row.id, "prod");
        assert!(!row.credential_present);
        assert_eq!(row.reachability, "unreachable");
        assert!(row.error.is_some());

        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let row = doctor().rows.remove(0);
        assert!(row.credential_present);
        assert_eq!(row.reachability, "reachable");
        assert_eq!(
            row.build.as_deref(),
            Some("glide-zurich-12-18-2025__patch0-hotfix1")
        );

        let connection = db.store().open().unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM signal_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "doctor must not write snapshots");
        let _ = std::fs::remove_file(&environments_path);
    }
    #[test]
    fn start_default_loop_returns_none_for_missing_and_empty_config() {
        let db = TempDb::new("start-default-loop");
        let shutdown = Arc::new(AtomicBool::new(false));
        let start = |path: &Path| {
            start_default_loop(
                path,
                db.store(),
                &DaemonSettings::default(),
                shutdown.clone(),
            )
        };

        let missing =
            std::env::temp_dir().join(format!("daku-missing-{}.json", uuid::Uuid::new_v4()));
        assert!(start(&missing).is_none());

        let empty = std::env::temp_dir().join(format!("daku-empty-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&empty, "[]").unwrap();
        assert!(start(&empty).is_none());
        let _ = std::fs::remove_file(&empty);
    }
}
