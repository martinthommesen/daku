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

pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;
pub const POLL_INTERVAL_SECS_KEY: &str = "poll_interval_secs";

pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    settings
        .extra
        .get(POLL_INTERVAL_SECS_KEY)
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
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
    use crate::config::{AuthMethod, EnvironmentConfig, MemoryCredentialStore};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        Clock, HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    #[test]
    fn poll_interval_secs_reads_top_level_json_key() {
        let settings: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs": 30}"#).unwrap();
        assert_eq!(poll_interval_secs(&settings), 30);
    }

    #[test]
    fn poll_interval_secs_falls_back_to_default_for_zero_or_non_number() {
        let zero: DaemonSettings = serde_json::from_str(r#"{"poll_interval_secs": 0}"#).unwrap();
        assert_eq!(poll_interval_secs(&zero), DEFAULT_POLL_INTERVAL_SECS);
        let text: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs": "fast"}"#).unwrap();
        assert_eq!(poll_interval_secs(&text), DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(
            poll_interval_secs(&DaemonSettings::default()),
            DEFAULT_POLL_INTERVAL_SECS
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
        let path = std::env::temp_dir().join(format!("daku-collector-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = AvailabilityCollector::new(
            vec![EnvironmentConfig {
                id: "prod".into(),
                label: "Production".into(),
                instance_url: "https://acme-prod.example.service-now.com".into(),
                auth_method: AuthMethod::Basic,
                sort_order: 0,
                clone_source: false,
            }],
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

        let connection = StateStore::daemon(path.clone()).open().unwrap();
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
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn collector_loop_run_invokes_after_tick() {
        let shutdown = AtomicBool::new(false);
        let called = AtomicBool::new(false);
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
            called.store(true, Ordering::Release);
        });
        assert!(called.load(Ordering::Acquire));
    }
}
