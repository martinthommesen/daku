//! Test-only helpers shared by daku-core unit tests.

use std::path::{Path, PathBuf};

use crate::config::{AuthMethod, EnvironmentConfig, Platform, Thresholds};
use crate::persistence::StateStore;

/// Unique SQLite path under the OS temp dir; removes the db and its WAL/SHM
/// sidecars on drop (also on panic).
pub struct TempDb {
    path: PathBuf,
}

impl TempDb {
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("daku-{label}-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn store(&self) -> StateStore {
        StateStore::daemon(self.path.clone())
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut sidecar = self.path.clone().into_os_string();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(sidecar);
        }
    }
}

/// Unique JSON path under the OS temp dir; removes the file on drop (also on
/// panic). The file only exists if the test wrote one — `new` hands out a path
/// for the tests that need a *missing* file.
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!("daku-{label}-{}.json", uuid::Uuid::new_v4())),
        }
    }

    pub fn with_contents(label: &str, contents: impl AsRef<[u8]>) -> Self {
        let file = Self::new(label);
        std::fs::write(&file.path, contents).unwrap();
        file
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The Basic-auth `prod` Environment used across collector tests.
pub fn prod() -> EnvironmentConfig {
    EnvironmentConfig {
        id: "prod".into(),
        label: "Production".into(),
        instance_url: "https://acme-prod.example.service-now.com".into(),
        auth_method: AuthMethod::Basic,
        sort_order: 0,
        clone_source: false,
        platform: Platform::Servicenow,
        thresholds: Thresholds::default(),
        expected_drift: Vec::new(),
    }
}

/// Shared scripted ServiceNow transport: queues canned `HttpResponse`s,
/// records every `HttpRequest` for assertion. Replaces the three ad-hoc
/// scripted transports (`servicenow.rs`, `collector.rs`, `payload_contract.rs`
/// each had its own). `Arc<MockTransport>` is itself a transport, so two
/// requests through one `ServiceNowClient` share one script + one log.
pub struct MockTransport {
    responses: std::sync::Mutex<std::collections::VecDeque<crate::servicenow::HttpResponse>>,
    requests: std::sync::Mutex<Vec<crate::servicenow::HttpRequest>>,
}

impl MockTransport {
    pub fn new(responses: Vec<crate::servicenow::HttpResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<crate::servicenow::HttpRequest> {
        self.requests.lock().expect("requests").clone()
    }

    pub fn urls(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.url)
            .collect()
    }
}

impl crate::servicenow::HttpTransport for MockTransport {
    fn execute(
        &self,
        request: &crate::servicenow::HttpRequest,
    ) -> anyhow::Result<crate::servicenow::HttpResponse> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.clone());
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no scripted response left for {}", request.url))
    }
}

impl crate::servicenow::HttpTransport for std::sync::Arc<MockTransport> {
    fn execute(
        &self,
        request: &crate::servicenow::HttpRequest,
    ) -> anyhow::Result<crate::servicenow::HttpResponse> {
        (**self).execute(request)
    }
}

/// `Clock` that never sleeps and reports a fixed time. Retries cost
/// `bun run check` zero real seconds.
pub struct NoSleepClock;

impl crate::servicenow::Clock for NoSleepClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
    }

    fn sleep(&self, _: std::time::Duration) {}
}

/// Canned responses shared by collector tests.
pub mod canned {
    use crate::servicenow::HttpResponse;

    pub fn ok_table(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
        }
    }

    pub fn availability_ok() -> HttpResponse {
        ok_table(include_str!("../tests/fixtures/availability/ok.json"))
    }

    pub fn rate_limited() -> HttpResponse {
        HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "1".into())],
            body: r#"{"error":{"message":"Rate limit exceeded"}}"#.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servicenow::{HttpRequest, HttpTransport, ServiceNowClient};
    use std::sync::Arc;

    #[test]
    fn temp_db_removes_db_and_sidecars_on_drop() {
        let path;
        {
            let db = TempDb::new("self");
            path = db.path().to_path_buf();
            let _connection = db.store().open().unwrap(); // creates .db (+ -wal/-shm under WAL)
        }
        assert!(!path.exists());
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        assert!(!Path::new(&wal).exists());
    }

    #[test]
    fn temp_file_is_removed_on_drop() {
        let path;
        {
            let file = TempFile::with_contents("self", "[]");
            path = file.path().to_path_buf();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
        }
        assert!(!path.exists());
    }

    #[test]
    fn mock_transport_scripts_responses_and_records_requests() {
        use crate::config::MemoryCredentialStore;

        let transport = Arc::new(MockTransport::new(vec![
            canned::availability_ok(),
            canned::rate_limited(),
            canned::availability_ok(),
        ]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport.clone(), NoSleepClock);
        let response = client
            .request(
                &prod(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties?sysparm_query=name=glide.war",
                None,
            )
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(transport.urls().len(), 1);
        assert!(transport.urls()[0].contains("glide.war"));
        // A 429 is retried once (NoSleepClock makes it free): the second
        // request consumes the rate-limited script plus its retry.
        let retried = client
            .request(
                &prod(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties?sysparm_query=name=glide.war",
                None,
            )
            .unwrap();
        assert_eq!(retried.status, 200);
        assert_eq!(transport.urls().len(), 3);
        // Exhausting the script is an error naming the URL, never a hang.
        let missing: anyhow::Result<_> = transport.execute(&HttpRequest {
            method: "GET".into(),
            url: "https://x.example.com/unscripted".into(),
            headers: Vec::new(),
            body: None,
        });
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("no scripted response")
        );
    }
}
