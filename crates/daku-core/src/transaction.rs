//! Slow transactions Signal: average `syslog_transaction` response time.
//!
//! The availability probe measures one synthetic read; this Signal measures
//! what Operators actually feel — the mean server-side response time across
//! real transactions in the last hour, via the Aggregate API
//! (`avg_fields=response_time`). While degraded it fetches the 10 slowest
//! transactions for the drill-in.
//!
//! Off by default: slowness varies wildly per instance (PDI hardware vs
//! prod), so the Operator opts in per Environment with
//! `transaction_avg_degraded_ms`. `syslog_transaction` is a rotated table,
//! so every query here is date-bounded — never a cross-shard union.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_avg};
use crate::signal_eval::{fetch_table_rows, redact_url, row_text};

pub const TRANSACTION_SIGNAL_ID: &str = "slow_txn";
pub const TRANSACTION_AVG_FIELD: &str = "response_time";
pub const TRANSACTION_AVG_PATH: &str = "/api/now/stats/syslog_transaction?sysparm_avg_fields=response_time&sysparm_query=sys_created_on>javascript:gs.hoursAgoStart(1)";
/// Slowest first, bounded for the drill-in. Date-bounded: this is a rotated
/// table, so an unbounded slowest-first page would union every shard.
pub const TRANSACTION_SLOW_ROWS_PATH: &str = "/api/now/table/syslog_transaction?sysparm_fields=sys_id,url,response_time,sys_created_on&sysparm_query=sys_created_on>javascript:gs.hoursAgoStart(1)^ORDERBYDESCresponse_time&sysparm_limit=10";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct SlowRow {
    sys_id: String,
    url: String,
    response_time: String,
    sys_created_on: String,
}

pub fn transaction_state(avg_ms: f64, thresholds: &Thresholds) -> SignalState {
    if thresholds
        .transaction_avg_degraded_ms
        .is_some_and(|ceiling| avg_ms >= ceiling as f64)
    {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

/// Slowest transactions, slowest first. A failed rows request yields no rows —
/// the average already determined the state.
fn fetch_slow_rows(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> (Vec<SlowRow>, bool) {
    fetch_table_rows(
        client,
        environment,
        credentials,
        TRANSACTION_SLOW_ROWS_PATH,
        |row| {
            let sys_id = row_text(row, "sys_id");
            if sys_id.is_empty() {
                return None;
            }
            Some(SlowRow {
                sys_id,
                url: redact_url(&row_text(row, "url")),
                response_time: row_text(row, "response_time"),
                sys_created_on: row_text(row, "sys_created_on"),
            })
        },
    )
}

#[derive(Default)]
pub struct TransactionSignal;

pub type TransactionCollector = PerEnvironmentCollector<TransactionSignal>;

impl Signal for TransactionSignal {
    fn id(&self) -> &'static str {
        TRANSACTION_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let avg_ms = fetch_aggregate_avg(
            client,
            environment,
            credentials,
            TRANSACTION_AVG_PATH,
            TRANSACTION_AVG_FIELD,
        )?;
        // Rows only while degraded: a healthy tick costs no extra request —
        // and with the default-off ceiling, ticks never fetch rows until the
        // Operator opts in.
        let state = transaction_state(avg_ms, &environment.thresholds);
        let (slow_rows, slow_rows_truncated) = if state == SignalState::Degraded {
            fetch_slow_rows(client, environment, credentials)
        } else {
            (Vec::new(), false)
        };
        Ok(Observation {
            state,
            payload: serde_json::json!({
                "transaction_avg_ms": avg_ms,
                "slow_rows": slow_rows,
                "slow_rows_truncated": slow_rows_truncated,
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

    fn opted_in(ceiling_ms: u64) -> EnvironmentConfig {
        EnvironmentConfig {
            thresholds: Thresholds {
                transaction_avg_degraded_ms: Some(ceiling_ms),
                ..Thresholds::default()
            },
            ..prod()
        }
    }

    #[test]
    fn transaction_signal_is_off_by_default() {
        assert_eq!(Thresholds::default().transaction_avg_degraded_ms, None);
        assert_eq!(
            transaction_state(25_000.0, &Thresholds::default()),
            SignalState::Healthy
        );
    }

    #[test]
    fn transaction_signal_degrades_at_the_ceiling_once_opted_in() {
        let thresholds = opted_in(500).thresholds;
        assert_eq!(transaction_state(499.9, &thresholds), SignalState::Healthy);
        assert_eq!(transaction_state(500.0, &thresholds), SignalState::Degraded);
    }

    struct TxnTransport {
        avg_body: &'static str,
        /// Rows body for table URLs; `None` panics, proving a healthy tick
        /// fetches no rows at all.
        rows: Option<&'static str>,
    }

    impl HttpTransport for TxnTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("/api/now/table/syslog_transaction") {
                let Some(rows) = self.rows else {
                    panic!(
                        "healthy transaction tick must not fetch rows: {}",
                        request.url
                    );
                };
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: rows.into(),
                });
            }
            assert!(
                request.url.contains("/api/now/stats/syslog_transaction"),
                "transaction collector must use Aggregate API: {}",
                request.url
            );
            assert!(
                request.url.contains("avg_fields=response_time"),
                "transaction query must average response_time: {}",
                request.url
            );
            assert!(
                request.url.contains("sys_created_on") && request.url.contains("hoursAgoStart"),
                "transaction query must be date-bound (rotated table): {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.avg_body.into(),
            })
        }
    }

    const AVG_SLOW: &str = r#"{"result":{"stats":{"avg":{"response_time":"842"}}}}"#;
    const AVG_OK: &str = r#"{"result":{"stats":{"avg":{"response_time":118}}}}"#;
    const SLOW_ROWS: &str = r#"{"result":[
        {"sys_id":"t1","url":"https://acme.example.service-now.com/navpage.do?session=abc","response_time":5200,"sys_created_on":"2026-01-15 12:00:01"}
    ]}"#;

    fn collect_with(
        environment: EnvironmentConfig,
        avg_body: &'static str,
        rows: Option<&'static str>,
    ) -> (TempDb, StateStore) {
        let db = TempDb::new("txn");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = TransactionCollector::new(
            vec![environment],
            credentials,
            ServiceNowClient::new(TxnTransport { avg_body, rows }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn transaction_signal_slow_avg_writes_degraded_snapshot_with_rows() {
        let (_db, store) = collect_with(opted_in(500), AVG_SLOW, Some(SLOW_ROWS));
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TRANSACTION_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["transaction_avg_ms"], 842.0);
        assert_eq!(
            payload["slow_rows"][0]["url"], "https://acme.example.service-now.com/navpage.do",
            "query strings never reach the drill-in"
        );
        assert_eq!(payload["slow_rows"][0]["response_time"], "5200");
        assert_eq!(payload["slow_rows_truncated"], false);
    }

    #[test]
    fn transaction_signal_ok_avg_writes_healthy_snapshot_without_rows() {
        let (_db, store) = collect_with(opted_in(500), AVG_OK, None);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TRANSACTION_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["transaction_avg_ms"], 118.0);
        assert_eq!(payload["slow_rows"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn transaction_signal_slow_avg_stays_healthy_until_opted_in() {
        let (_db, store) = collect_with(prod(), AVG_SLOW, None);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TRANSACTION_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
    }

    struct TxnFailTransport;

    impl HttpTransport for TxnFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn transaction_signal_probe_failure_is_down() {
        let db = TempDb::new("txn-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = TransactionCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(TxnFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TRANSACTION_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("transaction_avg_ms").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn transaction_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("txn-asleep");
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
        TransactionCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", TRANSACTION_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
