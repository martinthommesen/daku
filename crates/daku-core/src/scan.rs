//! Instance Scan findings Signal: outstanding P1/P2 findings.
//!
//! Instance Scan is a platform plugin (Quebec+), not core — some instances do
//! not have it. When the table itself is missing (404, or a 400 naming an
//! invalid table) the Signal reads `skipped` with reason `scan_unavailable`
//! instead of `down`: there is nothing to observe, and a permanent red card
//! with no opt-out would be noise. Any other failure (including 403, which
//! means the table exists but the monitoring account cannot read it) stays
//! `down` so a lost role is visible.
//!
//! The 50 newest findings are read and the open ones counted by priority:
//! P1 outstanding at its ceiling degrades (default 1); P2 is counted and
//! listed but never votes alone. "Open" excludes states reading as closed,
//! fixed, resolved, dismissed, or ignored (substring match — exact choice
//! values vary by release); an empty state counts as open. Priority takes
//! the leading digit (`1` and `1 - Critical` both read P1); a missing
//! priority counts as P2 rather than vanishing.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, ROW_LIST_LIMIT, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::ServiceNowClient;

pub const SCAN_SIGNAL_ID: &str = "scan";
pub const SCAN_FINDINGS_PATH: &str = "/api/now/table/scan_finding?sysparm_fields=sys_id,priority,state,sys_updated_on&sysparm_query=ORDERBYDESCsys_updated_on&sysparm_limit=50";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct FindingRow {
    sys_id: String,
    priority: String,
    state: String,
    sys_updated_on: String,
}

fn row_text(row: &serde_json::Value, key: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(value) if value.is_number() => value.to_string(),
        _ => String::new(),
    }
}

/// Closed-ish state substrings; anything else (including empty) reads open.
const CLOSED_MARKERS: [&str; 5] = ["clos", "fix", "resolv", "dismiss", "ignor"];

fn is_open(state: &str) -> bool {
    let lower = state.to_lowercase();
    !CLOSED_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Normalised priority bucket: leading digit 1–4, else P2 (visible,
/// non-critical) rather than vanished.
fn priority_bucket(priority: &str) -> &'static str {
    match priority.trim().chars().next() {
        Some('1') => "P1",
        Some('2') => "P2",
        Some('3') => "P3",
        Some('4') => "P4",
        _ => "P2",
    }
}

pub fn scan_state(p1_open: u64, thresholds: &Thresholds) -> SignalState {
    if p1_open >= thresholds.scan_p1_degraded_at {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

fn fetch_findings(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<Option<Vec<FindingRow>>> {
    let response = client.request(environment, credentials, "GET", SCAN_FINDINGS_PATH, None)?;
    // The plugin is not installed: nothing to observe, not a failure.
    if response.status == 404 || (response.status == 400 && response.body.contains("nvalid table"))
    {
        return Ok(None);
    }
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let rows: Vec<FindingRow> =
        serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
            .ok()
            .and_then(|value| {
                value
                    .get("result")
                    .and_then(|result| result.as_array())
                    .cloned()
            })
            .ok_or_else(|| anyhow::anyhow!("scan findings response has no result array"))?
            .iter()
            .filter_map(|row| {
                let sys_id = row_text(row, "sys_id");
                if sys_id.is_empty() {
                    return None;
                }
                let state = row_text(row, "state");
                if !is_open(&state) {
                    return None;
                }
                Some(FindingRow {
                    sys_id,
                    priority: priority_bucket(&row_text(row, "priority")).into(),
                    state,
                    sys_updated_on: row_text(row, "sys_updated_on"),
                })
            })
            .collect();
    Ok(Some(rows.into_iter().take(ROW_LIST_LIMIT).collect()))
}

#[derive(Default)]
pub struct ScanSignal;

pub type ScanCollector = PerEnvironmentCollector<ScanSignal>;

impl Signal for ScanSignal {
    fn id(&self) -> &'static str {
        SCAN_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let Some(findings) = fetch_findings(client, environment, credentials)? else {
            return Ok(Observation {
                state: SignalState::Skipped,
                payload: serde_json::json!({ "skipped": "scan_unavailable" }),
                sample: None,
            });
        };
        let p1_open = findings.iter().filter(|row| row.priority == "P1").count() as u64;
        let p2_open = findings.iter().filter(|row| row.priority != "P1").count() as u64;
        Ok(Observation {
            state: scan_state(p1_open, &environment.thresholds),
            payload: serde_json::json!({
                "p1_open": p1_open,
                "p2_open": p2_open,
                "finding_rows": findings,
                "finding_rows_truncated": findings.len() >= ROW_LIST_LIMIT,
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
    use crate::config::MemoryCredentialStore;
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    #[test]
    fn scan_state_zero_is_healthy() {
        assert_eq!(scan_state(0, &Thresholds::default()), SignalState::Healthy);
    }

    #[test]
    fn scan_state_nonzero_is_degraded() {
        assert_eq!(scan_state(1, &Thresholds::default()), SignalState::Degraded);
    }

    #[test]
    fn scan_threshold_override_tolerates_one_p1() {
        let thresholds = Thresholds {
            scan_p1_degraded_at: 2,
            ..Thresholds::default()
        };
        assert_eq!(scan_state(1, &thresholds), SignalState::Healthy);
        assert_eq!(scan_state(2, &thresholds), SignalState::Degraded);
    }

    #[test]
    fn is_open_excludes_closed_states_and_keeps_empty() {
        for state in ["Open", "New", "In Progress", ""] {
            assert!(is_open(state), "{state}");
        }
        for state in ["Closed", "Fixed", "Resolved", "Dismissed", "Ignored"] {
            assert!(!is_open(state), "{state}");
        }
    }

    #[test]
    fn priority_bucket_reads_the_leading_digit() {
        assert_eq!(priority_bucket("1"), "P1");
        assert_eq!(priority_bucket("1 - Critical"), "P1");
        assert_eq!(priority_bucket("2"), "P2");
        assert_eq!(priority_bucket("3 - Moderate"), "P3");
        assert_eq!(priority_bucket("4"), "P4");
        assert_eq!(priority_bucket(""), "P2");
        assert_eq!(priority_bucket("Critical"), "P2");
    }

    struct ScanTransport {
        status: u16,
        body: &'static str,
    }

    impl HttpTransport for ScanTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/table/scan_finding"),
                "scan collector must read the findings table: {}",
                request.url
            );
            Ok(HttpResponse {
                status: self.status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    const MIXED: &str = r#"{"result":[
        {"sys_id":"f1","priority":"1","state":"Open","sys_updated_on":"2026-01-15 12:00:01"},
        {"sys_id":"f2","priority":"2 - High","state":"Open","sys_updated_on":"2026-01-14 12:00:01"},
        {"sys_id":"f3","priority":"1","state":"Fixed","sys_updated_on":"2026-01-10 12:00:01"}
    ]}"#;
    const CLEAN: &str = r#"{"result":[
        {"sys_id":"f3","priority":"1","state":"Closed","sys_updated_on":"2026-01-10 12:00:01"}
    ]}"#;

    fn collect_with(status: u16, body: &'static str) -> (TempDb, StateStore) {
        let db = TempDb::new("scan");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = ScanCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(ScanTransport { status, body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn scan_signal_open_p1_writes_degraded_snapshot_with_rows() {
        let (_db, store) = collect_with(200, MIXED);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SCAN_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["p1_open"], 1);
        assert_eq!(payload["p2_open"], 1);
        // The fixed P1 is excluded from rows and counts alike.
        assert_eq!(payload["finding_rows"].as_array().unwrap().len(), 2);
        assert_eq!(payload["finding_rows"][0]["priority"], "P1");
        assert_eq!(payload["finding_rows"][1]["priority"], "P2");
    }

    #[test]
    fn scan_signal_clean_findings_are_healthy() {
        let (_db, store) = collect_with(200, CLEAN);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SCAN_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["p1_open"], 0);
        assert_eq!(payload["p2_open"], 0);
    }

    #[test]
    fn scan_signal_missing_table_is_skipped_not_down() {
        for (status, body) in [
            (404, r#"{"error":{"message":"Not found"}}"#),
            (
                400,
                r#"{"error":{"message":"Invalid table 'scan_finding'"}}"#,
            ),
        ] {
            let (_db, store) = collect_with(status, body);
            let connection = store.open().unwrap();
            let row = persistence::load_signal_snapshot(&connection, "prod", SCAN_SIGNAL_ID)
                .unwrap()
                .expect("snapshot");
            assert_eq!(row.state, "skipped", "status {status}");
            let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
            assert_eq!(payload["skipped"], "scan_unavailable");
        }
    }

    #[test]
    fn scan_signal_forbidden_table_is_down() {
        let (_db, store) = collect_with(403, r#"{"error":{"message":"Operation not allowed"}}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SCAN_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
    }

    struct ScanFailTransport;

    impl HttpTransport for ScanFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn scan_signal_probe_failure_is_down() {
        let db = TempDb::new("scan-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = ScanCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(ScanFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SCAN_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("p1_open").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn scan_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("scan-asleep");
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
        ScanCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SCAN_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
