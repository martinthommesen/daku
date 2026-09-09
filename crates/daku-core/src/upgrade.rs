//! Upgrade history Signal: what `sys_upgrade_history` says about the last
//! patches.
//!
//! An unnoticed upgrade (or a failed one) explains drift, new syslog errors,
//! and changed behaviour better than any other single row. This Signal reads
//! the newest upgrade records and degrades when one failed in the last 7
//! days. State matching is substring-based (`fail`, `error`, `cancel`,
//! `abort`) because the exact choice values vary by release; unknown states
//! never vote.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, ROW_LIST_LIMIT, Signal, unix_now};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::last_clone::age_days;
use crate::servicenow::ServiceNowClient;
use crate::signal_eval::{evaluate_ge, parse_result_array, row_text};

pub const UPGRADE_SIGNAL_ID: &str = "upgrade";
pub const UPGRADE_HISTORY_PATH: &str = "/api/now/table/sys_upgrade_history?sysparm_fields=sys_id,from_version,to_version,state,upgrade_started,upgrade_finished&sysparm_query=ORDERBYDESCupgrade_finished&sysparm_limit=10";

/// Upgrades in the last 7 days whose state reads as failed count toward
/// degraded. Unparseable dates count too (fail-loud): a failed upgrade with
/// an unreadable timestamp should still surface.
pub const UPGRADE_FAILURE_WINDOW_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct UpgradeRow {
    sys_id: String,
    from_version: String,
    to_version: String,
    state: String,
    upgrade_started: String,
    upgrade_finished: String,
}

fn is_failed(state: &str) -> bool {
    let lower = state.to_lowercase();
    ["fail", "error", "cancel", "abort"]
        .iter()
        .any(|marker| lower.contains(marker))
}

pub fn upgrade_state(failed_7d: u64, thresholds: &Thresholds) -> SignalState {
    evaluate_ge(failed_7d, thresholds.upgrade_failed_degraded_at)
}

fn fetch_upgrades(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> anyhow::Result<Vec<UpgradeRow>> {
    let response = client.request(environment, credentials, "GET", UPGRADE_HISTORY_PATH, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let rows: Vec<UpgradeRow> = parse_result_array(&response.body)
        .iter()
        .filter_map(|row| {
            let sys_id = row_text(row, "sys_id");
            if sys_id.is_empty() {
                return None;
            }
            Some(UpgradeRow {
                sys_id,
                from_version: row_text(row, "from_version"),
                to_version: row_text(row, "to_version"),
                state: row_text(row, "state"),
                upgrade_started: row_text(row, "upgrade_started"),
                upgrade_finished: row_text(row, "upgrade_finished"),
            })
        })
        .collect();
    Ok(rows.into_iter().take(ROW_LIST_LIMIT).collect())
}

#[derive(Default)]
pub struct UpgradeSignal;

pub type UpgradeCollector = PerEnvironmentCollector<UpgradeSignal>;

impl Signal for UpgradeSignal {
    fn id(&self) -> &'static str {
        UPGRADE_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let now = unix_now();
        let upgrades = fetch_upgrades(client, environment, credentials)?;
        let failed_7d = upgrades
            .iter()
            .filter(|row| is_failed(&row.state))
            .filter(|row| {
                age_days(&row.upgrade_finished, now)
                    .is_none_or(|days| days <= UPGRADE_FAILURE_WINDOW_DAYS)
            })
            .count() as u64;
        let last = upgrades.first();
        // Day-granularity age for the summary line; `None` (no upgrades, or
        // an unreadable timestamp) renders without an age below.
        let last_age_days = last.and_then(|row| age_days(&row.upgrade_finished, now));
        Ok(Observation {
            state: upgrade_state(failed_7d, &environment.thresholds),
            payload: serde_json::json!({
                "upgrades": upgrades,
                "upgrades_truncated": upgrades.len() >= ROW_LIST_LIMIT,
                "failed_7d": failed_7d,
                "last_from": last.map(|row| row.from_version.clone()),
                "last_to": last.map(|row| row.to_version.clone()),
                "last_finished": last.map(|row| row.upgrade_finished.clone()),
                "last_age_days": last_age_days,
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
    fn upgrade_state_zero_is_healthy() {
        assert_eq!(
            upgrade_state(0, &Thresholds::default()),
            SignalState::Healthy
        );
    }

    #[test]
    fn upgrade_state_nonzero_is_degraded() {
        assert_eq!(
            upgrade_state(1, &Thresholds::default()),
            SignalState::Degraded
        );
    }

    #[test]
    fn upgrade_threshold_override_tolerates_one_failure() {
        let thresholds = Thresholds {
            upgrade_failed_degraded_at: 2,
            ..Thresholds::default()
        };
        assert_eq!(upgrade_state(1, &thresholds), SignalState::Healthy);
        assert_eq!(upgrade_state(2, &thresholds), SignalState::Degraded);
    }

    #[test]
    fn is_failed_matches_failure_words_and_nothing_else() {
        for state in [
            "Failed",
            "FAILED",
            "Completed with errors",
            "Cancelled",
            "Aborted",
        ] {
            assert!(is_failed(state), "{state}");
        }
        for state in ["Completed", "In progress", "", "Successful"] {
            assert!(!is_failed(state), "{state}");
        }
    }

    struct UpgradeTransport {
        body: &'static str,
    }

    impl HttpTransport for UpgradeTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/table/sys_upgrade_history"),
                "upgrade collector must read the history table: {}",
                request.url
            );
            assert!(
                request.url.contains("ORDERBYDESCupgrade_finished"),
                "upgrade collector must read newest first: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    /// Finished dates are chosen relative to the real clock without formatting
    /// one: 2099 always reads age 0 (clamped), 2020 always reads centuries old.
    const FAILED_RECENT: &str = r#"{"result":[
        {"sys_id":"u1","from_version":"Zurich P0","to_version":"Zurich P1","state":"Failed","upgrade_started":"2099-01-01 01:00:00","upgrade_finished":"2099-01-01 02:00:00"},
        {"sys_id":"u0","from_version":"Yokohama","to_version":"Zurich","state":"Completed","upgrade_started":"2020-01-01 01:00:00","upgrade_finished":"2020-01-01 02:00:00"}
    ]}"#;
    const FAILED_OLD: &str = r#"{"result":[
        {"sys_id":"u1","from_version":"Xanadu","to_version":"Yokohama","state":"Failed","upgrade_started":"2020-01-01 01:00:00","upgrade_finished":"2020-01-01 02:00:00"}
    ]}"#;
    const CLEAN: &str = r#"{"result":[
        {"sys_id":"u1","from_version":"Zurich P0","to_version":"Zurich P1","state":"Completed","upgrade_started":"2099-01-01 01:00:00","upgrade_finished":"2099-01-01 02:00:00"}
    ]}"#;

    fn collect_with(body: &'static str) -> (TempDb, StateStore) {
        let db = TempDb::new("upgrade");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = UpgradeCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(UpgradeTransport { body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn upgrade_signal_recent_failure_writes_degraded_snapshot() {
        let (_db, store) = collect_with(FAILED_RECENT);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPGRADE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["failed_7d"], 1);
        assert_eq!(payload["last_to"], "Zurich P1");
        assert_eq!(payload["last_age_days"], 0);
        assert_eq!(payload["upgrades"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn upgrade_signal_old_failure_stays_healthy() {
        let (_db, store) = collect_with(FAILED_OLD);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPGRADE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["failed_7d"], 0);
    }

    #[test]
    fn upgrade_signal_clean_history_is_healthy() {
        let (_db, store) = collect_with(CLEAN);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPGRADE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["failed_7d"], 0);
        assert_eq!(payload["last_from"], "Zurich P0");
    }

    struct UpgradeFailTransport;

    impl HttpTransport for UpgradeFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn upgrade_signal_probe_failure_is_down() {
        let db = TempDb::new("upgrade-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = UpgradeCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(UpgradeFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPGRADE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("upgrades").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn upgrade_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("upgrade-asleep");
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
        UpgradeCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", UPGRADE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
