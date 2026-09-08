//! Active sessions Signal: how many users are logged in right now.
//!
//! Capacity context for every other Signal ("45 active sessions during the
//! slowdown"). Reads `v_user_session` (the Logged-in-users list) with a
//! bounded page and counts the rows — no field names beyond the universal
//! `sys_id`, so there is nothing release-specific to get wrong. The count is
//! capped at 100 (`truncated` says so); exactness does not matter at this
//! granularity.
//!
//! Informational only: this Signal never votes in the health rollup (see
//! `health.rs`) and has no threshold. A failed read still lands as `down`
//! with the error, so a lost read ACL is visible instead of silent.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::ServiceNowClient;

pub const SESSIONS_SIGNAL_ID: &str = "sessions";
/// One more than the cap so a full page proves truncation.
pub const SESSIONS_PAGE_LIMIT: usize = 101;
/// Display cap: counts above this read "100+".
pub const SESSIONS_DISPLAY_CAP: u64 = 100;
pub const SESSIONS_PATH: &str =
    "/api/now/table/v_user_session?sysparm_fields=sys_id&sysparm_limit=101";

pub fn sessions_state() -> SignalState {
    SignalState::Healthy
}

fn fetch_session_count(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<(u64, bool)> {
    let response = client.request(environment, credentials, "GET", SESSIONS_PATH, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let rows = serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
        .ok()
        .and_then(|value| value.get("result")?.as_array().cloned())
        .ok_or_else(|| anyhow::anyhow!("session list response has no result array"))?;
    let truncated = rows.len() >= SESSIONS_PAGE_LIMIT;
    Ok((
        rows.len().min(SESSIONS_DISPLAY_CAP as usize) as u64,
        truncated,
    ))
}

#[derive(Default)]
pub struct SessionsSignal;

pub type SessionsCollector = PerEnvironmentCollector<SessionsSignal>;

impl Signal for SessionsSignal {
    fn id(&self) -> &'static str {
        SESSIONS_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let (active_sessions, truncated) = fetch_session_count(client, environment, credentials)?;
        Ok(Observation {
            state: sessions_state(),
            payload: serde_json::json!({
                "active_sessions": active_sessions,
                "truncated": truncated,
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

    struct SessionsTransport {
        body: &'static str,
    }

    impl HttpTransport for SessionsTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/table/v_user_session"),
                "sessions collector must read the logged-in-users list: {}",
                request.url
            );
            assert!(
                request.url.contains("sysparm_limit=101"),
                "sessions read must stay bounded: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    fn collect_with(body: &'static str) -> (TempDb, StateStore) {
        let db = TempDb::new("sessions");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = SessionsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(SessionsTransport { body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn sessions_signal_counts_rows_and_stays_healthy() {
        let (_db, store) =
            collect_with(r#"{"result":[{"sys_id":"s1"},{"sys_id":"s2"},{"sys_id":"s3"}]}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["active_sessions"], 3);
        assert_eq!(payload["truncated"], false);
    }

    #[test]
    fn sessions_signal_empty_list_is_healthy_zero() {
        let (_db, store) = collect_with(r#"{"result":[]}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["active_sessions"], 0);
    }

    #[test]
    fn sessions_signal_full_page_reads_truncated_at_the_cap() {
        let mut rows = String::from(r#"{"result":["#);
        for index in 0..SESSIONS_PAGE_LIMIT {
            if index > 0 {
                rows.push(',');
            }
            rows.push_str(&format!(r#"{{"sys_id":"s{index}"}}"#));
        }
        rows.push_str("]}");
        let body = Box::leak(rows.into_boxed_str());
        let (_db, store) = collect_with(body);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["active_sessions"], SESSIONS_DISPLAY_CAP);
        assert_eq!(payload["truncated"], true);
    }

    #[test]
    fn sessions_signal_missing_result_array_is_down() {
        let (_db, store) = collect_with(r#"{"error":{"message":"nope"}}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
    }

    struct SessionsFailTransport;

    impl HttpTransport for SessionsFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn sessions_signal_probe_failure_is_down() {
        let db = TempDb::new("sessions-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = SessionsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(SessionsFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("active_sessions").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn sessions_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("sessions-asleep");
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
        SessionsCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
