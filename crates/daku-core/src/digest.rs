//! Weekly digest: Markdown over local history for one Environment.
//!
//! Read-only over SQLite (snapshots + health events). No ServiceNow calls,
//! no notifications — the Operator runs `daku-daemon digest --env <id>
//! [--days 7]` and pastes the result into a status note or an agent chat.

use crate::config::EnvironmentConfig;
use crate::health::health_rollup;
use crate::persistence::StateStore;
use daku_protocol::{Reachability, SignalState};

/// Renders the digest. `days` bounds the transitions/builds window; the
/// signal list is always current state.
pub fn weekly_digest(
    store: &StateStore,
    environment: &EnvironmentConfig,
    now: i64,
    days: i64,
) -> anyhow::Result<String> {
    let connection = store.open()?;
    let snapshots = crate::persistence::load_all_signal_snapshots(&connection)?
        .into_iter()
        .filter(|snapshot| snapshot.environment_id == environment.id)
        .collect::<Vec<_>>();
    let votes: Vec<(&str, SignalState)> = snapshots
        .iter()
        .map(|snapshot| {
            (
                snapshot.signal_id.as_str(),
                SignalState::parse(&snapshot.state).unwrap_or(SignalState::Skipped),
            )
        })
        .collect();
    let reachability = snapshots
        .iter()
        .find(|snapshot| snapshot.signal_id == crate::availability::AVAILABILITY_SIGNAL_ID)
        .and_then(|snapshot| {
            serde_json::from_str::<serde_json::Value>(&snapshot.payload_json)
                .ok()?
                .get("reachability")?
                .as_str()
                .and_then(Reachability::parse)
        })
        .unwrap_or(Reachability::Reachable);
    let health = health_rollup(reachability, &votes);
    let cutoff = now.saturating_sub(days.saturating_mul(86_400));
    let mut events = crate::persistence::load_health_events(&connection, &environment.id, 500)?;
    events.retain(|event| event.observed_at >= cutoff);
    events.sort_by_key(|event| event.observed_at);

    let mut out = format!(
        "# Daku digest: {} ({})\n\nLast {} day{} · health {} · {}\n",
        environment.label,
        environment.id,
        days,
        if days == 1 { "" } else { "s" },
        health.as_str(),
        reachability.as_str(),
    );
    let transitions: Vec<&crate::persistence::HealthEvent> = events
        .iter()
        .filter(|event| event.kind == "health")
        .collect();
    out.push_str("\n## Health transitions\n");
    if transitions.is_empty() {
        out.push_str("none\n");
    } else {
        for event in transitions {
            out.push_str(&format!(
                "- {}: {} → {}\n",
                ago(now, event.observed_at),
                event.from_health.as_deref().unwrap_or("?"),
                event.to_health
            ));
        }
    }
    let builds: Vec<&crate::persistence::HealthEvent> = events
        .iter()
        .filter(|event| event.kind == "build")
        .collect();
    out.push_str("\n## Builds\n");
    if builds.is_empty() {
        out.push_str("none\n");
    } else {
        for event in builds {
            out.push_str(&format!(
                "- {}: {}\n",
                ago(now, event.observed_at),
                event.build.as_deref().unwrap_or("?")
            ));
        }
    }
    out.push_str("\n## Signals now\n");
    let mut signals = snapshots;
    signals.sort_by_key(|snapshot| snapshot.signal_id.clone());
    for snapshot in signals {
        out.push_str(&format!("- {}: {}\n", snapshot.signal_id, snapshot.state));
    }
    Ok(out)
}

fn ago(now: i64, then: i64) -> String {
    let secs = now.saturating_sub(then).max(0);
    if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
    use crate::persistence::{self, HealthEvent};
    use crate::test_support::{TempDb, prod};
    use daku_protocol::SignalState as ProtocolState;

    fn seed(now: i64) -> TempDb {
        let db = TempDb::new("digest");
        let connection = db.store().open().unwrap();
        persist_availability_snapshot(
            &connection,
            "prod",
            &AvailabilityObservation {
                reachability: Reachability::Reachable,
                state: ProtocolState::Healthy,
                build: Some("glide-2".into()),
                rtt_ms: 10,
                error: None,
            },
            now,
        )
        .unwrap();
        persistence::persist_signal_snapshot(
            &connection,
            "prod",
            "jobs",
            now,
            ProtocolState::Degraded,
            r#"{"overdue_ready":2,"error":0}"#,
        )
        .unwrap();
        for (at, kind, from, to, build) in [
            (
                now - 6 * 86_400,
                "health",
                Some("healthy"),
                "degraded",
                None,
            ),
            (
                now - 2 * 86_400,
                "build",
                Some("healthy"),
                "degraded",
                Some("glide-2"),
            ),
        ] {
            persistence::record_health_event(
                &connection,
                &HealthEvent {
                    environment_id: "prod".into(),
                    observed_at: at,
                    kind: kind.into(),
                    from_health: from.map(str::to_owned),
                    to_health: to.into(),
                    build: build.map(str::to_owned),
                },
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn digest_lists_transitions_builds_and_current_signals() {
        let now = 1_700_000_000;
        let db = seed(now);
        let text = weekly_digest(&db.store(), &prod(), now, 7).unwrap();
        assert!(text.contains("# Daku digest: Production (prod)"), "{text}");
        assert!(text.contains("healthy → degraded"), "{text}");
        assert!(text.contains("glide-2"), "{text}");
        assert!(text.contains("- jobs: degraded"), "{text}");
        assert!(text.contains("- availability: healthy"), "{text}");
    }

    #[test]
    fn digest_window_excludes_old_events() {
        let now = 1_700_000_000;
        let db = seed(now);
        let text = weekly_digest(&db.store(), &prod(), now, 1).unwrap();
        assert!(!text.contains("healthy → degraded"), "{text}");
        assert!(text.contains("none"), "{text}");
        // Current state is window-independent.
        assert!(text.contains("- jobs: degraded"), "{text}");
    }

    #[test]
    fn ago_formats_minutes_hours_days() {
        assert_eq!(ago(1_000, 1_000 - 90), "1m ago");
        assert_eq!(ago(1_000_000, 1_000_000 - 5 * 3600), "5h ago");
        assert_eq!(ago(1_000_000, 1_000_000 - 3 * 86_400), "3d ago");
    }
}
