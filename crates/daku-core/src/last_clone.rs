//! Last-clone Signal: informational completed timestamp from the clone-source Environment.

use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::collector::SignalCollector;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::ServiceNowClient;

pub const LAST_CLONE_SIGNAL_ID: &str = "last_clone";
pub const CLONE_INSTANCE_PATH: &str = "/api/now/table/clone_instance?sysparm_query=state=Completed^ORDERBYDESCcompleted&sysparm_fields=state,completed,target&sysparm_limit=1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCloneObservation {
    pub supported: bool,
    pub completed: Option<String>,
}

pub fn parse_last_clone(status: u16, body: &str) -> LastCloneObservation {
    if status != 200 {
        return LastCloneObservation {
            supported: false,
            completed: None,
        };
    }
    let completed = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_array())
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("completed"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        });
    LastCloneObservation {
        supported: completed.is_some(),
        completed,
    }
}

pub struct LastCloneCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
}

impl LastCloneCollector {
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

impl SignalCollector for LastCloneCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let Some(source) = self
            .environments
            .iter()
            .find(|environment| environment.clone_source)
        else {
            return Ok(());
        };
        let connection = self.store.open()?;
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        match self.client.request(
            source,
            self.credentials.as_ref(),
            "GET",
            CLONE_INSTANCE_PATH,
            None,
        ) {
            Ok(response) if response.status == 200 || response.status == 403 => persist_last_clone(
                &connection,
                &source.id,
                &parse_last_clone(response.status, &response.body),
                observed_at,
            )
            .map_err(anyhow::Error::from),
            Ok(response) => persist_last_clone_unreachable(
                &connection,
                &source.id,
                &format!("HTTP {}", response.status),
                observed_at,
            )
            .map_err(anyhow::Error::from),
            Err(error) => persist_last_clone_unreachable(
                &connection,
                &source.id,
                &error.to_string(),
                observed_at,
            )
            .map_err(anyhow::Error::from),
        }
    }
}

fn persist_last_clone(
    connection: &Connection,
    environment_id: &str,
    observation: &LastCloneObservation,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({
        "supported": observation.supported,
        "completed": observation.completed,
    });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        LAST_CLONE_SIGNAL_ID,
        observed_at,
        "healthy",
        &payload.to_string(),
    )
}

fn persist_last_clone_unreachable(
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
        LAST_CLONE_SIGNAL_ID,
        observed_at,
        "healthy",
        &payload.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use crate::test_support::TempDb;
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::{AuthMethod, EnvironmentConfig, MemoryCredentialStore};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    #[test]
    fn last_clone_signal_completed_timestamp() {
        let observation = parse_last_clone(
            200,
            include_str!("../tests/fixtures/last_clone/completed.json"),
        );
        assert!(observation.supported);
        assert_eq!(
            observation.completed.as_deref(),
            Some("2026-01-15 12:00:00")
        );
    }

    #[test]
    fn last_clone_signal_403_is_unsupported() {
        let observation = parse_last_clone(403, r#"{"error":{"message":"Operation not allowed"}}"#);
        assert!(!observation.supported);
        assert_eq!(observation.completed, None);
    }

    #[test]
    fn last_clone_signal_empty_is_unsupported() {
        let observation =
            parse_last_clone(200, include_str!("../tests/fixtures/last_clone/empty.json"));
        assert!(!observation.supported);
        assert_eq!(observation.completed, None);
    }

    struct LastCloneTransport {
        status: u16,
        body: &'static str,
    }

    impl HttpTransport for LastCloneTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/table/clone_instance"),
                "last_clone must query clone_instance: {}",
                request.url
            );
            assert!(
                request.url.contains("state=Completed"),
                "last_clone must filter Completed: {}",
                request.url
            );
            assert!(
                request.url.contains("sysparm_limit=1"),
                "last_clone is a single-row fetch: {}",
                request.url
            );
            assert!(
                request.url.contains("acme-prod"),
                "last_clone must query the clone source only: {}",
                request.url
            );
            Ok(HttpResponse {
                status: self.status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    fn env(id: &str, host: &str, clone_source: bool) -> EnvironmentConfig {
        EnvironmentConfig {
            id: id.into(),
            label: id.into(),
            instance_url: format!("https://{host}.example.service-now.com"),
            auth_method: AuthMethod::Basic,
            sort_order: if clone_source { 0 } else { 1 },
            clone_source,
        }
    }

    fn collect_last_clone(status: u16, body: &'static str) -> (TempDb, StateStore) {
        let db = TempDb::new("last-clone");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        credentials.insert("test", r#"{"username":"reader","password":"secret"}"#);
        let collector = LastCloneCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(LastCloneTransport { status, body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn last_clone_signal_completed_writes_healthy_snapshot() {
        let (_db, store) = collect_last_clone(
            200,
            include_str!("../tests/fixtures/last_clone/completed.json"),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["supported"], true);
        assert_eq!(payload["completed"], "2026-01-15 12:00:00");
        assert!(
            persistence::load_signal_snapshot(&connection, "test", LAST_CLONE_SIGNAL_ID)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn last_clone_signal_403_writes_healthy_unsupported() {
        let (_db, store) =
            collect_last_clone(403, r#"{"error":{"message":"Operation not allowed"}}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["supported"], false);
        assert!(payload["completed"].is_null());
        assert!(payload.get("reachability").is_none());
    }

    #[test]
    fn last_clone_signal_probe_failure_is_healthy_unreachable() {
        let (_db, store) = collect_last_clone(500, r#"{"error":{"message":"boom"}}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("supported").is_none());
    }
}
