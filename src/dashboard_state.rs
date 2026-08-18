//! Pure dashboard model: protocol events in, sidebar/detail/compare out.

use std::collections::{HashMap, HashSet};

use daku_protocol::{
    EnvironmentHealth, EnvironmentSummary, Reachability, SamplePoint, ServerMessage,
    SignalSnapshotDto, is_supported_instance_url,
};

pub const SIGNAL_IDS: [&str; 7] = [
    "availability",
    "jobs",
    "syslog",
    "mid_ecc",
    "outbound",
    "drift",
    "last_clone",
];

pub const WAITING: &str = "Waiting";

pub fn signal_label(signal_id: &str) -> &'static str {
    match signal_id {
        "availability" => "Availability",
        "jobs" => "Scheduled jobs",
        "syslog" => "Syslog errors",
        "mid_ecc" => "MID / ECC",
        "outbound" => "Outbound",
        "drift" => "Version / plugins",
        "last_clone" => "Last clone",
        _ => "Signal",
    }
}

const TREND_SIGNALS: [&str; 2] = ["jobs", "syslog"];

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
}

/// "polled 42 s ago" / "polled 3 min ago" / "polled 2 h ago" for the selected
/// Environment. An Environment with no observation yet reads "never polled" and
/// is stale by definition — daku has not contacted it.
pub fn freshness(last_observed_at: Option<i64>, now: i64) -> Freshness {
    let Some(last_observed_at) = last_observed_at else {
        return Freshness {
            label: "never polled".to_owned(),
            stale: true,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    pub id: String,
    pub label: String,
    pub health: EnvironmentHealth,
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
    /// paints it grey. Unlike `SidebarRow.muted` this is `!connected` only.
    pub muted: bool,
}

/// Content of the Drill-in region under the Signal cards, built from what the
/// snapshot payload already carries.
#[derive(Clone, Debug, PartialEq)]
pub enum DrillIn {
    Rows {
        headers: Vec<&'static str>,
        rows: Vec<Vec<String>>,
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

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn has_environments(&self) -> bool {
        !self.environments.is_empty()
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

    pub fn drill_in(&self, signal_id: &str) -> DrillIn {
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
                        .map(|entry| {
                            vec![
                                text(entry, "id"),
                                text(entry, "source_version"),
                                text(entry, "other_version"),
                            ]
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
                        .map(|entry| {
                            vec![
                                text(entry, "host_name"),
                                text(entry, "status"),
                                text(entry, "version"),
                            ]
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
                    rows: vec![vec![
                        text(value, "completed"),
                        summarize_value(signal_id, value),
                        text(value, "source_id"),
                    ]],
                    truncated: false,
                }
            }
            "jobs" | "syslog" => {
                let points: Vec<f64> = self
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
                    .unwrap_or_default();
                if points.len() < 2 {
                    self.drill_in_text(signal_id)
                } else {
                    DrillIn::Trend(points)
                }
            }
            _ => self.drill_in_text(signal_id),
        }
    }

    fn drill_in_text(&self, signal_id: &str) -> DrillIn {
        for line in [self.card_detail(signal_id), self.card_summary(signal_id)] {
            if !line.is_empty() {
                return DrillIn::Text(line);
            }
        }
        DrillIn::Empty
    }

    pub fn sidebar(&self) -> Vec<SidebarRow> {
        self.environments
            .iter()
            .map(|environment| SidebarRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                health: environment.health,
                muted: !self.connected || environment.last_observed_at.is_none(),
            })
            .collect()
    }

    pub fn cards(&self) -> Vec<SignalCard> {
        let environment_id = self.selected_id.as_deref().unwrap_or("");
        let snapshots = self.snapshots.get(environment_id);
        SIGNAL_IDS
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
                SignalCard {
                    signal_id,
                    status: snapshot
                        .map(|snapshot| snapshot.dto.state.clone())
                        .unwrap_or_else(|| WAITING.to_owned()),
                    sparkline,
                    muted: !self.connected,
                }
            })
            .collect()
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
        summarize_value(signal_id, &snapshot.payload)
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
            (Some(ms), Some(build)) => format!("{ms} ms · {build}"),
            (Some(ms), None) => format!("{ms} ms"),
            (None, Some(build)) => build.to_owned(),
            _ => String::new(),
        },
        "jobs" => format!(
            "{} overdue · {} error",
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
            "{} errors / h",
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
            format!(
                "{}/{} up · queue {}",
                total.saturating_sub(unhealthy),
                total,
                value
                    .get("ecc_output_ready")
                    .and_then(|item| item.as_u64())
                    .unwrap_or(0)
            )
        }
        "outbound" => format!(
            "{} HTTP fail",
            value
                .get("outbound_http_4xx_5xx_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "drift" => {
            if value.get("role").and_then(|item| item.as_str()) == Some("source") {
                "source of truth".into()
            } else if let Some(count) = value.get("mismatches").and_then(|item| item.as_u64()) {
                format!("{count} plugins differ")
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
                "drift_compare",
                "last_clone_target_completed",
            ]
            .map(pinned)
            .into(),
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

    /// What the client must render for every payload
    /// `crates/daku-core/tests/fixtures/payloads.json` pins: (case,
    /// `card_summary`, `card_detail`). Add a pinned case there and
    /// `pinned_payloads_render` fails until it is listed here — that is the
    /// point of the pin.
    const RENDERED: [(&str, &str, &str); 27] = [
        ("availability_asleep", "142 ms", ""),
        (
            "availability_reachable",
            "142 ms \u{b7} glide-zurich-12-18-2025__patch0-hotfix1",
            "",
        ),
        (
            "availability_reachable_other_build",
            "142 ms \u{b7} glide-yokohama-07-02-2025__patch1",
            "",
        ),
        ("availability_unreachable", "142 ms", "HTTP 429"),
        // A failed probe carries no counts, so there is no summary to render:
        // the card falls back to its status word and the detail says why.
        ("down_probe_failed", "", "HTTP 429"),
        ("drift_compare", "3 plugins differ", ""),
        ("drift_source", "source of truth", ""),
        ("jobs_counts", "2 overdue \u{b7} 0 error", ""),
        ("jobs_zero", "0 overdue \u{b7} 0 error", ""),
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
        ("mid_ecc_healthy", "3/3 up \u{b7} queue 2", ""),
        ("mid_ecc_unhealthy", "1/3 up \u{b7} queue 2", ""),
        ("outbound_count", "3 HTTP fail", ""),
        ("outbound_zero", "0 HTTP fail", ""),
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
        ("syslog_count", "4 errors / h", ""),
        ("syslog_zero", "0 errors / h", ""),
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
    fn drill_in_drift_lists_mismatched_plugins() {
        let mut state = loaded();
        state.select("test");
        let DrillIn::Rows {
            headers,
            rows,
            truncated,
        } = state.drill_in("drift")
        else {
            panic!("drift drill-in must be rows");
        };
        assert_eq!(headers, vec!["Plugin", "Source", "Here"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            vec!["com.example.plugin_a", "1.0.0", "1.1.0"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        assert_eq!(rows[1][2], "\u{2014}");
        assert!(!truncated);
        // The clone source has no mismatch list.
        state.select("prod");
        assert_eq!(
            state.drill_in("drift"),
            DrillIn::Text("source of truth".into())
        );
    }

    #[test]
    fn drill_in_lists_unhealthy_mid_agents() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(
            state.drill_in("mid_ecc"),
            DrillIn::Rows {
                headers: vec!["MID", "Status", "Version"],
                rows: vec![
                    vec!["mid-b".to_owned(), "Down".to_owned(), "5.0.0".to_owned()],
                    vec!["\u{2014}".to_owned(), "Up".to_owned(), "5.0.1".to_owned()],
                ],
                truncated: false,
            }
        );
    }

    #[test]
    fn drill_in_falls_back_to_text_when_every_mid_is_healthy() {
        let state = loaded();
        assert_eq!(
            state.drill_in("mid_ecc"),
            DrillIn::Text("3/3 up \u{b7} queue 2".into())
        );
    }

    #[test]
    fn drill_in_trends_and_text() {
        let mut state = loaded();
        assert_eq!(state.drill_in("jobs"), DrillIn::Trend(vec![1.0, 2.0, 3.0]));
        // prod carries no syslog samples: fall back to the one-line summary.
        assert_eq!(
            state.drill_in("syslog"),
            DrillIn::Text("4 errors / h".into())
        );
        state.select("test");
        assert_eq!(state.drill_in("outbound"), DrillIn::Text("HTTP 429".into()));
        assert_eq!(
            state.drill_in("last_clone"),
            DrillIn::Rows {
                headers: vec!["Completed", "Age", "Source"],
                rows: vec![vec![
                    "2026-01-15 12:00:00".to_owned(),
                    "12 days ago".to_owned(),
                    "prod".to_owned(),
                ]],
                truncated: false,
            }
        );
        // prod has no last_clone snapshot at all.
        state.select("prod");
        assert_eq!(state.drill_in("last_clone"), DrillIn::Empty);
    }

    #[test]
    fn has_environments_reflects_loaded_state() {
        assert!(!DashboardState::new().has_environments());
        assert!(loaded().has_environments());
    }

    #[test]
    fn dashboard_state_environments_updated_preserves_ids_labels_order() {
        let state = loaded();
        let rows = state.sidebar();
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
        assert_eq!(state.sidebar()[0].health, EnvironmentHealth::Degraded);
        assert!(!state.sidebar()[0].muted);
    }

    #[test]
    fn dashboard_state_asleep_reachability_does_not_change_health() {
        let state = loaded();
        let test = state
            .sidebar()
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
            .cards()
            .into_iter()
            .find(|card| card.signal_id == "jobs")
            .unwrap();
        assert_eq!(jobs.sparkline.len(), 3);
        assert_eq!(state.card_summary("jobs"), "2 overdue · 0 error");
        let syslog = state
            .cards()
            .into_iter()
            .find(|card| card.signal_id == "syslog")
            .unwrap();
        assert!(syslog.sparkline.is_empty());
    }

    #[test]
    fn dashboard_state_missing_snapshot_is_waiting() {
        let state = loaded();
        let last_clone = state
            .cards()
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
        assert!(state.sidebar()[0].muted);
        summary.last_observed_at = Some(1_700_000_000);
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![summary],
        });
        assert!(!state.sidebar()[0].muted);
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
    fn dashboard_state_disconnected_mutes_every_row() {
        let mut state = loaded();
        state.set_connected(false);
        assert!(state.sidebar().iter().all(|row| row.muted));
        state.set_connected(true);
        assert!(state.sidebar().iter().all(|row| !row.muted));
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
        assert!(state.cards().iter().all(|card| card.status == WAITING));
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
            .cards()
            .into_iter()
            .find(|card| card.signal_id == "syslog")
            .unwrap();
        assert!(syslog.sparkline.is_empty());
    }

    #[test]
    fn signal_cards_are_muted_while_disconnected() {
        let mut state = loaded();
        assert!(state.cards().iter().all(|card| !card.muted));
        state.set_connected(false);
        assert!(state.cards().iter().all(|card| card.muted));
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
            signal_id: "availability".into(),
            points,
        });
        let card = |signal_id: &str| {
            state
                .cards()
                .into_iter()
                .find(|card| card.signal_id == signal_id)
                .unwrap()
        };
        assert_eq!(card("jobs").sparkline, vec![0.0, 4.0]);
        assert!(card("availability").sparkline.is_empty());
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
        assert_eq!(
            state.card_summary("availability"),
            "142 ms · glide-zurich-12-18-2025__patch0-hotfix1"
        );
        assert_eq!(state.card_summary("syslog"), "4 errors / h");
        assert_eq!(state.card_summary("mid_ecc"), "3/3 up · queue 2");
        assert_eq!(state.card_summary("outbound"), "3 HTTP fail");
        assert_eq!(state.card_summary("drift"), "source of truth");
        state.select("test");
        assert_eq!(state.card_summary("drift"), "3 plugins differ");
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
        assert_eq!(state.drill_in("jobs"), DrillIn::Empty);
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
        assert_eq!(
            state.card_summary("availability"),
            "142 ms · glide-zurich-12-18-2025__patch0-hotfix1"
        );
        assert_eq!(
            state.card_summary("availability"),
            "142 ms · glide-zurich-12-18-2025__patch0-hotfix1"
        );
        assert_eq!(state.drill_in("drift"), state.drill_in("drift"));
        assert_eq!(state.snapshots, before);
    }

    #[test]
    fn dashboard_state_ignores_non_dashboard_messages() {
        let mut state = loaded();
        state.apply(&ServerMessage::ShuttingDown);
        assert_eq!(state.sidebar().len(), 2);
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
