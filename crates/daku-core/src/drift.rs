//! Version / plugin drift Signal: compare builds and plugin/store-app inventories.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use daku_protocol::SignalState;
use rusqlite::Connection;

use crate::availability::{AVAILABILITY_SIGNAL_ID, GLIDE_WAR_PATH, classify_availability_response};
use crate::collector::{SignalCollector, unix_now};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::ServiceNowClient;

pub const DRIFT_SIGNAL_ID: &str = "drift";
pub const PLUGIN_PAGE_LIMIT: usize = 1000;
pub const SYS_PLUGINS_PATH: &str =
    "/api/now/table/sys_plugins?sysparm_fields=id,version,active&sysparm_limit=1000";
pub const SYS_STORE_APP_PATH: &str = "/api/now/table/sys_store_app?sysparm_fields=scope,id,version,latest_version,active&sysparm_limit=1000";

/// Plugin/store-app inventories change on the order of days; refetch this
/// often. Builds are still compared every tick via the availability snapshot.
pub const INVENTORY_REFRESH_SECS: i64 = 30 * 60;

pub fn drift_state(build_matches: bool, mismatches: u64) -> SignalState {
    if build_matches && mismatches == 0 {
        SignalState::Healthy
    } else {
        SignalState::Degraded
    }
}

struct EnvInventory {
    build: Option<String>,
    plugins: Vec<PluginRecord>,
    truncated: bool,
}

#[derive(Clone)]
struct CachedInventory {
    fetched_at: i64,
    plugins: Vec<PluginRecord>,
    truncated: bool,
}

pub struct DriftCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
    poll_interval: Duration,
    /// Last successful plugin/store-app fetch per Environment id.
    inventories: std::sync::Mutex<HashMap<String, CachedInventory>>,
}

impl DriftCollector {
    pub fn new(
        environments: Vec<EnvironmentConfig>,
        credentials: Arc<dyn CredentialStore>,
        client: impl Into<Arc<ServiceNowClient>>,
        store: StateStore,
        poll_interval: Duration,
    ) -> Self {
        Self {
            environments,
            credentials,
            client: client.into(),
            store,
            poll_interval,
            inventories: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn env_inventory(
        &self,
        environment: &EnvironmentConfig,
        connection: &Connection,
        observed_at: i64,
        max_age_secs: i64,
    ) -> anyhow::Result<EnvInventory> {
        let cached = self
            .inventories
            .lock()
            .expect("drift inventory cache")
            .get(&environment.id)
            .filter(|entry| observed_at.saturating_sub(entry.fetched_at) <= INVENTORY_REFRESH_SECS)
            .cloned();
        let (plugins, truncated) = match cached {
            Some(entry) => (entry.plugins, entry.truncated),
            None => {
                let (plugins, plugins_truncated) = fetch_plugin_page(
                    &self.client,
                    environment,
                    self.credentials.as_ref(),
                    SYS_PLUGINS_PATH,
                )?;
                let (store_apps, store_truncated) = fetch_plugin_page(
                    &self.client,
                    environment,
                    self.credentials.as_ref(),
                    SYS_STORE_APP_PATH,
                )?;
                let mut combined = plugins;
                combined.extend(store_apps);
                let truncated = plugins_truncated || store_truncated;
                self.inventories
                    .lock()
                    .expect("drift inventory cache")
                    .insert(
                        environment.id.clone(),
                        CachedInventory {
                            fetched_at: observed_at,
                            plugins: combined.clone(),
                            truncated,
                        },
                    );
                (combined, truncated)
            }
        };
        Ok(EnvInventory {
            build: fetch_build(
                &self.client,
                environment,
                self.credentials.as_ref(),
                connection,
                observed_at,
                max_age_secs,
            )?,
            plugins,
            truncated,
        })
    }

    fn collect_other(
        &self,
        connection: &Connection,
        environment: &EnvironmentConfig,
        observed_at: i64,
        max_age_secs: i64,
        source: Option<&EnvInventory>,
    ) -> anyhow::Result<()> {
        let Some(source) = source else {
            return persist_drift_down(
                connection,
                &environment.id,
                "clone source unreachable",
                observed_at,
            )
            .map_err(anyhow::Error::from);
        };
        match self.env_inventory(environment, connection, observed_at, max_age_secs) {
            Ok(other) => {
                persist_drift_compare(connection, &environment.id, source, &other, observed_at)
                    .map_err(anyhow::Error::from)
            }
            Err(error) => {
                persist_drift_down(connection, &environment.id, &error.to_string(), observed_at)
                    .map_err(anyhow::Error::from)
            }
        }
    }
}

impl SignalCollector for DriftCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = unix_now();
        let Some(source) = self
            .environments
            .iter()
            .find(|environment| environment.clone_source)
        else {
            return persist_all_skipped(&connection, &self.environments, observed_at);
        };
        if self.environments.len() < 2 {
            return persist_all_skipped(&connection, &self.environments, observed_at);
        }
        let max_age_secs = (self.poll_interval.as_secs() as i64).saturating_mul(2);
        let source_inventory = self.env_inventory(source, &connection, observed_at, max_age_secs);
        let mut first_error = None;
        let source_inventory = match source_inventory {
            Ok(inventory) => {
                if let Err(error) = persist_drift_source(&connection, &source.id, observed_at) {
                    first_error.get_or_insert_with(|| anyhow::Error::from(error));
                }
                Some(inventory)
            }
            Err(error) => {
                if let Err(persist_error) =
                    persist_drift_down(&connection, &source.id, &error.to_string(), observed_at)
                {
                    first_error.get_or_insert_with(|| anyhow::Error::from(persist_error));
                }
                None
            }
        };
        for environment in &self.environments {
            if environment.id == source.id {
                continue;
            }
            if let Err(error) = self.collect_other(
                &connection,
                environment,
                observed_at,
                max_age_secs,
                source_inventory.as_ref(),
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

fn fetch_plugin_page(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
) -> anyhow::Result<(Vec<PluginRecord>, bool)> {
    let response = client.request(environment, credentials, "GET", path, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let records = parse_plugin_records(response.body.as_bytes())?;
    let total = response
        .header("X-Total-Count")
        .and_then(|value| value.parse::<u64>().ok());
    let truncated = records.len() >= PLUGIN_PAGE_LIMIT
        || total.is_some_and(|count| count > records.len() as u64);
    Ok((records, truncated))
}

fn fetch_build(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    connection: &Connection,
    observed_at: i64,
    max_age_secs: i64,
) -> anyhow::Result<Option<String>> {
    if let Some(build) =
        reuse_availability_build(connection, &environment.id, observed_at, max_age_secs)
    {
        return Ok(Some(build));
    }
    let response = client.request(environment, credentials, "GET", GLIDE_WAR_PATH, None)?;
    Ok(classify_availability_response(
        response.status,
        response.header("content-type").unwrap_or(""),
        &response.body,
        0,
    )
    .build)
}

fn reuse_availability_build(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
    max_age_secs: i64,
) -> Option<String> {
    let snapshot =
        persistence::load_signal_snapshot(connection, environment_id, AVAILABILITY_SIGNAL_ID)
            .ok()
            .flatten()?;
    if observed_at.saturating_sub(snapshot.observed_at) > max_age_secs {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_str(&snapshot.payload_json).ok()?;
    payload
        .get("build")
        .and_then(|value| value.as_str())
        .filter(|build| !build.is_empty())
        .map(str::to_owned)
}

fn parse_plugin_records(body: &[u8]) -> anyhow::Result<Vec<PluginRecord>> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let rows = value
        .get("result")
        .and_then(|result| result.as_array())
        .ok_or_else(|| anyhow!("plugin response missing result array"))?;
    Ok(rows.iter().filter_map(plugin_record).collect())
}

fn plugin_record(row: &serde_json::Value) -> Option<PluginRecord> {
    let id = row
        .get("id")
        .and_then(|value| value.as_str())
        .or_else(|| row.get("scope").and_then(|value| value.as_str()))
        .filter(|id| !id.is_empty())?
        .to_owned();
    Some(PluginRecord {
        id,
        version: row
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned(),
        active: is_active(row.get("active")),
    })
}

fn is_active(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::String(text)) => text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn persist_drift_source(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
) -> io::Result<()> {
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        SignalState::Healthy,
        &serde_json::json!({ "role": "source" }).to_string(),
    )
}

fn persist_drift_compare(
    connection: &Connection,
    environment_id: &str,
    source: &EnvInventory,
    other: &EnvInventory,
    observed_at: i64,
) -> io::Result<()> {
    let mismatch_list = diff_plugin_inventory(&source.plugins, &other.plugins);
    let mismatches = mismatch_list.len() as u64;
    let build_matches = source.build == other.build;
    let payload = serde_json::json!({
        "mismatches": mismatches,
        "build_matches": build_matches,
        "truncated": source.truncated || other.truncated,
        "mismatch_list": &mismatch_list[..mismatch_list.len().min(MISMATCH_LIST_LIMIT)],
        "mismatch_list_truncated": mismatch_list.len() > MISMATCH_LIST_LIMIT,
    });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        drift_state(build_matches, mismatches),
        &payload.to_string(),
    )
}

fn persist_all_skipped(
    connection: &Connection,
    environments: &[EnvironmentConfig],
    observed_at: i64,
) -> anyhow::Result<()> {
    let mut first_error = None;
    for environment in environments {
        if let Err(error) = persist_drift_skipped(connection, &environment.id, observed_at) {
            first_error.get_or_insert_with(|| anyhow::Error::from(error));
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn persist_drift_skipped(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
) -> io::Result<()> {
    persistence::persist_signal_skipped(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        "need_two_environments",
    )
}

fn persist_drift_down(
    connection: &Connection,
    environment_id: &str,
    message: &str,
    observed_at: i64,
) -> io::Result<()> {
    persistence::persist_signal_down(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        message,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRecord {
    pub id: String,
    pub version: String,
    pub active: bool,
}

/// One plugin that differs between two Environments. `None` means the plugin is
/// absent on that side; an `active` difference is carried as an " (inactive)"
/// suffix on the version of the side where it is switched off.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginMismatch {
    pub id: String,
    pub source_version: Option<String>,
    pub other_version: Option<String>,
}

/// Bound on the list persisted per snapshot; the count stays exact.
// ponytail: 50 rows keeps a 3-Environment payload under ~10 KB per tick.
pub const MISMATCH_LIST_LIMIT: usize = 50;

fn mismatch_version(record: &PluginRecord) -> String {
    if record.active {
        record.version.clone()
    } else {
        format!("{} (inactive)", record.version)
    }
}

pub fn diff_plugin_inventory(
    source: &[PluginRecord],
    other: &[PluginRecord],
) -> Vec<PluginMismatch> {
    let source_by_id: HashMap<&str, &PluginRecord> = source
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let other_by_id: HashMap<&str, &PluginRecord> = other
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut mismatches = Vec::new();
    for (id, source_record) in &source_by_id {
        match other_by_id.get(id) {
            Some(other_record)
                if other_record.version == source_record.version
                    && other_record.active == source_record.active => {}
            other_record => mismatches.push(PluginMismatch {
                id: (*id).to_owned(),
                source_version: Some(mismatch_version(source_record)),
                other_version: other_record.map(|record| mismatch_version(record)),
            }),
        }
    }
    for (id, other_record) in &other_by_id {
        if !source_by_id.contains_key(id) {
            mismatches.push(PluginMismatch {
                id: (*id).to_owned(),
                source_version: None,
                other_version: Some(mismatch_version(other_record)),
            });
        }
    }
    mismatches.sort_by(|a, b| a.id.cmp(&b.id));
    mismatches
}

#[cfg(test)]
mod tests {
    use crate::test_support::TempDb;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::collector::SignalCollector;
    use crate::config::{AuthMethod, EnvironmentConfig, MemoryCredentialStore};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    fn plugin(id: &str, version: &str) -> PluginRecord {
        PluginRecord {
            id: id.into(),
            version: version.into(),
            active: true,
        }
    }

    #[test]
    fn diff_plugin_inventory_identical_is_empty() {
        let plugins = [plugin("com.example.plugin_a", "1.0.0")];
        assert_eq!(diff_plugin_inventory(&plugins, &plugins).len(), 0);
    }

    #[test]
    fn diff_plugin_inventory_version_mismatch_counts_one() {
        let source = [plugin("com.example.plugin_a", "1.0.0")];
        let other = [plugin("com.example.plugin_a", "1.1.0")];
        assert_eq!(
            diff_plugin_inventory(&source, &other),
            vec![PluginMismatch {
                id: "com.example.plugin_a".into(),
                source_version: Some("1.0.0".into()),
                other_version: Some("1.1.0".into()),
            }]
        );
    }

    #[test]
    fn diff_plugin_inventory_reports_inactive_side() {
        let source = [plugin("com.example.plugin_a", "1.0.0")];
        let other = [PluginRecord {
            active: false,
            ..plugin("com.example.plugin_a", "1.0.0")
        }];
        assert_eq!(
            diff_plugin_inventory(&source, &other),
            vec![PluginMismatch {
                id: "com.example.plugin_a".into(),
                source_version: Some("1.0.0".into()),
                other_version: Some("1.0.0 (inactive)".into()),
            }]
        );
    }

    #[test]
    fn diff_plugin_inventory_reports_missing_both_ways_sorted() {
        let source = [
            plugin("com.example.plugin_a", "1.0.0"),
            plugin("com.example.plugin_b", "2.0.0"),
        ];
        let other = [
            plugin("com.example.plugin_a", "1.1.0"),
            plugin("com.example.plugin_c", "0.9.0"),
        ];
        assert_eq!(
            diff_plugin_inventory(&source, &other),
            vec![
                PluginMismatch {
                    id: "com.example.plugin_a".into(),
                    source_version: Some("1.0.0".into()),
                    other_version: Some("1.1.0".into()),
                },
                PluginMismatch {
                    id: "com.example.plugin_b".into(),
                    source_version: Some("2.0.0".into()),
                    other_version: None,
                },
                PluginMismatch {
                    id: "com.example.plugin_c".into(),
                    source_version: None,
                    other_version: Some("0.9.0".into()),
                },
            ]
        );
    }

    struct DriftTransport {
        source_plugins: &'static str,
        other_plugins: &'static str,
        store_apps: &'static str,
        build: &'static str,
    }

    impl HttpTransport for DriftTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            let source = request.url.contains("acme-prod");
            let body = if request.url.contains("/api/now/table/sys_plugins") {
                assert!(
                    request.url.contains("sysparm_limit=1000"),
                    "sys_plugins must cap at 1000: {}",
                    request.url
                );
                if source {
                    self.source_plugins
                } else {
                    self.other_plugins
                }
            } else if request.url.contains("/api/now/table/sys_store_app") {
                assert!(
                    request.url.contains("sysparm_limit=1000"),
                    "sys_store_app must cap at 1000: {}",
                    request.url
                );
                self.store_apps
            } else if request.url.contains("glide.war") {
                self.build
            } else {
                panic!("unexpected drift URL: {}", request.url);
            };
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.into(),
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

    fn collect_pair(
        source_plugins: &'static str,
        other_plugins: &'static str,
    ) -> (TempDb, StateStore) {
        let db = TempDb::new("drift");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        credentials.insert("test", r#"{"username":"reader","password":"secret"}"#);
        let collector = DriftCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(
                DriftTransport {
                    source_plugins,
                    other_plugins,
                    store_apps: include_str!("../tests/fixtures/drift/store_apps_empty.json"),
                    build: include_str!("../tests/fixtures/availability/ok.json"),
                },
                SystemClock,
            ),
            store,
            Duration::from_secs(120),
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn drift_signal_identical_inventories_are_healthy() {
        let (_db, store) = collect_pair(
            include_str!("../tests/fixtures/drift/plugins_a.json"),
            include_str!("../tests/fixtures/drift/plugins_a.json"),
        );
        let connection = store.open().unwrap();
        let source = persistence::load_signal_snapshot(&connection, "prod", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("source snapshot");
        assert_eq!(source.state, "healthy");
        let source_payload: serde_json::Value = serde_json::from_str(&source.payload_json).unwrap();
        assert_eq!(source_payload["role"], "source");

        let other = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("other snapshot");
        assert_eq!(other.state, "healthy");
        let other_payload: serde_json::Value = serde_json::from_str(&other.payload_json).unwrap();
        assert_eq!(other_payload["mismatches"], 0);
        assert_eq!(other_payload["truncated"], false);
    }

    #[test]
    fn drift_signal_version_mismatch_is_degraded() {
        let (_db, store) = collect_pair(
            include_str!("../tests/fixtures/drift/plugins_a.json"),
            include_str!("../tests/fixtures/drift/plugins_a_v2.json"),
        );
        let connection = store.open().unwrap();
        let source = persistence::load_signal_snapshot(&connection, "prod", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("source snapshot");
        assert_eq!(source.state, "healthy");
        let other = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("other snapshot");
        assert_eq!(other.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&other.payload_json).unwrap();
        assert_eq!(payload["mismatches"], 1);
        assert_eq!(payload["mismatch_list"][0]["id"], "com.example.plugin_a");
        assert_eq!(payload["mismatch_list"][0]["other_version"], "1.1.0");
        assert_eq!(payload["mismatch_list_truncated"], false);
    }

    #[test]
    fn drift_payload_mismatch_list_is_bounded() {
        let db = TempDb::new("drift_bounded");
        let store = db.store();
        let connection = store.open().unwrap();
        let source: Vec<PluginRecord> = (0..60)
            .map(|index| plugin(&format!("com.example.plugin_{index:02}"), "1.0.0"))
            .collect();
        let other: Vec<PluginRecord> = (0..60)
            .map(|index| plugin(&format!("com.example.plugin_{index:02}"), "1.1.0"))
            .collect();
        persist_drift_compare(
            &connection,
            "test",
            &EnvInventory {
                build: None,
                plugins: source,
                truncated: false,
            },
            &EnvInventory {
                build: None,
                plugins: other,
                truncated: false,
            },
            1,
        )
        .unwrap();
        let snapshot = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&snapshot.payload_json).unwrap();
        assert_eq!(payload["mismatches"], 60);
        assert_eq!(
            payload["mismatch_list"].as_array().unwrap().len(),
            MISMATCH_LIST_LIMIT
        );
        assert_eq!(payload["mismatch_list_truncated"], true);
    }

    #[test]
    fn drift_signal_single_environment_skips() {
        let db = TempDb::new("drift-skip");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = DriftCollector::new(
            vec![env("prod", "acme-prod", true)],
            credentials,
            ServiceNowClient::new(
                DriftTransport {
                    source_plugins: include_str!("../tests/fixtures/drift/plugins_a.json"),
                    other_plugins: include_str!("../tests/fixtures/drift/plugins_a.json"),
                    store_apps: include_str!("../tests/fixtures/drift/store_apps_empty.json"),
                    build: include_str!("../tests/fixtures/availability/ok.json"),
                },
                SystemClock,
            ),
            store,
            Duration::from_secs(120),
        );
        collector.collect().unwrap();
        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "need_two_environments");
    }

    #[test]
    fn drift_signal_without_clone_source_skips() {
        let db = TempDb::new("drift-nosrc");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        credentials.insert("test", r#"{"username":"reader","password":"secret"}"#);
        let collector = DriftCollector::new(
            vec![
                env("prod", "acme-prod", false),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(
                DriftTransport {
                    source_plugins: include_str!("../tests/fixtures/drift/plugins_a.json"),
                    other_plugins: include_str!("../tests/fixtures/drift/plugins_a.json"),
                    store_apps: include_str!("../tests/fixtures/drift/store_apps_empty.json"),
                    build: include_str!("../tests/fixtures/availability/ok.json"),
                },
                SystemClock,
            ),
            store,
            Duration::from_secs(120),
        );
        collector.collect().unwrap();
        let connection = db.store().open().unwrap();
        for id in ["prod", "test"] {
            let row = persistence::load_signal_snapshot(&connection, id, DRIFT_SIGNAL_ID)
                .unwrap()
                .expect("snapshot");
            assert_eq!(row.state, "skipped");
            let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
            assert_eq!(payload["skipped"], "need_two_environments");
        }
    }

    struct TruncatedTransport;

    impl HttpTransport for TruncatedTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            let (body, extra) = if request.url.contains("/api/now/table/sys_plugins") {
                (
                    include_str!("../tests/fixtures/drift/plugins_a.json"),
                    Some(("X-Total-Count".into(), "1001".into())),
                )
            } else if request.url.contains("/api/now/table/sys_store_app") {
                (
                    include_str!("../tests/fixtures/drift/store_apps_empty.json"),
                    None,
                )
            } else if request.url.contains("glide.war") {
                (include_str!("../tests/fixtures/availability/ok.json"), None)
            } else {
                panic!("unexpected drift URL: {}", request.url);
            };
            let mut headers = vec![("content-type".into(), "application/json".into())];
            if let Some(header) = extra {
                headers.push(header);
            }
            Ok(HttpResponse {
                status: 200,
                headers,
                body: body.into(),
            })
        }
    }

    #[test]
    fn drift_signal_truncated_when_more_rows_exist() {
        let db = TempDb::new("drift-trunc");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        credentials.insert("test", r#"{"username":"reader","password":"secret"}"#);
        let collector = DriftCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(TruncatedTransport, SystemClock),
            store,
            Duration::from_secs(120),
        );
        collector.collect().unwrap();
        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["truncated"], true);
    }

    struct NoGlideTransport;

    impl HttpTransport for NoGlideTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                !request.url.contains("glide.war"),
                "fresh availability snapshot must reuse glide.war"
            );
            let body = if request.url.contains("/api/now/table/sys_plugins") {
                include_str!("../tests/fixtures/drift/plugins_a.json")
            } else if request.url.contains("/api/now/table/sys_store_app") {
                include_str!("../tests/fixtures/drift/store_apps_empty.json")
            } else {
                panic!("unexpected drift URL: {}", request.url);
            };
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.into(),
            })
        }
    }

    #[test]
    fn drift_signal_reuses_fresh_availability_build() {
        let db = TempDb::new("drift-reuse");
        let store = db.store();
        let connection = store.open().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        crate::availability::persist_availability_snapshot(
            &connection,
            "prod",
            &crate::availability::classify_availability_response(
                200,
                "application/json",
                include_str!("../tests/fixtures/availability/ok.json"),
                10,
            ),
            now,
        )
        .unwrap();
        crate::availability::persist_availability_snapshot(
            &connection,
            "test",
            &crate::availability::classify_availability_response(
                200,
                "application/json",
                include_str!("../tests/fixtures/availability/ok.json"),
                10,
            ),
            now,
        )
        .unwrap();
        drop(connection);
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        credentials.insert("test", r#"{"username":"reader","password":"secret"}"#);
        let collector = DriftCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(NoGlideTransport, SystemClock),
            store,
            Duration::from_secs(120),
        );
        collector.collect().unwrap();
        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
    }

    struct CountingTransport {
        inner: DriftTransport,
        plugin_requests: Arc<std::sync::atomic::AtomicUsize>,
        /// Statuses to return for the non-source plugin page, one per call.
        test_plugin_statuses: std::sync::Mutex<Vec<u16>>,
    }

    impl CountingTransport {
        fn new(
            source_plugins: &'static str,
            other_plugins: &'static str,
            test_plugin_statuses: Vec<u16>,
        ) -> (Self, Arc<std::sync::atomic::AtomicUsize>) {
            let plugin_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            (
                Self {
                    inner: DriftTransport {
                        source_plugins,
                        other_plugins,
                        store_apps: include_str!("../tests/fixtures/drift/store_apps_empty.json"),
                        build: include_str!("../tests/fixtures/availability/ok.json"),
                    },
                    plugin_requests: Arc::clone(&plugin_requests),
                    test_plugin_statuses: std::sync::Mutex::new(test_plugin_statuses),
                },
                plugin_requests,
            )
        }
    }

    impl HttpTransport for CountingTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("/api/now/table/sys_plugins")
                || request.url.contains("/api/now/table/sys_store_app")
            {
                self.plugin_requests
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if request.url.contains("/api/now/table/sys_plugins")
                    && !request.url.contains("acme-prod")
                {
                    let status = {
                        let mut statuses = self.test_plugin_statuses.lock().unwrap();
                        if statuses.is_empty() {
                            200
                        } else {
                            statuses.remove(0)
                        }
                    };
                    if status != 200 {
                        return Ok(HttpResponse {
                            status,
                            headers: vec![("content-type".into(), "application/json".into())],
                            body: String::new(),
                        });
                    }
                }
            }
            self.inner.execute(request)
        }
    }

    fn counting_collector(
        path: &std::path::Path,
        source_plugins: &'static str,
        other_plugins: &'static str,
        test_plugin_statuses: Vec<u16>,
    ) -> (DriftCollector, Arc<std::sync::atomic::AtomicUsize>) {
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        credentials.insert("test", r#"{"username":"reader","password":"secret"}"#);
        let (transport, plugin_requests) =
            CountingTransport::new(source_plugins, other_plugins, test_plugin_statuses);
        let collector = DriftCollector::new(
            vec![
                env("prod", "acme-prod", true),
                env("test", "acme-test", false),
            ],
            credentials,
            ServiceNowClient::new(transport, SystemClock),
            StateStore::daemon(path.to_path_buf()),
            Duration::from_secs(120),
        );
        (collector, plugin_requests)
    }

    fn mismatches(path: &std::path::Path, environment_id: &str) -> serde_json::Value {
        let connection = StateStore::daemon(path.to_path_buf()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, environment_id, DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        serde_json::json!({ "state": row.state, "mismatches": payload["mismatches"] })
    }

    #[test]
    fn drift_signal_reuses_inventory_within_refresh_window() {
        let db = TempDb::new("drift-cache");
        let (collector, plugin_requests) = counting_collector(
            db.path(),
            include_str!("../tests/fixtures/drift/plugins_a.json"),
            include_str!("../tests/fixtures/drift/plugins_a_v2.json"),
            vec![],
        );
        collector.collect().unwrap();
        assert_eq!(
            plugin_requests.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "cold cache: 2 pages x 2 environments"
        );
        assert_eq!(mismatches(db.path(), "test")["mismatches"], 1);

        collector.collect().unwrap();
        assert_eq!(
            plugin_requests.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "second tick must reuse the cached inventories"
        );
        assert_eq!(mismatches(db.path(), "test")["mismatches"], 1);
    }

    #[test]
    fn drift_signal_refetches_inventory_after_refresh_window() {
        let db = TempDb::new("drift-stale");
        let (collector, plugin_requests) = counting_collector(
            db.path(),
            include_str!("../tests/fixtures/drift/plugins_a.json"),
            include_str!("../tests/fixtures/drift/plugins_a_v2.json"),
            vec![],
        );
        collector.collect().unwrap();
        assert_eq!(plugin_requests.load(std::sync::atomic::Ordering::SeqCst), 4);

        collector
            .inventories
            .lock()
            .unwrap()
            .values_mut()
            .for_each(|entry| entry.fetched_at -= INVENTORY_REFRESH_SECS + 1);

        collector.collect().unwrap();
        assert_eq!(
            plugin_requests.load(std::sync::atomic::Ordering::SeqCst),
            8,
            "stale cache must refetch both environments"
        );
        assert_eq!(mismatches(db.path(), "test")["mismatches"], 1);
    }

    #[test]
    fn drift_signal_failed_inventory_is_not_cached() {
        let db = TempDb::new("drift-retry");
        let (collector, plugin_requests) = counting_collector(
            db.path(),
            include_str!("../tests/fixtures/drift/plugins_a.json"),
            include_str!("../tests/fixtures/drift/plugins_a.json"),
            vec![500],
        );
        // Tick 1: prod fetches both pages (2), test's sys_plugins 500s and bails (1).
        collector.collect().unwrap();
        assert_eq!(plugin_requests.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(mismatches(db.path(), "test")["state"], "down");

        // Tick 2: prod is cached (0), test retries both pages (2).
        collector.collect().unwrap();
        assert_eq!(
            plugin_requests.load(std::sync::atomic::Ordering::SeqCst),
            5,
            "a failed fetch must not be cached"
        );
        let snapshot = mismatches(db.path(), "test");
        assert_eq!(snapshot["state"], "healthy");
        assert_eq!(snapshot["mismatches"], 0);
    }
}
