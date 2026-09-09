//! Active sessions Signal: how many users are logged in right now, and who.
//!
//! Capacity context for every other Signal ("45 active sessions during the
//! slowdown"). Reads `v_user_session` (the Logged-in-users list) with a
//! bounded page and counts the rows. The same page carries the first
//! `ROW_LIST_LIMIT` rows for the Drill-in (user + login time), so a tick
//! costs one request whether or not anybody is logged on.
//!
//! `v_user_session` is per application node, not instance-wide — the count
//! and rows describe the node that answered. `sys_created_on` is the login
//! time; the JDBC schema also lists `last_accessed`, which is intentionally
//! not queried until its REST shape is confirmed against a live instance.
//!
//! Informational only: this Signal never votes in the health rollup (see
//! `health.rs`) and has no threshold. A failed read still lands as `down`
//! with the error, so a lost read ACL is visible instead of silent.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal, take_bounded};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::ServiceNowClient;
use crate::signal_eval::row_text;

pub const SESSIONS_SIGNAL_ID: &str = "sessions";
/// One more than the cap so a full page proves truncation.
pub const SESSIONS_PAGE_LIMIT: usize = 101;
/// Display cap: counts above this read "100+".
pub const SESSIONS_DISPLAY_CAP: u64 = 100;
/// Newest logins first so the bounded Drill-in rows are the most recent.
/// `user` is a reference to `sys_user`: the Table API returns it as a
/// `{display_value, link}` object (or a plain id string with display params),
/// so the row mapper below accepts both shapes.
pub const SESSIONS_PATH: &str = "/api/now/table/v_user_session?sysparm_fields=sys_id,user,sys_created_on&sysparm_query=ORDERBYDESCsys_created_on&sysparm_limit=101";

pub fn sessions_state() -> SignalState {
    SignalState::Healthy
}

/// One logged-on user for the Drill-in. `user` is the display name (em dash
/// when unreadable); `user_id` is the `sys_user` id for the record link, when
/// the reference shape carries it. `sys_created_on` is the login time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct SessionRow {
    sys_id: String,
    user: String,
    user_id: String,
    sys_created_on: String,
}

/// Display name out of a `user` reference in any Table API shape: a plain
/// string, `{display_value, link}`, or `{value}`. Empty when unreadable.
fn session_user(row: &serde_json::Value) -> String {
    match row.get("user") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Object(_)) => row
            .get("user")
            .and_then(|user| user.get("display_value"))
            .and_then(|name| name.as_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                row.get("user")
                    .and_then(|user| user.get("value"))
                    .and_then(|id| id.as_str())
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_default(),
        _ => row_text(row, "user"),
    }
}

/// `sys_user` id out of a `user` reference: the trailing segment of
/// `{link: ".../sys_user/<id>"}`, `{value}`, or the plain string itself.
fn session_user_id(row: &serde_json::Value) -> String {
    if let Some(user) = row.get("user") {
        if let Some(link) = user.get("link").and_then(|link| link.as_str())
            && let Some(id) = link.rsplit('/').next().filter(|id| !id.is_empty())
        {
            return id.to_owned();
        }
        if let Some(id) = user
            .get("value")
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
        {
            return id.to_owned();
        }
        if let Some(id) = user.as_str().filter(|id| !id.is_empty()) {
            return id.to_owned();
        }
    }
    String::new()
}

fn session_row(row: &serde_json::Value) -> Option<SessionRow> {
    let sys_id = row_text(row, "sys_id");
    if sys_id.is_empty() {
        return None;
    }
    Some(SessionRow {
        sys_id,
        user: session_user(row),
        user_id: session_user_id(row),
        sys_created_on: row_text(row, "sys_created_on"),
    })
}

fn fetch_session_count(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<(u64, bool, Vec<SessionRow>, bool)> {
    let response = client.request(environment, credentials, "GET", SESSIONS_PATH, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let rows = serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
        .ok()
        .and_then(|value| value.get("result")?.as_array().cloned())
        .ok_or_else(|| anyhow::anyhow!("session list response has no result array"))?;
    let truncated = rows.len() >= SESSIONS_PAGE_LIMIT;
    let count = rows.len().min(SESSIONS_DISPLAY_CAP as usize) as u64;
    let (session_rows, session_rows_truncated) =
        take_bounded(rows.iter().filter_map(session_row).collect());
    Ok((count, truncated, session_rows, session_rows_truncated))
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
        let (active_sessions, truncated, session_rows, session_rows_truncated) =
            fetch_session_count(client, environment, credentials)?;
        Ok(Observation {
            state: sessions_state(),
            payload: serde_json::json!({
                "active_sessions": active_sessions,
                "truncated": truncated,
                "session_rows": session_rows,
                "session_rows_truncated": session_rows_truncated,
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
            assert!(
                request.url.contains("sysparm_fields=") && request.url.contains("user"),
                "sessions read must fetch the user reference for the drill-in: {}",
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
        assert_eq!(payload["session_rows"].as_array().unwrap().len(), 3);
        assert_eq!(payload["session_rows_truncated"], false);
    }

    #[test]
    fn sessions_signal_persists_user_rows_with_record_ids() {
        let (_db, store) = collect_with(
            r#"{"result":[
                {"sys_id":"s1","user":{"display_value":"Fred Johnson","link":"https://x.example.service-now.com/api/now/table/sys_user/u1"},"sys_created_on":"2026-01-27 00:12:00"},
                {"sys_id":"s2","user":"u2","sys_created_on":"2026-01-27 00:10:00"}
            ]}"#,
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["active_sessions"], 2);
        let rows = payload["session_rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["user"], "Fred Johnson");
        assert_eq!(rows[0]["user_id"], "u1");
        assert_eq!(rows[0]["sys_created_on"], "2026-01-27 00:12:00");
        assert_eq!(rows[1]["user"], "u2");
        assert_eq!(rows[1]["user_id"], "u2");
    }

    #[test]
    fn sessions_signal_rows_bound_at_ten_while_count_uses_full_page() {
        let mut rows = String::from(r#"{"result":["#);
        for index in 0..20 {
            if index > 0 {
                rows.push(',');
            }
            rows.push_str(&format!(
                r#"{{"sys_id":"s{index}","user":{{"display_value":"User {index}","link":"https://x.example.service-now.com/api/now/table/sys_user/u{index}"}},"sys_created_on":"2026-01-27 00:12:00"}}"#
            ));
        }
        rows.push_str("]}");
        let body = Box::leak(rows.into_boxed_str());
        let (_db, store) = collect_with(body);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["active_sessions"], 20);
        assert_eq!(payload["truncated"], false);
        assert_eq!(payload["session_rows"].as_array().unwrap().len(), 10);
        assert_eq!(payload["session_rows_truncated"], true);
    }

    #[test]
    fn sessions_signal_rows_without_sys_id_do_not_break_the_count() {
        let (_db, store) = collect_with(
            r#"{"result":[{"sys_id":"s1","user":{"display_value":"Fred","link":"https://x/api/now/table/sys_user/u1"}},{"no_id":true}]}"#,
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SESSIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["active_sessions"], 2);
        let rows = payload["session_rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["user"], "Fred");
        assert_eq!(payload["session_rows_truncated"], false);
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
        assert_eq!(payload["session_rows"].as_array().unwrap().len(), 0);
        assert_eq!(payload["session_rows_truncated"], false);
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
        assert_eq!(
            payload["session_rows"].as_array().unwrap().len(),
            crate::collector::ROW_LIST_LIMIT
        );
        assert_eq!(payload["session_rows_truncated"], true);
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
