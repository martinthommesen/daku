//! MID / ECC Signal: `ecc_agent` health plus `ecc_queue` ready/error counts.

use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use rusqlite::Connection;

use crate::collector::SignalCollector;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const MID_ECC_SIGNAL_ID: &str = "mid_ecc";
pub const ECC_READY_DEGRADED_AT: u64 = 100;
// ponytail: one Table API page (default limit is 10); paginate if an Environment has >10000 MIDs.
pub const ECC_AGENTS_PATH: &str = "/api/now/table/ecc_agent?sysparm_fields=status,validated,version,host_name&sysparm_limit=10000";
pub const ECC_OUTPUT_READY_PATH: &str = "/api/now/stats/ecc_queue?sysparm_count=true&sysparm_query=queue=output^state=ready^sys_created_on>javascript:gs.daysAgoStart(7)";
pub const ECC_ERROR_PATH: &str = "/api/now/stats/ecc_queue?sysparm_count=true&sysparm_query=state=error^sys_created_on>javascript:gs.daysAgoStart(7)";

pub fn classify_mid_agents(body: &[u8]) -> anyhow::Result<(u64, u64)> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let agents = value
        .get("result")
        .and_then(|result| result.as_array())
        .ok_or_else(|| anyhow!("ecc_agent response missing result array"))?;
    let total = agents.len() as u64;
    let unhealthy = agents
        .iter()
        .filter(|agent| !mid_agent_healthy(agent))
        .count() as u64;
    Ok((total, unhealthy))
}

fn mid_agent_healthy(agent: &serde_json::Value) -> bool {
    agent.get("status").and_then(|status| status.as_str()) == Some("Up")
        && is_validated_true(agent.get("validated"))
}

fn is_validated_true(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::String(text)) => text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub fn mid_ecc_state(agents_unhealthy: u64, ecc_error: u64, ecc_output_ready: u64) -> &'static str {
    if agents_unhealthy == 0 && ecc_error == 0 && ecc_output_ready < ECC_READY_DEGRADED_AT {
        "healthy"
    } else {
        "degraded"
    }
}

pub struct MidEccCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
}

impl MidEccCollector {
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

impl SignalCollector for MidEccCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let mut first_error = None;
        for environment in &self.environments {
            if let Err(error) = collect_mid_ecc(
                &connection,
                environment,
                observed_at,
                fetch_mid_agents(&self.client, environment, self.credentials.as_ref()),
                fetch_aggregate_count(
                    &self.client,
                    environment,
                    self.credentials.as_ref(),
                    ECC_OUTPUT_READY_PATH,
                ),
                fetch_aggregate_count(
                    &self.client,
                    environment,
                    self.credentials.as_ref(),
                    ECC_ERROR_PATH,
                ),
            ) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn fetch_mid_agents(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<(u64, u64)> {
    let response = client.request(environment, credentials, "GET", ECC_AGENTS_PATH, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    classify_mid_agents(response.body.as_bytes())
}

fn collect_mid_ecc(
    connection: &Connection,
    environment: &EnvironmentConfig,
    observed_at: i64,
    agents: anyhow::Result<(u64, u64)>,
    ready: anyhow::Result<u64>,
    error: anyhow::Result<u64>,
) -> anyhow::Result<()> {
    match (agents, ready, error) {
        (Ok((agents_total, agents_unhealthy)), Ok(ecc_output_ready), Ok(ecc_error)) => {
            persist_mid_ecc_ok(
                connection,
                &environment.id,
                agents_total,
                agents_unhealthy,
                ecc_output_ready,
                ecc_error,
                observed_at,
            )
            .map_err(anyhow::Error::from)
        }
        (agents, ready, error) => {
            let message = agents
                .err()
                .or_else(|| ready.err())
                .or_else(|| error.err())
                .map(|error| error.to_string())
                .unwrap_or_else(|| "mid_ecc probe failed".into());
            persist_mid_ecc_down(connection, &environment.id, &message, observed_at)
                .map_err(anyhow::Error::from)
        }
    }
}

fn persist_mid_ecc_ok(
    connection: &Connection,
    environment_id: &str,
    agents_total: u64,
    agents_unhealthy: u64,
    ecc_output_ready: u64,
    ecc_error: u64,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({
        "agents_total": agents_total,
        "agents_unhealthy": agents_unhealthy,
        "ecc_output_ready": ecc_output_ready,
        "ecc_error": ecc_error,
    });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        MID_ECC_SIGNAL_ID,
        observed_at,
        mid_ecc_state(agents_unhealthy, ecc_error, ecc_output_ready),
        &payload.to_string(),
    )
}

fn persist_mid_ecc_down(
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
        MID_ECC_SIGNAL_ID,
        observed_at,
        "down",
        &payload.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::{AuthMethod, EnvironmentConfig, MemoryCredentialStore};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    struct MidEccTransport {
        agents: &'static str,
        ready: &'static str,
        error: &'static str,
    }

    impl HttpTransport for MidEccTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            let body = if request.url.contains("/api/now/table/ecc_agent") {
                assert!(
                    request.url.contains("sysparm_limit="),
                    "ecc_agent must set sysparm_limit (Table API default is 10): {}",
                    request.url
                );
                self.agents
            } else if request.url.contains("/api/now/stats/ecc_queue") {
                assert!(
                    request.url.contains("sys_created_on") && request.url.contains("daysAgoStart"),
                    "ecc_queue query must be date-bound: {}",
                    request.url
                );
                if request.url.contains("state=error") {
                    self.error
                } else {
                    assert!(
                        request.url.contains("queue=output") && request.url.contains("state=ready"),
                        "ready count must query output/ready: {}",
                        request.url
                    );
                    self.ready
                }
            } else {
                panic!("unexpected mid_ecc URL: {}", request.url);
            };
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.into(),
            })
        }
    }

    fn prod() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "prod".into(),
            label: "Production".into(),
            instance_url: "https://acme-prod.example.service-now.com".into(),
            auth_method: AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
        }
    }

    fn collect_with(
        agents: &'static str,
        ready: &'static str,
        error: &'static str,
    ) -> (std::path::PathBuf, StateStore) {
        let path = std::env::temp_dir().join(format!("daku-mid-ecc-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = MidEccCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(
                MidEccTransport {
                    agents,
                    ready,
                    error,
                },
                SystemClock,
            ),
            store,
        );
        collector.collect().unwrap();
        (path.clone(), StateStore::daemon(path))
    }

    #[test]
    fn classify_mid_agents_empty_is_ok() {
        let body = include_str!("../tests/fixtures/mid_ecc/agents_empty.json");
        let (total, unhealthy) = classify_mid_agents(body.as_bytes()).unwrap();
        assert_eq!(total, 0);
        assert_eq!(unhealthy, 0);
    }

    #[test]
    fn classify_mid_agents_down_is_unhealthy() {
        let body = include_str!("../tests/fixtures/mid_ecc/agents_down.json");
        let (total, unhealthy) = classify_mid_agents(body.as_bytes()).unwrap();
        assert_eq!(total, 1);
        assert_eq!(unhealthy, 1);
    }

    #[test]
    fn classify_mid_agents_validated_false_is_unhealthy() {
        let body = include_str!("../tests/fixtures/mid_ecc/agents_unvalidated.json");
        let (total, unhealthy) = classify_mid_agents(body.as_bytes()).unwrap();
        assert_eq!(total, 1);
        assert_eq!(unhealthy, 1);
    }

    #[test]
    fn mid_ecc_signal_empty_agents_zero_queue_is_healthy() {
        let (path, store) = collect_with(
            include_str!("../tests/fixtures/mid_ecc/agents_empty.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["agents_total"], 0);
        assert_eq!(payload["agents_unhealthy"], 0);
        assert_eq!(payload["ecc_output_ready"], 0);
        assert_eq!(payload["ecc_error"], 0);
        assert!(
            persistence::load_signal_samples(&connection, "prod", MID_ECC_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mid_ecc_signal_down_agent_is_degraded() {
        let (path, store) = collect_with(
            include_str!("../tests/fixtures/mid_ecc/agents_down.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["agents_total"], 1);
        assert_eq!(payload["agents_unhealthy"], 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mid_ecc_signal_ecc_error_is_degraded() {
        let (path, store) = collect_with(
            include_str!("../tests/fixtures/mid_ecc/agents_empty.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
            include_str!("../tests/fixtures/mid_ecc/count_2.json"),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["ecc_error"], 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mid_ecc_signal_ready_at_ceiling_is_degraded() {
        let (path, store) = collect_with(
            include_str!("../tests/fixtures/mid_ecc/agents_empty.json"),
            include_str!("../tests/fixtures/mid_ecc/count_100.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["ecc_output_ready"], 100);
        let _ = std::fs::remove_file(path);
    }

    struct MidEccFailTransport;

    impl HttpTransport for MidEccFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn mid_ecc_signal_probe_failure_is_down_without_sample() {
        let path =
            std::env::temp_dir().join(format!("daku-mid-ecc-fail-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = MidEccCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(MidEccFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("ecc_error").is_none());
        assert!(
            persistence::load_signal_samples(&connection, "prod", MID_ECC_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }
}
