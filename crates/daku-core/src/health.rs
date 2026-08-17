//! Environment health rollup. Reachability stays a separate field.

use anyhow::Context;
use crossbeam_channel::Sender;
use daku_protocol::{
    EnvironmentHealth, EnvironmentSummary, Reachability, SamplePoint, ServerMessage,
    SignalSnapshotDto, SignalState,
};

use crate::availability::AVAILABILITY_SIGNAL_ID;
use crate::config::EnvironmentConfig;
use crate::jobs::JOBS_SIGNAL_ID;
use crate::last_clone::LAST_CLONE_SIGNAL_ID;
use crate::persistence::{self, SAMPLE_RETENTION_SECS, StateStore};
use crate::syslog::SYSLOG_SIGNAL_ID;

pub const SERVICENOW_PLATFORM_ID: &str = "servicenow";

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

    let mut summaries = Vec::with_capacity(environments.len());
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
        summaries.push(EnvironmentSummary {
            id: environment.id.clone(),
            label: environment.label.clone(),
            instance_url: environment.instance_url.clone(),
            platform_id: SERVICENOW_PLATFORM_ID.into(),
            health: health_rollup(reachability, &votes),
            reachability,
            last_observed_at: env_snaps.iter().map(|snapshot| snapshot.observed_at).max(),
        });
    }
    let _ = sink.send(ServerMessage::EnvironmentsUpdated {
        environments: summaries,
    });

    for environment in environments {
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
        for signal_id in [JOBS_SIGNAL_ID, SYSLOG_SIGNAL_ID] {
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
        }
    }
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
    }
}
