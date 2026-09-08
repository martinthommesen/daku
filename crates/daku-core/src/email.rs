//! Email failures Signal: `send-failed` mail in the last hour.
//!
//! Failed outbound mail (`sys_email` type `send-failed`) is the quiet edge of
//! the notification story: approvals, password resets, and scheduled reports
//! that never arrived. Off by default — dev Environments fail mail noisily
//! (password resets, test notifications) — so the Operator opts in per
//! Environment with `email_failure_degraded_at`.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const EMAIL_SIGNAL_ID: &str = "email";
pub const EMAIL_FAILURE_PATH: &str = "/api/now/stats/sys_email?sysparm_count=true&sysparm_query=type=send-failed^sys_created_on>javascript:gs.hoursAgoStart(1)";
/// Newest failures first, bounded for the drill-in.
pub const EMAIL_FAILURE_ROWS_PATH: &str = "/api/now/table/sys_email?sysparm_fields=sys_id,subject,recipients,error_string,sys_created_on&sysparm_query=type=send-failed^sys_created_on>javascript:gs.hoursAgoStart(1)^ORDERBYDESCsys_created_on&sysparm_limit=10";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct EmailRow {
    sys_id: String,
    subject: String,
    recipients: String,
    sys_created_on: String,
}

fn row_text(row: &serde_json::Value, key: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(value) if value.is_number() => value.to_string(),
        _ => String::new(),
    }
}

pub fn email_state(email_failed_1h: u64, thresholds: &Thresholds) -> SignalState {
    if email_failed_1h >= thresholds.email_failure_degraded_at {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

/// Offending failure rows, newest first. A failed rows request yields no rows —
/// the count already determined the state. Subjects and recipients render
/// truncated by the client's `cell` helper; the error string stays out of the
/// snapshot (it can carry addresses and message bodies).
fn fetch_email_rows(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> (Vec<EmailRow>, bool) {
    let Ok(response) = client.request(
        environment,
        credentials,
        "GET",
        EMAIL_FAILURE_ROWS_PATH,
        None,
    ) else {
        return (Vec::new(), false);
    };
    if response.status != 200 {
        return (Vec::new(), false);
    }
    let rows: Vec<EmailRow> = serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
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
            let sys_id = row_text(row, "sys_id");
            if sys_id.is_empty() {
                return None;
            }
            Some(EmailRow {
                sys_id,
                subject: row_text(row, "subject"),
                recipients: row_text(row, "recipients"),
                sys_created_on: row_text(row, "sys_created_on"),
            })
        })
        .collect();
    crate::collector::take_bounded(rows)
}

#[derive(Default)]
pub struct EmailSignal;

pub type EmailCollector = PerEnvironmentCollector<EmailSignal>;

impl Signal for EmailSignal {
    fn id(&self) -> &'static str {
        EMAIL_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let email_failed_1h =
            fetch_aggregate_count(client, environment, credentials, EMAIL_FAILURE_PATH)?;
        // Rows only while unhealthy: a zero count costs no extra request.
        let (error_rows, error_rows_truncated) = if email_failed_1h > 0 {
            fetch_email_rows(client, environment, credentials)
        } else {
            (Vec::new(), false)
        };
        Ok(Observation {
            state: email_state(email_failed_1h, &environment.thresholds),
            payload: serde_json::json!({
                "email_failed_1h": email_failed_1h,
                "error_rows": error_rows,
                "error_rows_truncated": error_rows_truncated,
            }),
            sample: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TempDb, prod};
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::{EnvironmentConfig, MemoryCredentialStore};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    fn opted_in() -> EnvironmentConfig {
        EnvironmentConfig {
            thresholds: Thresholds {
                email_failure_degraded_at: 1,
                ..Thresholds::default()
            },
            ..prod()
        }
    }

    #[test]
    fn email_signal_is_off_by_default() {
        assert_eq!(Thresholds::default().email_failure_degraded_at, u64::MAX);
        assert_eq!(
            email_state(25, &Thresholds::default()),
            SignalState::Healthy
        );
    }

    #[test]
    fn email_signal_nonzero_is_degraded_once_opted_in() {
        assert_eq!(email_state(0, &opted_in().thresholds), SignalState::Healthy);
        assert_eq!(
            email_state(2, &opted_in().thresholds),
            SignalState::Degraded
        );
    }

    #[test]
    fn email_threshold_override_tolerates_noise() {
        let thresholds = Thresholds {
            email_failure_degraded_at: 5,
            ..Thresholds::default()
        };
        assert_eq!(email_state(4, &thresholds), SignalState::Healthy);
        assert_eq!(email_state(5, &thresholds), SignalState::Degraded);
    }

    struct EmailCountTransport {
        body: &'static str,
        /// Rows body for table URLs; `None` panics, proving a healthy tick
        /// fetches no rows at all.
        rows: Option<&'static str>,
    }

    impl HttpTransport for EmailCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("/api/now/table/sys_email") {
                let Some(rows) = self.rows else {
                    panic!("healthy email tick must not fetch rows: {}", request.url);
                };
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: rows.into(),
                });
            }
            assert!(
                request.url.contains("/api/now/stats/sys_email"),
                "email collector must use Aggregate API: {}",
                request.url
            );
            assert!(
                request.url.contains("type=send-failed"),
                "email query must count send failures: {}",
                request.url
            );
            assert!(
                request.url.contains("sys_created_on") && request.url.contains("hoursAgoStart"),
                "email query must be date-bound: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    const ERROR_ROWS: &str = r#"{"result":[
        {"sys_id":"e1","subject":"Approval requested","recipients":"owner@example.com","error_string":"SMTP 550","sys_created_on":"2026-01-15 12:00:01"}
    ]}"#;

    fn collect_with(
        environment: EnvironmentConfig,
        body: &'static str,
        rows: Option<&'static str>,
    ) -> (TempDb, StateStore) {
        let db = TempDb::new("email");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = EmailCollector::new(
            vec![environment],
            credentials,
            ServiceNowClient::new(EmailCountTransport { body, rows }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn email_signal_zero_writes_healthy_snapshot_without_sample() {
        let (_db, store) = collect_with(
            opted_in(),
            include_str!("../tests/fixtures/email/count_0.json"),
            None,
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", EMAIL_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["email_failed_1h"], 0);
        assert!(
            persistence::load_signal_samples(&connection, "prod", EMAIL_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn email_signal_nonzero_writes_degraded_snapshot_with_rows() {
        let (_db, store) = collect_with(
            opted_in(),
            include_str!("../tests/fixtures/email/count_2.json"),
            Some(ERROR_ROWS),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", EMAIL_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["email_failed_1h"], 2);
        assert_eq!(payload["error_rows"][0]["subject"], "Approval requested");
        assert!(payload["error_rows"][0].get("error_string").is_none());
        assert_eq!(payload["error_rows_truncated"], false);
    }

    #[test]
    fn email_signal_nonzero_stays_healthy_until_opted_in() {
        let (_db, store) = collect_with(
            prod(),
            include_str!("../tests/fixtures/email/count_2.json"),
            Some(ERROR_ROWS),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", EMAIL_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["email_failed_1h"], 2);
    }

    struct EmailFailTransport;

    impl HttpTransport for EmailFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn email_signal_probe_failure_is_down_without_sample() {
        let db = TempDb::new("email-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = EmailCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(EmailFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", EMAIL_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("email_failed_1h").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn email_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("email-asleep");
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
        EmailCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", EMAIL_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
