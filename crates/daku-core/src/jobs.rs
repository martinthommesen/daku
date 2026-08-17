//! Scheduled jobs Signal: overdue Ready and Error counts on `sys_trigger`.

use anyhow::anyhow;
use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const JOBS_SIGNAL_ID: &str = "jobs";
pub const JOBS_OVERDUE_PATH: &str = "/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=0^next_action<javascript:gs.minutesAgoStart(15)";
pub const JOBS_ERROR_PATH: &str =
    "/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=3";

pub fn jobs_state(overdue_ready: u64) -> SignalState {
    if overdue_ready > 0 {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

#[derive(Default)]
pub struct JobsSignal;

pub type JobsCollector = PerEnvironmentCollector<JobsSignal>;

impl Signal for JobsSignal {
    fn id(&self) -> &'static str {
        JOBS_SIGNAL_ID
    }

    fn keeps_samples(&self) -> bool {
        true
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let overdue_ready =
            fetch_aggregate_count(client, environment, credentials, JOBS_OVERDUE_PATH);
        let error = fetch_aggregate_count(client, environment, credentials, JOBS_ERROR_PATH);
        let (overdue_ready, error) = match (overdue_ready, error) {
            (Ok(overdue_ready), Ok(error)) => (overdue_ready, error),
            (overdue_ready, error) => {
                return Err(match overdue_ready.err().or_else(|| error.err()) {
                    Some(error) => error,
                    None => anyhow!("jobs probe failed"),
                });
            }
        };
        Ok(Observation {
            state: jobs_state(overdue_ready),
            payload: serde_json::json!({
                "overdue_ready": overdue_ready,
                "error": error,
            }),
            sample: Some((overdue_ready + error) as f64),
        })
    }
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
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("jobs-asleep");
        let store = db.store();
        let observed_at = crate::collector::unix_now();
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
