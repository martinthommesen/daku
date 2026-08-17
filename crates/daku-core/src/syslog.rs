//! Syslog error-rate Signal: 1h Error count on the rotated `syslog` table.

use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::collector::SignalCollector;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const SYSLOG_SIGNAL_ID: &str = "syslog";
pub const SYSLOG_ERROR_LEVEL: u8 = 2;

pub fn syslog_error_path() -> String {
    format!(
        "/api/now/stats/syslog?sysparm_count=true&sysparm_query=level={SYSLOG_ERROR_LEVEL}^sys_created_on>javascript:gs.hoursAgoStart(1)"
    )
}

pub fn syslog_state(error_count_1h: u64) -> &'static str {
    if error_count_1h > 0 {
        "degraded"
    } else {
        "healthy"
    }
}

pub struct SyslogCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
}

impl SyslogCollector {
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

impl SignalCollector for SyslogCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let path = syslog_error_path();
        let mut first_error = None;
        for environment in &self.environments {
            let count =
                fetch_aggregate_count(&self.client, environment, self.credentials.as_ref(), &path);
            if let Err(error) = collect_syslog(&connection, environment, observed_at, count) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = persistence::prune_signal_samples(&connection, observed_at) {
            first_error.get_or_insert_with(|| anyhow::Error::from(error));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn collect_syslog(
    connection: &Connection,
    environment: &EnvironmentConfig,
    observed_at: i64,
    count: anyhow::Result<u64>,
) -> anyhow::Result<()> {
    match count {
        Ok(error_count_1h) => {
            persist_syslog_ok(connection, &environment.id, error_count_1h, observed_at)
                .map_err(anyhow::Error::from)
        }
        Err(error) => {
            persist_syslog_down(connection, &environment.id, &error.to_string(), observed_at)
                .map_err(anyhow::Error::from)
        }
    }
}

fn persist_syslog_ok(
    connection: &Connection,
    environment_id: &str,
    error_count_1h: u64,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({ "error_count_1h": error_count_1h });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        SYSLOG_SIGNAL_ID,
        observed_at,
        syslog_state(error_count_1h),
        &payload.to_string(),
    )?;
    persistence::persist_signal_sample(
        connection,
        environment_id,
        SYSLOG_SIGNAL_ID,
        observed_at,
        Some(error_count_1h as f64),
        None,
    )
}

fn persist_syslog_down(
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
        SYSLOG_SIGNAL_ID,
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

    struct SyslogCountTransport {
        body: &'static str,
    }

    impl HttpTransport for SyslogCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/stats/syslog"),
                "syslog collector must use Aggregate API: {}",
                request.url
            );
            assert!(
                request.url.contains("sys_created_on") && request.url.contains("hoursAgoStart"),
                "syslog query must be date-bound: {}",
                request.url
            );
            assert!(
                request.url.contains(&format!("level={SYSLOG_ERROR_LEVEL}")),
                "syslog query must use SYSLOG_ERROR_LEVEL: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
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
        }
    }

    #[test]
    fn syslog_signal_zeros_are_healthy_and_write_sample() {
        let path = std::env::temp_dir().join(format!("daku-syslog-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = SyslogCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(
                SyslogCountTransport {
                    body: include_str!("../tests/fixtures/syslog/count_0.json"),
                },
                SystemClock,
            ),
            store,
        );
        collector.collect().unwrap();

        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SYSLOG_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["error_count_1h"], 0);
        let samples =
            persistence::load_signal_samples(&connection, "prod", SYSLOG_SIGNAL_ID).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value_real, Some(0.0));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn syslog_signal_errors_are_degraded() {
        let path =
            std::env::temp_dir().join(format!("daku-syslog-err-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = SyslogCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(
                SyslogCountTransport {
                    body: include_str!("../tests/fixtures/syslog/count_4.json"),
                },
                SystemClock,
            ),
            store,
        );
        collector.collect().unwrap();

        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SYSLOG_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["error_count_1h"], 4);
        let samples =
            persistence::load_signal_samples(&connection, "prod", SYSLOG_SIGNAL_ID).unwrap();
        assert_eq!(samples[0].value_real, Some(4.0));
        let _ = std::fs::remove_file(path);
    }
}
