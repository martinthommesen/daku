//! Scheduled jobs Signal: overdue Ready and Error counts on `sys_trigger`.

use anyhow::anyhow;
use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, ROW_LIST_LIMIT, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const JOBS_SIGNAL_ID: &str = "jobs";
pub const JOBS_OVERDUE_PATH: &str = "/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=0^next_action<javascript:gs.minutesAgoStart(15)";
pub const JOBS_ERROR_PATH: &str =
    "/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=3";
/// Oldest overdue first (waiting longest); errors newest first.
pub const JOBS_OVERDUE_ROWS_PATH: &str = "/api/now/table/sys_trigger?sysparm_fields=sys_id,name,state,next_action&sysparm_query=state=0^next_action<javascript:gs.minutesAgoStart(15)^ORDERBYnext_action&sysparm_limit=10";
pub const JOBS_ERROR_ROWS_PATH: &str = "/api/now/table/sys_trigger?sysparm_fields=sys_id,name,state,next_action&sysparm_query=state=3^ORDERBYDESCsys_updated_on&sysparm_limit=10";

pub fn jobs_state(overdue_ready: u64, error: u64, thresholds: &Thresholds) -> SignalState {
    if overdue_ready >= thresholds.jobs_overdue_degraded_at
        || error >= thresholds.jobs_error_degraded_at
    {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct JobRow {
    sys_id: String,
    name: String,
    detail: String,
}

fn text(row: &serde_json::Value, key: &str) -> String {
    row.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned()
}

/// Offending rows for one non-zero count. A failed rows request yields no
/// rows — the count already determined the state, and the drill-in header
/// still links the filtered list.
fn fetch_job_rows(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
    error_detail: bool,
) -> (Vec<JobRow>, bool) {
    let Ok(response) = client.request(environment, credentials, "GET", path, None) else {
        return (Vec::new(), false);
    };
    if response.status != 200 {
        return (Vec::new(), false);
    }
    let rows: Vec<JobRow> = serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_array())
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            let sys_id = text(row, "sys_id");
            if sys_id.is_empty() {
                return None;
            }
            let name = text(row, "name");
            Some(JobRow {
                sys_id: sys_id.clone(),
                name: if name.is_empty() { sys_id } else { name },
                detail: if error_detail {
                    text(row, "state")
                } else {
                    text(row, "next_action")
                },
            })
        })
        .collect();
    let truncated = rows.len() >= ROW_LIST_LIMIT;
    (rows.into_iter().take(ROW_LIST_LIMIT).collect(), truncated)
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
        // Rows only while unhealthy: a zero count costs no extra request.
        let (overdue_rows, overdue_rows_truncated) = if overdue_ready > 0 {
            fetch_job_rows(
                client,
                environment,
                credentials,
                JOBS_OVERDUE_ROWS_PATH,
                false,
            )
        } else {
            (Vec::new(), false)
        };
        let (error_rows, error_rows_truncated) = if error > 0 {
            fetch_job_rows(client, environment, credentials, JOBS_ERROR_ROWS_PATH, true)
        } else {
            (Vec::new(), false)
        };
        Ok(Observation {
            state: jobs_state(overdue_ready, error, &environment.thresholds),
            payload: serde_json::json!({
                "overdue_ready": overdue_ready,
                "error": error,
                "overdue_rows": overdue_rows,
                "overdue_rows_truncated": overdue_rows_truncated,
                "error_rows": error_rows,
                "error_rows_truncated": error_rows_truncated,
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
        /// Rows body for table URLs; `None` panics, proving a healthy tick
        /// fetches no rows at all.
        rows: Option<&'static str>,
    }

    impl HttpTransport for JobsCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("/api/now/table/sys_trigger") {
                let Some(rows) = self.rows else {
                    panic!("healthy jobs tick must not fetch rows: {}", request.url);
                };
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: rows.into(),
                });
            }
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
                    rows: None,
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
                    rows: Some(
                        r#"{"result":[
                            {"sys_id":"aaa","name":"Nightly sync","state":"0","next_action":"2026-01-01 02:00:00"},
                            {"sys_id":"bbb","name":"","state":"0","next_action":""}
                        ]}"#,
                    ),
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
        assert_eq!(payload["overdue_rows"][0]["name"], "Nightly sync");
        assert_eq!(payload["overdue_rows"][1]["name"], "bbb");
        assert_eq!(payload["overdue_rows_truncated"], false);
        assert_eq!(
            payload["error_rows"].as_array().unwrap().len(),
            0,
            "zero error count fetches no rows"
        );
        let samples =
            persistence::load_signal_samples(&connection, "prod", JOBS_SIGNAL_ID).unwrap();
        assert_eq!(samples[0].value_real, Some(2.0));
    }

    #[test]
    fn jobs_state_honours_threshold_overrides() {
        use crate::config::Thresholds;
        let defaults = Thresholds::default();
        assert_eq!(
            jobs_state(1, 0, &defaults),
            daku_protocol::SignalState::Degraded
        );
        assert_eq!(
            jobs_state(0, 0, &defaults),
            daku_protocol::SignalState::Healthy
        );
        // Error count never voted historically; the MAX default keeps that.
        assert_eq!(
            jobs_state(0, 41, &defaults),
            daku_protocol::SignalState::Healthy
        );
        let lenient = Thresholds {
            jobs_overdue_degraded_at: 5,
            jobs_error_degraded_at: 3,
            ..Thresholds::default()
        };
        assert_eq!(
            jobs_state(4, 2, &lenient),
            daku_protocol::SignalState::Healthy
        );
        assert_eq!(
            jobs_state(5, 0, &lenient),
            daku_protocol::SignalState::Degraded
        );
        assert_eq!(
            jobs_state(0, 3, &lenient),
            daku_protocol::SignalState::Degraded
        );
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
