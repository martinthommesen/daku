//! MID / ECC Signal: `ecc_agent` health plus `ecc_queue` ready/error counts.

use anyhow::anyhow;
use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const MID_ECC_SIGNAL_ID: &str = "mid_ecc";
pub const ECC_READY_DEGRADED_AT: u64 = 100;
// ponytail: one Table API page (default limit is 10); paginate if an Environment has >10000 MIDs.
pub const ECC_AGENTS_PATH: &str = "/api/now/table/ecc_agent?sysparm_fields=status,validated,version,host_name&sysparm_limit=10000";
pub const ECC_OUTPUT_READY_PATH: &str = "/api/now/stats/ecc_queue?sysparm_count=true&sysparm_query=queue=output^state=ready^sys_created_on>javascript:gs.daysAgoStart(7)";
pub const ECC_ERROR_PATH: &str = "/api/now/stats/ecc_queue?sysparm_count=true&sysparm_query=state=error^sys_created_on>javascript:gs.daysAgoStart(7)";

/// Unhealthy agents persisted alongside the counts, so the Drill-in can name
/// them. Bounded because ADR-0007 keeps one small snapshot per Signal.
pub const UNHEALTHY_LIST_LIMIT: usize = 10;

/// Returns the agent total and one entry per unhealthy agent — the list length
/// *is* `agents_unhealthy`, so the card and the Drill-in cannot disagree.
pub fn classify_mid_agents(body: &[u8]) -> anyhow::Result<(u64, Vec<serde_json::Value>)> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let agents = value
        .get("result")
        .and_then(|result| result.as_array())
        .ok_or_else(|| anyhow!("ecc_agent response missing result array"))?;
    let total = agents.len() as u64;
    let unhealthy = agents
        .iter()
        .filter(|agent| !mid_agent_healthy(agent))
        .map(unhealthy_entry)
        .collect();
    Ok((total, unhealthy))
}

/// A missing or blank field stays absent so the Drill-in renders an em-dash;
/// the agent is still listed, because it is still counted.
fn unhealthy_entry(agent: &serde_json::Value) -> serde_json::Value {
    let field = |key: &str| {
        agent
            .get(key)
            .and_then(|item| item.as_str())
            .filter(|text| !text.is_empty())
    };
    serde_json::json!({
        "host_name": field("host_name"),
        "status": field("status"),
        "version": field("version"),
    })
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

pub fn mid_ecc_state(agents_unhealthy: u64, ecc_error: u64, ecc_output_ready: u64) -> SignalState {
    if agents_unhealthy == 0 && ecc_error == 0 && ecc_output_ready < ECC_READY_DEGRADED_AT {
        SignalState::Healthy
    } else {
        SignalState::Degraded
    }
}

#[derive(Default)]
pub struct MidEccSignal;

pub type MidEccCollector = PerEnvironmentCollector<MidEccSignal>;

impl Signal for MidEccSignal {
    fn id(&self) -> &'static str {
        MID_ECC_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let agents = fetch_mid_agents(client, environment, credentials);
        let ready = fetch_aggregate_count(client, environment, credentials, ECC_OUTPUT_READY_PATH);
        let error = fetch_aggregate_count(client, environment, credentials, ECC_ERROR_PATH);
        let ((agents_total, unhealthy_list), ecc_output_ready, ecc_error) =
            match (agents, ready, error) {
                (Ok(agents), Ok(ready), Ok(error)) => (agents, ready, error),
                (agents, ready, error) => {
                    return Err(agents
                        .err()
                        .or_else(|| ready.err())
                        .or_else(|| error.err())
                        .unwrap_or_else(|| anyhow!("mid_ecc probe failed")));
                }
            };
        let agents_unhealthy = unhealthy_list.len() as u64;
        Ok(Observation {
            state: mid_ecc_state(agents_unhealthy, ecc_error, ecc_output_ready),
            payload: serde_json::json!({
                "agents_total": agents_total,
                "agents_unhealthy": agents_unhealthy,
                "agents_unhealthy_list": &unhealthy_list[..unhealthy_list.len().min(UNHEALTHY_LIST_LIMIT)],
                "agents_unhealthy_list_truncated": unhealthy_list.len() > UNHEALTHY_LIST_LIMIT,
                "ecc_output_ready": ecc_output_ready,
                "ecc_error": ecc_error,
            }),
            sample: None,
        })
    }
}

fn fetch_mid_agents(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<(u64, Vec<serde_json::Value>)> {
    let response = client.request(environment, credentials, "GET", ECC_AGENTS_PATH, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    classify_mid_agents(response.body.as_bytes())
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

    fn collect_with(
        agents: &'static str,
        ready: &'static str,
        error: &'static str,
    ) -> (TempDb, StateStore) {
        let db = TempDb::new("mid-ecc");
        let store = db.store();
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
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn classify_mid_agents_empty_is_ok() {
        let body = include_str!("../tests/fixtures/mid_ecc/agents_empty.json");
        let (total, unhealthy) = classify_mid_agents(body.as_bytes()).unwrap();
        assert_eq!(total, 0);
        assert!(unhealthy.is_empty());
    }

    #[test]
    fn classify_mid_agents_down_is_unhealthy() {
        let body = include_str!("../tests/fixtures/mid_ecc/agents_down.json");
        let (total, unhealthy) = classify_mid_agents(body.as_bytes()).unwrap();
        assert_eq!(total, 1);
        assert_eq!(unhealthy.len(), 1);
    }

    #[test]
    fn classify_mid_agents_validated_false_is_unhealthy() {
        let body = include_str!("../tests/fixtures/mid_ecc/agents_unvalidated.json");
        let (total, unhealthy) = classify_mid_agents(body.as_bytes()).unwrap();
        assert_eq!(total, 1);
        assert_eq!(unhealthy.len(), 1);
    }

    #[test]
    fn mid_ecc_signal_empty_agents_zero_queue_is_healthy() {
        let (_db, store) = collect_with(
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
    }

    #[test]
    fn mid_ecc_signal_down_agent_is_degraded() {
        let (_db, store) = collect_with(
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
    }

    fn mid_ecc_payload(agents: &'static str) -> serde_json::Value {
        let (_db, store) = collect_with(
            agents,
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
            include_str!("../tests/fixtures/mid_ecc/count_0.json"),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        serde_json::from_str(&row.payload_json).unwrap()
    }

    #[test]
    fn mid_ecc_payload_lists_unhealthy_agents() {
        let payload = mid_ecc_payload(include_str!("../tests/fixtures/mid_ecc/agents_mixed.json"));
        let list = payload["agents_unhealthy_list"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["host_name"], "mid-b");
        assert_eq!(list[0]["status"], "Down");
        assert_eq!(list[0]["version"], "5.0.0");
        // The healthy agent is not listed.
        assert!(list.iter().all(|entry| entry["host_name"] != "mid-a"));
        assert_eq!(payload["agents_unhealthy_list_truncated"], false);
    }

    #[test]
    fn mid_ecc_payload_bounds_the_unhealthy_list() {
        let payload = mid_ecc_payload(include_str!(
            "../tests/fixtures/mid_ecc/agents_many_down.json"
        ));
        assert_eq!(payload["agents_unhealthy"], 12);
        assert_eq!(
            payload["agents_unhealthy_list"].as_array().unwrap().len(),
            UNHEALTHY_LIST_LIMIT
        );
        assert_eq!(payload["agents_unhealthy_list_truncated"], true);
    }

    #[test]
    fn mid_ecc_unhealthy_list_matches_the_count() {
        let payload = mid_ecc_payload(include_str!("../tests/fixtures/mid_ecc/agents_mixed.json"));
        let list = payload["agents_unhealthy_list"].as_array().unwrap();
        assert_eq!(payload["agents_unhealthy"], list.len());
        // The nameless unhealthy agent is listed, not dropped.
        assert!(list[1]["host_name"].is_null());
        assert_eq!(list[1]["version"], "5.0.1");
    }

    #[test]
    fn mid_ecc_signal_ecc_error_is_degraded() {
        let (_db, store) = collect_with(
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
    }

    #[test]
    fn mid_ecc_signal_ready_at_ceiling_is_degraded() {
        let (_db, store) = collect_with(
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
    }

    struct MidEccFailTransport;

    impl HttpTransport for MidEccFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn mid_ecc_signal_probe_failure_is_down_without_sample() {
        let db = TempDb::new("mid-ecc-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = MidEccCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(MidEccFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
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
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn mid_ecc_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("mid-ecc-asleep");
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
        MidEccCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", MID_ECC_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
