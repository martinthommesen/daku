//! Shared poll loop. Later Signals register here; they do not start timers.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, unbounded};
use daku_protocol::ServerMessage;
use daku_protocol::settings::DaemonSettings;

use crate::availability::AvailabilityCollector;
use crate::config::{
    CredentialStore, EnvironmentConfig, KeychainCredentialStore, load_environments,
};
use crate::drift::DriftCollector;
use crate::health::publish_dashboard;
use crate::jobs::JobsCollector;
use crate::last_clone::LastCloneCollector;
use crate::mid_ecc::MidEccCollector;
use crate::outbound::OutboundCollector;
use crate::persistence::StateStore;
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

pub struct CollectorLoop {
    interval: Duration,
    collectors: Vec<Box<dyn SignalCollector>>,
}

impl CollectorLoop {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            collectors: Vec::new(),
        }
    }

    pub fn register(&mut self, collector: impl SignalCollector + 'static) {
        self.collectors.push(Box::new(collector));
    }

    pub fn tick(&self) -> anyhow::Result<()> {
        let mut first_error = None;
        for collector in &self.collectors {
            if let Err(error) = collector.collect() {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock, after: &dyn Fn()) {
        // Publish last-known state from SQLite so a fresh subscriber is not blank
        // until the first tick completes.
        after();
        while !shutdown.load(Ordering::Acquire) {
            if let Err(error) = self.tick() {
                eprintln!("daku collector tick failed: {error}");
            }
            after();
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            clock.sleep(self.interval);
        }
    }
}

pub fn spawn_collector_loop(
    loop_: CollectorLoop,
    shutdown: Arc<AtomicBool>,
    after_tick: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("daku-collector".into())
        .spawn(move || {
            loop_.run(&shutdown, &SystemClock, &after_tick);
        })
        .expect("spawn collector loop");
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
    loop_.register(AvailabilityCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
    ));
    loop_.register(JobsCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
    ));
    loop_.register(SyslogCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
    ));
    loop_.register(MidEccCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
    ));
    loop_.register(OutboundCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
    ));
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
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
}
