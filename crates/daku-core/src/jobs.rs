//! Scheduled jobs Signal: overdue Ready and Error counts on `sys_trigger`.

use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::availability::{REACHABILITY_REUSE_SECS, Reachability, recent_reachability};
use crate::collector::SignalCollector;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const JOBS_SIGNAL_ID: &str = "jobs";
pub const JOBS_OVERDUE_PATH: &str = "/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=0^next_action<javascript:gs.minutesAgoStart(15)";
pub const JOBS_ERROR_PATH: &str =
    "/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=3";

pub fn jobs_state(overdue_ready: u64) -> &'static str {
    if overdue_ready > 0 {
        "degraded"
    } else {
        "healthy"
    }
}

pub struct JobsCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
}

impl JobsCollector {
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
        }
    }
}

impl SignalCollector for JobsCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let mut first_error = None;
        for environment in &self.environments {
            if let Some(reachability @ (Reachability::Asleep | Reachability::Unreachable)) =
                recent_reachability(
                    &connection,
                    &environment.id,
                    observed_at,
                    REACHABILITY_REUSE_SECS,
                )
            {
                if let Err(error) = persistence::persist_signal_skipped(
                    &connection,
                    &environment.id,
                    JOBS_SIGNAL_ID,
                    observed_at,
                    reachability.as_str(),
                ) {
                    first_error.get_or_insert_with(|| anyhow::Error::from(error));
                }
                continue;
            }
            if let Err(error) = collect_jobs(
                &connection,
                environment,
                observed_at,
                fetch_aggregate_count(
                    &self.client,
                    environment,
                    self.credentials.as_ref(),
                    JOBS_OVERDUE_PATH,
                ),
                fetch_aggregate_count(
                    &self.client,
                    environment,
                    self.credentials.as_ref(),
                    JOBS_ERROR_PATH,
                ),
            ) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = persistence::prune_signal_samples(&connection, observed_at) {
            first_error.get_or_insert_with(|| anyhow::Error::from(error));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn collect_jobs(
    connection: &Connection,
    environment: &EnvironmentConfig,
    observed_at: i64,
    overdue_ready: anyhow::Result<u64>,
    error: anyhow::Result<u64>,
) -> anyhow::Result<()> {
    match (overdue_ready, error) {
        (Ok(overdue_ready), Ok(error)) => persist_jobs_ok(
            connection,
            &environment.id,
            overdue_ready,
            error,
            observed_at,
        )
        .map_err(anyhow::Error::from),
        (overdue_ready, error) => {
            let message = match overdue_ready.err().or_else(|| error.err()) {
                Some(error) => error.to_string(),
                None => "jobs probe failed".into(),
            };
            persist_jobs_down(connection, &environment.id, &message, observed_at)
                .map_err(anyhow::Error::from)
        }
    }
}

fn persist_jobs_ok(
    connection: &Connection,
    environment_id: &str,
    overdue_ready: u64,
    error: u64,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({
        "overdue_ready": overdue_ready,
        "error": error,
    });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        JOBS_SIGNAL_ID,
        observed_at,
        jobs_state(overdue_ready),
        &payload.to_string(),
    )?;
    persistence::persist_signal_sample(
        connection,
        environment_id,
        JOBS_SIGNAL_ID,
        observed_at,
        Some((overdue_ready + error) as f64),
        None,
    )
}

fn persist_jobs_down(
    connection: &Connection,
    environment_id: &str,
    message: &str,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({
        "reachability": "unreachable",
        "detail": message,
    });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        JOBS_SIGNAL_ID,
        observed_at,
        "down",
        &payload.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TempDb, prod};
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::MemoryCredentialStore;
    use crate::persistence;
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    struct JobsCountTransport {
        overdue: &'static str,
        error: &'static str,
    }

    impl HttpTransport for JobsCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/stats/sys_trigger"),
                "jobs collector must use Aggregate API: {}",
                request.url
            );
            let body = if request.url.contains("state=3") {
                self.error
            } else {
                self.overdue
            };
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.into(),
            })
        }
    }

    #[test]
    fn jobs_signal_zeros_are_healthy_and_write_sample() {
        let db = TempDb::new("jobs");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = JobsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(
                JobsCountTransport {
                    overdue: include_str!("../tests/fixtures/jobs/count_0.json"),
                    error: include_str!("../tests/fixtures/jobs/count_0.json"),
                },
                SystemClock,
            ),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", JOBS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["overdue_ready"], 0);
        assert_eq!(payload["error"], 0);
        let samples =
            persistence::load_signal_samples(&connection, "prod", JOBS_SIGNAL_ID).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value_real, Some(0.0));
    }

    #[test]
    fn jobs_signal_overdue_is_degraded() {
        let db = TempDb::new("jobs-overdue");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = JobsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(
                JobsCountTransport {
                    overdue: include_str!("../tests/fixtures/jobs/count_2.json"),
                    error: include_str!("../tests/fixtures/jobs/count_0.json"),
                },
                SystemClock,
            ),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", JOBS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["overdue_ready"], 2);
        assert_eq!(payload["error"], 0);
        let samples =
            persistence::load_signal_samples(&connection, "prod", JOBS_SIGNAL_ID).unwrap();
        assert_eq!(samples[0].value_real, Some(2.0));
    }

    struct JobsFailTransport;

    impl HttpTransport for JobsFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn jobs_signal_probe_failure_is_down_without_sample() {
        let db = TempDb::new("jobs-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = JobsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(JobsFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", JOBS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("error").is_none());
        assert!(
            persistence::load_signal_samples(&connection, "prod", JOBS_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn jobs_signal_skips_when_availability_asleep() {
        use crate::availability::{
            AvailabilityObservation, Reachability, SignalState, persist_availability_snapshot,
        };

        let db = TempDb::new("jobs-asleep");
        let store = db.store();
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        {
            let connection = store.open().unwrap();
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

        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        JobsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", JOBS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
        assert!(
            persistence::load_signal_samples(&connection, "prod", JOBS_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }
}
