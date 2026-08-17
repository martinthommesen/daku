//! Outbound / integration-failures Signal: 1h HTTP 4xx/5xx count on `sys_outbound_http_log`.

use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::collector::SignalCollector;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::persistence::{self, StateStore};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const OUTBOUND_SIGNAL_ID: &str = "outbound";
pub const OUTBOUND_HTTP_PATH: &str = "/api/now/stats/sys_outbound_http_log?sysparm_count=true&sysparm_query=http_status>=400^sys_created_on>javascript:gs.hoursAgoStart(1)";

pub fn outbound_state(outbound_http_4xx_5xx_1h: u64) -> &'static str {
    if outbound_http_4xx_5xx_1h == 0 {
        "healthy"
    } else {
        "degraded"
    }
}

pub struct OutboundCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
}

impl OutboundCollector {
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

impl SignalCollector for OutboundCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let mut first_error = None;
        for environment in &self.environments {
            let count = fetch_aggregate_count(
                &self.client,
                environment,
                self.credentials.as_ref(),
                OUTBOUND_HTTP_PATH,
            );
            if let Err(error) = collect_outbound(&connection, environment, observed_at, count) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn collect_outbound(
    connection: &Connection,
    environment: &EnvironmentConfig,
    observed_at: i64,
    count: anyhow::Result<u64>,
) -> anyhow::Result<()> {
    match count {
        Ok(outbound_http_4xx_5xx_1h) => persist_outbound_ok(
            connection,
            &environment.id,
            outbound_http_4xx_5xx_1h,
            observed_at,
        )
        .map_err(anyhow::Error::from),
        Err(error) => {
            persist_outbound_down(connection, &environment.id, &error.to_string(), observed_at)
                .map_err(anyhow::Error::from)
        }
    }
}

fn persist_outbound_ok(
    connection: &Connection,
    environment_id: &str,
    outbound_http_4xx_5xx_1h: u64,
    observed_at: i64,
) -> io::Result<()> {
    let payload = serde_json::json!({ "outbound_http_4xx_5xx_1h": outbound_http_4xx_5xx_1h });
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        OUTBOUND_SIGNAL_ID,
        observed_at,
        outbound_state(outbound_http_4xx_5xx_1h),
        &payload.to_string(),
    )
}

fn persist_outbound_down(
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
        OUTBOUND_SIGNAL_ID,
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

    #[test]
    fn outbound_signal_zero_is_healthy() {
        assert_eq!(outbound_state(0), "healthy");
    }

    #[test]
    fn outbound_signal_nonzero_is_degraded() {
        assert_eq!(outbound_state(3), "degraded");
    }

    struct OutboundCountTransport {
        body: &'static str,
    }

    impl HttpTransport for OutboundCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request.url.contains("/api/now/stats/sys_outbound_http_log"),
                "outbound collector must use Aggregate API: {}",
                request.url
            );
            assert!(
                request.url.contains("http_status>=400")
                    || request.url.contains("http_status%3E=400"),
                "outbound query must count HTTP 4xx/5xx: {}",
                request.url
            );
            assert!(
                request.url.contains("sys_created_on") && request.url.contains("hoursAgoStart"),
                "outbound query must be date-bound: {}",
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

    fn collect_with(body: &'static str) -> (std::path::PathBuf, StateStore) {
        let path = std::env::temp_dir().join(format!("daku-outbound-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = OutboundCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(OutboundCountTransport { body }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        (path.clone(), StateStore::daemon(path))
    }

    #[test]
    fn outbound_signal_zero_writes_healthy_snapshot_without_sample() {
        let (path, store) = collect_with(include_str!("../tests/fixtures/outbound/count_0.json"));
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["outbound_http_4xx_5xx_1h"], 0);
        assert!(
            persistence::load_signal_samples(&connection, "prod", OUTBOUND_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn outbound_signal_nonzero_writes_degraded_snapshot() {
        let (path, store) = collect_with(include_str!("../tests/fixtures/outbound/count_3.json"));
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["outbound_http_4xx_5xx_1h"], 3);
        assert!(
            persistence::load_signal_samples(&connection, "prod", OUTBOUND_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    struct OutboundFailTransport;

    impl HttpTransport for OutboundFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn outbound_signal_probe_failure_is_down_without_sample() {
        let path =
            std::env::temp_dir().join(format!("daku-outbound-fail-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        let store = StateStore::daemon(path.clone());
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = OutboundCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(OutboundFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = StateStore::daemon(path.clone()).open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("outbound_http_4xx_5xx_1h").is_none());
        assert!(
            persistence::load_signal_samples(&connection, "prod", OUTBOUND_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }
}
