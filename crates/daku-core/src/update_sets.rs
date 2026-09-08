//! Update-set backlog Signal: open `sys_update_set` records per Environment.
//!
//! Stale open update sets collide at commit time and hide unreviewed
//! customizations, so the Operator wants the backlog glanceable. This Signal
//! reads the 10 newest update sets and counts the ones still being worked
//! on. State matching is substring-based (`open`, `build`, `progress` — the
//! choice values vary by release between "Open" and "In Progress") because
//! an exact choice miss would read as a permanently clean backlog; closed
//! states (`complete`, `ignore`, `committed`, …) never match.
//!
//! Off by default: developers keep work-in-progress sets open while building,
//! so the Operator opts in per Environment with
//! `update_sets_open_degraded_at`. Conflict detection (preview problems via
//! the CI/CD Update Set API) is a later wave — previews are expensive,
//! role-gated reads that do not belong in a 2-minute poll.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::ServiceNowClient;

pub const UPDATE_SETS_SIGNAL_ID: &str = "update_sets";
pub const UPDATE_SETS_PATH: &str = "/api/now/table/sys_update_set?sysparm_fields=sys_id,name,state,sys_updated_on&sysparm_query=ORDERBYDESCsys_updated_on&sysparm_limit=10";

/// Substrings (lowercased state) that read as still being worked on.
const OPEN_MARKERS: [&str; 3] = ["open", "build", "progress"];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct UpdateSetRow {
    sys_id: String,
    name: String,
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

fn is_open(state: &str) -> bool {
    let lower = state.to_lowercase();
    OPEN_MARKERS.iter().any(|marker| lower.contains(marker))
}

pub fn update_sets_state(open_count: u64, thresholds: &Thresholds) -> SignalState {
    if open_count >= thresholds.update_sets_open_degraded_at {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

fn fetch_update_sets(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<(Vec<UpdateSetRow>, bool)> {
    let response = client.request(environment, credentials, "GET", UPDATE_SETS_PATH, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let rows: Vec<UpdateSetRow> =
        serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
            .ok()
            .and_then(|value| {
                value
                    .get("result")
                    .and_then(|result| result.as_array())
                    .cloned()
            })
            .ok_or_else(|| anyhow::anyhow!("update set response has no result array"))?
            .iter()
            .filter_map(|row| {
                let sys_id = row_text(row, "sys_id");
                if sys_id.is_empty() {
                    return None;
                }
                Some(UpdateSetRow {
                    sys_id,
                    name: row_text(row, "name"),
                    state: row_text(row, "state"),
                    sys_updated_on: row_text(row, "sys_updated_on"),
                })
            })
            .collect();
    Ok(crate::collector::take_bounded(rows))
}

#[derive(Default)]
pub struct UpdateSetsSignal;

pub type UpdateSetsCollector = PerEnvironmentCollector<UpdateSetsSignal>;

impl Signal for UpdateSetsSignal {
    fn id(&self) -> &'static str {
        UPDATE_SETS_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let (sets, truncated) = fetch_update_sets(client, environment, credentials)?;
        let open: Vec<&UpdateSetRow> = sets.iter().filter(|row| is_open(&row.state)).collect();
        let open_count = open.len() as u64;
        Ok(Observation {
            state: update_sets_state(open_count, &environment.thresholds),
            payload: serde_json::json!({
                "open_count": open_count,
                "open_rows": open,
                "open_rows_truncated": truncated,
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
                update_sets_open_degraded_at: 1,
                ..Thresholds::default()
            },
            ..prod()
        }
    }

    #[test]
    fn update_sets_signal_is_off_by_default() {
        assert_eq!(Thresholds::default().update_sets_open_degraded_at, u64::MAX);
        assert_eq!(
            update_sets_state(25, &Thresholds::default()),
            SignalState::Healthy
        );
    }

    #[test]
    fn is_open_matches_work_states_and_nothing_else() {
        for state in ["Open", "open", "build", "In Progress", "in_progress"] {
            assert!(is_open(state), "{state}");
        }
        for state in [
            "Complete",
            "complete",
            "ignore",
            "Committed",
            "Retrieved",
            "",
        ] {
            assert!(!is_open(state), "{state}");
        }
    }

    struct UpdateSetsTransport {
        body: &'static str,
    }

    impl HttpTransport for UpdateSetsTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/table/sys_update_set"),
                "update-sets collector must read the update set table: {}",
                request.url
            );
            assert!(
                request.url.contains("ORDERBYDESCsys_updated_on"),
                "update-sets collector must read newest first: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    const MIXED: &str = r#"{"result":[
        {"sys_id":"u1","name":"Add field X","state":"build","sys_updated_on":"2026-01-15 12:00:01"},
        {"sys_id":"u2","name":"Fix flow","state":"Open","sys_updated_on":"2026-01-14 12:00:01"},
        {"sys_id":"u3","name":"Shipped","state":"Complete","sys_updated_on":"2026-01-10 12:00:01"}
    ]}"#;
    const CLEAN: &str = r#"{"result":[
        {"sys_id":"u3","name":"Shipped","state":"Complete","sys_updated_on":"2026-01-10 12:00:01"}
    ]}"#;

    fn collect_with(environment: EnvironmentConfig, body: &'static str) -> (TempDb, StateStore) {
        let db = TempDb::new("updatesets");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = UpdateSetsCollector::new(
            vec![environment],
            credentials,
            ServiceNowClient::new(UpdateSetsTransport { body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn update_sets_signal_counts_open_and_writes_degraded_once_opted_in() {
        let (_db, store) = collect_with(opted_in(), MIXED);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPDATE_SETS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["open_count"], 2);
        assert_eq!(payload["open_rows"].as_array().unwrap().len(), 2);
        assert_eq!(payload["open_rows"][0]["name"], "Add field X");
    }

    #[test]
    fn update_sets_signal_open_backlog_stays_healthy_until_opted_in() {
        let (_db, store) = collect_with(prod(), MIXED);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPDATE_SETS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.payload_json).unwrap()["open_count"],
            2
        );
    }

    #[test]
    fn update_sets_signal_clean_history_is_healthy() {
        let (_db, store) = collect_with(opted_in(), CLEAN);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPDATE_SETS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["open_count"], 0);
        assert_eq!(payload["open_rows"].as_array().unwrap().len(), 0);
    }

    struct UpdateSetsFailTransport;

    impl HttpTransport for UpdateSetsFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn update_sets_signal_probe_failure_is_down() {
        let db = TempDb::new("updatesets-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = UpdateSetsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(UpdateSetsFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPDATE_SETS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("open_count").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn update_sets_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("updatesets-asleep");
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
        UpdateSetsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPDATE_SETS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
