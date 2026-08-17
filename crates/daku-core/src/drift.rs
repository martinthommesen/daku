//! Version / plugin drift Signal: compare builds and plugin/store-app inventories.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use rusqlite::Connection;

use crate::availability::{AVAILABILITY_SIGNAL_ID, GLIDE_WAR_PATH, classify_availability_response};
use crate::collector::SignalCollector;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::ServiceNowClient;

pub const DRIFT_SIGNAL_ID: &str = "drift";
pub const PLUGIN_PAGE_LIMIT: usize = 1000;
pub const SYS_PLUGINS_PATH: &str =
    "/api/now/table/sys_plugins?sysparm_fields=id,version,active&sysparm_limit=1000";
pub const SYS_STORE_APP_PATH: &str = "/api/now/table/sys_store_app?sysparm_fields=scope,id,version,latest_version,active&sysparm_limit=1000";

pub fn drift_state(build_matches: bool, mismatches: u64) -> &'static str {
    if build_matches && mismatches == 0 {
        "healthy"
    } else {
        "degraded"
    }
}

struct EnvInventory {
    build: Option<String>,
    plugins: Vec<PluginRecord>,
    truncated: bool,
}

pub struct DriftCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
    poll_interval: Duration,
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
        }
    }
}

impl SignalCollector for DriftCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
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
        let source_inventory = fetch_env_inventory(
            &self.client,
            source,
            self.credentials.as_ref(),
            &connection,
            observed_at,
            max_age_secs,
        );
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
            if let Err(error) = collect_other(
                &connection,
                &self.client,
                environment,
                self.credentials.as_ref(),
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

fn collect_other(
    connection: &Connection,
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
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
    match fetch_env_inventory(
        client,
        environment,
        credentials,
        connection,
        observed_at,
        max_age_secs,
    ) {
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

fn fetch_env_inventory(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    connection: &Connection,
    observed_at: i64,
    max_age_secs: i64,
) -> anyhow::Result<EnvInventory> {
    let (plugins, plugins_truncated) =
        fetch_plugin_page(client, environment, credentials, SYS_PLUGINS_PATH)?;
    let (store_apps, store_truncated) =
        fetch_plugin_page(client, environment, credentials, SYS_STORE_APP_PATH)?;
    let mut combined = plugins;
    combined.extend(store_apps);
    Ok(EnvInventory {
        build: fetch_build(
            client,
            environment,
            credentials,
            connection,
            observed_at,
            max_age_secs,
        )?,
        plugins: combined,
        truncated: plugins_truncated || store_truncated,
    })
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
        "healthy",
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
    let mismatches = diff_plugin_inventory(&source.plugins, &other.plugins);
    let build_matches = source.build == other.build;
    let payload = serde_json::json!({
        "mismatches": mismatches,
        "build_matches": build_matches,
        "truncated": source.truncated || other.truncated,
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
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        "healthy",
        &serde_json::json!({ "skipped": "need_two_environments" }).to_string(),
    )
}

fn persist_drift_down(
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
        DRIFT_SIGNAL_ID,
        observed_at,
        "down",
        &payload.to_string(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRecord {
    pub id: String,
    pub version: String,
    pub active: bool,
}

pub fn diff_plugin_inventory(source: &[PluginRecord], other: &[PluginRecord]) -> u64 {
    let source_by_id: HashMap<&str, &PluginRecord> = source
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let other_by_id: HashMap<&str, &PluginRecord> = other
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut mismatches = 0;
    for (id, source_record) in &source_by_id {
        match other_by_id.get(id) {
            Some(other_record)
                if other_record.version == source_record.version
                    && other_record.active == source_record.active => {}
            _ => mismatches += 1,
        }
    }
    for id in other_by_id.keys() {
        if !source_by_id.contains_key(id) {
            mismatches += 1;
        }
    }
    mismatches
}

#[cfg(test)]
mod tests {
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
        assert_eq!(diff_plugin_inventory(&plugins, &plugins), 0);
    }

    #[test]
    fn diff_plugin_inventory_version_mismatch_counts_one() {
        let source = [plugin("com.example.plugin_a", "1.0.0")];
        let other = [plugin("com.example.plugin_a", "1.1.0")];
        assert_eq!(diff_plugin_inventory(&source, &other), 1);
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
    ) -> (std::path::PathBuf, StateStore) {
        let path = std::env::temp_dir().join(format!("daku-drift-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
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
        (path.clone(), StateStore::daemon(path))
    }

    #[test]
    fn drift_signal_identical_inventories_are_healthy() {
        let (path, store) = collect_pair(
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
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn drift_signal_version_mismatch_is_degraded() {
        let (path, store) = collect_pair(
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
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn drift_signal_single_environment_skips() {
        let path =
            std::env::temp_dir().join(format!("daku-drift-skip-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
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
        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "need_two_environments");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn drift_signal_without_clone_source_skips() {
        let path =
            std::env::temp_dir().join(format!("daku-drift-nosrc-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
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
        let connection = StateStore::daemon(path.clone()).open().unwrap();
        for id in ["prod", "test"] {
            let row = persistence::load_signal_snapshot(&connection, id, DRIFT_SIGNAL_ID)
                .unwrap()
                .expect("snapshot");
            assert_eq!(row.state, "healthy");
            let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
            assert_eq!(payload["skipped"], "need_two_environments");
        }
        let _ = std::fs::remove_file(path);
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
        let path =
            std::env::temp_dir().join(format!("daku-drift-trunc-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
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
        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["truncated"], true);
        let _ = std::fs::remove_file(path);
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
        let path =
            std::env::temp_dir().join(format!("daku-drift-reuse-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
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
        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "test", DRIFT_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let _ = std::fs::remove_file(path);
    }
}
