//! GitHub Actions Signal: failed workflow runs in the last 24 h.
//!
//! The second proof of the platform registry: one `owner/repo` per
//! Environment (`instance_url` like `https://github.com/<owner>/<repo>`),
//! read through the REST API with a personal access token. The Credential
//! blob reuses the basic shape (`{"username":…,"password":"<token>"}` — the
//! username is ignored, the password rides a `Bearer` header); GitHub wants
//! a `User-Agent` on every call and gets one.
//!
//! Runs concluding `failure` or `timed_out` in the last 24 h count; the 10
//! newest back the drill-in with links to each run page.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, ROW_LIST_LIMIT, Signal, unix_now};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds, split_github_repo};
use crate::last_clone::age_days;
use crate::servicenow::{HttpRequest, ServiceNowClient};
use crate::signal_eval::{evaluate_ge, row_text};

pub const ACTIONS_SIGNAL_ID: &str = "actions";
pub const ACTIONS_RUNS_PATH: &str = "/repos/{owner}/{repo}/actions/runs?per_page=30";

/// Conclusions that count as failed.
const FAILED_CONCLUSIONS: [&str; 2] = ["failure", "timed_out"];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct RunRow {
    name: String,
    conclusion: String,
    created_at: String,
    html_url: String,
}

pub fn actions_state(failed_24h: u64, thresholds: &Thresholds) -> SignalState {
    evaluate_ge(failed_24h, thresholds.actions_failed_degraded_at)
}

fn bearer_token(credentials: &dyn CredentialStore, environment_id: &str) -> anyhow::Result<String> {
    let blob = credentials
        .get(environment_id)?
        .ok_or_else(|| anyhow::anyhow!("no credential for environment {environment_id}"))?;
    let value: serde_json::Value = serde_json::from_str(&blob)
        .map_err(|_| anyhow::anyhow!("github credential is not valid JSON"))?;
    value
        .get("password")
        .and_then(|item| item.as_str())
        .filter(|token| !token.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("github credential needs a token in its password field"))
}

fn fetch_failed_runs(
    client: &ServiceNowClient,
    token: &str,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Vec<RunRow>> {
    let path = ACTIONS_RUNS_PATH
        .replacen("{owner}", owner, 1)
        .replacen("{repo}", repo, 1);
    let response = client.execute_raw(&HttpRequest {
        method: "GET".into(),
        url: format!("https://api.github.com{path}"),
        headers: vec![
            ("User-Agent".into(), "daku".into()),
            ("Accept".into(), "application/vnd.github+json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: None,
    })?;
    if response.status == 401 || response.status == 403 {
        anyhow::bail!(
            "GitHub API returned HTTP {} (check the token)",
            response.status
        );
    }
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let now = unix_now();
    let rows: Vec<RunRow> = serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
        .ok()
        .and_then(|value| {
            value
                .get("workflow_runs")
                .and_then(|runs| runs.as_array())
                .cloned()
        })
        .ok_or_else(|| anyhow::anyhow!("actions response has no workflow_runs array"))?
        .iter()
        .filter_map(|run| {
            let conclusion = row_text(run, "conclusion");
            if !FAILED_CONCLUSIONS.contains(&conclusion.as_str()) {
                return None;
            }
            let created_at = row_text(run, "created_at");
            // Unparseable timestamps count (fail-loud); older than a day
            // does not.
            if let Some(days) = age_days(&created_at, now)
                && days > 1
            {
                return None;
            }
            let html_url = row_text(run, "html_url");
            if html_url.is_empty() {
                return None;
            }
            Some(RunRow {
                name: row_text(run, "name"),
                conclusion,
                created_at,
                html_url,
            })
        })
        .collect();
    Ok(rows.into_iter().take(ROW_LIST_LIMIT).collect())
}

#[derive(Default)]
pub struct ActionsSignal;

pub type ActionsCollector = PerEnvironmentCollector<ActionsSignal>;

impl Signal for ActionsSignal {
    fn id(&self) -> &'static str {
        ACTIONS_SIGNAL_ID
    }

    fn gated_by_availability(&self) -> bool {
        // No ServiceNow reachability snapshots exist for this Environment.
        false
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let (owner, repo) = split_github_repo(&environment.instance_url).ok_or_else(|| {
            anyhow::anyhow!("github instance_url must look like https://github.com/<owner>/<repo>")
        })?;
        let pat = bearer_token(credentials, &environment.id)?;
        let runs = fetch_failed_runs(client, &pat, &owner, &repo)?;
        let failed_24h = runs.len() as u64;
        Ok(Observation {
            state: actions_state(failed_24h, &environment.thresholds),
            payload: serde_json::json!({
                "failed_24h": failed_24h,
                "run_rows": runs,
                "run_rows_truncated": runs.len() >= ROW_LIST_LIMIT,
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

    fn github_env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "repo".into(),
            label: "Repo".into(),
            instance_url: "https://github.com/acme/app".into(),
            auth_method: crate::config::AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
            platform: Platform::Github,
            thresholds: Thresholds::default(),
            expected_drift: Vec::new(),
        }
    }

    const TOKEN: &str = r#"{"username":"token","password":"test-pat-token"}"#;

    /// Run timestamps relative to the real clock without formatting one:
    /// 2099 reads age 0 (clamped), 2020 reads centuries old.
    const RUNS: &str = r#"{"workflow_runs":[
        {"name":"ci","conclusion":"failure","created_at":"2099-01-01T00:12:00Z","html_url":"https://github.com/acme/app/actions/runs/1"},
        {"name":"lint","conclusion":"timed_out","created_at":"2099-01-01T01:12:00Z","html_url":"https://github.com/acme/app/actions/runs/2"},
        {"name":"ci","conclusion":"success","created_at":"2099-01-01T02:12:00Z","html_url":"https://github.com/acme/app/actions/runs/3"},
        {"name":"ci","conclusion":"failure","created_at":"2020-01-01T00:12:00Z","html_url":"https://github.com/acme/app/actions/runs/4"}
    ]}"#;

    struct RunsTransport {
        body: &'static str,
        status: u16,
    }

    impl HttpTransport for RunsTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            assert!(
                request
                    .url
                    .starts_with("https://api.github.com/repos/acme/app/actions/runs"),
                "actions collector must read the runs API: {}",
                request.url
            );
            let auth = request
                .headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.as_str())
                .unwrap_or("");
            assert_eq!(auth, "Bearer test-pat-token");
            assert!(
                request.headers.iter().any(|(name, _)| name == "User-Agent"),
                "GitHub requires a User-Agent"
            );
            Ok(HttpResponse {
                status: self.status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    fn collect_with(
        environment: EnvironmentConfig,
        body: &'static str,
        status: u16,
        credential: bool,
    ) -> (TempDb, StateStore) {
        let db = TempDb::new("actions");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        if credential {
            credentials.insert("repo", TOKEN);
        }
        let collector = ActionsCollector::new(
            vec![environment],
            credentials,
            ServiceNowClient::new(RunsTransport { body, status }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn actions_signal_counts_recent_failures_and_ignores_old_ones() {
        let (_db, store) = collect_with(github_env(), RUNS, 200, true);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "repo", ACTIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        // Two recent failures; the success and the 2020 failure do not count.
        assert_eq!(payload["failed_24h"], 2);
        assert_eq!(payload["run_rows"].as_array().unwrap().len(), 2);
        assert_eq!(payload["run_rows"][0]["name"], "ci");
    }

    #[test]
    fn actions_signal_clean_runs_are_healthy() {
        let (_db, store) = collect_with(
            github_env(),
            r#"{"workflow_runs":[{"name":"ci","conclusion":"success","created_at":"2099-01-01T00:12:00Z","html_url":"https://github.com/acme/app/actions/runs/3"}]}"#,
            200,
            true,
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "repo", ACTIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
    }

    #[test]
    fn actions_signal_missing_credential_is_down() {
        let (_db, store) = collect_with(github_env(), RUNS, 200, false);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "repo", ACTIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
    }

    #[test]
    fn actions_signal_unauthorized_is_down_with_a_hint() {
        let (_db, store) = collect_with(github_env(), "{}", 401, true);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "repo", ACTIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert!(
            payload["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("token")),
            "{}",
            payload["detail"]
        );
    }

    #[test]
    fn actions_signal_rejects_a_non_repo_url() {
        let mut env = github_env();
        env.instance_url = "https://github.com/acme".into();
        let (_db, store) = collect_with(env, RUNS, 200, true);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "repo", ACTIONS_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
    }

    #[test]
    fn actions_threshold_override_tolerates_flakes() {
        let thresholds = Thresholds {
            actions_failed_degraded_at: 3,
            ..Thresholds::default()
        };
        assert_eq!(actions_state(2, &thresholds), SignalState::Healthy);
        assert_eq!(actions_state(3, &thresholds), SignalState::Degraded);
    }
}
