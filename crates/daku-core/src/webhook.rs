//! Health-event webhook relay (opt-in, loopback-safe).
//!
//! When `webhook_url` is set in `settings.json`, the daemon POSTs every new
//! health/build event as JSON after each poll tick, so a local forwarder can
//! carry daku attention into Slack, Teams, or anything else with a webhook.
//! daku owns no secrets for those services and never will: the URL is the
//! whole integration.
//!
//! URL policy: `http` only to loopback (`127.0.0.1`, `localhost`, `::1`);
//! `https` anywhere (the operator's own forwarder). Anything else is refused
//! before any byte is sent — health events carry build strings and
//! Environment ids, which stay on this machine or behind TLS.
//!
//! Delivery is at-least-once per daemon run: successfully POSTed events
//! advance a per-Environment high-water mark kept in memory; a failed POST
//! retries on later ticks. Events older than 24 h are never backfilled, so
//! a webhook that was down for weeks does not burst hundreds of stale
//! POSTs on return.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::persistence::StateStore;

/// Backfill bound: events older than this are marked seen, never POSTed.
const BACKFILL_SECS: i64 = 24 * 3600;

/// Whether one health/build event should be relayed: newer than the
/// per-Environment high-water mark and inside the backfill window.
/// Pure decision — the test surface for the relay's "which events" rule.
///
/// Note the deliberate bypass, shared with `notifications::should_notify`:
/// the daemon fan-out posts health **and** build and ignores Mute,
/// quiet hours, per-Signal switches, and the desktop master switch — the
/// daemon has no desktop prefs (ADR-0009: Mute stays desktop-side, the
/// daemon keeps collecting). The desktop notification path respects all
/// gates; this path only dedupes (mark) and bounds (backfill).
pub fn should_relay_event(observed_at: i64, mark: i64, now: i64) -> bool {
    observed_at > mark && now.saturating_sub(observed_at) <= BACKFILL_SECS
}

/// True for URLs the relay may POST to.
pub fn webhook_url_allowed(url: &str) -> bool {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("https://") {
        if rest.is_empty() || rest.contains([' ', '\t', '\n']) {
            return false;
        }
        let authority = rest.split('/').next().unwrap_or("");
        // Userinfo (`user:secret@host`) must never be accepted: the
        // credential would ride the URL into logs, bundles, and the
        // request line. Callers must use a credential-free URL.
        if authority.contains('@') || authority.is_empty() {
            return false;
        }
        return true;
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        if authority.contains('@') {
            return false;
        }
        let host = if let Some(bracketed) = authority.strip_prefix('[') {
            bracketed.split(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        return matches!(host, "127.0.0.1" | "localhost" | "::1");
    }
    false
}

/// Advisory warning for allowed https URLs pointing at intranet, link-local,
/// or cloud-metadata-like hosts. The relay still sends (intranet forwarders
/// are legitimate); the warning surfaces the foot-gun in daemon logs so a
/// mistaken or socially engineered URL does not exfiltrate health metadata
/// silently. Returns the warning text when review is warranted.
pub fn webhook_url_needs_review(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("https://")?;
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split(']')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    } else {
        authority
            .rsplit('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    let private = host == "localhost"
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || host == "169.254.169.254"
        || host.starts_with("metadata.")
        || host == "metadata.google.internal"
        || host.starts_with("fc")
        || host.starts_with("fd")
        || host.starts_with("fe80")
        || (host
            .strip_prefix("172.")
            .and_then(|rest| rest.split('.').next()?.parse::<u8>().ok())
            .is_some_and(|second| (16..=31).contains(&second)));
    if private {
        Some(format!(
            "daku webhook posts health metadata to an intranet or metadata-like https target ({}); confirm the URL is intentional",
            redacted_url(url),
        ))
    } else {
        None
    }
}

/// What a refused webhook URL prints: scheme plus host only. Forwarder URLs
/// routinely carry tokens in the query string — and some integrations put
/// the secret in the path itself — so only the host survives into daemon
/// stderr, the most-copied log in the project. Pinned by the redaction
/// audit below.
pub fn refusal_line(url: &str) -> String {
    format!(
        "daku webhook refused (loopback http or any https only): {}",
        redacted_url(url)
    )
}

/// Scheme plus authority (`https://host`) with path, query, and fragment
/// dropped: everything after the host is secret-bearing until proven
/// otherwise. Userinfo is stripped even for rejected input so a
/// `user:secret@host` canary never survives into a log or bundle.
pub fn redacted_url(url: &str) -> String {
    let trimmed = url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(pair) => pair,
        None => return "<unparseable>".into(),
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return "<unparseable>".into();
    }
    // Strip userinfo (`user:secret@`) and fragment/query remnants.
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    if host.is_empty() {
        return "<unparseable>".into();
    }
    format!("{scheme}://{host}")
}

pub struct WebhookRelay {
    last_sent: Mutex<HashMap<String, i64>>,
    last_refused: Mutex<Option<String>>,
    last_warned: Mutex<Option<String>>,
    in_flight: AtomicBool,
}

impl Default for WebhookRelay {
    fn default() -> Self {
        Self {
            last_sent: Mutex::new(HashMap::new()),
            last_refused: Mutex::new(None),
            last_warned: Mutex::new(None),
            in_flight: AtomicBool::new(false),
        }
    }
}

impl WebhookRelay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the live `settings.json` for a configured URL: trimmed,
    /// non-empty. Allowed-ness is checked by `relay_tick`, not here.
    pub fn configured_url() -> Option<String> {
        let settings = crate::DaemonSettingsStore::open(crate::DaemonSettingsStore::default_path())
            .map(|handle| handle.get())
            .unwrap_or_default();
        settings
            .webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
    }

    /// POSTs every health event newer than the per-Environment mark.
    /// Synchronous; the caller (the collector's publish closure) runs this on
    /// a fire-and-forget thread so a slow forwarder never stalls polling.
    /// A disallowed URL logs once per distinct value, never sends.
    pub fn relay_tick(&self, store: &StateStore) {
        self.relay_tick_with(store, Self::configured_url(), &post_json);
    }

    /// `relay_tick` with the settings read and the sender injected, so tests
    /// pin the wiring without ambient `DAKU_HOME` or the network.
    fn relay_tick_with(
        &self,
        store: &StateStore,
        url: Option<String>,
        post: &dyn Fn(&str, &str) -> anyhow::Result<()>,
    ) {
        let Some(url) = url else { return };
        if !webhook_url_allowed(&url) {
            let mut refused = self.last_refused.lock().expect("webhook refusal");
            if refused.as_deref() != Some(url.as_str()) {
                eprintln!("{}", refusal_line(&url));
                *refused = Some(url);
            }
            return;
        }
        // Serialize overlapping tick runs; a slow endpoint coalesces instead
        // of piling a thread per tick.
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if let Some(warning) = webhook_url_needs_review(&url) {
            let mut warned = self.last_warned.lock().expect("webhook warning");
            if warned.as_deref() != Some(warning.as_str()) {
                eprintln!("{warning}");
                *warned = Some(warning);
            }
        }
        let result = self.relay_to(store, &url, post);
        self.in_flight.store(false, Ordering::Release);
        if let Err(error) = result {
            eprintln!("daku webhook post failed: {error:#}");
        }
    }

    fn relay_to(
        &self,
        store: &StateStore,
        url: &str,
        post: &dyn Fn(&str, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let connection = store.open()?;
        crate::persistence::ensure_webhook_tables(&connection)?;
        let now = crate::collector::unix_now();
        let mut last_sent = self.last_sent.lock().expect("webhook marks");
        // Environments are whatever the snapshots mention; the relay follows
        // data, not config, so a removed Environment's tail still flushes.
        let mut environments: Vec<String> = connection
            .prepare("SELECT DISTINCT environment_id FROM signal_snapshots")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        environments.sort();
        environments.dedup();
        for environment_id in environments {
            // Persisted cursor wins over memory so restarts resume; a brand
            // new Environment seeds at now to avoid an initial burst.
            let persisted = crate::persistence::load_webhook_cursor(&connection, &environment_id)?;
            let mark = match (last_sent.get(&environment_id).copied(), persisted) {
                (Some(memory), Some(stored)) => memory.max(stored),
                (Some(memory), None) => memory,
                (None, Some(stored)) => stored,
                (None, None) => {
                    crate::persistence::store_webhook_cursor(&connection, &environment_id, now)?;
                    last_sent.insert(environment_id.clone(), now);
                    continue;
                }
            };
            let mut events =
                crate::persistence::load_health_events(&connection, &environment_id, 500)?;
            events.retain(|event| event.observed_at > mark);
            // Stable total order: timestamp alone collapses same-second
            // health and build events, so a crash between two posts can
            // skip the sibling. `(observed_at, kind)` is unambiguous.
            events.sort_by(|a, b| (a.observed_at, &a.kind).cmp(&(b.observed_at, &b.kind)));
            for event in events {
                if !should_relay_event(event.observed_at, mark, now) {
                    last_sent.insert(environment_id.clone(), event.observed_at);
                    crate::persistence::store_webhook_cursor(
                        &connection,
                        &environment_id,
                        event.observed_at,
                    )?;
                    continue;
                }
                let body = serde_json::json!({
                    "environment_id": event.environment_id,
                    "observed_at": event.observed_at,
                    "kind": event.kind,
                    "from_health": event.from_health,
                    "to_health": event.to_health,
                    "build": event.build,
                })
                .to_string();
                // Bounded retry: one immediate second attempt for transient
                // failures before dead-lettering. Poison endpoints still
                // advance without head-of-line blocking.
                if post(url, &body).is_err() && post(url, &body).is_err() {
                    let error = "webhook post failed after 1 retry";
                    // Poison head must not starve newer events: dead-letter it
                    // and advance past it.
                    let _ = crate::persistence::record_webhook_dead_letter(
                        &connection,
                        &environment_id,
                        event.observed_at,
                        &event.kind,
                        error,
                    );
                    eprintln!(
                        "daku webhook skipped poison event for {}: {error}",
                        event.environment_id
                    );
                    last_sent.insert(environment_id.clone(), event.observed_at);
                    crate::persistence::store_webhook_cursor(
                        &connection,
                        &environment_id,
                        event.observed_at,
                    )?;
                    continue;
                }
                last_sent.insert(environment_id.clone(), event.observed_at);
                crate::persistence::store_webhook_cursor(
                    &connection,
                    &environment_id,
                    event.observed_at,
                )?;
            }
        }
        Ok(())
    }
}

fn post_json(url: &str, body: &str) -> anyhow::Result<()> {
    // One process-wide agent: a fresh pool + TLS setup per event POST would
    // pay a handshake per health event on every tick.
    static AGENT: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(10)))
                // Non-2xx is a reportable relay failure with its body, not a
                // transport error: the manual status check below owns it.
                .http_status_as_error(false)
                .build(),
        )
    });
    let mut response = AGENT
        .post(url)
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|error| anyhow::anyhow!("webhook post failed: {error}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // Bound the error body before truncating for the message: a
        // forwarder must not be able to amplify one failure into unbounded
        // allocation.
        let text = crate::servicenow::read_bounded_body(response.body_mut(), 64 * 1024)
            .unwrap_or_default();
        anyhow::bail!(
            "webhook returned HTTP {status}: {}",
            text.chars().take(160).collect::<String>()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
    use crate::persistence::{self, HealthEvent};
    use crate::test_support::TempDb;
    use daku_protocol::settings::DaemonSettings;
    use daku_protocol::{Reachability, SignalState};
    use std::sync::{Arc, Mutex as StdMutex};

    #[test]
    fn should_relay_event_dedupes_and_bounds_backfill() {
        assert!(should_relay_event(101, 100, 200));
        assert!(!should_relay_event(100, 100, 200));
        assert!(!should_relay_event(99, 100, 200));
        assert!(!should_relay_event(0, -1, BACKFILL_SECS + 1));
        assert!(should_relay_event(1, 0, 1 + BACKFILL_SECS));
    }

    #[test]
    fn webhook_url_intranet_targets_warn_without_blocking() {
        for url in [
            "https://192.168.1.10/hook",
            "https://10.0.0.5/hook",
            "https://169.254.169.254/hook",
            "https://metadata.google.internal/hook",
        ] {
            assert!(webhook_url_allowed(url), "{url}");
            assert!(webhook_url_needs_review(url).is_some(), "{url}");
        }
        assert!(webhook_url_needs_review("https://hooks.example.com/hook").is_none());
        assert!(webhook_url_needs_review("http://127.0.0.1:9000/hook").is_none());
    }

    #[test]
    fn webhook_url_policy_is_loopback_http_or_any_https() {
        for url in [
            "http://127.0.0.1:9000/hook",
            "http://127.0.0.1/hook",
            "http://localhost:9000/hook",
            "http://[::1]:9000/hook",
            "https://hooks.slack.com/services/abc",
            "https://example.com/hook",
        ] {
            assert!(webhook_url_allowed(url), "{url}");
        }
        for url in [
            "",
            "ftp://x/hook",
            "http://192.168.1.10/hook",
            "http://10.0.0.5/hook",
            "http://example.com/hook",
            "http://127.0.0.1.evil.com/hook",
            "https://",
            "http://",
            "//127.0.0.1/hook",
        ] {
            assert!(!webhook_url_allowed(url), "{url}");
        }
    }

    fn seed(now: i64) -> TempDb {
        seed_at(
            now,
            &[
                (now - 3600, "health", "degraded"),
                (now - 60, "build", "degraded"),
            ],
        )
    }

    fn seed_at(now: i64, events: &[(i64, &str, &str)]) -> TempDb {
        let db = TempDb::new("webhook");
        let connection = db.store().open().unwrap();
        persist_availability_snapshot(
            &connection,
            "prod",
            &AvailabilityObservation {
                reachability: Reachability::Reachable,
                state: SignalState::Healthy,
                build: None,
                rtt_ms: 1,
                error: None,
            },
            now,
        )
        .unwrap();
        for (at, kind, to) in events {
            persistence::record_health_event(
                &connection,
                &HealthEvent {
                    environment_id: "prod".into(),
                    observed_at: *at,
                    kind: (*kind).into(),
                    from_health: Some("healthy".into()),
                    to_health: (*to).into(),
                    build: (*kind == "build").then(|| "glide-9".into()),
                    note: None,
                },
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn relay_posts_new_events_once_and_advances_marks() {
        let now = crate::collector::unix_now();
        let db = seed(now);
        // Simulate an upgraded host with backlog: cursor starts at zero.
        {
            let connection = db.store().open().unwrap();
            persistence::store_webhook_cursor(&connection, "prod", 0).unwrap();
        }
        let relay = WebhookRelay::new();
        let posted: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = posted.clone();
        let post = move |_url: &str, body: &str| {
            sink.lock().expect("posted").push(body.to_owned());
            Ok(())
        };
        relay
            .relay_to(&db.store(), "http://127.0.0.1:9/hook", &post)
            .unwrap();
        let bodies = posted.lock().expect("posted").clone();
        assert_eq!(bodies.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(first["environment_id"], "prod");
        assert_eq!(first["kind"], "health");
        assert_eq!(first["to_health"], "degraded");
        // Second tick posts nothing new.
        relay
            .relay_to(&db.store(), "http://127.0.0.1:9/hook", &post)
            .unwrap();
        assert_eq!(posted.lock().expect("posted").len(), 2);
    }

    #[test]
    fn relay_skips_stale_events_but_marks_them_seen() {
        let now = crate::collector::unix_now();
        let db = seed_at(now, &[(now - BACKFILL_SECS - 1, "health", "degraded")]);
        {
            let connection = db.store().open().unwrap();
            persistence::store_webhook_cursor(&connection, "prod", 0).unwrap();
        }
        let relay = WebhookRelay::new();
        let posted: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = posted.clone();
        relay
            .relay_to(
                &db.store(),
                "http://127.0.0.1:9/hook",
                &move |_url: &str, body: &str| {
                    sink.lock().expect("posted").push(body.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        assert!(posted.lock().expect("posted").is_empty());
    }

    #[test]
    fn relay_skips_poison_head_and_dead_letters_it() {
        let now = crate::collector::unix_now();
        let db = seed(now);
        {
            let connection = db.store().open().unwrap();
            persistence::store_webhook_cursor(&connection, "prod", 0).unwrap();
        }
        let relay = WebhookRelay::new();
        let calls = Arc::new(StdMutex::new(0usize));
        let probe = calls.clone();
        let failing = move |_url: &str, _body: &str| {
            *probe.lock().expect("calls") += 1;
            anyhow::bail!("forwarder down")
        };
        // Poison head no longer blocks: both events are attempted, skipped,
        // and dead-lettered instead of returning an error. Each event gets
        // one retry before it dead-letters.
        relay
            .relay_to(&db.store(), "http://127.0.0.1:9/hook", &failing)
            .unwrap();
        assert_eq!(*calls.lock().expect("calls"), 4);
        let connection = db.store().open().unwrap();
        assert_eq!(
            persistence::load_webhook_dead_letter_count(&connection, "prod").unwrap(),
            2
        );
        // A later tick with a healthy forwarder posts nothing new.
        let posted: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = posted.clone();
        relay
            .relay_to(
                &db.store(),
                "http://127.0.0.1:9/hook",
                &move |_url: &str, body: &str| {
                    sink.lock().expect("posted").push(body.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        assert!(posted.lock().expect("posted").is_empty());
    }

    #[test]
    fn relay_seeds_fresh_cursor_at_now_without_burst() {
        let now = crate::collector::unix_now();
        let db = seed(now);
        let relay = WebhookRelay::new();
        let posted: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = posted.clone();
        relay
            .relay_to(
                &db.store(),
                "http://127.0.0.1:9/hook",
                &|_url: &str, body: &str| {
                    sink.lock().expect("posted").push(body.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        assert!(posted.lock().expect("posted").is_empty());
        // Restart resumes from the persisted cursor, not from zero.
        let relay2 = WebhookRelay::new();
        let sink2 = posted.clone();
        relay2
            .relay_to(
                &db.store(),
                "http://127.0.0.1:9/hook",
                &|_url: &str, body: &str| {
                    sink2.lock().expect("posted").push(body.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        assert!(posted.lock().expect("posted").is_empty());
    }

    #[test]
    fn relay_posts_to_a_loopback_receiver() {
        use std::io::Read as _;
        use std::net::TcpListener;
        let now = crate::collector::unix_now();
        let db = seed(now);
        {
            let connection = db.store().open().unwrap();
            persistence::store_webhook_cursor(&connection, "prod", 0).unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(StdMutex::new(Vec::new()));
        let sink = received.clone();
        let server = std::thread::spawn(move || {
            // Two events are seeded; answer both POSTs, then stop.
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                use std::io::BufRead as _;
                let mut length = 0usize;
                let mut chunked = false;
                loop {
                    head.clear();
                    reader.read_line(&mut head).unwrap();
                    let line = head.trim_end().to_owned();
                    if line.is_empty() {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if let Some(value) = lower.strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                        chunked = true;
                    }
                }
                let body = if chunked {
                    let mut chunks = Vec::new();
                    loop {
                        head.clear();
                        reader.read_line(&mut head).unwrap();
                        let size =
                            usize::from_str_radix(head.trim_end().split(';').next().unwrap(), 16)
                                .unwrap_or(0);
                        if size == 0 {
                            break;
                        }
                        let mut chunk = vec![0u8; size];
                        reader.read_exact(&mut chunk).unwrap();
                        chunks.extend_from_slice(&chunk);
                        head.clear();
                        reader.read_line(&mut head).unwrap();
                    }
                    chunks
                } else {
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).unwrap();
                    body
                };
                sink.lock()
                    .expect("received")
                    .push(String::from_utf8(body).unwrap());
                let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                use std::io::Write as _;
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        let relay = WebhookRelay::new();
        relay
            .relay_to(
                &db.store(),
                &format!("http://127.0.0.1:{port}/hook"),
                &post_json,
            )
            .unwrap();
        server.join().unwrap();
        let bodies = received.lock().expect("received").clone();
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0].contains("\"kind\":\"health\""), "{}", bodies[0]);
    }

    #[test]
    fn relay_tick_with_unset_or_refused_url_never_posts() {
        let now = crate::collector::unix_now();
        let db = seed(now);
        let relay = WebhookRelay::new();
        let posted: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let post = |sink: Arc<StdMutex<Vec<String>>>| {
            move |_url: &str, body: &str| {
                sink.lock().expect("posted").push(body.to_owned());
                Ok(())
            }
        };
        // Unset webhook: nothing to do.
        relay.relay_tick_with(&db.store(), None, &post(posted.clone()));
        // Disallowed URL twice: refused (dedup logs once), never sent.
        relay.relay_tick_with(
            &db.store(),
            Some("http://192.168.1.10/hook".into()),
            &post(posted.clone()),
        );
        relay.relay_tick_with(
            &db.store(),
            Some("http://192.168.1.10/hook".into()),
            &post(posted.clone()),
        );
        assert!(posted.lock().expect("posted").is_empty());
        // Allowed URL with an injected sender: the tick wiring posts.
        // Pre-seed the cursor to zero so the seeded backlog relays.
        {
            let connection = db.store().open().unwrap();
            persistence::store_webhook_cursor(&connection, "prod", 0).unwrap();
        }
        relay.relay_tick_with(
            &db.store(),
            Some("http://127.0.0.1:9/hook".into()),
            &post(posted.clone()),
        );
        assert_eq!(posted.lock().expect("posted").len(), 2);
    }

    #[test]
    fn post_json_reports_non_2xx_without_panicking() {
        use std::io::Read as _;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            use std::io::BufRead as _;
            loop {
                head.clear();
                reader.read_line(&mut head).unwrap();
                if head.trim_end().is_empty() {
                    break;
                }
            }
            let mut body = vec![0u8; 7]; // len(r#"{"a":1}"#)
            reader.read_exact(&mut body).unwrap();
            let reply = "HTTP/1.1 500 Oops\r\nContent-Length: 5\r\nConnection: close\r\n\r\nsorry";
            use std::io::Write as _;
            stream.write_all(reply.as_bytes()).unwrap();
        });
        let error = post_json(&format!("http://127.0.0.1:{port}/hook"), r#"{"a":1}"#)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(error.contains("HTTP 500"), "{error}");
        assert!(error.contains("sorry"), "{error}");
    }

    #[test]
    fn settings_webhook_url_defaults_off_and_round_trips() {
        assert_eq!(DaemonSettings::default().webhook_url, None);
        let parsed: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs":60,"webhook_url":"https://x/hook"}"#)
                .unwrap();
        assert_eq!(parsed.webhook_url.as_deref(), Some("https://x/hook"));
    }

    #[test]
    fn refusal_line_never_carries_query_fragment_or_path() {
        let line = refusal_line("http://192.168.1.10/hook?token=CANARY-9#frag");
        assert_eq!(
            line, "daku webhook refused (loopback http or any https only): http://192.168.1.10",
            "{line}"
        );
        assert!(!line.contains("CANARY-9"), "refusals must not leak: {line}");
        assert!(!line.contains('#'), "{line}");
        // Path-secret forwarders: the path is secret-bearing too.
        let line = refusal_line("https://hooks.example.com/secret/PATH-SECRET-1?x=1");
        assert!(!line.contains("PATH-SECRET-1"), "{line}");
        assert!(line.ends_with("https://hooks.example.com"), "{line}");
        assert_eq!(redacted_url("not a url"), "<unparseable>");
    }

    #[test]
    fn webhook_userinfo_is_rejected_and_redacted() {
        for url in [
            "https://u:secret@example.invalid/hook",
            "https://user@example.invalid/hook",
            "http://u:secret@127.0.0.1:9000/hook",
        ] {
            assert!(!webhook_url_allowed(url), "{url}");
            let redacted = redacted_url(url);
            assert!(!redacted.contains("secret"), "{redacted}");
            assert!(!redacted.contains("u:"), "{redacted}");
            assert!(!redacted.contains('@'), "{redacted}");
            let line = refusal_line(url);
            assert!(!line.contains("secret"), "{line}");
            assert!(!line.contains('@'), "{line}");
        }
        assert_eq!(
            redacted_url("https://u:secret@example.invalid/hook"),
            "https://example.invalid"
        );
    }
}
