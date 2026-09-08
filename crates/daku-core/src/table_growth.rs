//! Table growth Signal: row counts on the watched tables.
//!
//! Absolute counts (`syslog` at 41K and climbing) turn "the instance feels
//! slow" into something checkable, and a sudden jump after a clone or
//! integration change points at the table to look at first. Five Aggregate
//! `count` queries per tick — the cheapest read the platform offers.
//!
//! Informational only: counts are not health, so this Signal never votes in
//! the rollup (see `health.rs`) and has no threshold. A table the monitoring
//! account cannot read lands as `null` in the payload rather than failing
//! the whole probe; only when *every* table fails does the Signal read
//! `down`. Trends arrive later (Wave 5 roll-ups); today the card shows the
//! point-in-time counts.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const TABLE_GROWTH_SIGNAL_ID: &str = "table_growth";

/// Tables watched for growth. Order is stable: payload rows and the drill-in
/// render in this order.
pub const WATCHED_TABLES: [&str; 5] =
    ["syslog", "sys_email", "ecc_queue", "sys_attachment", "task"];

fn growth_path(table: &str) -> String {
    format!("/api/now/stats/{table}?sysparm_count=true")
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct TableCount {
    table: String,
    count: Option<u64>,
}

pub fn table_growth_state() -> SignalState {
    SignalState::Healthy
}

fn fetch_table_counts(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<Vec<TableCount>> {
    let mut tables = Vec::with_capacity(WATCHED_TABLES.len());
    let mut failures = 0;
    for table in WATCHED_TABLES {
        match fetch_aggregate_count(client, environment, credentials, &growth_path(table)) {
            Ok(count) => tables.push(TableCount {
                table: table.into(),
                count: Some(count),
            }),
            Err(_) => {
                failures += 1;
                tables.push(TableCount {
                    table: table.into(),
                    count: None,
                });
            }
        }
    }
    if failures == WATCHED_TABLES.len() {
        anyhow::bail!("all {} table counts failed", WATCHED_TABLES.len());
    }
    Ok(tables)
}

#[derive(Default)]
pub struct TableGrowthSignal;

pub type TableGrowthCollector = PerEnvironmentCollector<TableGrowthSignal>;

impl Signal for TableGrowthSignal {
    fn id(&self) -> &'static str {
        TABLE_GROWTH_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let tables = fetch_table_counts(client, environment, credentials)?;
        Ok(Observation {
            state: table_growth_state(),
            payload: serde_json::json!({ "tables": tables }),
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

    fn count_envelope(count: u64) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: format!(r#"{{"result":{{"stats":{{"count":"{count}"}}}}}}"#),
        }
    }

    struct GrowthTransport {
        /// Per-table counts by table name; missing tables 403.
        counts: Vec<(&'static str, u64)>,
        forbidden: Vec<&'static str>,
    }

    impl HttpTransport for GrowthTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            let table = request
                .url
                .split("/api/now/stats/")
                .nth(1)
                .and_then(|rest| rest.split('?').next())
                .unwrap_or("");
            assert!(
                WATCHED_TABLES.contains(&table),
                "growth collector must only count watched tables: {}",
                request.url
            );
            assert!(
                !request.url.contains("sysparm_query"),
                "growth counts are whole-table (no date bound): {}",
                request.url
            );
            if self.forbidden.contains(&table) {
                return Ok(HttpResponse {
                    status: 403,
                    headers: Vec::new(),
                    body: r#"{"error":{"message":"Operation not allowed"}}"#.into(),
                });
            }
            let (_, count) = self
                .counts
                .iter()
                .find(|(name, _)| *name == table)
                .expect("scripted count");
            Ok(count_envelope(*count))
        }
    }

    fn collect_with(transport: GrowthTransport) -> (TempDb, StateStore) {
        let db = TempDb::new("growth");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = TableGrowthCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(transport, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    fn all_counts() -> GrowthTransport {
        GrowthTransport {
            counts: vec![
                ("syslog", 41_000),
                ("sys_email", 12_000),
                ("ecc_queue", 300),
                ("sys_attachment", 8_000),
                ("task", 150_000),
            ],
            forbidden: Vec::new(),
        }
    }

    #[test]
    fn table_growth_writes_all_counts_and_stays_healthy() {
        let (_db, store) = collect_with(all_counts());
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TABLE_GROWTH_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        let tables = payload["tables"].as_array().unwrap();
        assert_eq!(tables.len(), WATCHED_TABLES.len());
        assert_eq!(tables[0]["table"], "syslog");
        assert_eq!(tables[0]["count"], 41_000);
        assert!(
            persistence::load_signal_samples(&connection, "prod", TABLE_GROWTH_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn table_growth_keeps_partial_counts_when_one_table_forbids() {
        let (_db, store) = collect_with(GrowthTransport {
            forbidden: vec!["task"],
            ..all_counts()
        });
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TABLE_GROWTH_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        let tables = payload["tables"].as_array().unwrap();
        let task = tables.iter().find(|row| row["table"] == "task").unwrap();
        assert!(task["count"].is_null(), "unreadable tables read null");
        let syslog = tables.iter().find(|row| row["table"] == "syslog").unwrap();
        assert_eq!(syslog["count"], 41_000);
    }

    #[test]
    fn table_growth_all_tables_failing_is_down() {
        let (_db, store) = collect_with(GrowthTransport {
            counts: Vec::new(),
            forbidden: WATCHED_TABLES.to_vec(),
        });
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TABLE_GROWTH_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("tables").is_none());
    }

    struct GrowthFailTransport;

    impl HttpTransport for GrowthFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn table_growth_probe_failure_is_down() {
        let db = TempDb::new("growth-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = TableGrowthCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(GrowthFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TABLE_GROWTH_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn table_growth_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("growth-asleep");
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
        TableGrowthCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TABLE_GROWTH_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
