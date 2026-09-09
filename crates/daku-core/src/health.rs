//! Environment health rollup. Reachability stays a separate field.

use anyhow::Context;
use crossbeam_channel::Sender;
use daku_protocol::{
    EnvironmentHealth, EnvironmentSummary, HealthEventDto, HealthEventKind, Reachability,
    RollupPoint, SamplePoint, ServerMessage, SignalEventDto, SignalSnapshotDto, SignalState,
    parse_build, parse_reachability,
};

use crate::availability::AVAILABILITY_SIGNAL_ID;
use crate::config::EnvironmentConfig;
use crate::jobs::JOBS_SIGNAL_ID;
use crate::persistence::{
    self, HealthEvent, PublishState, ROLLUP_BUCKET_SECS, ROLLUP_RETENTION_SECS,
    SAMPLE_RETENTION_SECS, StateStore,
};
use crate::syslog::SYSLOG_SIGNAL_ID;
use daku_protocol::NON_VOTING_SIGNALS;

pub const SERVICENOW_PLATFORM_ID: &str = "servicenow";

/// Signals whose 24 h samples ride `SignalSamplesUpdated` and whose hourly
/// buckets ride `SignalRollupsUpdated`. One const — a new trend Signal
/// edits here, not three call sites.
pub const TREND_SIGNALS: [&str; 3] = [AVAILABILITY_SIGNAL_ID, JOBS_SIGNAL_ID, SYSLOG_SIGNAL_ID];

/// Whether one vote counts toward Environment health.
/// Informational Signals never vote (`NON_VOTING_SIGNALS`); skipped probes
/// never vote either. Single predicate — daemon and desktop share the rule.
pub fn votes(signal_id: &str, state: SignalState) -> bool {
    !NON_VOTING_SIGNALS.contains(&signal_id) && state != SignalState::Skipped
}

/// Decided health for one Environment's snapshots: reachability, votes,
/// rollup, and build. Pure over loaded snapshots — the test surface for
/// votership, unknown-state fallback, and build extraction.
pub struct DecideOut {
    pub reachability: Reachability,
    pub health: EnvironmentHealth,
    pub build: Option<String>,
    pub votes: Vec<(String, SignalState)>,
}

pub fn decide(env_snaps: &[&persistence::SignalSnapshot]) -> DecideOut {
    let reachability = if let Some(snapshot) = env_snaps
        .iter()
        .find(|snapshot| snapshot.signal_id == AVAILABILITY_SIGNAL_ID)
    {
        wire_reachability(&snapshot.payload_json)
    } else {
        // No ServiceNow availability snapshot: single-signal platforms
        // (HTTP probe, GitHub Actions) carry their own reachability.
        // A persisted `unreachable` payload means the endpoint itself is
        // unreachable and the environment is down. A down primary probe
        // with no availability signal is likewise a platform outage, not
        // a degraded multi-signal rollup.
        let mut platform_reachability = Reachability::Reachable;
        for snapshot in env_snaps {
            if wire_reachability(&snapshot.payload_json) == Reachability::Unreachable {
                platform_reachability = Reachability::Unreachable;
                break;
            }
            let state = SignalState::parse(&snapshot.state).unwrap_or(SignalState::Skipped);
            if matches!(
                snapshot.signal_id.as_str(),
                crate::http_probe::HTTP_PROBE_SIGNAL_ID | crate::github::ACTIONS_SIGNAL_ID
            ) && state == SignalState::Down
            {
                platform_reachability = Reachability::Unreachable;
            }
        }
        platform_reachability
    };
    let votes: Vec<(String, SignalState)> = env_snaps
        .iter()
        .map(|snapshot| {
            (
                snapshot.signal_id.clone(),
                // Unknown state text never votes.
                SignalState::parse(&snapshot.state).unwrap_or(SignalState::Skipped),
            )
        })
        .collect();
    let vote_refs: Vec<(&str, SignalState)> = votes
        .iter()
        .map(|(id, state)| (id.as_str(), *state))
        .collect();
    // `health_rollup` already skips non-voters; `votes()` is the shared
    // predicate for desktop/tests that need the same rule without a rollup.
    let health = health_rollup(reachability, &vote_refs);
    let build = env_snaps
        .iter()
        .find(|snapshot| snapshot.signal_id == AVAILABILITY_SIGNAL_ID)
        .and_then(|snapshot| wire_build(&snapshot.payload_json));
    DecideOut {
        reachability,
        health,
        build,
        votes,
    }
}

/// Pure publish plan: summaries + messages without touching SQLite writes.
/// `publish_dashboard` stays the thin wrapper that records events, prunes,
/// and sends — the decision lives in `decide`.
pub struct PublishOut {
    pub summaries: Vec<EnvironmentSummary>,
}

/// Health events kept per Environment per publish (the table holds more;
/// the wire carries what the timeline needs).
pub const HEALTH_EVENT_PUBLISH_LIMIT: i64 = 100;

// Rollup points kept per Environment x Signal per publish: 90 days of hour
// buckets is a flat ~2160 points.

pub fn health_rollup(
    reachability: Reachability,
    signals: &[(&str, SignalState)],
) -> EnvironmentHealth {
    match reachability {
        // Reachability is reported separately; a sleeping Environment cannot
        // be observed, so its Signals must not vote.
        Reachability::Unreachable => return EnvironmentHealth::Down,
        Reachability::Asleep => return EnvironmentHealth::Healthy,
        Reachability::Reachable => {}
    }
    if signals.is_empty() {
        // No observation yet: never present cold start as healthy.
        return EnvironmentHealth::Waiting;
    }
    let mut health = EnvironmentHealth::Healthy;
    for &(signal_id, state) in signals {
        // Informational Signals never vote (`NON_VOTING_SIGNALS`: history and
        // capacity context). Skipped probes never vote either.
        if NON_VOTING_SIGNALS.contains(&signal_id) || state == SignalState::Skipped {
            continue;
        }
        if matches!(state, SignalState::Down | SignalState::Degraded) {
            health = EnvironmentHealth::Degraded;
        }
    }
    health
}

fn wire_reachability(payload_json: &str) -> Reachability {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return Reachability::Reachable;
    };
    parse_reachability(&value)
}

/// What one `publish_dashboard` pass measured, for tick telemetry. Kept
/// separate from `PublishOut` (the pure decision plan): this is observed
/// delivery cost, recorded best-effort after the send.
pub struct PublishSummary {
    /// Serialized bytes across every message sent this publish.
    pub bytes: u64,
    /// Snapshots whose payload carries the throttled flag.
    pub throttled: u64,
}

pub fn publish_dashboard(
    environments: &[EnvironmentConfig],
    store: &StateStore,
    sink: &Sender<ServerMessage>,
    now: i64,
    poll_interval_secs: u64,
) -> anyhow::Result<PublishSummary> {
    let connection = store.open().context("open state store for dashboard")?;
    let snapshots = persistence::load_all_signal_snapshots(&connection)?;
    let cutoff = now.saturating_sub(SAMPLE_RETENTION_SECS);
    let mut summary = PublishSummary {
        bytes: 0,
        throttled: 0,
    };
    /// Sends one dashboard message while accounting its wire cost.
    fn emit(sink: &Sender<ServerMessage>, summary: &mut PublishSummary, message: ServerMessage) {
        summary.bytes = summary
            .bytes
            .saturating_add(serde_json::to_vec(&message).map(|v| v.len()).unwrap_or(0) as u64);
        let _ = sink.send(message);
    }
    // One grouping pass: every per-Environment step below reuses it instead
    // of re-scanning the full snapshot vector twice per Environment.
    let mut by_environment: std::collections::HashMap<&str, Vec<_>> =
        std::collections::HashMap::new();
    for snapshot in &snapshots {
        if payload_is_throttled(&snapshot.payload_json) {
            summary.throttled = summary.throttled.saturating_add(1);
        }
        by_environment
            .entry(snapshot.environment_id.as_str())
            .or_default()
            .push(snapshot);
    }

    struct Published {
        environment: EnvironmentConfig,
        summary: EnvironmentSummary,
    }

    let mut published = Vec::with_capacity(environments.len());
    for environment in environments {
        let env_snaps: &[_] = by_environment
            .get(environment.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let decided = decide(env_snaps);
        let reachability = decided.reachability;
        let health = decided.health;
        let build = decided.build;
        let votes: Vec<(&str, SignalState)> = decided
            .votes
            .iter()
            .map(|(id, state)| (id.as_str(), *state))
            .collect();
        record_health_and_build_events(&connection, &environment.id, health, build.clone(), now)?;
        record_signal_events(&connection, &environment.id, &votes, now)?;
        published.push(Published {
            summary: EnvironmentSummary {
                id: environment.id.clone(),
                label: environment.label.clone(),
                instance_url: environment.instance_url.clone(),
                platform_id: environment.platform.id().into(),
                health,
                reachability,
                last_observed_at: env_snaps.iter().map(|snapshot| snapshot.observed_at).max(),
                auth_method: environment.auth_method,
                clone_source: environment.clone_source,
                thresholds: environment.thresholds.clone(),
                expected_drift: environment.expected_drift.clone(),
                sort_order: environment.sort_order,
                poll_interval_secs: Some(poll_interval_secs.max(1)),
            },
            environment: environment.clone(),
        });
    }
    emit(
        sink,
        &mut summary,
        ServerMessage::EnvironmentsUpdated {
            environments: published.iter().map(|item| item.summary.clone()).collect(),
        },
    );

    for item in &published {
        let environment = &item.environment;
        let env_snaps: Vec<SignalSnapshotDto> = by_environment
            .get(environment.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|snapshot| SignalSnapshotDto {
                signal_id: snapshot.signal_id.clone(),
                state: snapshot.state.clone(),
                observed_at: snapshot.observed_at,
                payload_json: snapshot.payload_json.clone(),
            })
            .collect();
        emit(
            sink,
            &mut summary,
            ServerMessage::SignalSnapshotsUpdated {
                environment_id: environment.id.clone(),
                snapshots: env_snaps,
            },
        );
        for signal_id in TREND_SIGNALS {
            let points = persistence::load_signal_samples(&connection, &environment.id, signal_id)?
                .into_iter()
                .filter(|sample| sample.observed_at >= cutoff)
                .map(|sample| SamplePoint {
                    observed_at: sample.observed_at,
                    value_real: sample.value_real,
                })
                .collect();
            emit(
                sink,
                &mut summary,
                ServerMessage::SignalSamplesUpdated {
                    environment_id: environment.id.clone(),
                    signal_id: signal_id.to_owned(),
                    points,
                },
            );
            // One idempotent recompute of the current hour bucket, then the
            // bounded 90-day series. Raw 24 h samples are untouched.
            let hour_start = now - now % ROLLUP_BUCKET_SECS;
            persistence::record_hour_rollup(&connection, &environment.id, signal_id, hour_start)?;
            let rollups = persistence::load_signal_rollups(
                &connection,
                &environment.id,
                signal_id,
                now.saturating_sub(ROLLUP_RETENTION_SECS),
            )?
            .into_iter()
            .map(|rollup| RollupPoint {
                hour_start: rollup.hour_start,
                avg_real: rollup.avg_real,
                max_real: rollup.max_real,
                sample_count: rollup.sample_count,
            })
            .collect();
            emit(
                sink,
                &mut summary,
                ServerMessage::SignalRollupsUpdated {
                    environment_id: environment.id.clone(),
                    signal_id: signal_id.to_owned(),
                    points: rollups,
                },
            );
        }
        persistence::prune_signal_rollups(&connection, now)?;
        let events = persistence::load_health_events(
            &connection,
            &environment.id,
            HEALTH_EVENT_PUBLISH_LIMIT,
        )?
        .into_iter()
        .filter_map(|event| {
            Some(HealthEventDto {
                observed_at: event.observed_at,
                kind: HealthEventKind::parse(&event.kind)?,
                from_health: event
                    .from_health
                    .as_deref()
                    .and_then(EnvironmentHealth::parse),
                to_health: EnvironmentHealth::parse(&event.to_health)?,
                build: event.build,
                note: event.note.filter(|note| !note.trim().is_empty()),
            })
        })
        .collect();
        emit(
            sink,
            &mut summary,
            ServerMessage::HealthEventsUpdated {
                environment_id: environment.id.clone(),
                events,
            },
        );
        let signal_events = persistence::load_signal_events(
            &connection,
            &environment.id,
            HEALTH_EVENT_PUBLISH_LIMIT,
        )?
        .into_iter()
        .filter_map(|event| {
            Some(SignalEventDto {
                signal_id: event.signal_id,
                observed_at: event.observed_at,
                from_state: event.from_state.as_deref().and_then(SignalState::parse),
                to_state: SignalState::parse(&event.to_state)?,
            })
        })
        .collect();
        emit(
            sink,
            &mut summary,
            ServerMessage::SignalEventsUpdated {
                environment_id: environment.id.clone(),
                events: signal_events,
            },
        );
    }
    Ok(summary)
}

/// Whether a persisted snapshot payload marks its drill-in rows as
/// throttled (HTTP 429 after the retry budget). Read at publish time so
/// tick telemetry can count pressure without probe plumbing.
fn payload_is_throttled(payload_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload_json)
        .ok()
        .and_then(|value| value.get("throttled")?.as_bool())
        .unwrap_or(false)
}

/// Build string from an availability snapshot payload, if the probe read one.
fn wire_build(payload_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    parse_build(&value)
}

/// Bounded `health_events` writes for one Environment publish.
///
/// * Health: a rollup change becomes an event only after two consecutive
///   publishes agree, so a single flap is not an event. `previous_health`
///   carries the value before the current streak as the event's `from`.
/// * Build: any newly observed build string is an event immediately,
///   including a bootstrap event (no `from`) on the first build seen.
///   A build that becomes unreadable updates state silently; its
///   reappearance then reads as a change.
fn record_health_and_build_events(
    connection: &rusqlite::Connection,
    environment_id: &str,
    health: EnvironmentHealth,
    build: Option<String>,
    now: i64,
) -> anyhow::Result<()> {
    let current = health.as_str().to_owned();
    match persistence::load_publish_state(connection, environment_id)? {
        None => {
            persistence::store_publish_state(
                connection,
                environment_id,
                &PublishState {
                    last_health: current.clone(),
                    consecutive: 1,
                    previous_health: None,
                    last_build: build.clone(),
                },
            )?;
            if let Some(build) = build {
                persistence::record_health_event(
                    connection,
                    &HealthEvent {
                        environment_id: environment_id.into(),
                        observed_at: now,
                        kind: HealthEventKind::Build.as_str().into(),
                        from_health: None,
                        to_health: current,
                        build: Some(build),
                        note: None,
                    },
                )?;
            }
        }
        Some(state) => {
            let (consecutive, previous_health) = if current == state.last_health {
                (
                    state.consecutive.saturating_add(1),
                    state.previous_health.clone(),
                )
            } else {
                (1, Some(state.last_health.clone()))
            };
            // Cold start must not notify: Waiting itself never emits, and the
            // Waiting-to-Healthy confirmation is the first real poll, not a
            // recovery. But a confirmed Waiting-to-Degraded/Down transition
            // is the first genuine incident and must alert (the webhook
            // relay posts health events).
            let cold_start_recovery = previous_health.as_deref()
                == Some(EnvironmentHealth::Waiting.as_str())
                && current.as_str() == EnvironmentHealth::Healthy.as_str();
            if current == state.last_health
                && consecutive == 2
                && current.as_str() != EnvironmentHealth::Waiting.as_str()
                && !cold_start_recovery
                && previous_health
                    .as_deref()
                    .is_some_and(|previous| previous != current.as_str())
            {
                persistence::record_health_event(
                    connection,
                    &HealthEvent {
                        environment_id: environment_id.into(),
                        observed_at: now,
                        kind: HealthEventKind::Health.as_str().into(),
                        from_health: previous_health.clone(),
                        to_health: current.clone(),
                        build: None,
                        note: None,
                    },
                )?;
            }
            if build.is_some() && build != state.last_build {
                persistence::record_health_event(
                    connection,
                    &HealthEvent {
                        environment_id: environment_id.into(),
                        observed_at: now,
                        kind: HealthEventKind::Build.as_str().into(),
                        from_health: Some(state.last_health.clone()),
                        to_health: current.clone(),
                        build: build.clone(),
                        note: None,
                    },
                )?;
            }
            persistence::store_publish_state(
                connection,
                environment_id,
                &PublishState {
                    last_health: current,
                    consecutive,
                    previous_health,
                    last_build: build,
                },
            )?;
        }
    }
    persistence::prune_health_events(connection, environment_id, now)?;
    Ok(())
}

/// Bounded per-Signal transition writes for one Environment publish.
///
/// Each non-skipped Signal gets the health treatment at its own grain: a
/// state change becomes an event only after two consecutive publishes
/// agree, so a single flap is not history. Skipped ticks leave the streak
/// untouched, so an asleep Environment neither confirms nor breaks one.
/// `previous_state` carries the value before the current streak as the
/// event's `from`.
fn record_signal_events(
    connection: &rusqlite::Connection,
    environment_id: &str,
    votes: &[(&str, SignalState)],
    now: i64,
) -> anyhow::Result<()> {
    for &(signal_id, state) in votes {
        if state == SignalState::Skipped {
            continue;
        }
        let current = state.as_str().to_owned();
        match persistence::load_signal_publish_state(connection, environment_id, signal_id)? {
            None => {
                persistence::store_signal_publish_state(
                    connection,
                    environment_id,
                    signal_id,
                    &persistence::SignalPublishState {
                        last_state: current,
                        consecutive: 1,
                        previous_state: None,
                    },
                )?;
            }
            Some(stored) => {
                let (consecutive, previous_state) = if current == stored.last_state {
                    (
                        stored.consecutive.saturating_add(1),
                        stored.previous_state.clone(),
                    )
                } else {
                    (1, Some(stored.last_state.clone()))
                };
                if current == stored.last_state
                    && consecutive == 2
                    && previous_state
                        .as_deref()
                        .is_some_and(|previous| previous != current.as_str())
                {
                    persistence::record_signal_event(
                        connection,
                        &persistence::SignalEvent {
                            environment_id: environment_id.into(),
                            signal_id: signal_id.into(),
                            observed_at: now,
                            from_state: previous_state.clone(),
                            to_state: current.clone(),
                        },
                    )?;
                }
                persistence::store_signal_publish_state(
                    connection,
                    environment_id,
                    signal_id,
                    &persistence::SignalPublishState {
                        last_state: current,
                        consecutive,
                        previous_state,
                    },
                )?;
            }
        }
    }
    persistence::prune_signal_events(connection, environment_id, now)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDb, prod};

    #[test]
    fn votes_skips_non_voting_and_skipped() {
        assert!(!votes("last_clone", SignalState::Degraded));
        assert!(!votes("sessions", SignalState::Down));
        assert!(!votes("jobs", SignalState::Skipped));
        assert!(votes("jobs", SignalState::Degraded));
        assert!(votes("jobs", SignalState::Healthy));
    }

    #[test]
    fn decide_unknown_state_never_votes_and_build_extracts() {
        let snaps = [
            persistence::SignalSnapshot {
                environment_id: "prod".into(),
                signal_id: AVAILABILITY_SIGNAL_ID.into(),
                observed_at: 100,
                state: "healthy".into(),
                payload_json: r#"{"reachability":"reachable","build":"v1"}"#.into(),
            },
            persistence::SignalSnapshot {
                environment_id: "prod".into(),
                signal_id: "jobs".into(),
                observed_at: 100,
                state: "bogus".into(),
                payload_json: r#"{"overdue_ready": 99}"#.into(),
            },
        ];
        let refs: Vec<&persistence::SignalSnapshot> = snaps.iter().collect();
        let out = decide(&refs);
        assert_eq!(out.health, EnvironmentHealth::Healthy);
        assert_eq!(out.build, Some("v1".into()));
        assert_eq!(out.votes.len(), 2);
    }

    #[test]
    fn decide_empty_is_waiting_never_healthy() {
        let out = decide(&[]);
        assert_eq!(out.health, EnvironmentHealth::Waiting);
    }

    #[test]
    fn trend_signals_cover_samples_and_rollups() {
        assert_eq!(
            TREND_SIGNALS,
            [AVAILABILITY_SIGNAL_ID, JOBS_SIGNAL_ID, SYSLOG_SIGNAL_ID]
        );
    }

    #[test]
    fn health_rollup_unreachable_is_down() {
        assert_eq!(
            health_rollup(Reachability::Unreachable, &[("jobs", SignalState::Healthy)]),
            EnvironmentHealth::Down
        );
    }

    #[test]
    fn health_rollup_asleep_without_degraded_signals_is_healthy() {
        let health = health_rollup(
            Reachability::Asleep,
            &[
                ("availability", SignalState::Healthy),
                ("jobs", SignalState::Healthy),
            ],
        );
        assert_eq!(health, EnvironmentHealth::Healthy);
        assert_ne!(health, EnvironmentHealth::Degraded);
    }

    #[test]
    fn health_rollup_asleep_with_no_signals_is_healthy() {
        assert_eq!(
            health_rollup(Reachability::Asleep, &[]),
            EnvironmentHealth::Healthy
        );
    }

    #[test]
    fn health_rollup_reachable_jobs_degraded_is_degraded() {
        assert_eq!(
            health_rollup(
                Reachability::Reachable,
                &[
                    ("availability", SignalState::Healthy),
                    ("jobs", SignalState::Degraded)
                ]
            ),
            EnvironmentHealth::Degraded
        );
    }

    #[test]
    fn health_rollup_reachable_all_healthy_is_healthy() {
        assert_eq!(
            health_rollup(
                Reachability::Reachable,
                &[
                    ("availability", SignalState::Healthy),
                    ("jobs", SignalState::Healthy)
                ]
            ),
            EnvironmentHealth::Healthy
        );
    }

    #[test]
    fn health_rollup_reachable_no_snapshots_is_waiting() {
        assert_eq!(
            health_rollup(Reachability::Reachable, &[]),
            EnvironmentHealth::Waiting
        );
    }

    #[test]
    fn health_rollup_non_voting_signals_never_vote() {
        // Informational Signals (history and capacity context): even down
        // (lost read ACL) must not flip the Environment — the card itself
        // shows the error.
        for signal_id in NON_VOTING_SIGNALS {
            for state in [
                SignalState::Healthy,
                SignalState::Degraded,
                SignalState::Down,
            ] {
                assert_eq!(
                    health_rollup(Reachability::Reachable, &[(signal_id, state)]),
                    EnvironmentHealth::Healthy,
                    "{signal_id} must not vote"
                );
            }
        }
    }

    #[test]
    fn health_rollup_skips_missing_and_skipped_signals() {
        assert_eq!(
            health_rollup(Reachability::Reachable, &[("drift", SignalState::Skipped)]),
            EnvironmentHealth::Healthy
        );
    }

    /// Snapshots driving `publish_dashboard` event tests: reachable
    /// availability (optional build) plus a jobs vote that sets the rollup.
    fn write_votes(
        connection: &rusqlite::Connection,
        now: i64,
        jobs: SignalState,
        build: Option<&str>,
    ) {
        let payload = match build {
            Some(build) => {
                format!("{{\"reachability\":\"reachable\",\"rtt_ms\":10,\"build\":\"{build}\"}}")
            }
            None => r#"{"reachability":"reachable","rtt_ms":10}"#.to_owned(),
        };
        persistence::persist_signal_snapshot(
            connection,
            "prod",
            AVAILABILITY_SIGNAL_ID,
            now,
            SignalState::Healthy,
            &payload,
        )
        .unwrap();
        persistence::persist_signal_snapshot(
            connection,
            "prod",
            JOBS_SIGNAL_ID,
            now,
            jobs,
            r#"{"overdue_ready":0,"error":0}"#,
        )
        .unwrap();
    }

    fn published_health_events(store: &StateStore, now: i64) -> Vec<HealthEventDto> {
        use crossbeam_channel::unbounded;
        let (tx, rx) = unbounded();
        publish_dashboard(&[prod()], store, &tx, now, 120).unwrap();
        let mut events = Vec::new();
        while let Ok(message) = rx.try_recv() {
            if let ServerMessage::HealthEventsUpdated {
                environment_id,
                events: list,
            } = message
            {
                assert_eq!(environment_id, "prod");
                events = list;
            }
        }
        events
    }

    #[test]
    fn single_flap_writes_no_health_event() {
        let db = TempDb::new("health-flap");
        let store = db.store();
        let connection = store.open().unwrap();
        write_votes(&connection, 100, SignalState::Healthy, None);
        assert!(published_health_events(&store, 100).is_empty());
        write_votes(&connection, 200, SignalState::Degraded, None);
        assert!(published_health_events(&store, 200).is_empty());
        write_votes(&connection, 300, SignalState::Healthy, None);
        assert!(published_health_events(&store, 300).is_empty());
    }

    #[test]
    fn two_consecutive_degraded_writes_one_event_then_stays_quiet() {
        let db = TempDb::new("health-confirm");
        let store = db.store();
        let connection = store.open().unwrap();
        write_votes(&connection, 100, SignalState::Healthy, None);
        assert!(published_health_events(&store, 100).is_empty());
        write_votes(&connection, 200, SignalState::Degraded, None);
        assert!(published_health_events(&store, 200).is_empty());
        write_votes(&connection, 300, SignalState::Degraded, None);
        let events = published_health_events(&store, 300);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, HealthEventKind::Health);
        assert_eq!(events[0].from_health, Some(EnvironmentHealth::Healthy));
        assert_eq!(events[0].to_health, EnvironmentHealth::Degraded);
        // Third consecutive publish must not duplicate the transition.
        write_votes(&connection, 400, SignalState::Degraded, None);
        assert_eq!(published_health_events(&store, 400).len(), 1);
    }

    #[test]
    fn cold_start_incident_after_waiting_still_emits_but_recovery_stays_quiet() {
        let db = TempDb::new("health-cold-start");
        let store = db.store();
        // No snapshots yet: the first publish records Waiting without events.
        assert!(published_health_events(&store, 100).is_empty());
        // First genuine incident confirms over two publishes and must alert.
        let connection = store.open().unwrap();
        write_votes(&connection, 200, SignalState::Degraded, None);
        assert!(published_health_events(&store, 200).is_empty());
        write_votes(&connection, 300, SignalState::Degraded, None);
        let events = published_health_events(&store, 300);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].from_health, Some(EnvironmentHealth::Waiting));
        assert_eq!(events[0].to_health, EnvironmentHealth::Degraded);
    }

    #[test]
    fn cold_start_recovery_to_healthy_emits_no_event() {
        let db = TempDb::new("health-cold-recovery");
        let store = db.store();
        assert!(published_health_events(&store, 100).is_empty());
        let connection = store.open().unwrap();
        write_votes(&connection, 200, SignalState::Healthy, None);
        assert!(published_health_events(&store, 200).is_empty());
        write_votes(&connection, 300, SignalState::Healthy, None);
        assert!(published_health_events(&store, 300).is_empty());
    }

    /// Drives one Signal through healthy → degraded → degraded and returns
    /// its published signal events, exercising the shared per-signal
    /// two-publish confirmation (and the skipped-tick blind spot).
    fn publish_signal(
        store: &StateStore,
        signal_id: &str,
        state: SignalState,
        now: i64,
    ) -> Vec<persistence::SignalEvent> {
        let connection = store.open().unwrap();
        let votes = vec![("jobs", SignalState::Healthy), (signal_id, state)];
        record_signal_events(&connection, "prod", &votes, now).unwrap();
        let connection = store.open().unwrap();
        persistence::load_signal_events(&connection, "prod", 100).unwrap()
    }

    #[test]
    fn signal_events_confirm_over_two_publishes_and_skip_skipped_ticks() {
        let db = TempDb::new("signal-confirm");
        let store = db.store();
        assert!(publish_signal(&store, "syslog", SignalState::Healthy, 100).is_empty());
        // First degraded publish: streak starts, nothing recorded.
        assert!(publish_signal(&store, "syslog", SignalState::Degraded, 200).is_empty());
        // Second agreeing publish: one event naming the turn.
        let events = publish_signal(&store, "syslog", SignalState::Degraded, 300);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].signal_id, "syslog");
        assert_eq!(events[0].from_state.as_deref(), Some("healthy"));
        assert_eq!(events[0].to_state, "degraded");
        // A skipped tick leaves the streak untouched: no duplicate, and the
        // recovery below still confirms against the streak.
        assert_eq!(
            publish_signal(&store, "syslog", SignalState::Skipped, 400).len(),
            1
        );
        assert_eq!(
            publish_signal(&store, "syslog", SignalState::Healthy, 500).len(),
            1,
            "single healthy publish after the skip is only streak-start"
        );
        let events = publish_signal(&store, "syslog", SignalState::Healthy, 600);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].from_state.as_deref(), Some("degraded"));
        assert_eq!(events[1].to_state, "healthy");
    }

    #[test]
    fn signal_flap_writes_no_signal_event() {
        let db = TempDb::new("signal-flap");
        let store = db.store();
        assert!(publish_signal(&store, "syslog", SignalState::Healthy, 100).is_empty());
        assert!(publish_signal(&store, "syslog", SignalState::Degraded, 200).is_empty());
        assert!(publish_signal(&store, "syslog", SignalState::Healthy, 300).is_empty());
    }

    #[test]
    fn first_observed_build_writes_a_bootstrap_event() {
        let db = TempDb::new("health-bootstrap");
        let store = db.store();
        let connection = store.open().unwrap();
        write_votes(&connection, 100, SignalState::Healthy, Some("glide-1"));
        let events = published_health_events(&store, 100);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, HealthEventKind::Build);
        assert_eq!(events[0].from_health, None);
        assert_eq!(events[0].to_health, EnvironmentHealth::Healthy);
        assert_eq!(events[0].build.as_deref(), Some("glide-1"));
        // Same tick re-published (daemon restart) stays one row.
        assert_eq!(published_health_events(&store, 100).len(), 1);
    }

    #[test]
    fn build_change_writes_an_event_without_waiting() {
        let db = TempDb::new("health-build-change");
        let store = db.store();
        let connection = store.open().unwrap();
        write_votes(&connection, 100, SignalState::Healthy, Some("glide-1"));
        assert_eq!(published_health_events(&store, 100).len(), 1);
        write_votes(&connection, 200, SignalState::Healthy, Some("glide-2"));
        let events = published_health_events(&store, 200);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, HealthEventKind::Build);
        assert_eq!(events[1].from_health, Some(EnvironmentHealth::Healthy));
        assert_eq!(events[1].build.as_deref(), Some("glide-2"));
    }

    #[test]
    fn health_rollup_asleep_ignores_signal_votes() {
        assert_eq!(
            health_rollup(Reachability::Asleep, &[("jobs", SignalState::Degraded)]),
            EnvironmentHealth::Healthy
        );
        assert_eq!(
            health_rollup(
                Reachability::Asleep,
                &[("jobs", SignalState::Down), ("syslog", SignalState::Down)]
            ),
            EnvironmentHealth::Healthy
        );
    }

    #[test]
    fn health_rollup_reachable_signal_down_is_degraded() {
        assert_eq!(
            health_rollup(Reachability::Reachable, &[("jobs", SignalState::Down)]),
            EnvironmentHealth::Degraded
        );
    }

    #[test]
    fn health_rollup_publish_emits_dashboard_events_after_fixture() {
        use crate::jobs::JOBS_SIGNAL_ID;
        use crate::last_clone::LAST_CLONE_SIGNAL_ID;
        use crate::persistence;
        use crate::syslog::SYSLOG_SIGNAL_ID;
        use crossbeam_channel::unbounded;
        use daku_protocol::ServerMessage;

        let db = TempDb::new("health-publish");
        let store = db.store();
        let connection = store.open().unwrap();
        let now = 1_700_000_000;
        persistence::persist_signal_snapshot(
            &connection,
            "prod",
            "availability",
            now,
            SignalState::Healthy,
            r#"{"reachability":"asleep"}"#,
        )
        .unwrap();
        persistence::persist_signal_snapshot(
            &connection,
            "prod",
            JOBS_SIGNAL_ID,
            now,
            SignalState::Healthy,
            r#"{"overdue_ready":0}"#,
        )
        .unwrap();
        persistence::persist_signal_snapshot(
            &connection,
            "prod",
            LAST_CLONE_SIGNAL_ID,
            now,
            SignalState::Degraded,
            r#"{"supported":true}"#,
        )
        .unwrap();
        persistence::persist_signal_sample(
            &connection,
            "prod",
            JOBS_SIGNAL_ID,
            now - 25 * 60 * 60,
            Some(9.0),
            None,
        )
        .unwrap();
        persistence::persist_signal_sample(
            &connection,
            "prod",
            JOBS_SIGNAL_ID,
            now - 60,
            Some(1.0),
            None,
        )
        .unwrap();
        persistence::persist_signal_sample(
            &connection,
            "prod",
            JOBS_SIGNAL_ID,
            now,
            Some(2.0),
            None,
        )
        .unwrap();

        let (tx, rx) = unbounded();
        publish_dashboard(&[prod()], &store, &tx, now, 120).unwrap();

        let mut environments = None;
        let mut snapshots = None;
        let mut jobs_samples = None;
        let mut syslog_samples = None;
        let mut health_events = None;
        let mut rollups = std::collections::HashMap::new();
        while let Ok(message) = rx.try_recv() {
            match message {
                ServerMessage::EnvironmentsUpdated { environments: list } => {
                    environments = Some(list);
                }
                ServerMessage::SignalSnapshotsUpdated {
                    environment_id,
                    snapshots: list,
                } => {
                    assert_eq!(environment_id, "prod");
                    snapshots = Some(list);
                }
                ServerMessage::SignalSamplesUpdated {
                    environment_id,
                    signal_id,
                    points,
                } => {
                    assert_eq!(environment_id, "prod");
                    if signal_id == JOBS_SIGNAL_ID {
                        jobs_samples = Some(points);
                    } else if signal_id == SYSLOG_SIGNAL_ID {
                        syslog_samples = Some(points);
                    }
                }
                ServerMessage::HealthEventsUpdated {
                    environment_id,
                    events,
                } => {
                    assert_eq!(environment_id, "prod");
                    health_events = Some(events);
                }
                ServerMessage::SignalEventsUpdated {
                    environment_id,
                    events,
                } => {
                    assert_eq!(environment_id, "prod");
                    assert!(
                        events.is_empty(),
                        "first publish only seeds per-signal streaks"
                    );
                }
                ServerMessage::SignalRollupsUpdated {
                    environment_id,
                    signal_id,
                    points,
                } => {
                    assert_eq!(environment_id, "prod");
                    rollups.insert(signal_id, points);
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        let environments = environments.expect("EnvironmentsUpdated");
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].health, EnvironmentHealth::Healthy);
        assert_ne!(environments[0].health, EnvironmentHealth::Degraded);
        assert_eq!(environments[0].reachability, Reachability::Asleep);
        assert_eq!(environments[0].platform_id, SERVICENOW_PLATFORM_ID);
        assert_eq!(environments[0].instance_url, prod().instance_url);
        assert_eq!(environments[0].last_observed_at, Some(now));

        let snapshots = snapshots.expect("SignalSnapshotsUpdated");
        assert_eq!(snapshots.len(), 3);

        let jobs_samples = jobs_samples.expect("jobs SignalSamplesUpdated");
        assert_eq!(jobs_samples.len(), 2);
        assert_eq!(jobs_samples[0].value_real, Some(1.0));
        assert_eq!(jobs_samples[1].value_real, Some(2.0));
        assert!(
            syslog_samples
                .expect("syslog SignalSamplesUpdated")
                .is_empty()
        );
        // No build in the availability payload, so no bootstrap event — but
        // the message itself is always published for Hub replay parity.
        assert!(health_events.expect("HealthEventsUpdated").is_empty());
        // Jobs samples fall in the current hour: one rollup point.
        let jobs_rollups = rollups.remove(JOBS_SIGNAL_ID).expect("jobs rollups");
        assert_eq!(jobs_rollups.len(), 1);
        assert_eq!(jobs_rollups[0].sample_count, 2);
        assert_eq!(jobs_rollups[0].avg_real, Some(1.5));
    }
}
