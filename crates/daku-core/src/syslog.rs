//! Syslog error-rate Signal: 1h Error count on the rotated `syslog` table.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const SYSLOG_SIGNAL_ID: &str = "syslog";
pub const SYSLOG_ERROR_LEVEL: u8 = 2;

pub fn syslog_error_path() -> String {
    format!(
        "/api/now/stats/syslog?sysparm_count=true&sysparm_query=level={SYSLOG_ERROR_LEVEL}^sys_created_on>javascript:gs.hoursAgoStart(1)"
    )
}

pub fn syslog_state(error_count_1h: u64) -> SignalState {
    if error_count_1h > 0 {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

#[derive(Default)]
pub struct SyslogSignal;

pub type SyslogCollector = PerEnvironmentCollector<SyslogSignal>;

impl Signal for SyslogSignal {
    fn id(&self) -> &'static str {
        SYSLOG_SIGNAL_ID
    }

    fn keeps_samples(&self) -> bool {
        true
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let error_count_1h =
            fetch_aggregate_count(client, environment, credentials, &syslog_error_path())?;
        Ok(Observation {
            state: syslog_state(error_count_1h),
            payload: serde_json::json!({ "error_count_1h": error_count_1h }),
            sample: Some(error_count_1h as f64),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TempDb, prod};
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::MemoryCredentialStore;
    use crate::persistence;
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

    #[test]
    fn syslog_signal_zeros_are_healthy_and_write_sample() {
        let db = TempDb::new("syslog");
        let store = db.store();
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

        let connection = db.store().open().unwrap();
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
    }

    #[test]
    fn syslog_signal_errors_are_degraded() {
        let db = TempDb::new("syslog-err");
        let store = db.store();
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

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SYSLOG_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["error_count_1h"], 4);
        let samples =
            persistence::load_signal_samples(&connection, "prod", SYSLOG_SIGNAL_ID).unwrap();
        assert_eq!(samples[0].value_real, Some(4.0));
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn syslog_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("syslog-asleep");
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
        SyslogCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", SYSLOG_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
        assert!(
            persistence::load_signal_samples(&connection, "prod", SYSLOG_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }
}
