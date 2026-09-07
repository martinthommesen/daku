//! Environment health rollup. Reachability stays a separate field.

use anyhow::Context;
use crossbeam_channel::Sender;
use daku_protocol::{
    EnvironmentHealth, EnvironmentSummary, HealthEventDto, HealthEventKind, Reachability,
    RollupPoint, SamplePoint, ServerMessage, SignalSnapshotDto, SignalState,
};

use crate::availability::AVAILABILITY_SIGNAL_ID;
use crate::config::EnvironmentConfig;
use crate::jobs::JOBS_SIGNAL_ID;
use crate::last_clone::LAST_CLONE_SIGNAL_ID;
use crate::persistence::{
    self, HealthEvent, PublishState, ROLLUP_BUCKET_SECS, ROLLUP_RETENTION_SECS,
    SAMPLE_RETENTION_SECS, StateStore,
};
use crate::syslog::SYSLOG_SIGNAL_ID;

pub const SERVICENOW_PLATFORM_ID: &str = "servicenow";

/// Health events kept per Environment per publish (the table holds more;
/// the wire carries what the timeline needs).
pub const HEALTH_EVENT_PUBLISH_LIMIT: i64 = 100;

// Rollup points kept per Environment x Signal per publish: 30 days of hour
// buckets is a flat 720 points.

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
    let mut health = EnvironmentHealth::Healthy;
    for &(signal_id, state) in signals {
        if signal_id == LAST_CLONE_SIGNAL_ID || state == SignalState::Skipped {
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
    value
        .get("reachability")
        .and_then(|item| item.as_str())
        .and_then(Reachability::parse)
        .unwrap_or(Reachability::Reachable)
}

pub fn publish_dashboard(
    environments: &[EnvironmentConfig],
    store: &StateStore,
    sink: &Sender<ServerMessage>,
    now: i64,
) -> anyhow::Result<()> {
    let connection = store.open().context("open state store for dashboard")?;
    let snapshots = persistence::load_all_signal_snapshots(&connection)?;
    let cutoff = now.saturating_sub(SAMPLE_RETENTION_SECS);

    struct Published {
        environment: EnvironmentConfig,
        summary: EnvironmentSummary,
    }

    let mut published = Vec::with_capacity(environments.len());
    for environment in environments {
        let env_snaps: Vec<_> = snapshots
            .iter()
            .filter(|snapshot| snapshot.environment_id == environment.id)
            .collect();
        let reachability = env_snaps
            .iter()
            .find(|snapshot| snapshot.signal_id == AVAILABILITY_SIGNAL_ID)
            .map(|snapshot| wire_reachability(&snapshot.payload_json))
            .unwrap_or(Reachability::Reachable);
        let votes: Vec<_> = env_snaps
            .iter()
            .map(|snapshot| {
                (
                    snapshot.signal_id.as_str(),
                    // Unknown state text never votes.
                    SignalState::parse(&snapshot.state).unwrap_or(SignalState::Skipped),
                )
            })
            .collect();
        let health = health_rollup(reachability, &votes);
        let build = env_snaps
            .iter()
            .find(|snapshot| snapshot.signal_id == AVAILABILITY_SIGNAL_ID)
            .and_then(|snapshot| wire_build(&snapshot.payload_json));
        record_health_and_build_events(&connection, &environment.id, health, build.clone(), now)?;
        published.push(Published {
            summary: EnvironmentSummary {
                id: environment.id.clone(),
                label: environment.label.clone(),
                instance_url: environment.instance_url.clone(),
                platform_id: SERVICENOW_PLATFORM_ID.into(),
                health,
                reachability,
                last_observed_at: env_snaps.iter().map(|snapshot| snapshot.observed_at).max(),
            },
            environment: environment.clone(),
        });
    }
    let _ = sink.send(ServerMessage::EnvironmentsUpdated {
        environments: published.iter().map(|item| item.summary.clone()).collect(),
    });

    for item in &published {
        let environment = &item.environment;
        let env_snaps: Vec<SignalSnapshotDto> = snapshots
            .iter()
            .filter(|snapshot| snapshot.environment_id == environment.id)
            .map(|snapshot| SignalSnapshotDto {
                signal_id: snapshot.signal_id.clone(),
                state: snapshot.state.clone(),
                observed_at: snapshot.observed_at,
                payload_json: snapshot.payload_json.clone(),
            })
            .collect();
        let _ = sink.send(ServerMessage::SignalSnapshotsUpdated {
            environment_id: environment.id.clone(),
            snapshots: env_snaps,
        });
        for signal_id in [AVAILABILITY_SIGNAL_ID, JOBS_SIGNAL_ID, SYSLOG_SIGNAL_ID] {
            let points = persistence::load_signal_samples(&connection, &environment.id, signal_id)?
                .into_iter()
                .filter(|sample| sample.observed_at >= cutoff)
                .map(|sample| SamplePoint {
                    observed_at: sample.observed_at,
                    value_real: sample.value_real,
                })
                .collect();
            let _ = sink.send(ServerMessage::SignalSamplesUpdated {
                environment_id: environment.id.clone(),
                signal_id: signal_id.to_owned(),
                points,
            });
            // One idempotent recompute of the current hour bucket, then the
            // bounded 30-day series. Raw 24 h samples are untouched.
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
            let _ = sink.send(ServerMessage::SignalRollupsUpdated {
                environment_id: environment.id.clone(),
                signal_id: signal_id.to_owned(),
                points: rollups,
            });
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
            })
        })
        .collect();
        let _ = sink.send(ServerMessage::HealthEventsUpdated {
            environment_id: environment.id.clone(),
            events,
        });
    }
    Ok(())
}

/// Build string from an availability snapshot payload, if the probe read one.
fn wire_build(payload_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    value
        .get("build")
        .and_then(|item| item.as_str())
        .filter(|build| !build.is_empty())
        .map(str::to_owned)
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
            if current == state.last_health
                && consecutive == 2
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDb, prod};

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
    fn health_rollup_reachable_no_snapshots_is_healthy() {
        assert_eq!(
            health_rollup(Reachability::Reachable, &[]),
            EnvironmentHealth::Healthy
        );
    }

    #[test]
    fn health_rollup_last_clone_never_votes_degraded() {
        for state in [SignalState::Degraded, SignalState::Down] {
            assert_eq!(
                health_rollup(Reachability::Reachable, &[(LAST_CLONE_SIGNAL_ID, state)]),
                EnvironmentHealth::Healthy
            );
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
        publish_dashboard(&[prod()], store, &tx, now).unwrap();
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
        publish_dashboard(&[prod()], &store, &tx, now).unwrap();

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
