//! Last-clone Signal: informational clone age per clone **target**, read once
//! from the clone-source Environment (the `clone_instance` record lives there).

use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use daku_protocol::SignalState;
use rusqlite::Connection;

use crate::collector::{SignalCollector, unix_now};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::ServiceNowClient;

pub const LAST_CLONE_SIGNAL_ID: &str = "last_clone";
pub const CLONE_INSTANCE_PATH: &str = "/api/now/table/clone_instance?sysparm_query=state=Completed^ORDERBYDESCcompleted&sysparm_fields=state,completed,target&sysparm_limit=10";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneRow {
    pub target: String,
    pub completed: String,
}

/// Newest Completed clone per target, in response order (already newest-first).
/// `None` = the source cannot answer (non-200 or unreadable body) — nothing is
/// then known about any target. `Some(vec![])` = no clone has ever completed.
pub fn parse_last_clones(status: u16, body: &str) -> Option<Vec<CloneRow>> {
    if status != 200 {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let rows = value.get("result")?.as_array()?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut newest = Vec::new();
    for row in rows {
        let field = |name: &str| {
            row.get(name)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let (Some(target), Some(completed)) = (field("target"), field("completed")) else {
            continue;
        };
        if seen.insert(target.to_ascii_lowercase()) {
            newest.push(CloneRow {
                target: target.to_owned(),
                completed: completed.to_owned(),
            });
        }
    }
    Some(newest)
}

/// `clone_instance.target` is an instance name; tolerate a full hostname on
/// either side (see `docs/research/servicenow-signals.md`, item 10).
pub fn target_matches(target: &str, environment: &EnvironmentConfig) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    let host = instance_host(&environment.instance_url);
    [host, first_label(host), environment.id.as_str()]
        .iter()
        .any(|candidate| {
            candidate.eq_ignore_ascii_case(target)
                || candidate.eq_ignore_ascii_case(first_label(target))
        })
}

fn instance_host(instance_url: &str) -> &str {
    instance_url
        .rsplit("://")
        .next()
        .unwrap_or(instance_url)
        .split('/')
        .next()
        .unwrap_or("")
}

fn first_label(host: &str) -> &str {
    host.split('.').next().unwrap_or(host)
}

/// Whole days between the clone and `observed_at`, comparing **date parts
/// only** — the two clocks are different machines and hours of skew are
/// irrelevant at day granularity. `None` when `completed` is not
/// `YYYY-MM-DD HH:MM:SS`.
fn age_days(completed: &str, observed_at: i64) -> Option<i64> {
    let mut parts = completed.split(' ').next()?.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((observed_at.div_euclid(86_400) - days_from_civil(year, month, day)).max(0))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
        let connection = self.store.open()?;
        let observed_at = unix_now();
        let Some(source) = self
            .environments
            .iter()
            .find(|environment| environment.clone_source)
        else {
            // Without a clone source the card would sit on "Waiting" forever
            // (and its Skeleton would animate forever); say why instead.
            skip_targets(
                &connection,
                &self.environments,
                None,
                observed_at,
                "no_clone_source",
            )?;
            return Ok(());
        };
        let response = match self.client.request(
            source,
            self.credentials.as_ref(),
            "GET",
            CLONE_INSTANCE_PATH,
            None,
        ) {
            Ok(response) if response.status == 200 || response.status == 403 => response,
            // The source's own `down` snapshot lands first, then every target
            // learns why it has no answer instead of waiting forever.
            Ok(response) => {
                persist_last_clone_unreachable(
                    &connection,
                    &source.id,
                    &format!("HTTP {}", response.status),
                    observed_at,
                )?;
                skip_targets(
                    &connection,
                    &self.environments,
                    Some(source.id.as_str()),
                    observed_at,
                    "clone_source_unreachable",
                )?;
                return Ok(());
            }
            Err(error) => {
                persist_last_clone_unreachable(
                    &connection,
                    &source.id,
                    &error.to_string(),
                    observed_at,
                )?;
                skip_targets(
                    &connection,
                    &self.environments,
                    Some(source.id.as_str()),
                    observed_at,
                    "clone_source_unreachable",
                )?;
                return Ok(());
            }
        };
        let rows = parse_last_clones(response.status, &response.body);
        persist_clone_source(&connection, &source.id, rows.is_some(), observed_at)?;
        // 403: the source cannot list clones, so nothing is known about the
        // targets — record that rather than claiming "never" or waiting forever.
        let Some(rows) = rows else {
            skip_targets(
                &connection,
                &self.environments,
                Some(source.id.as_str()),
                observed_at,
                "clone_source_cannot_list_clones",
            )?;
            return Ok(());
        };
        for environment in self
            .environments
            .iter()
            .filter(|environment| environment.id != source.id)
        {
            let row = rows
                .iter()
                .find(|row| target_matches(&row.target, environment));
            persist_clone_target(&connection, &environment.id, row, &source.id, observed_at)?;
        }
        Ok(())
    }
}

/// Records on every clone target that last-clone has no answer this tick and
/// why, so the card says something instead of animating "Waiting" forever.
fn skip_targets(
    connection: &Connection,
    environments: &[EnvironmentConfig],
    source_id: Option<&str>,
    observed_at: i64,
    reason: &str,
) -> io::Result<()> {
    for environment in environments
        .iter()
        .filter(|environment| Some(environment.id.as_str()) != source_id)
    {
        persistence::persist_signal_skipped(
            connection,
            &environment.id,
            LAST_CLONE_SIGNAL_ID,
            observed_at,
            reason,
        )?;
    }
    Ok(())
}

fn persist_clone_source(
    connection: &Connection,
    environment_id: &str,
    supported: bool,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({ "role": "source", "supported": supported });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        LAST_CLONE_SIGNAL_ID,
        observed_at,
        SignalState::Healthy,
        &payload.to_string(),
    )
}

fn persist_clone_target(
    connection: &Connection,
    environment_id: &str,
    row: Option<&CloneRow>,
    source_id: &str,
    observed_at: i64,
) -> io::Result<()> {
    let payload = match row {
        Some(row) => {
            let mut payload = serde_json::json!({
                "completed": row.completed,
                "source_id": source_id,
            });
            if let Some(age) = age_days(&row.completed, observed_at) {
                payload["age_days"] = age.into();
            }
            payload
        }
        None => serde_json::json!({ "supported": true, "completed": null }),
    };
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        LAST_CLONE_SIGNAL_ID,
        observed_at,
        SignalState::Healthy,
        &payload.to_string(),
    )
}

/// A failed read renders `down` — the Signal never votes in the rollup, so a
/// red card is informational, not a health regression.
fn persist_last_clone_unreachable(
    connection: &Connection,
    environment_id: &str,
    message: &str,
    observed_at: i64,
) -> io::Result<()> {
    persistence::persist_signal_down(
        connection,
        environment_id,
        LAST_CLONE_SIGNAL_ID,
        observed_at,
        message,
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
        let rows = parse_last_clones(
            200,
            include_str!("../tests/fixtures/last_clone/completed.json"),
        )
        .expect("supported");
        assert_eq!(
            rows,
            vec![CloneRow {
                target: "acme-test".into(),
                completed: "2026-01-15 12:00:00".into(),
            }]
        );
    }

    #[test]
    fn last_clone_signal_403_is_unsupported() {
        assert_eq!(
            parse_last_clones(403, r#"{"error":{"message":"Operation not allowed"}}"#),
            None
        );
    }

    #[test]
    fn last_clone_signal_empty_is_supported_with_no_rows() {
        let rows = parse_last_clones(200, include_str!("../tests/fixtures/last_clone/empty.json"))
            .expect("supported");
        assert!(rows.is_empty());
    }

    #[test]
    fn parse_last_clones_groups_newest_per_target() {
        let rows = parse_last_clones(
            200,
            include_str!("../tests/fixtures/last_clone/two_targets.json"),
        )
        .expect("supported");
        assert_eq!(
            rows,
            vec![
                CloneRow {
                    target: "acme-test".into(),
                    completed: "2026-01-15 12:00:00".into(),
                },
                CloneRow {
                    target: "acme-dev".into(),
                    completed: "2026-01-02 03:00:00".into(),
                },
            ]
        );
    }

    #[test]
    fn target_matches_host_label_and_id() {
        let environment = env("test", "acme-test", false);
        assert!(target_matches("acme-test", &environment));
        assert!(target_matches("ACME-TEST", &environment));
        assert!(target_matches(
            "acme-test.example.service-now.com",
            &environment
        ));
        assert!(target_matches("test", &environment));
        assert!(!target_matches("acme-dev", &environment));
        assert!(!target_matches("", &environment));
    }

    #[test]
    fn age_days_counts_whole_days_from_the_date_part() {
        let observed_at = days_from_civil(2026, 1, 27) * 86_400 + 3_600;
        assert_eq!(age_days("2026-01-15 12:00:00", observed_at), Some(12));
        assert_eq!(age_days("2026-01-27 23:00:00", observed_at), Some(0));
        // A source clock ahead of the daemon must not render a negative age.
        assert_eq!(age_days("2026-02-01 00:00:00", observed_at), Some(0));
        assert_eq!(age_days("not a date", observed_at), None);
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
                request.url.contains("sysparm_limit=10"),
                "last_clone reads the newest rows across targets: {}",
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
        for id in ["prod", "test", "dev"] {
            credentials.insert(id, r#"{"username":"reader","password":"secret"}"#);
        }
        let collector = LastCloneCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
                env("dev", "acme-dev", false),
            ],
            credentials,
            ServiceNowClient::new(LastCloneTransport { status, body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    fn payload(store: &StateStore, environment_id: &str) -> serde_json::Value {
        let connection = store.open().unwrap();
        let row =
            persistence::load_signal_snapshot(&connection, environment_id, LAST_CLONE_SIGNAL_ID)
                .unwrap()
                .expect("snapshot");
        assert_eq!(row.state, "healthy");
        serde_json::from_str(&row.payload_json).unwrap()
    }

    #[test]
    fn last_clone_signal_completed_writes_healthy_snapshot() {
        let (_db, store) = collect_last_clone(
            200,
            include_str!("../tests/fixtures/last_clone/completed.json"),
        );
        let test = payload(&store, "test");
        assert_eq!(test["completed"], "2026-01-15 12:00:00");
        assert_eq!(test["source_id"], "prod");
        assert!(test["age_days"].as_i64().expect("age_days") >= 0);
        let prod = payload(&store, "prod");
        assert_eq!(prod["role"], "source");
        assert_eq!(prod["supported"], true);
        // No clone to dev in this fixture.
        let dev = payload(&store, "dev");
        assert_eq!(dev["supported"], true);
        assert!(dev["completed"].is_null());
    }

    #[test]
    fn last_clone_signal_two_targets_each_get_a_row() {
        let (_db, store) = collect_last_clone(
            200,
            include_str!("../tests/fixtures/last_clone/two_targets.json"),
        );
        assert_eq!(payload(&store, "test")["completed"], "2026-01-15 12:00:00");
        assert_eq!(payload(&store, "dev")["completed"], "2026-01-02 03:00:00");
    }

    #[test]
    fn last_clone_signal_403_writes_healthy_unsupported() {
        let (_db, store) =
            collect_last_clone(403, r#"{"error":{"message":"Operation not allowed"}}"#);
        let prod = payload(&store, "prod");
        assert_eq!(prod["supported"], false);
        assert!(prod.get("reachability").is_none());
        // Nothing is known about the targets: say so instead of leaving the
        // card on "Waiting" forever.
        let connection = store.open().unwrap();
        let test = persistence::load_signal_snapshot(&connection, "test", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("skipped snapshot");
        assert_eq!(test.state, "skipped");
        assert!(
            test.payload_json
                .contains("clone_source_cannot_list_clones")
        );
    }

    #[test]
    fn last_clone_signal_without_clone_source_is_skipped_everywhere() {
        let db = TempDb::new("last-clone-no-source");
        let store = db.store();
        let collector = LastCloneCollector::new(
            vec![env("pdi", "acme-pdi", false)],
            Arc::new(MemoryCredentialStore::default()),
            ServiceNowClient::new(
                LastCloneTransport {
                    status: 200,
                    body: "{}",
                },
                SystemClock,
            ),
            store,
        );
        collector.collect().unwrap();
        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "pdi", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("skipped snapshot");
        assert_eq!(row.state, "skipped");
        assert!(row.payload_json.contains("no_clone_source"));
    }

    #[test]
    fn last_clone_signal_probe_failure_is_down_unreachable() {
        let (_db, store) = collect_last_clone(500, r#"{"error":{"message":"boom"}}"#);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("supported").is_none());
    }

    fn skipped_reason(store: &StateStore, environment_id: &str) -> String {
        let connection = store.open().unwrap();
        let row =
            persistence::load_signal_snapshot(&connection, environment_id, LAST_CLONE_SIGNAL_ID)
                .unwrap()
                .expect("skipped snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        payload["skipped"].as_str().expect("reason").to_owned()
    }

    #[test]
    fn last_clone_signal_probe_failure_skips_every_target() {
        let (_db, store) = collect_last_clone(500, r#"{"error":{"message":"boom"}}"#);
        for id in ["test", "dev"] {
            assert_eq!(skipped_reason(&store, id), "clone_source_unreachable");
        }
        // The source's own snapshot must survive the target loop.
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
    }

    struct FailingTransport;

    impl HttpTransport for FailingTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            Err(anyhow::anyhow!("connection refused"))
        }
    }

    #[test]
    fn last_clone_signal_transport_error_skips_every_target() {
        let db = TempDb::new("last-clone-transport-error");
        let credentials = Arc::new(MemoryCredentialStore::default());
        for id in ["prod", "test"] {
            credentials.insert(id, r#"{"username":"reader","password":"secret"}"#);
        }
        LastCloneCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(FailingTransport, SystemClock),
            db.store(),
        )
        .collect()
        .unwrap();
        let store = db.store();
        assert_eq!(skipped_reason(&store, "test"), "clone_source_unreachable");
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", LAST_CLONE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        // Pins the transport arm rather than a credential lookup failure.
        assert!(row.payload_json.contains("connection refused"));
    }
}
