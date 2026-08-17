//! Shared poll loop. Later Signals register here; they do not start timers.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use daku_protocol::settings::DaemonSettings;

use crate::availability::AvailabilityCollector;
use crate::config::{
    load_environments, CredentialStore, EnvironmentConfig, KeychainCredentialStore,
};
use crate::persistence::StateStore;
use crate::servicenow::{Clock, ServiceNowClient, SystemClock, UreqTransport};

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

    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock) {
        while !shutdown.load(Ordering::Acquire) {
            if let Err(error) = self.tick() {
                eprintln!("daku collector tick failed: {error}");
            }
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            clock.sleep(self.interval);
        }
    }
}

pub fn spawn_collector_loop(loop_: CollectorLoop, shutdown: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("daku-collector".into())
        .spawn(move || {
            loop_.run(&shutdown, &SystemClock);
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
    let mut loop_ = CollectorLoop::new(interval);
    loop_.register(AvailabilityCollector::new(
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
) {
    let environments = match load_environments(environments_path) {
        Ok(environments) => environments,
        Err(error) => {
            if is_not_found(&error) {
                eprintln!(
                    "daku collector idle: missing {}",
                    environments_path.display()
                );
                return;
            }
            eprintln!("daku collector not started: {error}");
            return;
        }
    };
    if environments.is_empty() {
        return;
    }
    let loop_ = build_default_loop(
        environments,
        Arc::new(KeychainCredentialStore),
        store,
        Duration::from_secs(poll_interval_secs(settings)),
        ServiceNowClient::new(UreqTransport::default(), SystemClock),
    );
    spawn_collector_loop(loop_, shutdown);
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
    use crate::availability::{AvailabilityCollector, AVAILABILITY_SIGNAL_ID};
    use crate::config::{AuthMethod, EnvironmentConfig, MemoryCredentialStore};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

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
}
