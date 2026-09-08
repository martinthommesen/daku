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
use std::time::Duration;

use crate::persistence::StateStore;

/// Backfill bound: events older than this are marked seen, never POSTed.
const BACKFILL_SECS: i64 = 24 * 3600;

/// True for URLs the relay may POST to.
pub fn webhook_url_allowed(url: &str) -> bool {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty() && !rest.contains([' ', '\t', '\n']);
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        let host = if let Some(bracketed) = authority.strip_prefix('[') {
            bracketed.split(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        return matches!(host, "127.0.0.1" | "localhost" | "::1");
    }
    false
}

pub struct WebhookRelay {
    last_sent: Mutex<HashMap<String, i64>>,
    last_refused: Mutex<Option<String>>,
}

impl Default for WebhookRelay {
    fn default() -> Self {
        Self {
            last_sent: Mutex::new(HashMap::new()),
            last_refused: Mutex::new(None),
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
        let Some(url) = Self::configured_url() else {
            return;
        };
        if !webhook_url_allowed(&url) {
            let mut refused = self.last_refused.lock().expect("webhook refusal");
            if refused.as_deref() != Some(url.as_str()) {
                eprintln!("daku webhook refused (loopback http or any https only): {url}");
                *refused = Some(url);
            }
            return;
        }
        if let Err(error) = self.relay_to(store, &url, &post_json) {
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
            let mark = last_sent.get(&environment_id).copied().unwrap_or(0);
            let mut events =
                crate::persistence::load_health_events(&connection, &environment_id, 500)?;
            events.retain(|event| event.observed_at > mark);
            events.sort_by_key(|event| event.observed_at);
            for event in events {
                if now.saturating_sub(event.observed_at) > BACKFILL_SECS {
                    last_sent.insert(environment_id.clone(), event.observed_at);
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
                post(url, &body)?;
                last_sent.insert(environment_id.clone(), event.observed_at);
            }
        }
        Ok(())
    }
}

fn post_json(url: &str, body: &str) -> anyhow::Result<()> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .post(url)
        .header("Content-Type", "application/json")
        .send(body)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let text = response.body_mut().read_to_string().unwrap_or_default();
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
    fn relay_stops_at_the_first_failure_and_retries_next_tick() {
        let now = crate::collector::unix_now();
        let db = seed(now);
        let relay = WebhookRelay::new();
        let calls = Arc::new(StdMutex::new(0usize));
        let probe = calls.clone();
        let failing = move |_url: &str, _body: &str| {
            *probe.lock().expect("calls") += 1;
            anyhow::bail!("forwarder down")
        };
        assert!(
            relay
                .relay_to(&db.store(), "http://127.0.0.1:9/hook", &failing)
                .is_err()
        );
        assert_eq!(*calls.lock().expect("calls"), 1);
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
        assert_eq!(posted.lock().expect("posted").len(), 2);
    }

    #[test]
    fn relay_posts_to_a_loopback_receiver() {
        use std::io::Read as _;
        use std::net::TcpListener;
        let now = crate::collector::unix_now();
        let db = seed(now);
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
    fn settings_webhook_url_defaults_off_and_round_trips() {
        assert_eq!(DaemonSettings::default().webhook_url, None);
        let parsed: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs":60,"webhook_url":"https://x/hook"}"#)
                .unwrap();
        assert_eq!(parsed.webhook_url.as_deref(), Some("https://x/hook"));
    }
}
