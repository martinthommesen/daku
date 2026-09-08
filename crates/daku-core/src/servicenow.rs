//! Shared ServiceNow HTTP client: OAuth/basic, 429/`Retry-After`, injectable transport.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, anyhow};
use base64::Engine;
use serde::Deserialize;

use crate::config::{AuthMethod, CredentialStore, EnvironmentConfig};

const MAX_429_RETRIES: u8 = 2;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);
/// Upper bound on a single 429 back-off. Anything longer would stall the
/// shared collector thread for every Environment; the collector will retry
/// naturally on its next tick.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// Upper bound on what one tick thread actually sleeps for a 429. Short
/// backoffs (the usual `Retry-After: 1`) are absorbed in-tick; longer ones
/// return the 429 immediately as a transient error (availability already
/// reads it that way and sustained 429s gate the other Signals) instead of
/// parking the whole per-Environment chain for up to a minute.
const MAX_TICK_429_SLEEP: Duration = Duration::from_secs(5);
/// Longest we will trust an OAuth grant regardless of what the server says.
const MAX_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;
/// Shortest OAuth grant we will act on. A server reporting a near-zero
/// `expires_in` would otherwise write a cache entry that is already expired,
/// turning every request into a fresh token POST plus a Keychain read.
const MIN_TOKEN_TTL_SECS: u64 = 60;
/// Subtracted from the advertised lifetime so a token that is seconds from
/// expiry is refreshed rather than sent and rejected.
const TOKEN_TTL_SKEW_SECS: u64 = 30;

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

/// Process-wide HTTP concurrency guard: at most 8 requests in flight
/// globally, 2 per host. Threads already isolate Environments from each
/// other; this keeps a slow one from crowding the rest into 429s.
pub struct LimitedTransport<T> {
    inner: T,
    global: std::sync::Arc<CountingSemaphore>,
    hosts: std::sync::Mutex<HashMap<String, std::sync::Arc<CountingSemaphore>>>,
}

impl<T> LimitedTransport<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            global: std::sync::Arc::new(CountingSemaphore::new(8)),
            hosts: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn host_key(&self, url: &str) -> std::sync::Arc<CountingSemaphore> {
        let host = crate::collector::host_of_url(url).unwrap_or_else(|| "?".into());
        self.hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(host)
            .or_insert_with(|| std::sync::Arc::new(CountingSemaphore::new(2)))
            .clone()
    }
}

impl<T: HttpTransport> HttpTransport for LimitedTransport<T> {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let _global = self.global.acquire();
        let host = self.host_key(&request.url);
        let _host = host.acquire();
        self.inner.execute(request)
    }
}

struct CountingSemaphore {
    max: usize,
    used: std::sync::Mutex<usize>,
    changed: std::sync::Condvar,
}

impl CountingSemaphore {
    fn new(max: usize) -> Self {
        Self {
            max,
            used: std::sync::Mutex::new(0),
            changed: std::sync::Condvar::new(),
        }
    }

    fn acquire(&self) -> SemaphoreGuard<'_> {
        let mut used = crate::lock_or_poisoned(&self.used);
        while *used >= self.max {
            used = self
                .changed
                .wait(used)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *used += 1;
        SemaphoreGuard { semaphore: self }
    }
}

struct SemaphoreGuard<'a> {
    semaphore: &'a CountingSemaphore,
}

impl Drop for SemaphoreGuard<'_> {
    fn drop(&mut self) {
        let mut used = crate::lock_or_poisoned(&self.semaphore.used);
        *used = used.saturating_sub(1);
        self.semaphore.changed.notify_one();
    }
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
    /// Memoized credential blobs per Environment, so a healthy tick costs
    /// one store read per Environment instead of one per probe (Basic auth
    /// hits the Keychain or re-reads `credentials.json` otherwise). A 401
    /// drops the entry and retries once (`request`), so rotation is picked
    /// up on the next probe after at most one stale request.
    credential_blobs: Mutex<HashMap<String, String>>,
}

impl ServiceNowClient {
    pub fn new(transport: impl HttpTransport + 'static, clock: impl Clock + 'static) -> Self {
        Self {
            transport: Box::new(transport),
            clock: Box::new(clock),
            tokens: Mutex::new(HashMap::new()),
            credential_blobs: Mutex::new(HashMap::new()),
        }
    }

    /// Raw request without auth, retry, or refresh: platform probes (HTTP,
    /// GitHub) that carry their own credentials. No `Retry-After` handling —
    /// a rate-limited probe reads `down` and retries next tick.
    pub fn execute_raw(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        self.transport.execute(request)
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
            // A 401 means the memoized secret is stale (rotation) or wrong:
            // drop the cache and retry once. Steady-state wrong credentials
            // cost one extra request per tick; rotation recovers transparently.
            if response.status == 401 && !refreshed {
                if environment.auth_method == AuthMethod::OauthClientCredentials {
                    crate::lock_or_poisoned(&self.tokens).remove(&environment.id);
                } else {
                    crate::lock_or_poisoned(&self.credential_blobs).remove(&environment.id);
                }
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
        if environment.auth_method == AuthMethod::OauthClientCredentials
            && let Some(access) = self.cached_access_token(&environment.id)
        {
            return Ok(vec![("Authorization".into(), format!("Bearer {access}"))]);
        }
        let blob = self.credential_blob(environment, credentials)?;
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

    /// One store read per Environment until a 401 drops the entry: see the
    /// field docs for the rotation contract.
    fn credential_blob(
        &self,
        environment: &EnvironmentConfig,
        credentials: &dyn CredentialStore,
    ) -> anyhow::Result<String> {
        if let Some(blob) = crate::lock_or_poisoned(&self.credential_blobs)
            .get(&environment.id)
            .cloned()
        {
            return Ok(blob);
        }
        let blob = credentials
            .get(&environment.id)?
            .ok_or_else(|| anyhow!("no credential for environment {}", environment.id))?;
        crate::lock_or_poisoned(&self.credential_blobs)
            .insert(environment.id.clone(), blob.clone());
        Ok(blob)
    }

    fn cached_access_token(&self, environment_id: &str) -> Option<String> {
        let cache = crate::lock_or_poisoned(&self.tokens);
        cache
            .get(environment_id)
            .filter(|cached| self.clock.now() < cached.valid_until)
            .map(|cached| cached.access_token.clone())
    }

    fn oauth_access(&self, environment: &EnvironmentConfig, blob: &str) -> anyhow::Result<String> {
        if let Some(access) = self.cached_access_token(&environment.id) {
            return Ok(access);
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
        let expires_in = grant
            .expires_in
            .unwrap_or(1800)
            .clamp(MIN_TOKEN_TTL_SECS, MAX_TOKEN_TTL_SECS)
            .saturating_sub(TOKEN_TTL_SKEW_SECS)
            .max(MIN_TOKEN_TTL_SECS / 2);
        let valid_until = self
            .clock
            .now()
            .checked_add(Duration::from_secs(expires_in))
            .unwrap_or_else(|| self.clock.now());
        crate::lock_or_poisoned(&self.tokens).insert(
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
                    .sleep(retry_after_delay(&response, self.clock.now()).min(MAX_TICK_429_SLEEP));
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

pub(crate) fn basic_authorization(username: &str, password: &str) -> String {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {encoded}")
}

fn join_url(instance_url: &str, path: &str) -> String {
    format!("{}{}", instance_url.trim_end_matches('/'), path)
}

/// Count-only Aggregate API body: `{ "result": { "stats": { "count": "N" } } }`.
/// `count` is a string in the documented envelope; a JSON number is also accepted.
pub fn parse_aggregate_count(body: &[u8]) -> anyhow::Result<u64> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let count = value
        .pointer("/result/stats/count")
        .ok_or_else(|| anyhow!("aggregate response missing result.stats.count"))?;
    match count {
        serde_json::Value::String(text) => text
            .parse()
            .with_context(|| format!("aggregate count {text:?}")),
        serde_json::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| anyhow!("aggregate count {number} is not a u64")),
        other => Err(anyhow!("aggregate count {other} is not a string or number")),
    }
}

pub fn fetch_aggregate_count(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
) -> anyhow::Result<u64> {
    let response = client.request(environment, credentials, "GET", path, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    parse_aggregate_count(response.body.as_bytes())
}

/// Average-only Aggregate API body: `{ "result": { "stats": { "avg": {
/// "response_time": "123.5" } } } }`. The average is a string in the
/// documented envelope; a JSON number is also accepted. `field` is the
/// `sysparm_avg_fields` name the caller asked for.
pub fn parse_aggregate_avg(body: &[u8], field: &str) -> anyhow::Result<f64> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let average = value
        .pointer(&format!("/result/stats/avg/{field}"))
        .ok_or_else(|| anyhow!("aggregate response missing result.stats.avg.{field}"))?;
    match average {
        serde_json::Value::String(text) => text
            .parse()
            .with_context(|| format!("aggregate avg {text:?}")),
        serde_json::Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| anyhow!("aggregate avg {number} is not a number")),
        other => Err(anyhow!("aggregate avg {other} is not a string or number")),
    }
}

pub fn fetch_aggregate_avg(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
    field: &str,
) -> anyhow::Result<f64> {
    let response = client.request(environment, credentials, "GET", path, None)?;
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    parse_aggregate_avg(response.body.as_bytes(), field)
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
    let delay = match value.trim().parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => http_date_delay(value, now).unwrap_or(DEFAULT_RETRY_AFTER),
    };
    delay.min(MAX_RETRY_AFTER)
}

fn http_date_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let parsed = httpdate::parse_http_date(value).ok()?;
    // A past-dated (stale / skewed-clock / cached-429) Retry-After must fall
    // back to the default backoff, not sleep zero and burn a retry.
    parsed
        .duration_since(now)
        .ok()
        .filter(|delay| !delay.is_zero())
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        // Platform verifier = macOS Keychain roots, so Environments behind
        // corporate TLS interception / private CAs work like the WS client.
        // `redirect_auth_headers` stays at ureq's default `Never`.
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

/// ureq 3 parses URLs as strict RFC 3986 `http::Uri`s, so the ServiceNow
/// encoded-query characters the Signals use verbatim (`^`, `<`, `>`, space)
/// must be percent-encoded at the wire; unreserved, reserved and `%` pass through.
fn percent_encode_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for byte in url.bytes() {
        let keep = byte.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&byte);
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl HttpTransport for UreqTransport {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let url = percent_encode_url(&request.url);
        let response = match request.method.as_str() {
            "GET" => {
                let mut call = self.agent.get(&url);
                for (name, value) in &request.headers {
                    call = call.header(name.as_str(), value.as_str());
                }
                call.call()
            }
            "POST" => {
                let mut call = self.agent.post(&url);
                for (name, value) in &request.headers {
                    call = call.header(name.as_str(), value.as_str());
                }
                call.send(request.body.as_deref().unwrap_or(""))
            }
            other => return Err(anyhow!("unsupported HTTP method {other}")),
        };
        read_ureq_response(response?)
    }
}

fn read_ureq_response(
    mut response: ureq::http::Response<ureq::Body>,
) -> anyhow::Result<HttpResponse> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
        })
        .collect();
    let body = response
        .body_mut()
        .read_to_string()
        .context("reading HTTP body")?;
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_url_encodes_servicenow_query_operators_only() {
        assert_eq!(
            percent_encode_url(
                "https://x.service-now.com/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=0^next_action<javascript:gs.minutesAgo(5) 1&sysparm_fields=a,b"
            ),
            "https://x.service-now.com/api/now/stats/sys_trigger?sysparm_count=true&sysparm_query=state=0%5Enext_action%3Cjavascript:gs.minutesAgo(5)%201&sysparm_fields=a,b"
        );
        assert_eq!(percent_encode_url("https://x/a?b=%5E"), "https://x/a?b=%5E");
    }
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::config::{MemoryCredentialStore, Platform, Thresholds};

    #[test]
    fn parse_aggregate_count_reads_stats_count_string() {
        let zero = include_bytes!("../tests/fixtures/jobs/count_0.json");
        let two = include_bytes!("../tests/fixtures/jobs/count_2.json");
        assert_eq!(parse_aggregate_count(zero).unwrap(), 0);
        assert_eq!(parse_aggregate_count(two).unwrap(), 2);
    }

    #[test]
    fn parse_aggregate_avg_reads_stats_avg_field_as_string_or_number() {
        let string = br#"{"result":{"stats":{"avg":{"response_time":"842.5"}}}}"#;
        assert_eq!(parse_aggregate_avg(string, "response_time").unwrap(), 842.5);
        let number = br#"{"result":{"stats":{"avg":{"response_time":118}}}}"#;
        assert_eq!(parse_aggregate_avg(number, "response_time").unwrap(), 118.0);
        let missing = br#"{"result":{"stats":{"count":"3"}}}"#;
        assert!(parse_aggregate_avg(missing, "response_time").is_err());
        let wrong_field = br#"{"result":{"stats":{"avg":{"sql_count":"4"}}}}"#;
        assert!(parse_aggregate_avg(wrong_field, "response_time").is_err());
    }

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

    /// Lets a test walk past a token's expiry without sleeping.
    struct AdvancingClock(Mutex<SystemTime>);

    impl AdvancingClock {
        fn new() -> Self {
            Self(Mutex::new(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            ))
        }

        fn advance(&self, by: Duration) {
            let mut now = self.0.lock().expect("clock");
            *now += by;
        }
    }

    impl Clock for AdvancingClock {
        fn now(&self) -> SystemTime {
            *self.0.lock().expect("clock")
        }

        fn sleep(&self, duration: Duration) {
            self.advance(duration);
        }
    }

    fn basic_env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "dev".into(),
            label: "Dev".into(),
            instance_url: "https://acme-dev.example.service-now.com".into(),
            auth_method: AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
            platform: Platform::Servicenow,
            thresholds: Thresholds::default(),
            expected_drift: Vec::new(),
        }
    }

    fn oauth_env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "prod".into(),
            label: "Production".into(),
            instance_url: "https://acme-prod.example.service-now.com".into(),
            auth_method: AuthMethod::OauthClientCredentials,
            sort_order: 0,
            clone_source: false,
            platform: Platform::Servicenow,
            thresholds: Thresholds::default(),
            expected_drift: Vec::new(),
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

    /// The retry budget is finite: a rate limit that outlasts it is handed to
    /// the caller as a 429, which `classify_availability_response` reads as
    /// unreachable. It is never an `Err` and never an endless retry loop.
    #[test]
    fn servicenow_http_gives_up_after_the_429_budget_and_returns_the_429() {
        let rate_limited = || HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "1".into())],
            body: r#"{"error":{"message":"Rate limit exceeded"}}"#.into(),
        };
        let transport = ScriptedTransport::new(
            (0..=MAX_429_RETRIES)
                .map(|_| rate_limited())
                .collect::<Vec<_>>(),
        );
        let clock = Arc::new(RecordingClock::default());
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, clock.clone());
        let response = client
            .request(
                &basic_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(
            clock.sleeps.lock().expect("sleeps").len(),
            usize::from(MAX_429_RETRIES)
        );
    }

    #[test]
    fn servicenow_http_caps_each_429_sleep_at_five_seconds() {
        let rate_limited = || HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "120".into())],
            body: String::new(),
        };
        let transport =
            ScriptedTransport::new((0..=MAX_429_RETRIES).map(|_| rate_limited()).collect());
        let clock = Arc::new(RecordingClock::default());
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, clock.clone());
        let response = client
            .request(
                &basic_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap();
        assert_eq!(response.status, 429);
        // Two retries, five seconds each — never the full two minutes, so
        // one rate-limited Signal cannot park its Environment's tick.
        assert_eq!(
            *clock.sleeps.lock().expect("sleeps"),
            vec![Duration::from_secs(5); usize::from(MAX_429_RETRIES)]
        );
    }

    #[test]
    fn servicenow_http_retries_on_429_http_date() {
        let transport = ScriptedTransport::new(vec![
            HttpResponse {
                status: 429,
                headers: vec![("Retry-After".into(), "Tue, 14 Nov 2023 22:13:30 GMT".into())],
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
                .request(
                    &basic_env(),
                    &credentials,
                    "GET",
                    "/api/now/table/sys_properties",
                    None
                )
                .unwrap()
                .status,
            200
        );
        // The date parses to a 10 s delay, but the tick cap holds the actual
        // sleep at five seconds.
        assert_eq!(*clock.sleeps.lock().expect("sleeps"), [MAX_TICK_429_SLEEP]);
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
            .request(
                &basic_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
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

    #[derive(Default)]
    struct CountingCredentialStore {
        inner: MemoryCredentialStore,
        gets: AtomicUsize,
    }

    impl CredentialStore for CountingCredentialStore {
        fn get(&self, environment_id: &str) -> anyhow::Result<Option<String>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.inner.get(environment_id)
        }

        fn set(&self, environment_id: &str, secret: &str) -> anyhow::Result<()> {
            self.inner.set(environment_id, secret)
        }

        fn delete(&self, environment_id: &str) -> anyhow::Result<()> {
            self.inner.delete(environment_id)
        }
    }

    #[test]
    fn servicenow_http_oauth_reads_keychain_once_while_token_is_cached() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            token_ok("tok-1"),
            ok_table(),
            ok_table(),
        ]));
        let credentials = CountingCredentialStore::default();
        credentials
            .inner
            .insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let path = "/api/now/table/sys_properties";
        for _ in 0..2 {
            assert_eq!(
                client
                    .request(&oauth_env(), &credentials, "GET", path, None)
                    .unwrap()
                    .status,
                200
            );
        }
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
        assert_eq!(credentials.gets.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn servicenow_http_basic_reads_store_once_per_environment() {
        let transport = Arc::new(ScriptedTransport::new(vec![ok_table(), ok_table()]));
        let credentials = CountingCredentialStore::default();
        credentials
            .inner
            .insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let path = "/api/now/table/sys_properties";
        for _ in 0..2 {
            assert_eq!(
                client
                    .request(&basic_env(), &credentials, "GET", path, None)
                    .unwrap()
                    .status,
                200
            );
        }
        assert_eq!(credentials.gets.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn servicenow_http_basic_refreshes_once_on_401() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            HttpResponse {
                status: 401,
                headers: vec![("content-type".into(), "application/json".into())],
                body: include_str!("../tests/fixtures/availability/401.json").into(),
            },
            ok_table(),
        ]));
        let credentials = CountingCredentialStore::default();
        credentials
            .inner
            .insert("dev", r#"{"username":"reader","password":"secret"}"#);
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
        // One stale read, one retry after the 401 drops the memo.
        assert_eq!(credentials.gets.load(Ordering::SeqCst), 2);
        assert_eq!(transport.requests().len(), 2);
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

    #[test]
    fn servicenow_http_caps_huge_retry_after_seconds() {
        let transport = ScriptedTransport::new(vec![
            HttpResponse {
                status: 429,
                headers: vec![("Retry-After".into(), "86400".into())],
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
                .request(
                    &basic_env(),
                    &credentials,
                    "GET",
                    "/api/now/table/sys_properties",
                    None
                )
                .unwrap()
                .status,
            200
        );
        // The parse-level cap is 30 s, but a tick thread never sleeps more
        // than the tick cap: long backoffs surface as transient 429s.
        assert_eq!(*clock.sleeps.lock().expect("sleeps"), [MAX_TICK_429_SLEEP]);
    }

    #[test]
    fn servicenow_http_caps_far_future_retry_after_date() {
        let transport = ScriptedTransport::new(vec![
            HttpResponse {
                status: 429,
                headers: vec![("Retry-After".into(), "Fri, 11 Nov 2033 22:13:20 GMT".into())],
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
                .request(
                    &basic_env(),
                    &credentials,
                    "GET",
                    "/api/now/table/sys_properties",
                    None
                )
                .unwrap()
                .status,
            200
        );
        // The parse-level cap is 30 s, but a tick thread never sleeps more
        // than the tick cap: long backoffs surface as transient 429s.
        assert_eq!(*clock.sleeps.lock().expect("sleeps"), [MAX_TICK_429_SLEEP]);
    }

    #[test]
    fn servicenow_http_oauth_huge_expires_in_does_not_panic() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: r#"{"access_token":"t","expires_in":18446744073709551615}"#.into(),
            },
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
        for _ in 0..2 {
            assert_eq!(
                client
                    .request(&oauth_env(), &credentials, "GET", path, None)
                    .unwrap()
                    .status,
                200
            );
        }
        assert_eq!(
            transport
                .requests()
                .iter()
                .filter(|request| request.url.contains("oauth_token.do"))
                .count(),
            1
        );
    }

    fn token_with_expiry(expires_in: u64) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: format!(r#"{{"access_token":"tok","expires_in":{expires_in}}}"#),
        }
    }

    /// Two requests against a client scripted with one token response; returns
    /// how many times the token endpoint was hit.
    fn token_fetches_over_two_requests(token: HttpResponse) -> usize {
        let transport = Arc::new(ScriptedTransport::new(vec![token, ok_table(), ok_table()]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let path = "/api/now/table/sys_properties";
        for _ in 0..2 {
            assert_eq!(
                client
                    .request(&oauth_env(), &credentials, "GET", path, None)
                    .unwrap()
                    .status,
                200
            );
        }
        transport
            .requests()
            .iter()
            .filter(|request| request.url.contains("oauth_token.do"))
            .count()
    }

    #[test]
    fn servicenow_http_oauth_tiny_expires_in_is_floored() {
        assert_eq!(token_fetches_over_two_requests(token_with_expiry(0)), 1);
    }

    #[test]
    fn servicenow_http_oauth_normal_expires_in_is_unchanged_in_spirit() {
        assert_eq!(token_fetches_over_two_requests(token_with_expiry(1800)), 1);
    }

    #[test]
    fn servicenow_http_oauth_expiry_keeps_a_skew_margin() {
        let transport =
            ScriptedTransport::new(vec![token_with_expiry(MIN_TOKEN_TTL_SECS + 1), ok_table()]);
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let clock = Arc::new(AdvancingClock::new());
        let client = ServiceNowClient::new(transport, clock.clone());
        client
            .request(
                &oauth_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap();
        assert_eq!(
            client.cached_access_token("prod").as_deref(),
            Some("tok"),
            "a just-above-the-floor grant must not be born expired"
        );
        // The server said 61 s (MIN_TOKEN_TTL_SECS + 1); the 30 s skew
        // makes it 31 s. Step 32 s: past the skewed expiry but before the
        // server's, so only the margin can retire it. Spelled out literally —
        // deriving the advance from the constants would keep this green if
        // the skew itself drifted.
        clock.advance(Duration::from_secs(32));
        assert_eq!(
            client.cached_access_token("prod"),
            None,
            "the skew margin must retire the token before the server does"
        );
    }

    struct SharedTransport(Arc<ScriptedTransport>);

    impl HttpTransport for SharedTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            self.0.execute(request)
        }
    }
    #[test]
    fn servicenow_http_transport_error_propagates() {
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", r#"{"username":"reader","password":"secret"}"#);
        let client =
            ServiceNowClient::new(ScriptedTransport::new(vec![]), RecordingClock::default());
        let error = client
            .request(
                &basic_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("no scripted response left"), "{error}");
    }

    #[test]
    fn servicenow_http_missing_credential_is_an_error() {
        let client =
            ServiceNowClient::new(ScriptedTransport::new(vec![]), RecordingClock::default());
        let error = client
            .request(
                &basic_env(),
                &MemoryCredentialStore::default(),
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap_err()
            .to_string();
        assert_eq!(error, "no credential for environment dev");
    }

    #[test]
    fn servicenow_http_oauth_token_endpoint_non_200_is_an_error() {
        let transport = Arc::new(ScriptedTransport::new(vec![HttpResponse {
            status: 401,
            headers: vec![],
            body: "{}".into(),
        }]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        let error = client
            .request(
                &oauth_env(),
                &credentials,
                "GET",
                "/api/now/table/sys_properties",
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("oauth_token.do returned HTTP 401"),
            "{error}"
        );
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.contains("oauth_token.do"));
    }

    #[test]
    fn servicenow_http_oauth_token_body_not_json_is_an_error() {
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(
            ScriptedTransport::new(vec![HttpResponse {
                status: 200,
                headers: vec![],
                body: "<html>".into(),
            }]),
            RecordingClock::default(),
        );
        let error = format!(
            "{:#}",
            client
                .request(
                    &oauth_env(),
                    &credentials,
                    "GET",
                    "/api/now/table/sys_properties",
                    None,
                )
                .unwrap_err()
        );
        assert!(error.contains("oauth token JSON"), "{error}");
    }

    #[test]
    fn servicenow_http_oauth_refetches_after_expiry() {
        let short_token = |token: &str| HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: format!(r#"{{"access_token":"{token}","expires_in":60}}"#),
        };
        let transport = Arc::new(ScriptedTransport::new(vec![
            short_token("tok-1"),
            ok_table(),
            short_token("tok-2"),
            ok_table(),
        ]));
        let clock = Arc::new(AdvancingClock::new());
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"secret"}"#);
        let client = ServiceNowClient::new(SharedTransport(transport.clone()), clock.clone());
        let path = "/api/now/table/sys_properties";
        let call = || {
            client
                .request(&oauth_env(), &credentials, "GET", path, None)
                .unwrap()
                .status
        };
        assert_eq!(call(), 200);
        clock.advance(Duration::from_secs(61));
        assert_eq!(call(), 200);
        let requests = transport.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.contains("oauth_token.do"))
                .count(),
            2
        );
        let last = requests.last().expect("data request");
        assert_eq!(
            last.headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.as_str()),
            Some("Bearer tok-2")
        );
    }

    #[test]
    fn servicenow_http_oauth_secret_is_form_urlencoded() {
        let transport = Arc::new(ScriptedTransport::new(vec![token_ok("t"), ok_table()]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"client_id":"id","client_secret":"a&b=c d+e%"}"#);
        let client = ServiceNowClient::new(
            SharedTransport(transport.clone()),
            RecordingClock::default(),
        );
        assert_eq!(
            client
                .request(
                    &oauth_env(),
                    &credentials,
                    "GET",
                    "/api/now/table/sys_properties",
                    None
                )
                .unwrap()
                .status,
            200
        );
        let body = transport.requests()[0].body.clone().expect("token body");
        let encoded = body
            .split('&')
            .find_map(|parameter| parameter.strip_prefix("client_secret="))
            .expect("client_secret parameter");
        assert_eq!(encoded, "a%26b%3Dc%20d%2Be%25");
    }

    #[test]
    fn urlencode_keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(urlencode("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(urlencode(" /?#"), "%20%2F%3F%23");
    }

    #[test]
    fn limited_transport_caps_in_flight_requests_per_host() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Sleeper {
            current: Arc<AtomicUsize>,
            high_water: Arc<AtomicUsize>,
        }

        impl HttpTransport for Sleeper {
            fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                let at = self.current.fetch_add(1, Ordering::SeqCst) + 1;
                self.high_water.fetch_max(at, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                self.current.fetch_sub(1, Ordering::SeqCst);
                Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: request.url.clone(),
                })
            }
        }

        let current = Arc::new(AtomicUsize::new(0));
        let high_water = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(LimitedTransport::new(Sleeper {
            current: current.clone(),
            high_water: high_water.clone(),
        }));
        // Eight threads on one host: the per-host cap of 2 must hold, and
        // every request still completes exactly once.
        std::thread::scope(|scope| {
            for index in 0..8 {
                let transport = transport.clone();
                scope.spawn(move || {
                    transport
                        .execute(&HttpRequest {
                            method: "GET".into(),
                            url: format!("https://slow.example.com/{index}"),
                            headers: Vec::new(),
                            body: None,
                        })
                        .unwrap()
                });
            }
        });
        assert_eq!(current.load(Ordering::SeqCst), 0);
        assert_eq!(high_water.load(Ordering::SeqCst), 2);
    }
}
