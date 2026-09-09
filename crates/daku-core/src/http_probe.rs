//! Generic HTTP probe Signal: status + latency for `platform = "http"`.
//!
//! The proof that the platform registry is real: any `https://` URL can be
//! watched — status pages, health endpoints, small services — with no
//! ServiceNow semantics attached. `2xx`–`3xx` reads healthy with the
//! round-trip time; anything else (including transport errors) reads down.
//! An optional RTT ceiling degrades slow-but-up targets. Query and fragment
//! never reach the snapshot: probe URLs can carry tokens.

use std::time::Instant;

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::{HttpRequest, ServiceNowClient, basic_authorization};
use crate::signal_eval::redact_url;

pub const HTTP_PROBE_SIGNAL_ID: &str = "http_probe";

fn basic_header(blob: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(blob).ok()?;
    let username = value.get("username")?.as_str()?;
    let password = value.get("password")?.as_str()?;
    if username.trim().is_empty() || password.trim().is_empty() {
        return None;
    }
    Some((
        "Authorization".into(),
        basic_authorization(username, password),
    ))
}

pub fn http_probe_state(status: u16, rtt_ms: u64, thresholds: &Thresholds) -> SignalState {
    if !(200..400).contains(&status) {
        SignalState::Down
    } else if thresholds
        .http_probe_rtt_degraded_ms
        .is_some_and(|ceiling| rtt_ms >= ceiling)
    {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

#[derive(Default)]
pub struct HttpProbeSignal;

pub type HttpProbeCollector = PerEnvironmentCollector<HttpProbeSignal>;

impl Signal for HttpProbeSignal {
    fn id(&self) -> &'static str {
        HTTP_PROBE_SIGNAL_ID
    }

    fn gated_by_availability(&self) -> bool {
        // The platform's own availability probe: never gated by ServiceNow
        // reachability snapshots (there are none for this Environment).
        false
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let mut headers = vec![("User-Agent".into(), "daku".into())];
        if let Ok(Some(blob)) = credentials.get(&environment.id)
            && let Some(header) = basic_header(&blob)
        {
            headers.push(header);
        }
        let started = Instant::now();
        let response = client.execute_raw(&HttpRequest {
            method: "GET".into(),
            url: environment.instance_url.clone(),
            headers,
            body: None,
        });
        let rtt_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u64;
        let response = response?;
        Ok(Observation {
            state: http_probe_state(response.status, rtt_ms, &environment.thresholds),
            payload: serde_json::json!({
                "http_status": response.status,
                "rtt_ms": rtt_ms,
                "url": redact_url(&environment.instance_url),
                "reachability": "reachable",
            }),
            sample: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::TempDb;
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::{EnvironmentConfig, MemoryCredentialStore, Platform};
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    fn http_env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "status".into(),
            label: "Status page".into(),
            instance_url: "https://status.example.com/health?token=secret".into(),
            auth_method: crate::config::AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
            platform: Platform::Http,
            thresholds: Thresholds::default(),
            expected_drift: Vec::new(),
        }
    }

    #[test]
    fn http_probe_state_maps_status_and_ceiling() {
        assert_eq!(
            http_probe_state(200, 10, &Thresholds::default()),
            SignalState::Healthy
        );
        assert_eq!(
            http_probe_state(301, 10, &Thresholds::default()),
            SignalState::Healthy
        );
        assert_eq!(
            http_probe_state(500, 10, &Thresholds::default()),
            SignalState::Down
        );
        assert_eq!(
            http_probe_state(404, 10, &Thresholds::default()),
            SignalState::Down
        );
        let ceiling = Thresholds {
            http_probe_rtt_degraded_ms: Some(100),
            ..Thresholds::default()
        };
        assert_eq!(http_probe_state(200, 99, &ceiling), SignalState::Healthy);
        assert_eq!(http_probe_state(200, 100, &ceiling), SignalState::Degraded);
    }

    struct ProbeTransport {
        status: u16,
    }

    impl HttpTransport for ProbeTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert_eq!(request.method, "GET");
            assert!(
                request.url.contains("status.example.com/health"),
                "probe hits the configured URL verbatim: {}",
                request.url
            );
            Ok(HttpResponse {
                status: self.status,
                headers: Vec::new(),
                body: String::new(),
            })
        }
    }

    fn collect_with(
        environment: EnvironmentConfig,
        status: u16,
        credential: Option<&str>,
    ) -> (TempDb, StateStore) {
        let db = TempDb::new("probe");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        if let Some(blob) = credential {
            credentials.insert("status", blob);
        }
        let collector = HttpProbeCollector::new(
            vec![environment],
            credentials,
            ServiceNowClient::new(ProbeTransport { status }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn http_probe_ok_writes_healthy_snapshot_without_secrets() {
        let (_db, store) = collect_with(http_env(), 200, None);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "status", HTTP_PROBE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["http_status"], 200);
        assert_eq!(payload["url"], "https://status.example.com/health");
    }

    #[test]
    fn http_probe_500_is_down() {
        let (_db, store) = collect_with(http_env(), 503, None);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "status", HTTP_PROBE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
    }

    #[test]
    fn http_probe_sends_basic_auth_when_stored() {
        struct Authed(u16);
        impl HttpTransport for Authed {
            fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                let auth = request
                    .headers
                    .iter()
                    .find(|(name, _)| name == "Authorization")
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default();
                assert!(
                    auth.starts_with("Basic "),
                    "stored basic blob must ride the probe: {auth}"
                );
                Ok(HttpResponse {
                    status: self.0,
                    headers: Vec::new(),
                    body: String::new(),
                })
            }
        }
        let db = TempDb::new("probe-auth");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("status", r#"{"username":"u","password":"p"}"#);
        HttpProbeCollector::new(
            vec![http_env()],
            credentials,
            ServiceNowClient::new(Authed(200), SystemClock),
            store,
        )
        .collect()
        .unwrap();
        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "status", HTTP_PROBE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
    }

    #[test]
    fn http_probe_transport_error_is_down() {
        struct Offline;
        impl HttpTransport for Offline {
            fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                anyhow::bail!("connection refused")
            }
        }
        let db = TempDb::new("probe-offline");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        HttpProbeCollector::new(
            vec![http_env()],
            credentials,
            ServiceNowClient::new(Offline, SystemClock),
            store,
        )
        .collect()
        .unwrap();
        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "status", HTTP_PROBE_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
    }

    #[test]
    fn http_probe_ignores_availability_gating() {
        // No availability snapshot exists for an HTTP Environment; the probe
        // declares itself ungated so the intent is pinned even if the
        // shared gating rule changes.
        assert!(!HttpProbeSignal.gated_by_availability());
    }
}
