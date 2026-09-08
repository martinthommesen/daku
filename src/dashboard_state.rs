//! Pure dashboard model: protocol events in, sidebar/detail/compare out.

use std::collections::{HashMap, HashSet};

use daku_protocol::{
    EnvironmentHealth, EnvironmentSummary, HealthEventDto, HealthEventKind, Reachability,
    RollupPoint, SamplePoint, ServerMessage, SignalSnapshotDto, is_supported_instance_url,
};

pub const SIGNAL_IDS: [&str; 11] = [
    "availability",
    "jobs",
    "syslog",
    "mid_ecc",
    "outbound",
    "flow",
    "email",
    "upgrade",
    "sessions",
    "drift",
    "last_clone",
];

pub const WAITING: &str = "Waiting";

/// True for the Signals with trends (raw 24 h samples + hourly roll-ups).
pub fn is_trend_signal(signal_id: &str) -> bool {
    TREND_SIGNALS.contains(&signal_id)
}

pub fn signal_label(signal_id: &str) -> &'static str {
    match signal_id {
        "availability" => "Availability",
        "jobs" => "Scheduled jobs",
        "syslog" => "Syslog errors",
        "mid_ecc" => "MID / ECC",
        "outbound" => "Outbound",
        "flow" => "Flow errors",
        "email" => "Email failures",
        "upgrade" => "Upgrades",
        "sessions" => "Sessions",
        "drift" => "Version / plugins",
        "last_clone" => "Last clone",
        _ => "Signal",
    }
}

const TREND_SIGNALS: [&str; 3] = ["availability", "jobs", "syslog"];

/// The daemon keeps samples this long (`persistence::SAMPLE_RETENTION_SECS`);
/// every sparkline spans it.
pub const TREND_WINDOW_LABEL: &str = "24 h";

/// The Drill-in is a bounded region, not a table browser.
const DRILL_IN_ROW_LIMIT: usize = 50;

/// ServiceNow encoded-query operators are not legal in a URL; percent-encode
/// the four the deep-link paths use verbatim.
fn encode_query(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            '^' => "%5E".to_owned(),
            '<' => "%3C".to_owned(),
            '>' => "%3E".to_owned(),
            ' ' => "%20".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

/// Older than this and the header tints "polled … ago" as stale.
// ponytail: fixed threshold (2.5x default cadence); put poll_interval_secs on
// EnvironmentsUpdated if Operators start tuning cadence.
pub const STALE_AFTER_SECS: i64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Freshness {
    pub label: String,
    pub stale: bool,
    /// Stale for over an hour: the numbers on screen are history, not state.
    pub critical: bool,
}

const CRITICAL_AFTER_SECS: i64 = 3600;

/// "polled 42 s ago" / "polled 3 min ago" / "polled 2 h ago" for the selected
/// Environment. An Environment with no observation yet reads "never polled" and
/// is stale by definition — daku has not contacted it.
pub fn freshness(last_observed_at: Option<i64>, now: i64) -> Freshness {
    let Some(last_observed_at) = last_observed_at else {
        return Freshness {
            label: "never polled".to_owned(),
            stale: true,
            critical: false,
        };
    };
    let age = now.saturating_sub(last_observed_at).max(0);
    let label = if age < 60 {
        format!("polled {age} s ago")
    } else if age < 3600 {
        format!("polled {} min ago", age / 60)
    } else {
        format!("polled {} h ago", age / 3600)
    };
    Freshness {
        label,
        stale: age > STALE_AFTER_SECS,
        critical: age > CRITICAL_AFTER_SECS,
    }
}

/// A snapshot plus its payload parsed once, on arrival. Every accessor reads
/// keys out of `payload`; re-parsing per accessor per frame is what this
/// exists to avoid — a Waiting card animates a `Skeleton`, which repaints the
/// whole shell continuously until the first poll lands.
///
/// Invariant: `payload` is derived from `dto.payload_json`, so only `apply`
/// may construct a `Snapshot`. An unparseable payload becomes `Value::Null`.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    dto: SignalSnapshotDto,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, Default)]
pub struct DashboardState {
    connected: bool,
    environments: Vec<EnvironmentSummary>,
    selected_id: Option<String>,
    selected_card: Option<&'static str>,
    snapshots: HashMap<String, HashMap<String, Snapshot>>,
    samples: HashMap<(String, String), Vec<SamplePoint>>,
    /// Health-transition + build-change events per Environment, published by
    /// the daemon (`072`). Rendered by the Recent timeline (`077`).
    health_events: HashMap<String, Vec<HealthEventDto>>,
    /// Operator mutes: Environment id → unix seconds the attention surfaces
    /// stay silent. Mirrors `AppSettings::mutes`; the daemon keeps collecting.
    mutes: HashMap<String, i64>,
    /// Hourly roll-ups per (Environment, Signal), published by the daemon
    /// (`104`). The 24 h raw samples in `samples` are untouched.
    rollups: HashMap<(String, String), Vec<RollupPoint>>,
    trend_window: TrendWindow,
}

/// Trend range for the drill-in sparklines. 24 h reads the raw samples the
/// daemon keeps; 7 d / 30 d read the hourly roll-ups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrendWindow {
    #[default]
    Day24,
    Day7,
    Day30,
}

impl TrendWindow {
    pub const ALL: [(TrendWindow, &'static str); 3] = [
        (TrendWindow::Day24, "24h"),
        (TrendWindow::Day7, "7d"),
        (TrendWindow::Day30, "30d"),
    ];

    /// Rollup cutoff in unix seconds: points at or after this render.
    pub fn cutoff_secs(self, now: i64) -> i64 {
        match self {
            TrendWindow::Day24 => now.saturating_sub(24 * 3600),
            TrendWindow::Day7 => now.saturating_sub(7 * 86_400),
            TrendWindow::Day30 => now.saturating_sub(30 * 86_400),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    pub id: String,
    pub label: String,
    pub health: EnvironmentHealth,
    /// Never observed or disconnected: grey regardless of health.
    pub dimmed: bool,
    /// Operator mute (`074`): grey and silent on attention surfaces.
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRow {
    pub id: String,
    pub label: String,
    pub build: Option<String>,
    /// True when this Environment's build is known, the reference build is
    /// known, and they differ. Unknown on either side is never a mismatch —
    /// the Compare strip must not tint what it could not read.
    pub mismatch: bool,
    pub drift: String,
    pub last_clone: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignalCard {
    pub signal_id: &'static str,
    pub status: String,
    pub sparkline: Vec<f64>,
    /// Disconnected: the status colour is stale, so the Environment detail
    /// paints it grey. Unlike `dimmed` this never covers an Operator mute.
    pub dimmed: bool,
    /// The selected Environment is Operator-muted: grey and quiet.
    pub muted: bool,
}

/// One drill-in table row: cells plus an optional deep link rendered on
/// the first cell (job rows link their ServiceNow records).
#[derive(Clone, Debug, PartialEq)]
pub struct DrillInRow {
    pub cells: Vec<String>,
    pub link: Option<String>,
}

/// Content of the Drill-in region under the Signal cards, built from what the
/// snapshot payload already carries.
#[derive(Clone, Debug, PartialEq)]
pub enum DrillIn {
    Rows {
        headers: Vec<&'static str>,
        rows: Vec<DrillInRow>,
        truncated: bool,
    },
    Trend(Vec<f64>),
    Text(String),
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareStrip {
    pub visible: bool,
    pub has_mismatch: bool,
}

impl DashboardState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    /// True while `now` is before the mute deadline for `id`.
    pub fn is_muted(&self, id: &str, now: i64) -> bool {
        self.mutes.get(id).is_some_and(|until| now < *until)
    }

    pub fn muted_until(&self, id: &str) -> Option<i64> {
        self.mutes.get(id).copied()
    }

    /// Last `limit` health/build events for the selected Environment, oldest
    /// first. Empty when the daemon never published any (pre-072 data or a
    /// fresh Environment) — the shell omits the Recent block then.
    pub fn recent_events(&self, limit: usize) -> Vec<HealthEventDto> {
        let Some(id) = self.selected_id.as_deref() else {
            return Vec::new();
        };
        let Some(events) = self.health_events.get(id) else {
            return Vec::new();
        };
        events.iter().rev().take(limit).rev().cloned().collect()
    }

    /// The selected Environment's current build plus when daku first saw this
    /// build string (latest matching build event). `None` without a known
    /// build or without events yet — the header omits the line then.
    pub fn build_age(&self) -> Option<(String, i64)> {
        let id = self.selected_id.as_deref()?;
        let build = environment_build(&self.snapshots, id)?;
        let since = self
            .health_events
            .get(id)?
            .iter()
            .filter(|event| {
                event.kind == HealthEventKind::Build && event.build.as_deref() == Some(&build)
            })
            .map(|event| event.observed_at)
            .max()?;
        Some((build, since))
    }

    pub fn set_mute(&mut self, id: &str, until: i64) {
        self.mutes.insert(id.to_owned(), until);
    }

    pub fn clear_mute(&mut self, id: &str) {
        self.mutes.remove(id);
    }

    pub fn apply_mutes(&mut self, mutes: &HashMap<String, i64>, now: i64) {
        self.mutes = mutes.clone();
        self.mutes.retain(|_, until| now < *until);
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn has_environments(&self) -> bool {
        !self.environments.is_empty()
    }

    pub fn environments(&self) -> &[EnvironmentSummary] {
        &self.environments
    }

    pub fn apply_all(&mut self, messages: &[ServerMessage]) {
        for message in messages {
            self.apply(message);
        }
    }

    pub fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::EnvironmentsUpdated { environments } => {
                self.environments = environments.clone();
                if self.selected_id.as_ref().is_none_or(|id| {
                    !self
                        .environments
                        .iter()
                        .any(|environment| &environment.id == id)
                }) {
                    self.selected_id = self
                        .environments
                        .first()
                        .map(|environment| environment.id.clone());
                }
                // An Environment that left the config must not leave its last
                // snapshots behind: adding the same id back would render last
                // session's health until the next poll overwrites it.
                let known: HashSet<&str> = self
                    .environments
                    .iter()
                    .map(|environment| environment.id.as_str())
                    .collect();
                self.snapshots.retain(|id, _| known.contains(id.as_str()));
                self.samples
                    .retain(|(id, _), _| known.contains(id.as_str()));
                self.health_events
                    .retain(|id, _| known.contains(id.as_str()));
                self.rollups
                    .retain(|(id, _), _| known.contains(id.as_str()));
            }
            ServerMessage::SignalSnapshotsUpdated {
                environment_id,
                snapshots,
            } => {
                self.snapshots.insert(
                    environment_id.clone(),
                    snapshots
                        .iter()
                        .map(|snapshot| {
                            let payload = serde_json::from_str(&snapshot.payload_json)
                                .unwrap_or(serde_json::Value::Null);
                            (
                                snapshot.signal_id.clone(),
                                Snapshot {
                                    dto: snapshot.clone(),
                                    payload,
                                },
                            )
                        })
                        .collect(),
                );
            }
            ServerMessage::SignalSamplesUpdated {
                environment_id,
                signal_id,
                points,
            } => {
                self.samples
                    .insert((environment_id.clone(), signal_id.clone()), points.clone());
            }
            ServerMessage::HealthEventsUpdated {
                environment_id,
                events,
            } => {
                self.health_events
                    .insert(environment_id.clone(), events.clone());
            }
            ServerMessage::SignalRollupsUpdated {
                environment_id,
                signal_id,
                points,
            } => {
                self.rollups
                    .insert((environment_id.clone(), signal_id.clone()), points.clone());
            }
            _ => {}
        }
    }

    pub fn select(&mut self, id: &str) {
        if self
            .environments
            .iter()
            .any(|environment| environment.id == id)
        {
            self.selected_id = Some(id.to_owned());
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    pub fn environment_label(&self, id: &str) -> Option<&str> {
        self.environments
            .iter()
            .find(|environment| environment.id == id)
            .map(|environment| environment.label.as_str())
    }

    pub fn selected(&self) -> Option<&EnvironmentSummary> {
        let id = self.selected_id.as_deref()?;
        self.environments
            .iter()
            .find(|environment| environment.id == id)
    }

    /// Clicking the open card closes the Drill-in; selecting an Environment
    /// keeps the card open so the same Signal can be compared across them.
    pub fn select_card(&mut self, signal_id: &str) {
        let Some(&id) = SIGNAL_IDS.iter().find(|&&id| id == signal_id) else {
            return;
        };
        self.selected_card = if self.selected_card == Some(id) {
            None
        } else {
            Some(id)
        };
    }

    pub fn selected_card(&self) -> Option<&'static str> {
        self.selected_card
    }

    pub fn trend_window(&self) -> TrendWindow {
        self.trend_window
    }

    /// Environments needing attention right now: observed, degraded or down,
    /// and not Operator-muted. Drives the Dock badge count (`107`).
    pub fn troubled_count(&self, now: i64) -> usize {
        if !self.connected {
            return 0;
        }
        self.environments
            .iter()
            .filter(|environment| environment.last_observed_at.is_some())
            .filter(|environment| !self.is_muted(&environment.id, now))
            .filter(|environment| {
                matches!(
                    environment.health,
                    EnvironmentHealth::Degraded | EnvironmentHealth::Down
                )
            })
            .count()
    }

    pub fn set_trend_window(&mut self, window: TrendWindow) {
        self.trend_window = window;
    }

    /// Opens the Drill-in for a Signal without toggling: selecting an
    /// Environment from the compare strip, a notification or the menu bar
    /// always lands on the drift card, never closes it.
    pub fn open_card(&mut self, signal_id: &str) {
        if SIGNAL_IDS.contains(&signal_id) {
            self.selected_card = SIGNAL_IDS.iter().find(|&&id| id == signal_id).copied();
        }
    }

    /// Plain-text state block for ⌘⇧C: Environment, health, freshness, one
    /// line per Signal, the build. Pasted into Slack or a ticket.
    pub fn summary_text(&self, now: i64) -> String {
        let Some(selected) = self.selected() else {
            return "daku — no Environment selected".to_owned();
        };
        let health = match selected.health {
            EnvironmentHealth::Healthy => "healthy",
            EnvironmentHealth::Degraded => "degraded",
            EnvironmentHealth::Down => "down",
        };
        let fresh = freshness(selected.last_observed_at, now);
        let mut lines = vec![format!(
            "{} ({}) — {}{}{}",
            selected.id,
            selected.label,
            health,
            if self.is_muted(&selected.id, now) {
                " (muted)"
            } else {
                ""
            },
            format!(", {}", fresh.label),
        )];
        for signal_id in SIGNAL_IDS {
            let status = self
                .snapshots
                .get(&selected.id)
                .and_then(|map| map.get(signal_id))
                .map(|snapshot| snapshot.dto.state.as_str())
                .unwrap_or(WAITING);
            let summary = self.card_summary(signal_id);
            let detail = self.card_detail(signal_id);
            let body = if !summary.is_empty() { summary } else { detail };
            lines.push(if body.is_empty() {
                format!("{}: {status}", signal_label(signal_id))
            } else {
                format!("{}: {status} — {body}", signal_label(signal_id))
            });
        }
        lines.push(format!(
            "Build: {}",
            environment_build(&self.snapshots, &selected.id).unwrap_or_else(|| "—".to_owned())
        ));
        lines.join("\n")
    }

    /// Deep link into the ServiceNow list the Signal is measured from, mirroring
    /// the collectors' encoded queries. `None` without a selected Environment.
    pub fn signal_url(&self, signal_id: &str) -> Option<String> {
        let path = match signal_id {
            "availability" => "/sys_properties_list.do?sysparm_query=name=glide.war",
            "jobs" => {
                "/sys_trigger_list.do?sysparm_query=state=0^next_action<javascript:gs.minutesAgoStart(15)"
            }
            "syslog" => {
                "/syslog_list.do?sysparm_query=level=2^sys_created_on>javascript:gs.hoursAgoStart(1)"
            }
            "mid_ecc" => "/ecc_agent_list.do",
            "outbound" => {
                "/sys_outbound_http_log_list.do?sysparm_query=http_status>=400^sys_created_on>javascript:gs.hoursAgoStart(1)"
            }
            "flow" => {
                "/sys_flow_context_list.do?sysparm_query=state=ERROR^sys_updated_on>javascript:gs.hoursAgoStart(1)"
            }
            "email" => {
                "/sys_email_list.do?sysparm_query=type=send-failed^sys_created_on>javascript:gs.hoursAgoStart(1)"
            }
            "upgrade" => "/sys_upgrade_history_list.do",
            "sessions" => "/v_user_session_list.do",
            "drift" => "/v_plugin_list.do",
            "last_clone" => "/clone_instance_list.do",
            _ => return None,
        };
        let instance_url = &self.selected()?.instance_url;
        // The daemon validates this when it loads environments.json, but the
        // desktop can be attached to a daemon it does not own
        // (DAKU_DAEMON_ADDRESS), and this string reaches the OS URL opener.
        if !is_supported_instance_url(instance_url) {
            return None;
        }
        let base = instance_url.trim_end_matches('/');
        Some(format!("{base}{}", encode_query(path)))
    }

    pub fn drill_in(&self, signal_id: &str, now: i64) -> DrillIn {
        let missing = serde_json::Value::Null;
        let value = self
            .selected_id
            .as_deref()
            .and_then(|environment_id| self.snapshots.get(environment_id))
            .and_then(|map| map.get(signal_id))
            .map_or(&missing, |snapshot| &snapshot.payload);
        let text = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(|item| item.as_str())
                .unwrap_or("\u{2014}")
                .to_owned()
        };
        match signal_id {
            "drift" => {
                let Some(list) = value.get("mismatch_list").and_then(|item| item.as_array()) else {
                    return self.drill_in_text(signal_id);
                };
                DrillIn::Rows {
                    headers: vec!["Plugin", "Source", "Here"],
                    rows: list
                        .iter()
                        .take(DRILL_IN_ROW_LIMIT)
                        .map(|entry| DrillInRow {
                            cells: vec![
                                text(entry, "id"),
                                text(entry, "source_version"),
                                text(entry, "other_version"),
                            ],
                            link: None,
                        })
                        .collect(),
                    truncated: value.get("mismatch_list_truncated")
                        == Some(&serde_json::Value::Bool(true))
                        || list.len() > DRILL_IN_ROW_LIMIT,
                }
            }
            "mid_ecc" => {
                let Some(list) = value
                    .get("agents_unhealthy_list")
                    .and_then(|item| item.as_array())
                    .filter(|list| !list.is_empty())
                else {
                    return self.drill_in_text(signal_id);
                };
                DrillIn::Rows {
                    headers: vec!["MID", "Status", "Version"],
                    rows: list
                        .iter()
                        .take(DRILL_IN_ROW_LIMIT)
                        .map(|entry| DrillInRow {
                            cells: vec![
                                text(entry, "host_name"),
                                text(entry, "status"),
                                text(entry, "version"),
                            ],
                            link: None,
                        })
                        .collect(),
                    truncated: value.get("agents_unhealthy_list_truncated")
                        == Some(&serde_json::Value::Bool(true))
                        || list.len() > DRILL_IN_ROW_LIMIT,
                }
            }
            "last_clone" => {
                if value
                    .get("completed")
                    .and_then(|item| item.as_str())
                    .is_none()
                {
                    return self.drill_in_text(signal_id);
                }
                DrillIn::Rows {
                    headers: vec!["Completed", "Age", "Source"],
                    rows: vec![DrillInRow {
                        cells: vec![
                            text(value, "completed"),
                            summarize_value(signal_id, value),
                            text(value, "source_id"),
                        ],
                        link: None,
                    }],
                    truncated: false,
                }
            }
            "jobs" => {
                if let Some(rows) = self.job_rows(value) {
                    rows
                } else {
                    self.drill_in_trend(signal_id, now)
                }
            }
            "syslog" => {
                if let Some(rows) = self.syslog_rows(value) {
                    rows
                } else {
                    self.drill_in_trend(signal_id, now)
                }
            }
            "outbound" => self
                .outbound_rows(value)
                .unwrap_or_else(|| self.drill_in_text(signal_id)),
            "flow" => self
                .flow_rows(value)
                .unwrap_or_else(|| self.drill_in_text(signal_id)),
            "email" => self
                .email_rows(value)
                .unwrap_or_else(|| self.drill_in_text(signal_id)),
            "upgrade" => self
                .upgrade_rows(value)
                .unwrap_or_else(|| self.drill_in_text(signal_id)),
            "availability" => self.drill_in_trend(signal_id, now),
            _ => self.drill_in_text(signal_id),
        }
    }

    fn drill_in_trend(&self, signal_id: &str, now: i64) -> DrillIn {
        // 24 h renders the raw samples; 7 d / 30 d render hourly roll-up
        // averages filtered to the window. Either needs two points to draw.
        let points: Vec<f64> = match self.trend_window {
            TrendWindow::Day24 => self
                .samples
                .get(&(
                    self.selected_id.clone().unwrap_or_default(),
                    signal_id.to_owned(),
                ))
                .map(|points| {
                    points
                        .iter()
                        .map(|point| point.value_real.unwrap_or(0.0))
                        .collect()
                })
                .unwrap_or_default(),
            window @ (TrendWindow::Day7 | TrendWindow::Day30) => {
                let cutoff = window.cutoff_secs(now);
                self.rollups
                    .get(&(
                        self.selected_id.clone().unwrap_or_default(),
                        signal_id.to_owned(),
                    ))
                    .map(|points| {
                        points
                            .iter()
                            .filter(|point| point.hour_start >= cutoff)
                            .map(|point| point.avg_real.unwrap_or(0.0))
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };
        if points.len() < 2 {
            self.drill_in_text(signal_id)
        } else {
            DrillIn::Trend(points)
        }
    }

    /// Job rows the daemon fetched while unhealthy (`101`): overdue first,
    /// then errors. Each row links its `sys_trigger` record. `None` when
    /// there are no rows, so the trend renders instead.
    fn job_rows(&self, value: &serde_json::Value) -> Option<DrillIn> {
        let mut rows: Vec<DrillInRow> = Vec::new();
        let mut truncated = false;
        for (list_key, trunc_key) in [
            ("overdue_rows", "overdue_rows_truncated"),
            ("error_rows", "error_rows_truncated"),
        ] {
            truncated |= value.get(trunc_key) == Some(&serde_json::Value::Bool(true));
            let Some(list) = value.get(list_key).and_then(|item| item.as_array()) else {
                continue;
            };
            rows.extend(list.iter().take(DRILL_IN_ROW_LIMIT).map(|entry| {
                let sys_id = entry
                    .get("sys_id")
                    .and_then(|item| item.as_str())
                    .unwrap_or("");
                // The collector persists each row as {sys_id, name,
                // detail} — next_action for overdue rows, state for errors.
                DrillInRow {
                    cells: vec![cell(entry, "name"), cell(entry, "detail")],
                    link: self.record_url("sys_trigger", sys_id),
                }
            }));
        }
        if rows.is_empty() {
            return None;
        }
        rows.truncate(DRILL_IN_ROW_LIMIT);
        let full_page = rows.len() >= DRILL_IN_ROW_LIMIT;
        Some(DrillIn::Rows {
            headers: vec!["Job", "Detail"],
            rows,
            truncated: truncated || full_page,
        })
    }

    /// Syslog error rows the daemon fetched while unhealthy. `None` when
    /// there are none, so the trend renders instead.
    fn syslog_rows(&self, value: &serde_json::Value) -> Option<DrillIn> {
        let list = value
            .get("error_rows")
            .and_then(|item| item.as_array())
            .filter(|list| !list.is_empty())?;
        Some(DrillIn::Rows {
            headers: vec!["Time", "Source", "Message"],
            rows: list
                .iter()
                .take(DRILL_IN_ROW_LIMIT)
                .map(|entry| DrillInRow {
                    cells: vec![
                        cell(entry, "sys_created_on"),
                        cell(entry, "source"),
                        cell(entry, "message"),
                    ],
                    link: None,
                })
                .collect(),
            truncated: value.get("error_rows_truncated") == Some(&serde_json::Value::Bool(true))
                || list.len() > DRILL_IN_ROW_LIMIT,
        })
    }

    /// Outbound failure rows the daemon fetched while unhealthy.
    fn outbound_rows(&self, value: &serde_json::Value) -> Option<DrillIn> {
        let list = value
            .get("failure_rows")
            .and_then(|item| item.as_array())
            .filter(|list| !list.is_empty())?;
        Some(DrillIn::Rows {
            headers: vec!["Time", "URL", "Status"],
            rows: list
                .iter()
                .take(DRILL_IN_ROW_LIMIT)
                .map(|entry| DrillInRow {
                    cells: vec![
                        cell(entry, "sys_created_on"),
                        url_host(cell(entry, "url")),
                        cell(entry, "http_status"),
                    ],
                    link: None,
                })
                .collect(),
            truncated: value.get("failure_rows_truncated") == Some(&serde_json::Value::Bool(true))
                || list.len() > DRILL_IN_ROW_LIMIT,
        })
    }

    /// Flow error rows the daemon fetched while unhealthy. Each row links
    /// its `sys_flow_context` record.
    fn flow_rows(&self, value: &serde_json::Value) -> Option<DrillIn> {
        let list = value
            .get("error_rows")
            .and_then(|item| item.as_array())
            .filter(|list| !list.is_empty())?;
        Some(DrillIn::Rows {
            headers: vec!["Flow", "Updated"],
            rows: list
                .iter()
                .take(DRILL_IN_ROW_LIMIT)
                .map(|entry| {
                    let sys_id = entry
                        .get("sys_id")
                        .and_then(|item| item.as_str())
                        .unwrap_or("");
                    DrillInRow {
                        cells: vec![cell(entry, "name"), cell(entry, "sys_updated_on")],
                        link: self.record_url("sys_flow_context", sys_id),
                    }
                })
                .collect(),
            truncated: value.get("error_rows_truncated") == Some(&serde_json::Value::Bool(true))
                || list.len() > DRILL_IN_ROW_LIMIT,
        })
    }

    /// Email failure rows the daemon fetched while unhealthy. Each row links
    /// its `sys_email` record.
    fn email_rows(&self, value: &serde_json::Value) -> Option<DrillIn> {
        let list = value
            .get("error_rows")
            .and_then(|item| item.as_array())
            .filter(|list| !list.is_empty())?;
        Some(DrillIn::Rows {
            headers: vec!["Subject", "To", "Time"],
            rows: list
                .iter()
                .take(DRILL_IN_ROW_LIMIT)
                .map(|entry| {
                    let sys_id = entry
                        .get("sys_id")
                        .and_then(|item| item.as_str())
                        .unwrap_or("");
                    DrillInRow {
                        cells: vec![
                            cell(entry, "subject"),
                            cell(entry, "recipients"),
                            cell(entry, "sys_created_on"),
                        ],
                        link: self.record_url("sys_email", sys_id),
                    }
                })
                .collect(),
            truncated: value.get("error_rows_truncated") == Some(&serde_json::Value::Bool(true))
                || list.len() > DRILL_IN_ROW_LIMIT,
        })
    }

    /// Upgrade history rows. Each row links its `sys_upgrade_history` record.
    fn upgrade_rows(&self, value: &serde_json::Value) -> Option<DrillIn> {
        let list = value
            .get("upgrades")
            .and_then(|item| item.as_array())
            .filter(|list| !list.is_empty())?;
        Some(DrillIn::Rows {
            headers: vec!["From", "To", "Finished"],
            rows: list
                .iter()
                .take(DRILL_IN_ROW_LIMIT)
                .map(|entry| {
                    let sys_id = entry
                        .get("sys_id")
                        .and_then(|item| item.as_str())
                        .unwrap_or("");
                    DrillInRow {
                        cells: vec![
                            cell(entry, "from_version"),
                            cell(entry, "to_version"),
                            cell(entry, "upgrade_finished"),
                        ],
                        link: self.record_url("sys_upgrade_history", sys_id),
                    }
                })
                .collect(),
            truncated: value.get("upgrades_truncated") == Some(&serde_json::Value::Bool(true))
                || list.len() > DRILL_IN_ROW_LIMIT,
        })
    }

    /// Deep link to one ServiceNow record. `None` without a selected
    /// Environment, an untrusted URL, or an empty id.
    fn record_url(&self, table: &str, sys_id: &str) -> Option<String> {
        if sys_id.is_empty() {
            return None;
        }
        let instance_url = &self.selected()?.instance_url;
        if !is_supported_instance_url(instance_url) {
            return None;
        }
        let base = instance_url.trim_end_matches('/');
        Some(format!("{base}/{table}.do?sys_id={sys_id}"))
    }

    fn drill_in_text(&self, signal_id: &str) -> DrillIn {
        for line in [self.card_detail(signal_id), self.card_summary(signal_id)] {
            if !line.is_empty() {
                return DrillIn::Text(line);
            }
        }
        DrillIn::Empty
    }

    pub fn sidebar(&self, now: i64) -> Vec<SidebarRow> {
        self.environments
            .iter()
            .map(|environment| SidebarRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                health: environment.health,
                dimmed: !self.connected || environment.last_observed_at.is_none(),
                muted: self.is_muted(&environment.id, now),
            })
            .collect()
    }

    pub fn cards(&self, now: i64) -> Vec<SignalCard> {
        let environment_id = self.selected_id.as_deref().unwrap_or("");
        let snapshots = self.snapshots.get(environment_id);
        let muted = self
            .selected_id
            .as_deref()
            .is_some_and(|id| self.is_muted(id, now));
        let mut cards = SIGNAL_IDS
            .iter()
            .map(|&signal_id| {
                let snapshot = snapshots.and_then(|map| map.get(signal_id));
                let sparkline = if TREND_SIGNALS.contains(&signal_id) {
                    self.samples
                        .get(&(environment_id.to_owned(), signal_id.to_owned()))
                        .map(|points| {
                            points
                                .iter()
                                .map(|point| point.value_real.unwrap_or(0.0))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let status = match snapshot {
                    // Zero MID servers is "nothing to measure", not health.
                    Some(snapshot)
                        if signal_id == "mid_ecc"
                            && snapshot
                                .payload
                                .get("agents_total")
                                .and_then(|v| v.as_u64())
                                == Some(0) =>
                    {
                        "unknown".to_owned()
                    }
                    Some(snapshot) => snapshot.dto.state.clone(),
                    None => WAITING.to_owned(),
                };
                SignalCard {
                    signal_id,
                    status,
                    sparkline,
                    dimmed: !self.connected,
                    muted,
                }
            })
            .collect::<Vec<_>>();
        // Problems first, then the rest in Signal order; unconfigured probes
        // last so they stop competing with live ones. Stable sort keeps
        // `SIGNAL_IDS` order within a rank.
        cards.sort_by_key(|card| severity_rank(&card.status));
        cards
    }

    pub fn compare_strip(&self) -> CompareStrip {
        if self.environments.len() < 2 {
            return CompareStrip {
                visible: false,
                has_mismatch: false,
            };
        }
        let source_id = self.clone_source_id();
        let reference = self.reference_build();
        let build_mismatch = reference.as_ref().is_some_and(|reference| {
            self.environments.iter().any(|environment| {
                environment_build(&self.snapshots, &environment.id)
                    .is_some_and(|build| &build != reference)
            })
        });
        let plugin_mismatch = self.environments.iter().any(|environment| {
            source_id != Some(environment.id.as_str())
                && self
                    .snapshots
                    .get(&environment.id)
                    .and_then(|map| map.get("drift"))
                    .is_some_and(|snapshot| drift_mismatch(&snapshot.payload))
        });
        let has_mismatch = build_mismatch || plugin_mismatch;
        CompareStrip {
            visible: true,
            has_mismatch,
        }
    }

    /// The build the Compare strip measures every Environment against: the
    /// clone source's, or — when there is no clone source or its build is
    /// unknown — the first known build in Environment order.
    fn reference_build(&self) -> Option<String> {
        self.clone_source_id()
            .and_then(|id| environment_build(&self.snapshots, id))
            .or_else(|| {
                self.environments
                    .iter()
                    .find_map(|environment| environment_build(&self.snapshots, &environment.id))
            })
    }

    fn clone_source_id(&self) -> Option<&str> {
        self.environments
            .iter()
            .find(|environment| {
                let Some(snapshot) = self
                    .snapshots
                    .get(&environment.id)
                    .and_then(|map| map.get("drift"))
                else {
                    return false;
                };
                snapshot.payload.get("role").and_then(|item| item.as_str()) == Some("source")
            })
            .map(|environment| environment.id.as_str())
    }

    pub fn card_summary(&self, signal_id: &str) -> String {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return String::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
        else {
            return String::new();
        };
        let summary = summarize_value(signal_id, &snapshot.payload);
        // The build string is inventory, so it lives with the plugin inventory,
        // not under the latency number.
        if signal_id == "drift" && !summary.is_empty() {
            let build = self
                .snapshots
                .get(environment_id)
                .and_then(|map| map.get("availability"))
                .and_then(|snapshot| snapshot.payload.get("build"))
                .and_then(|build| build.as_str());
            if let Some(build) = build {
                return format!("{summary} \u{b7} {build}");
            }
        }
        summary
    }

    /// What to do about a skipped probe, when it is a configuration matter
    /// rather than the instance's state. Empty otherwise.
    pub fn card_hint(&self, signal_id: &str) -> &'static str {
        let reason = self
            .selected_id
            .as_deref()
            .and_then(|environment_id| self.snapshots.get(environment_id))
            .and_then(|map| map.get(signal_id))
            .and_then(|snapshot| snapshot.payload.get("skipped"))
            .and_then(|reason| reason.as_str());
        match reason {
            Some("no_clone_source") => "mark one Environment \"clone_source\": true",
            Some("need_two_environments") => "add a second Environment",
            _ => "",
        }
    }

    /// The worst health across observed, unmuted Environments — what the
    /// menu-bar dot, Dock badge and notifications consult. `None` when
    /// disconnected, nothing polled yet, or everything muted.
    pub fn worst_health_excluding_muted(&self, now: i64) -> Option<EnvironmentHealth> {
        if !self.connected {
            return None;
        }
        self.environments
            .iter()
            .filter(|environment| environment.last_observed_at.is_some())
            .filter(|environment| !self.is_muted(&environment.id, now))
            .map(|environment| environment.health)
            .max_by_key(|health| match health {
                EnvironmentHealth::Healthy => 0,
                EnvironmentHealth::Degraded => 1,
                EnvironmentHealth::Down => 2,
            })
    }

    /// Worst degraded/down line for one Environment: "{Signal label}:
    /// {summary}". Drives notification bodies. `None` when nothing needs
    /// attention (in particular on recovery — the caller then says so).
    pub fn headline_for(&self, env_id: &str) -> Option<(EnvironmentHealth, String)> {
        let environment = self.environments.iter().find(|env| env.id == env_id)?;
        let snapshots = self.snapshots.get(env_id)?;
        for signal_id in SIGNAL_IDS {
            let snapshot = snapshots.get(signal_id)?;
            if !matches!(snapshot.dto.state.as_str(), "degraded" | "down") {
                continue;
            }
            let summary = summarize_value(signal_id, &snapshot.payload);
            let detail = detail_from_value(signal_id, &snapshot.payload);
            let body = if !summary.is_empty() { summary } else { detail };
            let line = if body.is_empty() {
                signal_label(signal_id).to_owned()
            } else {
                format!("{}: {body}", signal_label(signal_id))
            };
            return Some((environment.health, line));
        }
        None
    }

    /// One-line diagnostic for the selected Environment's Signal: the daemon's
    /// persisted `error` (availability) or `detail` (other Signals) string,
    /// or a human phrase for a skipped probe. Empty when there is nothing to say.
    pub fn card_detail(&self, signal_id: &str) -> String {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return String::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
        else {
            return String::new();
        };
        detail_from_value(signal_id, &snapshot.payload)
    }

    /// Up to `limit` human lines ("id: 1.0.0 → 1.1.0", "id: missing here",
    /// "id: only here") for the selected Environment's drift snapshot, plus a
    /// "… and N more" line when the exact count exceeds the lines shown.
    /// Empty for the clone source and when there is no drift.
    pub fn drift_mismatch_lines(&self, limit: usize) -> Vec<String> {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return Vec::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get("drift"))
        else {
            return Vec::new();
        };
        let value = &snapshot.payload;
        let Some(list) = value.get("mismatch_list").and_then(|item| item.as_array()) else {
            return Vec::new();
        };
        let mut lines: Vec<String> = list
            .iter()
            .take(limit)
            .map(|entry| {
                let id = entry
                    .get("id")
                    .and_then(|item| item.as_str())
                    .unwrap_or("?");
                let version = |key: &str| {
                    entry
                        .get(key)
                        .and_then(|item| item.as_str())
                        .map(str::to_owned)
                };
                match (version("source_version"), version("other_version")) {
                    (Some(source), Some(other)) => format!("{id}: {source} → {other}"),
                    (Some(_), None) => format!("{id}: missing here"),
                    (None, Some(_)) => format!("{id}: only here"),
                    (None, None) => id.to_owned(),
                }
            })
            .collect();
        // `mismatches` stays exact even when the persisted list is capped.
        let total = value
            .get("mismatches")
            .and_then(|item| item.as_u64())
            .unwrap_or(list.len() as u64);
        let remaining = total.saturating_sub(lines.len() as u64);
        if remaining > 0 {
            lines.push(format!("… and {remaining} more"));
        }
        lines
    }

    pub fn compare_rows(&self) -> Vec<CompareRow> {
        let reference = self.reference_build();
        self.environments
            .iter()
            .map(|environment| {
                let build = environment_build(&self.snapshots, &environment.id);
                CompareRow {
                    id: environment.id.clone(),
                    label: environment.label.clone(),
                    mismatch: matches!(
                        (&build, &reference),
                        (Some(build), Some(reference)) if build != reference
                    ),
                    build,
                    drift: self.signal_summary(&environment.id, "drift"),
                    last_clone: self.signal_summary(&environment.id, "last_clone"),
                }
            })
            .collect()
    }

    fn signal_summary(&self, environment_id: &str, signal_id: &str) -> String {
        self.snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
            .map(|snapshot| summarize_value(signal_id, &snapshot.payload))
            .unwrap_or_default()
    }
}

/// Compact age without a prefix: "40 s", "45 min", "3 h", "12 d".
/// Recent timelines and build age share it; `freshness` keeps its own
/// "polled … ago" phrasing untouched.
pub fn age_phrase(age_secs: i64) -> String {
    let age = age_secs.max(0);
    if age < 60 {
        format!("{age} s")
    } else if age < 3600 {
        format!("{} min", age / 60)
    } else if age < 86_400 {
        format!("{} h", age / 3600)
    } else {
        format!("{} d", age / 86_400)
    }
}

/// One Recent-timeline line for a health/build event: "healthy → degraded,
/// 12 min ago" / "build glide-…, 3 d ago". Pure and unit-tested; the shell
/// only places the lines.
pub fn format_health_event(event: &HealthEventDto, now: i64) -> String {
    let ago = age_phrase(now.saturating_sub(event.observed_at));
    match event.kind {
        HealthEventKind::Health => {
            let from = event.from_health.map(health_word).unwrap_or("?");
            format!("{from} → {}, {ago} ago", health_word(event.to_health))
        }
        HealthEventKind::Build => {
            let build = event.build.as_deref().unwrap_or("?");
            format!("build {build}, {ago} ago")
        }
    }
}

fn health_word(health: EnvironmentHealth) -> &'static str {
    match health {
        EnvironmentHealth::Healthy => "healthy",
        EnvironmentHealth::Degraded => "degraded",
        EnvironmentHealth::Down => "down",
    }
}

/// "Muted 45m left" / "Muted 3h left" for the Environment header. Pure so
/// the shell never formats clocks itself.
pub fn mute_remaining_label(until: i64, now: i64) -> String {
    let left = until.saturating_sub(now).max(0);
    if left < 3600 {
        format!("Muted {}m left", (left / 60).max(1))
    } else {
        format!("Muted {}h left", left / 3600)
    }
}

/// One drill-in table cell: the string value or an em dash.
fn cell(entry: &serde_json::Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(|item| item.as_str())
        .filter(|text| !text.is_empty())
        .unwrap_or("\u{2014}")
        .to_owned()
}

/// Host (plus path) of a URL for the outbound table. The collector already
/// stripped query and fragment; this keeps the cell to what identifies the
/// integration.
fn url_host(url: String) -> String {
    if url == "\u{2014}" {
        return url;
    }
    url.split("://")
        .nth(1)
        .unwrap_or(&url)
        .split('/')
        .next()
        .filter(|host| !host.is_empty())
        .unwrap_or("\u{2014}")
        .to_owned()
}

/// Lower sorts first on the card grid.
pub fn severity_rank(status: &str) -> u8 {
    match status {
        "down" => 0,
        "degraded" => 1,
        WAITING => 2,
        "healthy" => 3,
        "skipped" => 5,
        _ => 4,
    }
}

fn environment_build(
    snapshots: &HashMap<String, HashMap<String, Snapshot>>,
    environment_id: &str,
) -> Option<String> {
    snapshots
        .get(environment_id)?
        .get("availability")?
        .payload
        .get("build")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn drift_mismatch(value: &serde_json::Value) -> bool {
    value.get("build_matches") == Some(&serde_json::Value::Bool(false))
        || value
            .get("mismatches")
            .and_then(|item| item.as_u64())
            .is_some_and(|count| count > 0)
}

/// `Value::Null` stands in for a payload that did not parse, and an
/// unreadable payload has nothing to summarize — without this guard the
/// counting arms below would report a confident "0 overdue · 0 error".
fn summarize_value(signal_id: &str, value: &serde_json::Value) -> String {
    if value.is_null() {
        return String::new();
    }
    if value.get("skipped").is_some() {
        return String::new();
    }
    // `persist_signal_down` writes `{reachability, detail}` for any failed
    // probe — no counts at all — so the counting arms would invent a "0
    // overdue · 0 error". Availability's own unreachable payload carries
    // `error` (and a real `rtt_ms`), never `detail`, so it stays summarised.
    if value.get("reachability").and_then(|item| item.as_str()) == Some("unreachable")
        && value.get("detail").is_some()
    {
        return String::new();
    }
    match signal_id {
        "availability" => match (
            value.get("rtt_ms").and_then(|item| item.as_u64()),
            value.get("build").and_then(|item| item.as_str()),
        ) {
            (Some(ms), _) => format!("{ms} ms"),
            (None, Some(build)) => build.to_owned(),
            _ => String::new(),
        },
        "jobs" => format!(
            "{} overdue · {} in error",
            value
                .get("overdue_ready")
                .and_then(|item| item.as_u64())
                .unwrap_or(0),
            value
                .get("error")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "syslog" => format!(
            "{} errors · last hour",
            value
                .get("error_count_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "mid_ecc" => {
            let total = value
                .get("agents_total")
                .and_then(|item| item.as_u64())
                .unwrap_or(0);
            let unhealthy = value
                .get("agents_unhealthy")
                .and_then(|item| item.as_u64())
                .unwrap_or(0);
            if total == 0 {
                return "no MID servers".into();
            }
            format!(
                "{}/{} MID up · queue {}",
                total.saturating_sub(unhealthy),
                total,
                value
                    .get("ecc_output_ready")
                    .and_then(|item| item.as_u64())
                    .unwrap_or(0)
            )
        }
        "outbound" => format!(
            "{} HTTP failures · last hour",
            value
                .get("outbound_http_4xx_5xx_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "flow" => format!(
            "{} flow errors · last hour",
            value
                .get("flow_error_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "email" => format!(
            "{} email failures · last hour",
            value
                .get("email_failed_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "upgrade" => {
            if let Some(failed) = value.get("failed_7d").and_then(|item| item.as_u64())
                && failed > 0
            {
                format!("{failed} failed · last 7d")
            } else if let Some(to) = value.get("last_to").and_then(|item| item.as_str()) {
                match value.get("last_age_days").and_then(|item| item.as_i64()) {
                    Some(0) => format!("{to} · today"),
                    Some(1) => format!("{to} · 1 day ago"),
                    Some(days) => format!("{to} · {days} days ago"),
                    None => to.to_owned(),
                }
            } else {
                "no upgrades found".into()
            }
        }
        "sessions" => {
            let count = value
                .get("active_sessions")
                .and_then(|item| item.as_u64())
                .unwrap_or(0);
            if value.get("truncated").and_then(|item| item.as_bool()) == Some(true) {
                "100+ active sessions".into()
            } else if count == 1 {
                "1 active session".into()
            } else {
                format!("{count} active sessions")
            }
        }
        "drift" => {
            if value.get("role").and_then(|item| item.as_str()) == Some("source") {
                "source of truth".into()
            } else if let Some(count) = value.get("mismatches").and_then(|item| item.as_u64()) {
                let expected = value
                    .get("expected_mismatches")
                    .and_then(|item| item.as_u64())
                    .unwrap_or(0);
                if expected > 0 {
                    format!("{count} differ · {expected} expected")
                } else {
                    format!("{count} plugins differ")
                }
            } else {
                String::new()
            }
        }
        "last_clone" => {
            if value.get("role").and_then(|item| item.as_str()) == Some("source") {
                // `supported: false` is the source's own 403: it cannot list
                // clones, so no target will ever get an answer from it.
                if value.get("supported") == Some(&serde_json::Value::Bool(false)) {
                    "clone source \u{b7} cannot list clones".into()
                } else {
                    "clone source".into()
                }
            } else if let Some(days) = value.get("age_days").and_then(|item| item.as_i64()) {
                match days {
                    0 => "today".into(),
                    1 => "1 day ago".into(),
                    days => format!("{days} days ago"),
                }
            } else if value.get("unknown").and_then(|item| item.as_str()) == Some("older_than_page")
            {
                // Must precede the null-`completed` branch: this payload
                // carries a null `completed` too. The 10 mirrors
                // daku-core's CLONE_PAGE_LIMIT (the client does not depend on
                // that crate).
                "not in the last 10 clones".into()
            } else if value
                .get("completed")
                .is_some_and(serde_json::Value::is_null)
            {
                "no clone found".into()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

fn detail_from_value(signal_id: &str, value: &serde_json::Value) -> String {
    if let Some(reason) = value.get("skipped").and_then(|item| item.as_str()) {
        // The card's main line already reads "skipped"; do not repeat the word.
        return match reason {
            "asleep" => "Environment asleep".to_owned(),
            "unreachable" => "Environment unreachable".to_owned(),
            "need_two_environments" => "needs two Environments".to_owned(),
            "no_clone_source" => "no clone source configured".to_owned(),
            "clone_source_cannot_list_clones" => "clone source cannot list clones".to_owned(),
            "clone_source_unreachable" => "clone source unreachable".to_owned(),
            "clone_source_asleep" => "clone source asleep".to_owned(),
            other => other.to_owned(),
        };
    }
    // Drift's `truncated` says the inventory page was capped, so the mismatch
    // count is a floor. Distinct from `mismatch_list_truncated`, which bounds
    // the drill-in list; keyed on the Signal so no other payload's `truncated`
    // can pick this phrase up.
    if signal_id == "drift" && value.get("truncated").and_then(|item| item.as_bool()) == Some(true)
    {
        return "partial inventory — plugin counts may be incomplete".to_owned();
    }
    ["error", "detail"]
        .iter()
        .find_map(|key| value.get(*key).and_then(|item| item.as_str()))
        .map(|text| text.chars().take(160).collect())
        .unwrap_or_default()
}

/// String-taking wrappers so the tests keep exercising these through the same
/// call shape the wire uses (a payload JSON string), without a `from_str` on
/// any render path.
#[cfg(test)]
fn summarize_payload(signal_id: &str, payload_json: &str) -> String {
    summarize_value(signal_id, &parse_payload(payload_json))
}

#[cfg(test)]
fn detail_from_payload(signal_id: &str, payload_json: &str) -> String {
    detail_from_value(signal_id, &parse_payload(payload_json))
}

#[cfg(test)]
fn parse_payload(payload_json: &str) -> serde_json::Value {
    serde_json::from_str(payload_json).unwrap_or(serde_json::Value::Null)
}

/// The payloads `crates/daku-core` regenerates from its own collectors and
/// pins (see `crates/daku-core/src/payload_contract.rs`). The fixture UI and
/// the payload tests below both read them from here, so neither can drift
/// from what the daemon writes.
const PINNED_PAYLOADS: &str = include_str!("../crates/daku-core/tests/fixtures/payloads.json");

/// One pinned case as the wire would carry it. Panics on an unknown name: the
/// only callers are the fixture below and its tests.
fn pinned(name: &str) -> SignalSnapshotDto {
    let cases: serde_json::Value =
        serde_json::from_str(PINNED_PAYLOADS).expect("pinned payloads parse");
    let case = cases
        .get(name)
        .unwrap_or_else(|| panic!("no pinned payload named {name}"));
    SignalSnapshotDto {
        signal_id: case["signal_id"].as_str().expect("signal_id").to_owned(),
        state: case["state"].as_str().expect("state").to_owned(),
        observed_at: 1_700_000_000,
        payload_json: case["payload"].to_string(),
    }
}

pub fn ui_fixture_enabled() -> bool {
    matches!(std::env::var("DAKU_UI_FIXTURE").as_deref(), Ok("1"))
}

pub fn fixture_events() -> Vec<ServerMessage> {
    vec![
        ServerMessage::EnvironmentsUpdated {
            environments: vec![
                env(
                    "prod",
                    "Production",
                    EnvironmentHealth::Degraded,
                    Reachability::Reachable,
                ),
                env(
                    "test",
                    "Test",
                    EnvironmentHealth::Healthy,
                    Reachability::Asleep,
                ),
            ],
        },
        ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            // No last_clone: prod is the clone source in this fixture and the
            // card stays on Waiting, which the shell must also render.
            snapshots: [
                "availability_reachable",
                "jobs_counts",
                "syslog_count",
                "mid_ecc_healthy",
                "outbound_count",
                "flow_count",
                "email_count",
                "upgrade_failed",
                "sessions_count",
                "drift_source",
            ]
            .map(pinned)
            .into(),
        },
        ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: [
                "availability_reachable_other_build",
                "jobs_zero",
                "syslog_zero",
                "mid_ecc_unhealthy",
                "down_probe_failed",
                "flow_zero",
                "email_zero",
                "upgrade_clean",
                "sessions_zero",
                "drift_compare",
                "last_clone_target_completed",
            ]
            .map(pinned)
            .into(),
        },
        ServerMessage::HealthEventsUpdated {
            environment_id: "prod".into(),
            events: vec![
                HealthEventDto {
                    observed_at: 1_699_913_600,
                    kind: HealthEventKind::Build,
                    from_health: None,
                    to_health: EnvironmentHealth::Degraded,
                    build: Some("glide-zurich-12-18-2025__patch0-hotfix1".into()),
                },
                HealthEventDto {
                    observed_at: 1_699_996_400,
                    kind: HealthEventKind::Health,
                    from_health: Some(EnvironmentHealth::Healthy),
                    to_health: EnvironmentHealth::Degraded,
                    build: None,
                },
            ],
        },
        ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: vec![
                SamplePoint {
                    observed_at: 10,
                    value_real: Some(1.0),
                },
                SamplePoint {
                    observed_at: 20,
                    value_real: Some(2.0),
                },
                SamplePoint {
                    observed_at: 30,
                    value_real: Some(3.0),
                },
            ],
        },
        ServerMessage::SignalRollupsUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            // Real-now-relative hours so every window renders in fixture
            // mode; the 24 h view reads the raw samples above instead.
            points: vec![
                fixture_rollup(200, 8.0),
                fixture_rollup(26, 5.0),
                fixture_rollup(1, 2.0),
            ],
        },
        ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "syslog".into(),
            points: vec![],
        },
        // Test carries syslog samples so the restyled card's status-coloured
        // sparkline is visible somewhere; prod stays empty on purpose.
        ServerMessage::SignalSamplesUpdated {
            environment_id: "test".into(),
            signal_id: "syslog".into(),
            points: vec![
                SamplePoint {
                    observed_at: 10,
                    value_real: Some(9.0),
                },
                SamplePoint {
                    observed_at: 20,
                    value_real: Some(3.0),
                },
                SamplePoint {
                    observed_at: 30,
                    value_real: Some(6.0),
                },
                SamplePoint {
                    observed_at: 40,
                    value_real: Some(4.0),
                },
            ],
        },
    ]
}

/// One rollup point `hours_ago` hours before the current hour, so the
/// 7 d / 30 d drill-in windows have something to draw in fixture mode.
fn fixture_rollup(hours_ago: i64, avg: f64) -> RollupPoint {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(1_700_000_000);
    let hour = now - now % 3600 - hours_ago * 3600;
    RollupPoint {
        hour_start: hour,
        avg_real: Some(avg),
        max_real: Some(avg + 1.0),
        sample_count: 30,
    }
}

fn env(
    id: &str,
    label: &str,
    health: EnvironmentHealth,
    reachability: Reachability,
) -> EnvironmentSummary {
    EnvironmentSummary {
        id: id.into(),
        label: label.into(),
        instance_url: format!("https://{id}.example.service-now.com"),
        platform_id: "servicenow".into(),
        health,
        reachability,
        last_observed_at: Some(1_700_000_000),
        auth_method: daku_protocol::AuthMethod::Basic,
        clone_source: false,
        thresholds: daku_protocol::Thresholds::default(),
        expected_drift: Vec::new(),
        sort_order: 0,
    }
}

#[cfg(test)]
fn snap(signal_id: &str, state: &str, payload_json: &str) -> SignalSnapshotDto {
    SignalSnapshotDto {
        signal_id: signal_id.into(),
        state: state.into(),
        observed_at: 1_700_000_000,
        payload_json: payload_json.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `now` for sidebar()/cards() calls in tests: the fixture observed at
    /// 1_700_000_000, so nothing fixture-loaded is stale and no test mute (set
    /// explicitly per test) is active unless the test sets one.
    const TEST_NOW: i64 = 1_700_000_000;

    /// What the client must render for every payload
    /// `crates/daku-core/tests/fixtures/payloads.json` pins: (case,
    /// `card_summary`, `card_detail`). Add a pinned case there and
    /// `pinned_payloads_render` fails until it is listed here — that is the
    /// point of the pin.
    const RENDERED: [(&str, &str, &str); 35] = [
        ("availability_asleep", "142 ms", ""),
        // The build string is shown with the plugin inventory (drift), not
        // under the latency number.
        ("availability_reachable", "142 ms", ""),
        ("availability_reachable_other_build", "142 ms", ""),
        ("availability_unreachable", "142 ms", "HTTP 429"),
        // A failed probe carries no counts, so there is no summary to render:
        // the card falls back to its status word and the detail says why.
        ("down_probe_failed", "", "HTTP 429"),
        ("drift_compare", "3 plugins differ", ""),
        ("drift_source", "source of truth", ""),
        ("jobs_counts", "2 overdue \u{b7} 0 in error", ""),
        ("jobs_zero", "0 overdue \u{b7} 0 in error", ""),
        (
            "last_clone_source_cannot_list",
            "clone source \u{b7} cannot list clones",
            "",
        ),
        ("last_clone_source_supported", "clone source", ""),
        ("last_clone_target_completed", "12 days ago", ""),
        ("last_clone_target_never", "no clone found", ""),
        (
            "last_clone_target_older_than_page",
            "not in the last 10 clones",
            "",
        ),
        ("mid_ecc_healthy", "3/3 MID up \u{b7} queue 2", ""),
        ("mid_ecc_unhealthy", "1/3 MID up \u{b7} queue 2", ""),
        ("outbound_count", "3 HTTP failures \u{b7} last hour", ""),
        ("outbound_zero", "0 HTTP failures \u{b7} last hour", ""),
        ("flow_count", "2 flow errors \u{b7} last hour", ""),
        ("flow_zero", "0 flow errors \u{b7} last hour", ""),
        ("email_count", "2 email failures \u{b7} last hour", ""),
        ("email_zero", "0 email failures \u{b7} last hour", ""),
        ("upgrade_failed", "1 failed \u{b7} last 7d", ""),
        ("upgrade_clean", "Zurich P1 \u{b7} today", ""),
        ("sessions_count", "3 active sessions", ""),
        ("sessions_zero", "0 active sessions", ""),
        ("skipped_asleep", "", "Environment asleep"),
        ("skipped_clone_source_asleep", "", "clone source asleep"),
        (
            "skipped_clone_source_cannot_list_clones",
            "",
            "clone source cannot list clones",
        ),
        (
            "skipped_clone_source_unreachable",
            "",
            "clone source unreachable",
        ),
        (
            "skipped_need_two_environments",
            "",
            "needs two Environments",
        ),
        ("skipped_no_clone_source", "", "no clone source configured"),
        ("skipped_unreachable", "", "Environment unreachable"),
        ("syslog_count", "4 errors \u{b7} last hour", ""),
        ("syslog_zero", "0 errors \u{b7} last hour", ""),
    ];

    /// One Environment carrying one pinned snapshot, selected.
    fn with_pinned(case: &str) -> (DashboardState, String) {
        let snapshot = pinned(case);
        let signal_id = snapshot.signal_id.clone();
        let mut state = DashboardState::new();
        state.set_connected(true);
        state.apply_all(&[
            ServerMessage::EnvironmentsUpdated {
                environments: vec![env(
                    "e",
                    "E",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                )],
            },
            ServerMessage::SignalSnapshotsUpdated {
                environment_id: "e".into(),
                snapshots: vec![snapshot],
            },
        ]);
        state.select("e");
        (state, signal_id)
    }

    #[test]
    fn pinned_payloads_render() {
        let cases: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(PINNED_PAYLOADS).unwrap();
        let mut listed: Vec<&str> = RENDERED.iter().map(|(case, ..)| *case).collect();
        listed.sort_unstable();
        assert_eq!(
            listed,
            cases.keys().map(String::as_str).collect::<Vec<_>>(),
            "every pinned payload needs a rendering here, and vice versa"
        );
        for (case, summary, detail) in RENDERED {
            let (state, signal_id) = with_pinned(case);
            assert_eq!(state.card_summary(&signal_id), summary, "{case} summary");
            assert_eq!(state.card_detail(&signal_id), detail, "{case} detail");
            assert!(
                !summary.is_empty() || !detail.is_empty(),
                "{case} renders nothing at all"
            );
            // A skip reason with no phrase would leak the raw snake_case token.
            if let Some(reason) = cases[case]["payload"]["skipped"].as_str() {
                assert_ne!(detail, reason, "{case} detail is the raw reason");
            }
        }
    }

    #[test]
    fn every_signal_id_has_a_pinned_payload() {
        let cases: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(PINNED_PAYLOADS).unwrap();
        for signal_id in SIGNAL_IDS {
            assert!(
                cases.values().any(|case| case["signal_id"] == signal_id),
                "no pinned payload for {signal_id}"
            );
        }
    }

    #[test]
    fn fixture_events_cover_every_signal_id() {
        use std::collections::HashSet;
        let mut covered = HashSet::new();
        for event in fixture_events() {
            if let ServerMessage::SignalSnapshotsUpdated { snapshots, .. } = event {
                for snapshot in snapshots {
                    covered.insert(snapshot.signal_id.clone());
                }
            }
        }
        for signal_id in SIGNAL_IDS {
            assert!(
                covered.contains(signal_id),
                "fixture_events() has no snapshot for {signal_id} — extend it alongside payloads.json"
            );
        }
    }

    fn loaded() -> DashboardState {
        let mut state = DashboardState::new();
        state.set_connected(true);
        state.apply_all(&fixture_events());
        state
    }

    #[test]
    fn select_card_toggles_and_survives_environment_selection() {
        let mut state = loaded();
        assert_eq!(state.selected_card(), None);
        state.select_card("drift");
        assert_eq!(state.selected_card(), Some("drift"));
        state.select_card("jobs");
        assert_eq!(state.selected_card(), Some("jobs"));
        state.select_card("jobs");
        assert_eq!(state.selected_card(), None);
        state.select_card("nonsense");
        assert_eq!(state.selected_card(), None);
        state.select_card("drift");
        state.select("test");
        assert_eq!(state.selected_card(), Some("drift"));
    }

    #[test]
    fn open_card_sets_without_toggling() {
        let mut state = loaded();
        state.open_card("drift");
        assert_eq!(state.selected_card(), Some("drift"));
        // Compare-strip clicks must land, never close.
        state.open_card("drift");
        assert_eq!(state.selected_card(), Some("drift"));
        state.open_card("nonsense");
        assert_eq!(state.selected_card(), Some("drift"));
    }

    #[test]
    fn summary_text_copies_state_as_plain_text() {
        let state = loaded();
        let text = state.summary_text(1_700_000_012);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "prod (Production) — degraded, polled 12 s ago");
        assert!(
            lines.iter().any(|line| line.starts_with("Availability: ")),
            "{text}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Scheduled jobs: degraded")),
            "{text}"
        );
        assert!(lines.last().unwrap().starts_with("Build: "), "{text}");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Flow errors: degraded")),
            "{text}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Email failures: healthy")),
            "{text}"
        );
        assert!(
            lines.iter().any(|line| line.contains("Upgrades: degraded")),
            "{text}"
        );
        assert!(
            lines.iter().any(|line| line.contains("Sessions: healthy")),
            "{text}"
        );
        assert_eq!(lines.len(), 13, "{text}");
    }

    #[test]
    fn summary_text_marks_muted_environments() {
        let mut state = loaded();
        state.set_mute("prod", TEST_NOW + 3600);
        assert!(
            state
                .summary_text(TEST_NOW)
                .lines()
                .next()
                .unwrap()
                .contains("(muted)"),
        );
    }

    #[test]
    fn signal_url_encodes_query_operators() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(
            state.signal_url("syslog").unwrap(),
            "https://test.example.service-now.com/syslog_list.do?sysparm_query=level=2%5Esys_created_on%3Ejavascript:gs.hoursAgoStart(1)"
        );
        assert_eq!(
            state.signal_url("drift").unwrap(),
            "https://test.example.service-now.com/v_plugin_list.do"
        );
        assert!(state.signal_url("nonsense").is_none());
        assert!(DashboardState::new().signal_url("drift").is_none());
    }

    /// `loaded()` with the selected Environment's `instance_url` replaced.
    fn with_instance_url(url: &str) -> DashboardState {
        let mut state = loaded();
        state.select("test");
        for environment in &mut state.environments {
            if environment.id == "test" {
                environment.instance_url = url.into();
            }
        }
        state
    }

    #[test]
    fn signal_url_rejects_a_non_https_instance_url() {
        assert!(
            with_instance_url("http://test.example.service-now.com")
                .signal_url("drift")
                .is_none()
        );
        assert!(
            with_instance_url("file:///etc/passwd")
                .signal_url("drift")
                .is_none()
        );
    }

    #[test]
    fn signal_url_rejects_userinfo() {
        assert!(
            with_instance_url("https://user@evil.example.com/")
                .signal_url("drift")
                .is_none()
        );
    }

    #[test]
    fn signal_url_rejects_a_query_or_fragment() {
        assert!(
            with_instance_url("https://test.example.service-now.com/?x=1")
                .signal_url("drift")
                .is_none()
        );
        assert!(
            with_instance_url("https://test.example.service-now.com/#f")
                .signal_url("drift")
                .is_none()
        );
    }

    #[test]
    fn signal_url_still_builds_for_a_valid_environment() {
        assert_eq!(
            with_instance_url("https://test.example.service-now.com/")
                .signal_url("drift")
                .unwrap(),
            "https://test.example.service-now.com/v_plugin_list.do"
        );
    }

    #[test]
    fn drift_summary_shows_expected_count_when_planned() {
        let mut state = DashboardState::new();
        state.set_connected(true);
        state.apply_all(&[
            ServerMessage::EnvironmentsUpdated {
                environments: vec![env(
                    "e",
                    "E",
                    EnvironmentHealth::Degraded,
                    Reachability::Reachable,
                )],
            },
            ServerMessage::SignalSnapshotsUpdated {
                environment_id: "e".into(),
                snapshots: vec![snap(
                    "drift",
                    "degraded",
                    r#"{"mismatches":3,"expected_mismatches":2,"build_matches":true}"#,
                )],
            },
        ]);
        state.select("e");
        assert_eq!(state.card_summary("drift"), "3 differ · 2 expected");
    }

    #[test]
    fn drill_in_drift_lists_mismatched_plugins() {
        let mut state = loaded();
        state.select("test");
        let DrillIn::Rows {
            headers,
            rows,
            truncated,
        } = state.drill_in("drift", TEST_NOW)
        else {
            panic!("drift drill-in must be rows");
        };
        assert_eq!(headers, vec!["Plugin", "Source", "Here"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].cells,
            vec!["com.example.plugin_a", "1.0.0", "1.1.0"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        assert_eq!(rows[0].link, None);
        assert_eq!(rows[1].cells[2], "\u{2014}");
        assert!(!truncated);
        // The clone source has no mismatch list.
        state.select("prod");
        assert_eq!(
            state.drill_in("drift", TEST_NOW),
            DrillIn::Text("source of truth · glide-zurich-12-18-2025__patch0-hotfix1".into())
        );
    }

    #[test]
    fn drill_in_lists_unhealthy_mid_agents() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(
            state.drill_in("mid_ecc", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["MID", "Status", "Version"],
                rows: vec![
                    DrillInRow {
                        cells: vec!["mid-b".to_owned(), "Down".to_owned(), "5.0.0".to_owned()],
                        link: None,
                    },
                    DrillInRow {
                        cells: vec!["\u{2014}".to_owned(), "Up".to_owned(), "5.0.1".to_owned()],
                        link: None,
                    },
                ],
                truncated: false,
            }
        );
    }

    #[test]
    fn drill_in_falls_back_to_text_when_every_mid_is_healthy() {
        let state = loaded();
        assert_eq!(
            state.drill_in("mid_ecc", TEST_NOW),
            DrillIn::Text("3/3 MID up \u{b7} queue 2".into())
        );
    }

    #[test]
    fn drill_in_lists_errored_flows_with_record_links() {
        let state = loaded();
        assert_eq!(
            state.drill_in("flow", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["Flow", "Updated"],
                rows: vec![DrillInRow {
                    cells: vec!["Sync orders".to_owned(), "2026-01-27 00:12:00".to_owned()],
                    link: Some(
                        "https://prod.example.service-now.com/sys_flow_context.do?sys_id=flow-1"
                            .to_owned()
                    ),
                }],
                truncated: false,
            }
        );
    }

    #[test]
    fn drill_in_lists_failed_mail_with_record_links() {
        let state = loaded();
        assert_eq!(
            state.drill_in("email", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["Subject", "To", "Time"],
                rows: vec![DrillInRow {
                    cells: vec![
                        "Approval requested".to_owned(),
                        "owner@example.com".to_owned(),
                        "2026-01-27 00:12:00".to_owned(),
                    ],
                    link: Some(
                        "https://prod.example.service-now.com/sys_email.do?sys_id=email-1"
                            .to_owned()
                    ),
                }],
                truncated: false,
            }
        );
    }

    #[test]
    fn drill_in_lists_upgrades_with_record_links() {
        let state = loaded();
        assert_eq!(
            state.drill_in("upgrade", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["From", "To", "Finished"],
                rows: vec![
                    DrillInRow {
                        cells: vec![
                            "Zurich P0".to_owned(),
                            "Zurich P1".to_owned(),
                            "2099-01-01 02:00:00".to_owned(),
                        ],
                        link: Some(
                            "https://prod.example.service-now.com/sys_upgrade_history.do?sys_id=up-1"
                                .to_owned()
                        ),
                    },
                    DrillInRow {
                        cells: vec![
                            "Yokohama".to_owned(),
                            "Zurich".to_owned(),
                            "2020-01-01 02:00:00".to_owned(),
                        ],
                        link: Some(
                            "https://prod.example.service-now.com/sys_upgrade_history.do?sys_id=up-0"
                                .to_owned()
                        ),
                    },
                ],
                truncated: false,
            }
        );
    }

    #[test]
    fn drill_in_trends_and_text() {
        let mut state = loaded();
        // prod jobs has both rows and samples: rows win (actionable, unhealthy-only).
        assert_eq!(
            state.drill_in("jobs", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["Job", "Detail"],
                rows: vec![DrillInRow {
                    cells: vec!["Nightly sync".to_owned(), "2026-01-27 02:00:00".to_owned()],
                    link: Some(
                        "https://prod.example.service-now.com/sys_trigger.do?sys_id=job-1"
                            .to_owned()
                    ),
                }],
                truncated: false,
            }
        );
        // prod syslog likewise carries one fetched row.
        assert_eq!(
            state.drill_in("syslog", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["Time", "Source", "Message"],
                rows: vec![DrillInRow {
                    cells: vec![
                        "2026-01-27 00:12:00".to_owned(),
                        "Scheduled job".to_owned(),
                        "Null pointer in transform".to_owned(),
                    ],
                    link: None,
                }],
                truncated: false,
            }
        );
        state.select("test");
        assert_eq!(
            state.drill_in("outbound", TEST_NOW),
            DrillIn::Text("HTTP 429".into())
        );
        assert_eq!(
            state.drill_in("last_clone", TEST_NOW),
            DrillIn::Rows {
                headers: vec!["Completed", "Age", "Source"],
                rows: vec![DrillInRow {
                    cells: vec![
                        "2026-01-15 12:00:00".to_owned(),
                        "12 days ago".to_owned(),
                        "prod".to_owned(),
                    ],
                    link: None,
                }],
                truncated: false,
            }
        );
        // prod has no last_clone snapshot at all.
        state.select("prod");
        assert_eq!(state.drill_in("last_clone", TEST_NOW), DrillIn::Empty);
    }

    #[test]
    fn has_environments_reflects_loaded_state() {
        assert!(!DashboardState::new().has_environments());
        assert!(loaded().has_environments());
    }

    #[test]
    fn dashboard_state_environments_updated_preserves_ids_labels_order() {
        let state = loaded();
        let rows = state.sidebar(TEST_NOW);
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.label.as_str()))
                .collect::<Vec<_>>(),
            vec![("prod", "Production"), ("test", "Test")]
        );
    }

    #[test]
    fn dashboard_state_health_degraded_maps_dot() {
        let state = loaded();
        assert_eq!(
            state.sidebar(TEST_NOW)[0].health,
            EnvironmentHealth::Degraded
        );
        assert!(!state.sidebar(TEST_NOW)[0].dimmed);
    }

    #[test]
    fn health_events_are_stored_per_environment_and_pruned_with_it() {
        use daku_protocol::{HealthEventDto, HealthEventKind};

        let mut state = loaded();
        let events = vec![HealthEventDto {
            observed_at: 1_700_000_100,
            kind: HealthEventKind::Health,
            from_health: Some(EnvironmentHealth::Healthy),
            to_health: EnvironmentHealth::Degraded,
            build: None,
        }];
        state.apply(&ServerMessage::HealthEventsUpdated {
            environment_id: "prod".into(),
            events: events.clone(),
        });
        assert_eq!(state.health_events.get("prod"), Some(&events));
        // Removing the Environment drops its events with its snapshots.
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "test",
                "Test",
                EnvironmentHealth::Healthy,
                Reachability::Reachable,
            )],
        });
        assert!(!state.health_events.contains_key("prod"));
    }

    #[test]
    fn dashboard_state_asleep_reachability_does_not_change_health() {
        let state = loaded();
        let test = state
            .sidebar(TEST_NOW)
            .into_iter()
            .find(|row| row.id == "test")
            .unwrap();
        assert_eq!(test.health, EnvironmentHealth::Healthy);
        let selected = {
            let mut state = loaded();
            state.select("test");
            state.selected().cloned().unwrap()
        };
        assert_eq!(selected.reachability, Reachability::Asleep);
        assert_eq!(selected.health, EnvironmentHealth::Healthy);
        assert_ne!(selected.health, EnvironmentHealth::Degraded);
    }

    #[test]
    fn dashboard_state_jobs_samples_fill_sparkline() {
        let state = loaded();
        let jobs = state
            .cards(TEST_NOW)
            .into_iter()
            .find(|card| card.signal_id == "jobs")
            .unwrap();
        assert_eq!(jobs.sparkline.len(), 3);
        assert_eq!(state.card_summary("jobs"), "2 overdue · 0 in error");
        let syslog = state
            .cards(TEST_NOW)
            .into_iter()
            .find(|card| card.signal_id == "syslog")
            .unwrap();
        assert!(syslog.sparkline.is_empty());
    }

    #[test]
    fn dashboard_state_missing_snapshot_is_waiting() {
        let state = loaded();
        let last_clone = state
            .cards(TEST_NOW)
            .into_iter()
            .find(|card| card.signal_id == "last_clone")
            .unwrap();
        assert_eq!(last_clone.status, WAITING);
        assert_eq!(last_clone.status, "Waiting");
    }

    #[test]
    fn dashboard_state_compare_strip_build_mismatch() {
        let state = loaded();
        let strip = state.compare_strip();
        assert!(strip.visible);
        assert!(strip.has_mismatch);
    }

    #[test]
    fn last_clone_summary_shows_age() {
        assert_eq!(
            summarize_payload("last_clone", r#"{"role":"source","supported":true}"#),
            "clone source"
        );
        assert_eq!(
            summarize_payload("last_clone", r#"{"completed":"x","age_days":0}"#),
            "today"
        );
        assert_eq!(
            summarize_payload("last_clone", r#"{"supported":true,"completed":null}"#),
            "no clone found"
        );
        assert_eq!(
            summarize_payload(
                "last_clone",
                r#"{"supported":true,"completed":null,"unknown":"older_than_page"}"#
            ),
            "not in the last 10 clones"
        );
    }

    #[test]
    fn summarize_payload_is_empty_for_skipped() {
        assert_eq!(summarize_payload("jobs", r#"{"skipped":"asleep"}"#), "");
        assert_eq!(
            summarize_payload("drift", r#"{"skipped":"need_two_environments"}"#),
            ""
        );
    }

    #[test]
    fn summarize_payload_is_empty_for_a_failed_probe() {
        let down = r#"{"reachability":"unreachable","detail":"HTTP 429"}"#;
        assert_eq!(summarize_payload("jobs", down), "");
        assert_eq!(summarize_payload("mid_ecc", down), "");
        // Availability's unreachable snapshot carries real numbers, not a
        // `detail`, so the guard above must leave it alone.
        assert_eq!(
            summarize_payload(
                "availability",
                r#"{"reachability":"unreachable","rtt_ms":142,"build":null,"error":"HTTP 429"}"#
            ),
            "142 ms"
        );
    }

    #[test]
    fn card_detail_reads_error_and_detail() {
        assert_eq!(
            detail_from_payload(
                "availability",
                r#"{"reachability":"unreachable","error":"no credential for environment prod"}"#
            ),
            "no credential for environment prod"
        );
        assert_eq!(
            detail_from_payload(
                "availability",
                r#"{"reachability":"unreachable","detail":"HTTP 429"}"#
            ),
            "HTTP 429"
        );
        assert_eq!(detail_from_payload("availability", "{}"), "");
        assert_eq!(detail_from_payload("availability", "not json"), "");
        // `error` is a count in the jobs payload, not a message.
        assert_eq!(
            detail_from_payload("jobs", r#"{"overdue_ready":2,"error":1}"#),
            ""
        );
    }

    #[test]
    fn card_detail_phrases_skipped() {
        assert_eq!(
            detail_from_payload("jobs", r#"{"skipped":"asleep"}"#),
            "Environment asleep"
        );
        assert_eq!(
            detail_from_payload("drift", r#"{"skipped":"need_two_environments"}"#),
            "needs two Environments"
        );
        assert_eq!(
            detail_from_payload("last_clone", r#"{"skipped":"clone_source_unreachable"}"#),
            "clone source unreachable"
        );
        assert_eq!(
            detail_from_payload("last_clone", r#"{"skipped":"clone_source_asleep"}"#),
            "clone source asleep"
        );
    }

    #[test]
    fn card_detail_flags_a_partial_drift_inventory() {
        assert_eq!(
            detail_from_payload("drift", r#"{"mismatches":3,"truncated":true}"#),
            "partial inventory — plugin counts may be incomplete"
        );
        assert_eq!(
            detail_from_payload("drift", r#"{"mismatches":3,"truncated":false}"#),
            ""
        );
        // A skipped drift probe read no inventory at all.
        assert_eq!(
            detail_from_payload(
                "drift",
                r#"{"skipped":"clone_source_asleep","truncated":true}"#
            ),
            "clone source asleep"
        );
    }

    #[test]
    fn card_detail_for_selected_environment() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(state.card_detail("outbound"), "HTTP 429");
        assert_eq!(state.card_detail("jobs"), "");
    }

    #[test]
    fn freshness_formats_seconds_minutes_hours() {
        let now_secs = freshness(Some(1000), 1042);
        assert_eq!(now_secs.label, "polled 42 s ago");
        assert!(!now_secs.stale);
        assert_eq!(freshness(Some(1000), 1000 + 180).label, "polled 3 min ago");
        let hours = freshness(Some(1000), 1000 + 7200);
        assert_eq!(hours.label, "polled 2 h ago");
        assert!(hours.stale);
    }

    #[test]
    fn freshness_without_an_observation_says_never_polled() {
        let never = freshness(None, 1_700_000_000);
        assert_eq!(never.label, "never polled");
        assert!(never.stale);
    }

    #[test]
    fn freshness_keeps_its_existing_labels() {
        let now = 1_700_000_000;
        let recent = freshness(Some(now - 42), now);
        assert_eq!(recent.label, "polled 42 s ago");
        assert!(!recent.stale);
        assert!(freshness(Some(now - 400), now).stale);
    }

    #[test]
    fn sidebar_mutes_an_environment_with_no_observation() {
        let mut state = DashboardState::new();
        state.set_connected(true);
        let mut summary = env(
            "prod",
            "Production",
            EnvironmentHealth::Healthy,
            Reachability::Reachable,
        );
        summary.last_observed_at = None;
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![summary.clone()],
        });
        assert!(state.sidebar(TEST_NOW)[0].dimmed);
        summary.last_observed_at = Some(1_700_000_000);
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![summary],
        });
        assert!(!state.sidebar(TEST_NOW)[0].dimmed);
    }

    #[test]
    fn freshness_stale_after_threshold() {
        assert!(!freshness(Some(0), STALE_AFTER_SECS).stale);
        assert!(freshness(Some(0), STALE_AFTER_SECS + 1).stale);
    }

    #[test]
    fn drift_mismatch_lines_formats_three_kinds() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(
            state.drift_mismatch_lines(10),
            vec![
                "com.example.plugin_a: 1.0.0 → 1.1.0",
                "com.example.plugin_b: missing here",
                "com.example.plugin_c: only here",
            ]
        );
    }

    #[test]
    fn drift_mismatch_lines_respects_limit() {
        let mut state = loaded();
        state.select("test");
        let lines = state.drift_mismatch_lines(2);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "… and 1 more");
    }

    #[test]
    fn drift_mismatch_lines_empty_for_source() {
        let mut state = loaded();
        state.select("prod");
        assert!(state.drift_mismatch_lines(10).is_empty());
    }

    #[test]
    fn compare_rows_include_drift_and_last_clone() {
        let rows = loaded().compare_rows();
        let test = rows.iter().find(|row| row.id == "test").unwrap();
        assert_eq!(test.drift, "3 plugins differ");
        assert_eq!(test.last_clone, "12 days ago");
        let prod = rows.iter().find(|row| row.id == "prod").unwrap();
        assert_eq!(prod.drift, "source of truth");
        assert_eq!(prod.last_clone, "");
    }
    #[test]
    fn dashboard_state_removed_selected_environment_falls_back_to_first() {
        let mut state = loaded();
        state.select("test");
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "prod",
                "Production",
                EnvironmentHealth::Healthy,
                Reachability::Reachable,
            )],
        });
        assert_eq!(state.selected_id(), Some("prod"));
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        assert_eq!(state.selected_id(), None);
    }

    #[test]
    fn dashboard_state_select_unknown_id_is_noop() {
        let mut state = loaded();
        state.select("nope");
        assert_eq!(state.selected_id(), Some("prod"));
    }

    #[test]
    fn dashboard_state_disconnected_dims_every_row() {
        let mut state = loaded();
        state.set_connected(false);
        assert!(state.sidebar(TEST_NOW).iter().all(|row| row.dimmed));
        state.set_connected(true);
        assert!(state.sidebar(TEST_NOW).iter().all(|row| !row.dimmed));
    }

    #[test]
    fn removing_an_environment_drops_its_snapshots() {
        let mut state = loaded();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "prod",
                "Production",
                EnvironmentHealth::Degraded,
                Reachability::Reachable,
            )],
        });
        state.apply_all(&fixture_events()[..1]);
        state.select("test");
        assert!(
            state
                .cards(TEST_NOW)
                .iter()
                .all(|card| card.status == WAITING)
        );
    }

    #[test]
    fn removing_an_environment_drops_its_samples() {
        let mut state = loaded();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "prod",
                "Production",
                EnvironmentHealth::Degraded,
                Reachability::Reachable,
            )],
        });
        state.apply_all(&fixture_events()[..1]);
        state.select("test");
        let syslog = state
            .cards(TEST_NOW)
            .into_iter()
            .find(|card| card.signal_id == "syslog")
            .unwrap();
        assert!(syslog.sparkline.is_empty());
    }

    #[test]
    fn signal_cards_are_dimmed_while_disconnected() {
        let mut state = loaded();
        assert!(state.cards(TEST_NOW).iter().all(|card| !card.dimmed));
        state.set_connected(false);
        assert!(state.cards(TEST_NOW).iter().all(|card| card.dimmed));
    }

    #[test]
    fn muted_environment_marks_rows_and_cards_without_dimming() {
        let mut state = loaded();
        assert!(!state.is_muted("prod", TEST_NOW));
        state.set_mute("prod", TEST_NOW + 3600);
        assert!(state.is_muted("prod", TEST_NOW));
        assert!(!state.is_muted("prod", TEST_NOW + 3600));
        assert!(!state.is_muted("test", TEST_NOW));
        let prod = state
            .sidebar(TEST_NOW)
            .into_iter()
            .find(|row| row.id == "prod")
            .unwrap();
        assert!(prod.muted);
        assert!(!prod.dimmed);
        assert!(state.cards(TEST_NOW).iter().all(|card| card.muted));
        assert!(state.cards(TEST_NOW).iter().all(|card| !card.dimmed));
        state.clear_mute("prod");
        assert!(!state.is_muted("prod", TEST_NOW));
    }

    #[test]
    fn worst_health_excluding_muted_skips_muted_environments() {
        let mut state = loaded();
        // Fixture: prod degraded, test healthy.
        assert_eq!(
            state.worst_health_excluding_muted(TEST_NOW),
            Some(EnvironmentHealth::Degraded)
        );
        state.set_mute("prod", TEST_NOW + 3600);
        assert_eq!(
            state.worst_health_excluding_muted(TEST_NOW),
            Some(EnvironmentHealth::Healthy)
        );
        state.set_mute("test", TEST_NOW + 3600);
        assert_eq!(state.worst_health_excluding_muted(TEST_NOW), None);
        state.set_connected(false);
        assert_eq!(state.worst_health_excluding_muted(TEST_NOW), None);
    }

    #[test]
    fn mute_remaining_label_counts_down() {
        assert_eq!(
            mute_remaining_label(1_700_003_600, 1_700_000_000),
            "Muted 1h left"
        );
        assert_eq!(
            mute_remaining_label(1_700_000_600, 1_700_000_000),
            "Muted 10m left"
        );
        assert_eq!(
            mute_remaining_label(1_700_000_000, 1_700_000_000),
            "Muted 1m left"
        );
    }

    #[test]
    fn age_phrase_tiers_match_freshness_words() {
        assert_eq!(age_phrase(12), "12 s");
        assert_eq!(age_phrase(600), "10 min");
        assert_eq!(age_phrase(7200), "2 h");
        assert_eq!(age_phrase(3 * 86_400), "3 d");
    }

    #[test]
    fn recent_events_returns_last_n_oldest_first() {
        let state = loaded();
        // Fixture prod carries bootstrap build + one health transition.
        let events = state.recent_events(10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, HealthEventKind::Build);
        assert_eq!(events[1].kind, HealthEventKind::Health);
        assert_eq!(state.recent_events(1).len(), 1);
        assert_eq!(
            state.recent_events(1)[0].kind,
            HealthEventKind::Health,
            "limit keeps the newest"
        );
        assert_eq!(
            format_health_event(&events[1], 1_700_000_000),
            "healthy → degraded, 1 h ago"
        );
        assert!(format_health_event(&events[0], 1_700_000_000).starts_with("build glide-"));
        // Test carries no events.
        let mut test = loaded();
        test.select("test");
        assert!(test.recent_events(10).is_empty());
    }

    #[test]
    fn build_age_uses_latest_matching_build_event() {
        let state = loaded();
        let (build, since) = state.build_age().expect("prod build age");
        assert_eq!(build, "glide-zurich-12-18-2025__patch0-hotfix1");
        assert_eq!(since, 1_699_913_600);
        let mut test = loaded();
        test.select("test");
        assert!(test.build_age().is_none(), "test carries no build events");
        assert!(DashboardState::new().build_age().is_none());
    }

    #[test]
    fn headline_for_names_worst_signal_or_nothing() {
        let state = loaded();
        let (health, line) = state.headline_for("prod").expect("prod headline");
        assert_eq!(health, EnvironmentHealth::Degraded);
        assert!(line.starts_with("Scheduled jobs: "), "{line}");
        assert!(state.headline_for("test").is_some());
        assert!(state.headline_for("nope").is_none());
        // Recovery: an all-healthy Environment has no headline.
        let mut calm = loaded();
        calm.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![snap("jobs", "healthy", r#"{"overdue_ready":0,"error":0}"#)],
        });
        assert!(calm.headline_for("prod").is_none());
    }

    #[test]
    fn troubled_count_skips_healthy_muted_and_disconnected() {
        let mut state = loaded();
        // Fixture: prod degraded, test healthy.
        assert_eq!(state.troubled_count(TEST_NOW), 1);
        state.set_mute("prod", TEST_NOW + 3600);
        assert_eq!(state.troubled_count(TEST_NOW), 0);
        state.clear_mute("prod");
        state.set_connected(false);
        assert_eq!(state.troubled_count(TEST_NOW), 0);
    }

    #[test]
    fn trend_window_cutoffs_span_day_week_month() {
        let now = 1_700_000_000;
        assert_eq!(TrendWindow::Day24.cutoff_secs(now), now - 86_400);
        assert_eq!(TrendWindow::Day7.cutoff_secs(now), now - 7 * 86_400);
        assert_eq!(TrendWindow::Day30.cutoff_secs(now), now - 30 * 86_400);
        assert_eq!(
            TrendWindow::ALL.map(|(_, label)| label),
            ["24h", "7d", "30d"]
        );
    }

    /// Signal with samples + rollups but no rows, so the window alone
    /// decides what the trend draws.
    fn trendable() -> DashboardState {
        let mut state = DashboardState::new();
        state.set_connected(true);
        state.apply_all(&[
            ServerMessage::EnvironmentsUpdated {
                environments: vec![env(
                    "e",
                    "E",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                )],
            },
            ServerMessage::SignalSnapshotsUpdated {
                environment_id: "e".into(),
                snapshots: vec![snap("jobs", "healthy", r#"{"overdue_ready":0,"error":0,"overdue_rows":[],"overdue_rows_truncated":false,"error_rows":[],"error_rows_truncated":false}"#)],
            },
            ServerMessage::SignalSamplesUpdated {
                environment_id: "e".into(),
                signal_id: "jobs".into(),
                points: vec![
                    SamplePoint {
                        observed_at: 10,
                        value_real: Some(1.0),
                    },
                    SamplePoint {
                        observed_at: 20,
                        value_real: Some(2.0),
                    },
                ],
            },
            ServerMessage::SignalRollupsUpdated {
                environment_id: "e".into(),
                signal_id: "jobs".into(),
                points: vec![
                    RollupPoint {
                        hour_start: 1_700_000_000 - 8 * 86_400,
                        avg_real: Some(8.0),
                        max_real: Some(9.0),
                        sample_count: 30,
                    },
                    RollupPoint {
                        hour_start: 1_700_000_000 - 2 * 86_400,
                        avg_real: Some(5.0),
                        max_real: Some(6.0),
                        sample_count: 30,
                    },
                    RollupPoint {
                        hour_start: 1_700_000_000 - 3600,
                        avg_real: Some(2.0),
                        max_real: Some(3.0),
                        sample_count: 30,
                    },
                ],
            },
        ]);
        state.select("e");
        state
    }

    #[test]
    fn trend_window_switches_between_samples_and_rollups() {
        let mut state = trendable();
        let now = 1_700_000_000;
        assert_eq!(state.trend_window(), TrendWindow::Day24);
        assert_eq!(state.drill_in("jobs", now), DrillIn::Trend(vec![1.0, 2.0]));
        state.set_trend_window(TrendWindow::Day7);
        assert_eq!(state.drill_in("jobs", now), DrillIn::Trend(vec![5.0, 2.0]));
        state.set_trend_window(TrendWindow::Day30);
        assert_eq!(
            state.drill_in("jobs", now),
            DrillIn::Trend(vec![8.0, 5.0, 2.0])
        );
        // One in-window point is no trend: fall back to text.
        state.set_trend_window(TrendWindow::Day7);
        state.apply(&ServerMessage::SignalRollupsUpdated {
            environment_id: "e".into(),
            signal_id: "jobs".into(),
            points: vec![RollupPoint {
                hour_start: now - 3600,
                avg_real: Some(2.0),
                max_real: Some(3.0),
                sample_count: 30,
            }],
        });
        assert_eq!(
            state.drill_in("jobs", now),
            DrillIn::Text("0 overdue · 0 in error".into())
        );
    }

    #[test]
    fn rollups_are_dropped_with_their_environment() {
        let mut state = trendable();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        assert!(state.rollups.is_empty());
    }

    #[test]
    fn apply_mutes_prunes_expired_deadlines() {
        let mut state = loaded();
        let mut mutes = HashMap::new();
        mutes.insert("prod".to_owned(), TEST_NOW - 1);
        mutes.insert("test".to_owned(), TEST_NOW + 100);
        state.apply_mutes(&mutes, TEST_NOW);
        assert!(!state.is_muted("prod", TEST_NOW));
        assert!(state.is_muted("test", TEST_NOW));
        assert_eq!(state.muted_until("test"), Some(TEST_NOW + 100));
    }

    #[test]
    fn dashboard_state_none_sample_becomes_zero() {
        let mut state = loaded();
        let points = vec![
            SamplePoint {
                observed_at: 1,
                value_real: None,
            },
            SamplePoint {
                observed_at: 2,
                value_real: Some(4.0),
            },
        ];
        state.apply(&ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: points.clone(),
        });
        // Samples for a non-trend Signal are kept but never charted.
        state.apply(&ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "outbound".into(),
            points,
        });
        let card = |signal_id: &str| {
            state
                .cards(TEST_NOW)
                .into_iter()
                .find(|card| card.signal_id == signal_id)
                .unwrap()
        };
        assert_eq!(card("jobs").sparkline, vec![0.0, 4.0]);
        assert!(card("outbound").sparkline.is_empty());
    }

    #[test]
    fn cards_sort_problems_first_and_skipped_last() {
        let mut state = loaded();
        state.select("test");
        let statuses: Vec<(&str, String)> = state
            .cards(TEST_NOW)
            .into_iter()
            .map(|card| (card.signal_id, card.status))
            .collect();
        let ranks: Vec<u8> = statuses
            .iter()
            .map(|(_, status)| severity_rank(status))
            .collect();
        let mut sorted = ranks.clone();
        sorted.sort_unstable();
        assert_eq!(ranks, sorted, "{statuses:?}");
        assert!(severity_rank("down") < severity_rank("degraded"));
        assert!(severity_rank("degraded") < severity_rank("healthy"));
        assert!(severity_rank("healthy") < severity_rank("skipped"));
    }

    #[test]
    fn mid_ecc_with_no_agents_is_unknown_not_healthy() {
        let mut state = DashboardState::new();
        state.set_connected(true);
        state.apply_all(&[
            ServerMessage::EnvironmentsUpdated {
                environments: vec![env(
                    "e",
                    "E",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                )],
            },
            ServerMessage::SignalSnapshotsUpdated {
                environment_id: "e".into(),
                snapshots: vec![snap(
                    "mid_ecc",
                    "healthy",
                    r#"{"agents_total":0,"agents_unhealthy":0,"ecc_output_ready":0}"#,
                )],
            },
        ]);
        state.select("e");
        let card = state
            .cards(TEST_NOW)
            .into_iter()
            .find(|card| card.signal_id == "mid_ecc")
            .unwrap();
        assert_eq!(card.status, "unknown");
        assert_eq!(state.card_summary("mid_ecc"), "no MID servers");
    }

    #[test]
    fn worst_health_and_card_hint() {
        let mut state = loaded();
        assert_eq!(
            state.worst_health_excluding_muted(TEST_NOW),
            Some(EnvironmentHealth::Degraded)
        );
        state.set_connected(false);
        assert_eq!(state.worst_health_excluding_muted(TEST_NOW), None);
        state.set_connected(true);
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![snap(
                "last_clone",
                "skipped",
                r#"{"skipped":"no_clone_source"}"#,
            )],
        });
        state.select("prod");
        assert!(state.card_hint("last_clone").contains("clone_source"));
        assert_eq!(state.card_hint("jobs"), "");
    }

    #[test]
    fn dashboard_state_single_environment_hides_compare_strip() {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "prod",
                "Production",
                EnvironmentHealth::Healthy,
                Reachability::Reachable,
            )],
        });
        assert_eq!(
            state.compare_strip(),
            CompareStrip {
                visible: false,
                has_mismatch: false,
            }
        );
    }

    /// Two Environments, neither flagged `role: source` — the strip falls back
    /// to comparing builds pairwise.
    #[test]
    fn dashboard_state_pairwise_build_mismatch_without_clone_source() {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![
                env(
                    "prod",
                    "Production",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
                env(
                    "test",
                    "Test",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
            ],
        });
        let availability = |build: &str| {
            vec![snap(
                "availability",
                "healthy",
                &format!(r#"{{"build":"{build}"}}"#),
            )]
        };
        for id in ["prod", "test"] {
            state.apply(&ServerMessage::SignalSnapshotsUpdated {
                environment_id: id.into(),
                snapshots: availability("a"),
            });
        }
        assert!(!state.compare_strip().has_mismatch);
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: availability("b"),
        });
        assert!(state.compare_strip().has_mismatch);
    }

    #[test]
    fn dashboard_state_plugin_only_mismatch() {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![
                env(
                    "prod",
                    "Production",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
                env(
                    "test",
                    "Test",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
            ],
        });
        // One message per Environment: it replaces that Environment's whole map.
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![
                snap("availability", "healthy", r#"{"build":"a"}"#),
                snap("drift", "healthy", r#"{"role":"source"}"#),
            ],
        });
        let test_drift = |drift: &str| ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: vec![
                snap("availability", "healthy", r#"{"build":"a"}"#),
                snap("drift", "healthy", drift),
            ],
        };
        state.apply(&test_drift(r#"{"mismatches":1,"build_matches":true}"#));
        assert!(state.compare_strip().has_mismatch);
        state.apply(&test_drift(r#"{"mismatches":0,"build_matches":true}"#));
        assert!(!state.compare_strip().has_mismatch);
    }

    #[test]
    fn dashboard_state_card_summary_per_signal() {
        let mut state = loaded();
        assert_eq!(state.card_summary("availability"), "142 ms");
        assert_eq!(state.card_summary("syslog"), "4 errors · last hour");
        assert_eq!(state.card_summary("mid_ecc"), "3/3 MID up · queue 2");
        assert_eq!(
            state.card_summary("outbound"),
            "3 HTTP failures · last hour"
        );
        // Drift carries the build from the availability snapshot as context.
        assert_eq!(
            state.card_summary("drift"),
            "source of truth · glide-zurich-12-18-2025__patch0-hotfix1"
        );
        state.select("test");
        assert_eq!(
            state.card_summary("drift"),
            "3 plugins differ · glide-yokohama-07-02-2025__patch1"
        );
        assert_eq!(state.card_summary("last_clone"), "12 days ago");
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: vec![snap("jobs", "healthy", "not json")],
        });
        assert_eq!(state.card_summary("jobs"), "");
    }

    /// A payload that does not parse is stored as `Value::Null`, and every
    /// accessor must land in the same empty branch it did when each of them
    /// re-parsed the string and failed.
    #[test]
    fn unparseable_payload_still_renders_empty() {
        let mut state = loaded();
        state.select("test");
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: vec![
                snap("jobs", "healthy", "not json"),
                snap("drift", "healthy", "not json"),
            ],
        });
        assert_eq!(state.card_summary("jobs"), "");
        assert_eq!(state.card_detail("jobs"), "");
        assert_eq!(state.drill_in("jobs", TEST_NOW), DrillIn::Empty);
        assert_eq!(state.drift_mismatch_lines(5), Vec::<String>::new());
        assert!(!state.compare_strip().has_mismatch);
    }

    /// Pins idempotence, **not** the parse count — the number of parses is not
    /// observable without instrumentation, and a test claiming to count them
    /// would be a lie. What it does catch: an accessor that mutates or
    /// consumes the cached payload it now reads.
    #[test]
    fn payload_is_parsed_once_per_apply() {
        let state = loaded();
        let before = state.snapshots.clone();
        assert_eq!(state.card_summary("availability"), "142 ms");
        assert_eq!(state.card_summary("availability"), "142 ms");
        assert_eq!(
            state.drill_in("drift", TEST_NOW),
            state.drill_in("drift", TEST_NOW)
        );
        assert_eq!(state.snapshots, before);
    }

    #[test]
    fn dashboard_state_ignores_non_dashboard_messages() {
        let mut state = loaded();
        state.apply(&ServerMessage::ShuttingDown);
        assert_eq!(state.sidebar(TEST_NOW).len(), 2);
        assert_eq!(state.selected_id(), Some("prod"));
    }

    /// Three Environments with the clone source (`prod`) deliberately *not*
    /// first, so the test tells the clone-source reference build apart from
    /// the first-known-build fallback.
    fn three_environments(builds: [Option<&str>; 3]) -> DashboardState {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: ["dev", "prod", "test"]
                .into_iter()
                .map(|id| env(id, id, EnvironmentHealth::Healthy, Reachability::Reachable))
                .collect(),
        });
        for (id, build) in ["dev", "prod", "test"].into_iter().zip(builds) {
            let mut snapshots = vec![snap(
                "drift",
                "healthy",
                if id == "prod" {
                    r#"{"role":"source"}"#
                } else {
                    r#"{"mismatches":0,"build_matches":true}"#
                },
            )];
            if let Some(build) = build {
                snapshots.push(snap(
                    "availability",
                    "healthy",
                    &format!(r#"{{"build":"{build}"}}"#),
                ));
            }
            state.apply(&ServerMessage::SignalSnapshotsUpdated {
                environment_id: id.into(),
                snapshots,
            });
        }
        state
    }

    fn tinted(state: &DashboardState) -> Vec<String> {
        state
            .compare_rows()
            .into_iter()
            .filter(|row| row.mismatch)
            .map(|row| row.id)
            .collect()
    }

    #[test]
    fn compare_rows_tint_the_drifted_environment_not_the_selected_one() {
        let mut state = three_environments([Some("b"), Some("a"), Some("a")]);
        assert_eq!(tinted(&state), ["dev"]);
        state.select("dev");
        assert_eq!(tinted(&state), ["dev"]);
    }

    #[test]
    fn compare_rows_do_not_tint_an_unknown_build() {
        let state = three_environments([None, Some("a"), Some("a")]);
        let rows = state.compare_rows();
        assert_eq!(rows[0].id, "dev");
        assert!(rows[0].build.is_none());
        assert!(!rows[0].mismatch);
        assert!(tinted(&state).is_empty());
    }

    #[test]
    fn compare_rows_do_not_tint_when_the_reference_build_is_unknown() {
        let state = three_environments([None, None, None]);
        assert!(tinted(&state).is_empty());
    }

    #[test]
    fn compare_strip_and_rows_agree_on_mismatch() {
        let state = three_environments([Some("b"), Some("a"), Some("a")]);
        assert!(state.compare_strip().has_mismatch);
        assert!(!tinted(&state).is_empty());
    }
}
