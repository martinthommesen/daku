//! Shared ServiceNow HTTP client: OAuth/basic, 429/`Retry-After`, injectable transport.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context};
use base64::Engine;
use serde::Deserialize;

use crate::config::{AuthMethod, CredentialStore, EnvironmentConfig};

const MAX_429_RETRIES: u8 = 2;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
    }
}

pub trait HttpTransport: Send + Sync {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
    fn sleep(&self, duration: Duration);
}

impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> SystemTime {
        (**self).now()
    }

    fn sleep(&self, duration: Duration) {
        (**self).sleep(duration);
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

struct CachedToken {
    access_token: String,
    valid_until: SystemTime,
}

pub struct ServiceNowClient {
    transport: Box<dyn HttpTransport>,
    clock: Box<dyn Clock>,
    tokens: Mutex<HashMap<String, CachedToken>>,
}

impl ServiceNowClient {
    pub fn new(transport: impl HttpTransport + 'static, clock: impl Clock + 'static) -> Self {
        Self {
            transport: Box::new(transport),
            clock: Box::new(clock),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    pub fn request(
        &self,
        environment: &EnvironmentConfig,
        credentials: &dyn CredentialStore,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> anyhow::Result<HttpResponse> {
        let mut refreshed = false;
        loop {
            let headers = self.authorize(environment, credentials)?;
            let request = HttpRequest {
                method: method.to_owned(),
                url: join_url(&environment.instance_url, path),
                headers,
                body: body.map(str::to_owned),
            };
            let response = self.send(&request)?;
            if response.status == 401
                && environment.auth_method == AuthMethod::OauthClientCredentials
                && !refreshed
            {
                self.tokens
                    .lock()
                    .expect("token cache")
                    .remove(&environment.id);
                refreshed = true;
                continue;
            }
            return Ok(response);
        }
    }

    fn authorize(
        &self,
        environment: &EnvironmentConfig,
        credentials: &dyn CredentialStore,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let blob = credentials
            .get(&environment.id)?
            .ok_or_else(|| anyhow!("no credential for environment {}", environment.id))?;
        match environment.auth_method {
            AuthMethod::Basic => {
                let parsed: BasicCred =
                    serde_json::from_str(&blob).context("basic credential JSON")?;
                Ok(vec![(
                    "Authorization".into(),
                    basic_authorization(&parsed.username, &parsed.password),
                )])
            }
            AuthMethod::OauthClientCredentials => {
                let access = self.oauth_access(environment, &blob)?;
                Ok(vec![("Authorization".into(), format!("Bearer {access}"))])
            }
        }
    }

    fn oauth_access(&self, environment: &EnvironmentConfig, blob: &str) -> anyhow::Result<String> {
        {
            let cache = self.tokens.lock().expect("token cache");
            if let Some(cached) = cache.get(&environment.id) {
                if self.clock.now() < cached.valid_until {
                    return Ok(cached.access_token.clone());
                }
            }
        }
        let parsed: OauthCred = serde_json::from_str(blob).context("oauth credential JSON")?;
        let body = format!(
            "grant_type=client_credentials&client_id={}&client_secret={}",
            urlencode(&parsed.client_id),
            urlencode(&parsed.client_secret)
        );
        let request = HttpRequest {
            method: "POST".into(),
            url: join_url(&environment.instance_url, "/oauth_token.do"),
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: Some(body),
        };
        let response = self.send(&request)?;
        if response.status != 200 {
            return Err(anyhow!(
                "oauth_token.do returned HTTP {} for {}",
                response.status,
                environment.id
            ));
        }
        let grant: AccessGrant =
            serde_json::from_str(&response.body).context("oauth token JSON")?;
        let expires_in = grant.expires_in.unwrap_or(1800);
        let valid_until = self.clock.now() + Duration::from_secs(expires_in);
        self.tokens.lock().expect("token cache").insert(
            environment.id.clone(),
            CachedToken {
                access_token: grant.access_token.clone(),
                valid_until,
            },
        );
        Ok(grant.access_token)
    }

    fn send(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let mut retries = 0;
        loop {
            let response = self.transport.execute(request)?;
            if response.status == 429 && retries < MAX_429_RETRIES {
                self.clock
                    .sleep(retry_after_delay(&response, self.clock.now()));
                retries += 1;
                continue;
            }
            return Ok(response);
        }
    }
}

#[derive(Deserialize)]
struct BasicCred {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct OauthCred {
    client_id: String,
    client_secret: String,
}

#[derive(Deserialize)]
struct AccessGrant {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn basic_authorization(username: &str, password: &str) -> String {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {encoded}")
}

fn join_url(instance_url: &str, path: &str) -> String {
    format!("{}{}", instance_url.trim_end_matches('/'), path)
}

fn urlencode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn retry_after_delay(response: &HttpResponse, now: SystemTime) -> Duration {
    let Some(value) = response.header("Retry-After") else {
        return DEFAULT_RETRY_AFTER;
    };
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Duration::from_secs(seconds);
    }
    http_date_delay(value, now).unwrap_or(DEFAULT_RETRY_AFTER)
}

fn http_date_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let parsed = httpdate::parse_http_date(value).ok()?;
    Some(parsed.duration_since(now).unwrap_or(Duration::ZERO))
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        Self { agent }
    }
}

impl HttpTransport for UreqTransport {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let mut call = match request.method.as_str() {
            "GET" => self.agent.get(&request.url),
            "POST" => self.agent.post(&request.url),
            other => return Err(anyhow!("unsupported HTTP method {other}")),
        };
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let response = match &request.body {
            Some(body) => call.send_string(body),
            None => call.call(),
        };
        match response {
            Ok(response) | Err(ureq::Error::Status(_, response)) => read_ureq_response(response),
            Err(error) => Err(error.into()),
        }
    }
}

fn read_ureq_response(response: ureq::Response) -> anyhow::Result<HttpResponse> {
    let status = response.status();
    let headers = response
        .headers_names()
        .into_iter()
        .filter_map(|name| response.header(&name).map(|value| (name, value.to_owned())))
        .collect();
    let body = response.into_string().context("reading HTTP body")?;
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::config::MemoryCredentialStore;

    struct ScriptedTransport {
        responses: Mutex<VecDeque<HttpResponse>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().expect("requests").clone()
        }
    }

    impl HttpTransport for ScriptedTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            self.requests
                .lock()
                .expect("requests")
                .push(request.clone());
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .ok_or_else(|| anyhow!("no scripted response left for {}", request.url))
        }
    }

    struct RecordingClock {
        sleeps: Mutex<Vec<Duration>>,
    }

    impl Default for RecordingClock {
        fn default() -> Self {
            Self {
                sleeps: Mutex::new(Vec::new()),
            }
        }
    }

    impl Clock for RecordingClock {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        }

        fn sleep(&self, duration: Duration) {
            self.sleeps.lock().expect("sleeps").push(duration);
        }
    }

    fn basic_env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "dev".into(),
            label: "Dev".into(),
            instance_url: "https://acme-dev.example.service-now.com".into(),
            auth_method: AuthMethod::Basic,
            sort_order: 0,
        }
    }

    fn oauth_env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "prod".into(),
            label: "Production".into(),
            instance_url: "https://acme-prod.example.service-now.com".into(),
            auth_method: AuthMethod::OauthClientCredentials,
            sort_order: 0,
        }
    }

    fn ok_table() -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: include_str!("../tests/fixtures/availability/ok.json").into(),
        }
    }

    fn token_ok(token: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: format!(r#"{{"access_token":"{token}","expires_in":1800}}"#),
        }
    }

    #[test]
    fn servicenow_http_retries_on_429_retry_after() {
        let transport = ScriptedTransport::new(vec![
            HttpResponse {
                status: 429,
                headers: vec![("Retry-After".into(), "1".into())],
                body: r#"{"error":{"message":"Rate limit exceeded"}}"#.into(),
            },
            ok_table(),
        ]);
        let clock = Arc::new(RecordingClock::default());
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, clock.clone());
        let response = client
            .request(
                &basic_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties?sysparm_query=name=glide.war&sysparm_fields=value&sysparm_limit=1",
                None,
            )
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            *clock.sleeps.lock().expect("sleeps"),
            [Duration::from_secs(1)]
        );
    }

    #[test]
    fn servicenow_http_retries_on_429_http_date() {
        let transport = ScriptedTransport::new(vec![
            HttpResponse {
                status: 429,
                headers: vec![(
                    "Retry-After".into(),
                    "Tue, 14 Nov 2023 22:13:30 GMT".into(),
                )],
                body: String::new(),
            },
            ok_table(),
        ]);
        let clock = Arc::new(RecordingClock::default());
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, clock.clone());
        assert_eq!(
            client
                .request(&basic_env(), &credentials, "GET", "/api/now/table/sys_properties", None)
                .unwrap()
                .status,
            200
        );
        assert_eq!(
            *clock.sleeps.lock().expect("sleeps"),
            [Duration::from_secs(10)]
        );
    }

    #[test]
    fn servicenow_http_429_exhausted_returns_429() {
        let rate_limited = HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "1".into())],
            body: String::new(),
        };
        let transport = ScriptedTransport::new(vec![
            rate_limited.clone(),
            rate_limited.clone(),
            rate_limited,
        ]);
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, RecordingClock::default());
        let response = client
            .request(&basic_env(), &credentials, "GET", "/api/now/table/sys_properties", None)
            .unwrap();
        assert_eq!(response.status, 429);
    }

    #[test]
    fn servicenow_http_basic_auth_sends_authorization_header() {
        let transport = Arc::new(ScriptedTransport::new(vec![ok_table()]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let response = client
            .request(
                &basic_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap();
        assert_eq!(response.status, 200);
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].url.contains("oauth_token"));
        let expected = basic_authorization("reader", "secret");
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.as_str()),
            Some(expected.as_str())
        );
    }

    #[test]
    fn servicenow_http_oauth_cache_skips_second_token_fetch() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            token_ok("tok-1"),
            ok_table(),
            ok_table(),
        ]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let path = "/api/now/table/sys_properties";
        assert_eq!(
            client
                .request(&oauth_env(), &credentials, "GET", path, None)
                .unwrap()
                .status,
            200
        );
        assert_eq!(
            client
                .request(&oauth_env(), &credentials, "GET", path, None)
                .unwrap()
                .status,
            200
        );
        let urls: Vec<_> = transport
            .requests()
            .into_iter()
            .map(|request| request.url)
            .collect();
        assert_eq!(
            urls.iter()
                .filter(|url| url.contains("oauth_token.do"))
                .count(),
            1
        );
        assert_eq!(urls.len(), 3);
    }

    #[test]
    fn servicenow_http_oauth_refreshes_once_on_401() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            token_ok("tok-1"),
            HttpResponse {
                status: 401,
                headers: vec![("content-type".into(), "application/json".into())],
                body: include_str!("../tests/fixtures/availability/401.json").into(),
            },
            token_ok("tok-2"),
            ok_table(),
        ]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let response = client
            .request(
                &oauth_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap();
        assert_eq!(response.status, 200);
        let auths: Vec<_> = transport
            .requests()
            .into_iter()
            .filter(|request| !request.url.contains("oauth_token.do"))
            .map(|request| {
                request
                    .headers
                    .into_iter()
                    .find(|(name, _)| name == "Authorization")
                    .map(|(_, value)| value)
            })
            .collect();
        assert_eq!(
            auths,
            [Some("Bearer tok-1".into()), Some("Bearer tok-2".into())]
        );
    }

    struct SharedTransport(Arc<ScriptedTransport>);

    impl HttpTransport for SharedTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            self.0.execute(request)
        }
    }
}
