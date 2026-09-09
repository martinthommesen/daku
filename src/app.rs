//! Environments overview — sidebar + detail (variant C).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, mpsc};

use daku_client::DaemonSupervisor;
use daku_protocol::{
    EnvironmentHealth, HealthEventDto, Reachability, ServerMessage, is_supported_instance_url,
};
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight,
    IntoElement, KeyDownEvent, PathBuilder, Pixels, Point, SharedString, Window, canvas, div,
    point, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, TitleBar, h_flex,
    input::{Input, InputState},
    separator::Separator,
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu,
        SidebarMenuItem,
    },
    skeleton::Skeleton,
    tag::Tag,
    tooltip::Tooltip,
    v_flex,
};

use crate::AddEnvironment;
use crate::ClosePalette;
use crate::CloseWindow;
use crate::CopyAgentContext;
use crate::CopySummary;
use crate::DetachSelectedEnvironment;
use crate::ExportSnapshot;
use crate::ReloadDaemon;
use crate::SelectEnvironment;
use crate::SelectEnvironmentSlot;
use crate::SetQuietHours;
use crate::ToggleNotifications;
use crate::TogglePalette;
use crate::ToggleSignalNotify;
use crate::ToggleWeeklyDigest;
use crate::dashboard_state::{
    DashboardState, DrillIn, SignalCard, TREND_WINDOW_LABEL, TrendWindow, age_phrase, freshness,
    is_trend_signal, is_voting_signal, mute_remaining_label, signal_label,
};
use crate::env_sheet::{
    BuildConfig, EnvSheet, auth_label, build_config, credential_blob, credential_captions,
    url_caption,
};
use crate::notifications::{
    notification_body_grouped, notification_title, post_health_notification, select_notification,
};
use crate::persistence::{AppSettings, save_app_settings};
use crate::{fixture_events, ui_fixture_enabled};

const SIDEBAR_WIDTH: f32 = 220.0;

pub struct Daku {
    state: DashboardState,
    supervisor: Option<DaemonSupervisor>,
    settings: Arc<Mutex<AppSettings>>,
    /// Copy-summary confirmation, cleared ~2 s after copy actions. Carries
    /// the flashed text.
    copied_flash: Option<String>,
    /// Export confirmation path, cleared ~4 s after ⌘⇧E.
    exported_path: Option<String>,
    /// Recent-timeline search text. Read during render, so keystrokes
    /// re-render through the input entity.
    timeline_filter: Entity<InputState>,
    /// Note editor input for the annotated timeline event, if any.
    note_input: Entity<InputState>,
    /// Health event awaiting annotation: (environment, observed_at, kind).
    /// Signal rows take no notes.
    note_target: Option<(String, i64, String)>,
    /// Command palette (⌘K) visibility and filter text.
    palette_open: bool,
    palette_input: Entity<InputState>,
    /// Header verdict disclosure: collapsed shows the first two voting lines
    /// plus a "+N more" toggle; expanded shows all of them.
    verdict_expanded: bool,
    /// Desktop boot time: health events observed before this record as seen
    /// without firing, so launch/reconnect replays never storm.
    boot_now: i64,
    /// (Environment, observed_at, kind) triples already decided on.
    notify_seen: HashSet<(String, i64, String)>,
    /// Last ambient reflection (connected, worst, troubled): the menu-bar
    /// dot and Dock badge update only on change, never per-frame.
    last_ambient: Option<(bool, Option<EnvironmentHealth>, usize)>,
    /// Add/edit Environment sheet (`106`); `None` hides it.
    env_sheet: Option<EnvSheet>,
    /// Detached single-Environment window (`108`): sidebar dropped,
    /// selection pinned, notifications owned by the main window.
    detached: Option<String>,
    /// `Root` owns the window's root dispatch node, so the shell only receives
    /// menu- and keystroke-dispatched actions while this handle is focused.
    focus_handle: FocusHandle,
}

/// Mute durations offered in the Environment header.
const MUTE_OPTIONS: [(i64, &str); 3] = [(3600, "1h"), (14_400, "4h"), (86_400, "24h")];

/// Upper bound on remembered notification decisions. Health events are
/// bounded per Environment in SQLite, but this desktop-side dedup set would
/// otherwise grow one entry per event forever — including entries for
/// removed Environments that can never match again.
const NOTIFY_SEEN_CAP: usize = 2000;

impl Daku {
    pub fn new(
        window: &mut Window,
        cx: &mut App,
        supervisor: Option<DaemonSupervisor>,
        settings: Arc<Mutex<AppSettings>>,
        notify_clicks: Option<Arc<Mutex<mpsc::Receiver<String>>>>,
        detached: Option<String>,
    ) -> Entity<Self> {
        let focus_handle = cx.focus_handle();
        let pinned = detached.clone();
        let entity = cx.new(|cx| {
            let mut state = DashboardState::new();
            state.apply_mutes(&settings.lock().expect("app settings").mutes, unix_now());
            if ui_fixture_enabled() {
                state.set_connected(true);
                state.apply_all(&fixture_events());
            } else if let Some(supervisor) = supervisor.as_ref() {
                listen_dashboard(supervisor, pinned.clone(), cx);
            }
            if let Some(pinned) = pinned.as_deref() {
                state.select(pinned);
            }
            let timeline_filter = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Filter recent…")
                    .default_value("")
            });
            let note_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Add context for the next operator…")
                    .default_value("")
            });
            let palette_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Type a command or environment…")
                    .default_value("")
            });
            tick_freshness(cx);
            // Detached windows never pump clicks or post: the main window
            // owns notifications, so a second window cannot double-fire.
            if detached.is_none()
                && let Some(clicks) = notify_clicks
            {
                pump_notification_clicks(clicks, cx);
            }
            Self {
                state,
                supervisor,
                settings,
                copied_flash: None,
                exported_path: None,
                timeline_filter,
                note_input,
                note_target: None,
                palette_open: false,
                palette_input,
                verdict_expanded: false,
                boot_now: unix_now(),
                notify_seen: HashSet::new(),
                last_ambient: None,
                env_sheet: None,
                detached,
                focus_handle: focus_handle.clone(),
            }
        });
        window.focus(&focus_handle, cx);
        entity
    }

    fn mute_selected(&mut self, secs: i64, cx: &mut Context<Self>) {
        let now = unix_now();
        if let Some(id) = self.state.selected_id().map(str::to_owned) {
            let until = now.saturating_add(secs);
            self.state.set_mute(&id, until);
            if let Ok(mut settings) = self.settings.lock() {
                settings.mute_until(&id, until);
            }
            self.persist_settings();
        }
        cx.notify();
    }

    fn unmute_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.state.selected_id().map(str::to_owned) {
            self.state.clear_mute(&id);
            if let Ok(mut settings) = self.settings.lock() {
                settings.unmute(&id);
            }
            self.persist_settings();
        }
        cx.notify();
    }

    fn persist_settings(&self) {
        let snapshot = self
            .settings
            .lock()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        if let Err(error) = save_app_settings(&snapshot) {
            eprintln!("could not save daku app settings: {error:#}");
        }
    }

    /// Rebuilds the app menus so notification checkmarks render current
    /// prefs. Called after every prefs toggle; a poisoned lock keeps the
    /// previous menus rather than clearing them.
    fn refresh_menus(&self, cx: &mut App) {
        let updater_available = cx.global::<crate::updater::UpdaterState>().0.is_some();
        if let Ok(settings) = self.settings.lock() {
            crate::set_app_menus_with_prefs(
                cx,
                updater_available,
                &crate::NotifyMenuPrefs {
                    master: settings.notifications_enabled,
                    signals: settings.notify_signals.clone(),
                    quiet: settings.quiet_hours,
                    digest_weekly: settings.digest_weekly,
                },
            );
        }
    }

    /// Re-reads the shared desktop preferences into this window's state.
    /// Windows share one `Arc<Mutex<AppSettings>>`, so a mute set in one
    /// window appears in the other on its next render or freshness tick.
    fn sync_shared_settings(&mut self) {
        if let Ok(settings) = self.settings.lock() {
            self.state.apply_mutes(&settings.mutes, unix_now());
        }
    }

    /// Decides on one batch of published health events: records everything
    /// seen, posts at most the latest novel transition. Pre-boot history,
    /// replays, muted Environments, disabled Signals, quiet hours and the
    /// global off switch stay silent. Detached windows never post: the main
    /// window owns notifications.
    fn note_health_events(&mut self, env_id: &str, events: &[HealthEventDto]) {
        if self.detached.is_some() {
            return;
        }
        let (record, fire) = select_notification(env_id, events, &self.notify_seen, self.boot_now);
        self.notify_seen.extend(record);
        // Prune decisions for removed Environments, then evict oldest past
        // the cap so a long-lived desktop process cannot grow without bound.
        self.notify_seen
            .retain(|(id, _, _)| self.state.environments().iter().any(|env| env.id == *id));
        if self.notify_seen.len() > NOTIFY_SEEN_CAP {
            let mut seen: Vec<_> = self.notify_seen.iter().cloned().collect();
            seen.sort_by_key(|(_, observed_at, _)| *observed_at);
            self.notify_seen = seen
                .into_iter()
                .skip(self.notify_seen.len() - NOTIFY_SEEN_CAP)
                .collect();
        }
        let Some(event) = fire else { return };
        let now = unix_now();
        // One locked read; a poisoned lock fails safe to silent.
        let prefs = self.settings.lock().map(|settings| {
            (
                settings.notifications_enabled,
                settings.notify_signals.clone(),
                settings.quiet_hours,
            )
        });
        let Ok((master, notify_signals, quiet_hours)) = prefs else {
            return;
        };
        let gate = crate::notifications::NotifyGate {
            master,
            muted: self.state.is_muted(env_id, now),
            headline_signal: self.state.worst_signal_id(env_id),
            notify_signals: &notify_signals,
            quiet_hours,
        };
        // Shared attention decision: batch rule (health-kind, post-boot,
        // unseen — guaranteed by `select_notification` above) plus gates.
        let decision = crate::notifications::AttentionInput {
            kind: event.kind,
            observed_at: event.observed_at,
            boot_now: self.boot_now,
            seen: false,
            gate,
            now,
        };
        if !crate::notifications::should_notify(&decision) {
            return;
        }
        let label = self
            .state
            .environment_label(env_id)
            .unwrap_or(env_id)
            .to_owned();
        // Grouped body: every voting Signal, not just the worst line. Falls
        // back to the event health on recovery (no votes left to name).
        let health = self
            .state
            .headline_for(env_id)
            .map(|(health, _)| health)
            .unwrap_or(event.to_health);
        let lines = self.state.health_explain_for(env_id);
        post_health_notification(
            env_id,
            &notification_title(&label),
            &notification_body_grouped(health, &lines),
        );
    }

    /// Weekly digest slot: on Monday from 09:00 local, once per ~day, every
    /// configured Environment gets a `GetDigest` RPC and a notification with
    /// the week's transitions. Opt-in via `digest_weekly`; detached windows
    /// never send. The sent mark records before firing so a slow RPC cannot
    /// double-send; a failed RPC keeps the mark and retries next Monday.
    fn maybe_send_weekly_digest(&mut self, cx: &mut Context<Self>) {
        if self.detached.is_some() {
            return;
        }
        let now = unix_now();
        if !crate::notifications::is_digest_slot(now) {
            return;
        }
        let mut fire = false;
        if let Ok(mut settings) = self.settings.lock() {
            if !settings.digest_weekly {
                return;
            }
            if settings
                .digest_last_sent
                .is_some_and(|sent| now - sent < 20 * 3600)
            {
                return;
            }
            settings.digest_last_sent = Some(now);
            fire = true;
        }
        if !fire {
            return;
        }
        self.persist_settings();
        let ids: Vec<String> = self
            .state
            .environments()
            .iter()
            .map(|environment| environment.id.clone())
            .collect();
        for env_id in ids {
            self.digest_rpc(env_id, cx);
        }
        cx.notify();
    }

    /// One background digest fetch; the notification click selects the
    /// Environment through the shared health-notification path.
    fn digest_rpc(&mut self, env_id: String, cx: &mut Context<Self>) {
        let Some(client) = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.client())
        else {
            return;
        };
        let notify_id = env_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(daku_protocol::Command::GetDigest {
                        environment_id: env_id,
                        days: 7,
                    })
                })
                .await;
            let _ = this.update(cx, move |this, cx| {
                let Ok(daku_protocol::ResponsePayload::Digest { markdown }) = result else {
                    return;
                };
                let label = this
                    .state
                    .environment_label(&notify_id)
                    .unwrap_or(&notify_id)
                    .to_owned();
                post_health_notification(
                    &notify_id,
                    &format!("{} — weekly digest", notification_title(&label)),
                    &crate::notifications::digest_notification_body(&markdown),
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// The one "take me there" primitive: selects an Environment and
    /// optionally opens its drift drill-in. Compare-row clicks dispatch the
    /// `SelectEnvironment` action into this; notification and menu-bar clicks
    /// (079/081) will call it directly.
    fn select_environment(&mut self, env_id: &str, open_drift: bool, cx: &mut Context<Self>) {
        // A detached window pins its Environment; card selection (view-local)
        // still works, switching away does not.
        if let Some(pinned) = self.detached.as_deref()
            && pinned != env_id
        {
            return;
        }
        self.state.select(env_id);
        if open_drift {
            self.state.open_card("drift");
        }
        // A fresh verdict starts collapsed.
        self.verdict_expanded = false;
        cx.notify();
    }

    /// Opens the selected Environment in its own window (palette and menu
    /// share this; the detached window pins, drops the sidebar, and owns
    /// no notifications).
    fn detach_selected(&mut self, cx: &mut App) {
        let Some(id) = self.state.selected_id().map(str::to_owned) else {
            return;
        };
        let supervisor = self.supervisor.clone();
        let settings = self.settings.clone();
        let bounds = gpui::WindowBounds::Windowed(gpui::Bounds::centered(
            None,
            gpui::size(gpui::px(1100.0), gpui::px(800.0)),
            cx,
        ));
        let _ = crate::open_daku_window(cx, supervisor, settings, None, Some(id), bounds);
    }

    fn select_slot(&mut self, slot: usize, cx: &mut Context<Self>) {
        if self.detached.is_some() {
            return;
        }
        if let Some(id) = self
            .state
            .sidebar(unix_now())
            .get(slot)
            .map(|row| row.id.clone())
        {
            self.select_environment(&id, false, cx);
        }
    }

    fn open_add_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.env_sheet = Some(EnvSheet::add(window, cx));
        cx.notify();
    }

    fn open_edit_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.state.selected().cloned();
        if let Some(summary) = selected {
            self.env_sheet = Some(EnvSheet::edit(
                window,
                cx,
                &daku_protocol::EnvironmentConfig {
                    id: summary.id,
                    label: summary.label,
                    instance_url: summary.instance_url,
                    auth_method: summary.auth_method,
                    sort_order: summary.sort_order,
                    clone_source: summary.clone_source,
                    platform: daku_protocol::Platform::parse(&summary.platform_id)
                        .unwrap_or(daku_protocol::Platform::Servicenow),
                    thresholds: summary.thresholds,
                    expected_drift: summary.expected_drift,
                },
            ));
        }
        cx.notify();
    }

    /// Reads the open sheet into a saveable config plus an optional fresh
    /// Credential blob. `None` without an open sheet.
    fn read_sheet(&self, cx: &App) -> Option<(daku_protocol::EnvironmentConfig, Option<String>)> {
        let sheet = self.env_sheet.as_ref()?;
        let values = sheet.values(cx);
        let existing = sheet
            .editing_id
            .as_deref()
            .and_then(|id| self.state.environments().iter().find(|env| env.id == id));
        let (id, sort_order) = match &sheet.editing_id {
            // Ids are immutable: sort order and tuning ride along untouched.
            Some(id) => (
                id.clone(),
                self.state
                    .environments()
                    .iter()
                    .find(|env| &env.id == id)
                    .map(|env| env.sort_order)
                    .unwrap_or(0),
            ),
            None => (
                values.id.trim().to_owned(),
                self.state.environments().len() as i64,
            ),
        };
        let existing_config = existing.map(|summary| daku_protocol::EnvironmentConfig {
            id: summary.id.clone(),
            label: summary.label.clone(),
            instance_url: summary.instance_url.clone(),
            auth_method: summary.auth_method,
            sort_order: summary.sort_order,
            clone_source: summary.clone_source,
            platform: daku_protocol::Platform::parse(&summary.platform_id)
                .unwrap_or(daku_protocol::Platform::Servicenow),
            thresholds: summary.thresholds.clone(),
            expected_drift: summary.expected_drift.clone(),
        });
        let config = build_config(BuildConfig {
            id,
            label: values.label.trim().to_owned(),
            instance_url: values.url.trim().to_owned(),
            auth_method: sheet.auth,
            platform: sheet.platform,
            clone_source: sheet.clone_source,
            sort_order,
            existing: existing_config.as_ref(),
        });
        let blob = credential_blob(sheet.auth, &values.secret_a, &values.secret_b);
        Some((config, blob))
    }

    fn set_sheet_notice(&mut self, is_error: bool, text: String) {
        if let Some(sheet) = self.env_sheet.as_mut() {
            sheet.busy = false;
            sheet.notice = Some((is_error, text));
        }
    }

    /// Pre-flight: plain fields plus Credential shape when a fresh blob was
    /// entered, plus threshold and expected-drift parsing. Reports every
    /// problem at once so a 25-field sheet never fails one field at a time.
    fn validate_sheet(
        &mut self,
        cx: &App,
    ) -> Option<(daku_protocol::EnvironmentConfig, Option<String>)> {
        let (mut config, blob) = self.read_sheet(cx)?;
        let mut problems: Vec<String> = Vec::new();
        if let Err(error) = crate::env_sheet::validate_fields(
            &config.id,
            &config.label,
            &config.instance_url,
            config.platform,
        ) {
            problems.push(error);
        }
        // Ids are immutable on edit, so only a new Environment can collide.
        let sheet = self.env_sheet.as_ref()?;
        if sheet.editing_id.is_none() {
            let ids: Vec<String> = self
                .state
                .environments()
                .iter()
                .map(|env| env.id.clone())
                .collect();
            if let Err(error) = crate::env_sheet::validate_id_unique(&config.id, &ids) {
                problems.push(error);
            }
        }
        let thresholds = match sheet.thresholds_result(cx) {
            Ok(thresholds) => Some(thresholds),
            Err(error) => {
                problems.push(error);
                None
            }
        };
        let expected_drift = match sheet.expected_drift_result(cx) {
            Ok(expected_drift) => Some(expected_drift),
            Err(error) => {
                problems.push(error);
                None
            }
        };
        if let (Some(thresholds), Some(expected_drift)) = (thresholds, expected_drift) {
            crate::env_sheet::apply_sheet_tuning(&mut config, thresholds, expected_drift);
        }
        if let Some(blob) = &blob
            && let Err(error) = daku_protocol::validate_credential(config.auth_method, blob)
        {
            problems.push(error.to_string());
        }
        if problems.is_empty() {
            Some((config, blob))
        } else {
            self.set_sheet_notice(true, problems.join(" · "));
            None
        }
    }

    /// Runs one management RPC off the UI thread; `done` applies the outcome
    /// back on the entity. Secrets ride the caller's owned command only.
    fn sheet_rpc(
        &mut self,
        command: daku_protocol::Command,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, anyhow::Result<daku_protocol::ResponsePayload>, &mut Context<Self>)
        + 'static,
    ) {
        let Some(client) = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.client())
        else {
            self.set_sheet_notice(true, "daemon unavailable".to_owned());
            cx.notify();
            return;
        };
        if let Some(sheet) = self.env_sheet.as_mut() {
            sheet.busy = true;
            sheet.notice = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.request(command) })
                .await;
            let _ = this.update(cx, |this, cx| done(this, result, cx));
        })
        .detach();
    }

    fn test_sheet(&mut self, cx: &mut Context<Self>) {
        if self.env_sheet.as_ref().is_some_and(|sheet| sheet.busy) {
            return;
        }
        let Some((config, blob)) = self.validate_sheet(cx) else {
            cx.notify();
            return;
        };
        self.sheet_rpc(
            daku_protocol::Command::TestEnvironment {
                environment: Box::new(config),
                credential_json: blob,
            },
            cx,
            |this, result, cx| {
                match result {
                    Ok(daku_protocol::ResponsePayload::EnvironmentTest {
                        reachability,
                        state,
                        build,
                        error,
                        rtt_ms,
                    }) => {
                        let mut line = format!(
                            "{} · {} · {} ms",
                            reachability.as_str(),
                            state.as_str(),
                            rtt_ms
                        );
                        if let Some(build) = build {
                            line.push_str(&format!(" · build {build}"));
                        }
                        if let Some(error) = error {
                            line.push_str(&format!(" · {error}"));
                        }
                        this.set_sheet_notice(false, line);
                    }
                    Ok(other) => {
                        this.set_sheet_notice(true, format!("unexpected daemon reply: {other:?}"))
                    }
                    Err(error) => this.set_sheet_notice(true, format!("{error:#}")),
                }
                cx.notify();
            },
        );
    }

    fn save_sheet(&mut self, cx: &mut Context<Self>) {
        if self.env_sheet.as_ref().is_some_and(|sheet| sheet.busy) {
            return;
        }
        let Some((config, blob)) = self.validate_sheet(cx) else {
            cx.notify();
            return;
        };
        let id = config.id.clone();
        self.sheet_rpc(
            daku_protocol::Command::SaveEnvironment {
                environment: Box::new(config),
                credential_json: blob,
            },
            cx,
            move |this, result, cx| {
                match result {
                    Ok(_) => {
                        // The sheet ends by triggering the 070 reload, which
                        // re-reads the file this just wrote. A remote daemon
                        // cannot be reloaded from here: say so and stay open.
                        let local = this
                            .supervisor
                            .as_ref()
                            .is_some_and(|supervisor| supervisor.is_local());
                        if local {
                            this.supervisor
                                .as_ref()
                                .map(|supervisor| supervisor.reload());
                            this.env_sheet = None;
                        } else {
                            this.set_sheet_notice(
                                false,
                                format!(
                                    "Saved {id} on the daemon host — restart that daemon to apply."
                                ),
                            );
                        }
                    }
                    Err(error) => this.set_sheet_notice(true, format!("{error:#}")),
                }
                cx.notify();
            },
        );
    }

    fn delete_sheet(&mut self, cx: &mut Context<Self>) {
        if self.env_sheet.as_ref().is_some_and(|sheet| sheet.busy) {
            return;
        }
        let Some(sheet) = self.env_sheet.as_ref() else {
            return;
        };
        let Some(id) = sheet.editing_id.clone() else {
            return;
        };
        if !sheet.delete_armed {
            if let Some(sheet) = self.env_sheet.as_mut() {
                sheet.delete_armed = true;
                sheet.notice = Some((true, "Click Delete again to confirm.".into()));
            }
            cx.notify();
            return;
        }
        self.sheet_rpc(
            daku_protocol::Command::DeleteEnvironment { id: id.clone() },
            cx,
            move |this, result, cx| {
                match result {
                    Ok(_) => {
                        let local = this
                            .supervisor
                            .as_ref()
                            .is_some_and(|supervisor| supervisor.is_local());
                        if local {
                            this.supervisor.as_ref().map(|supervisor| supervisor.reload());
                            this.env_sheet = None;
                        } else {
                            this.set_sheet_notice(
                                false,
                                format!(
                                    "Deleted {id} on the daemon host — restart that daemon to apply."
                                ),
                            );
                        }
                    }
                    Err(error) => this.set_sheet_notice(true, format!("{error:#}")),
                }
                cx.notify();
            },
        );
    }

    /// Pushes worst-health + troubled-count to the menu-bar dot and Dock
    /// badge when anything changed. Render-side and idempotent; the menus
    /// themselves stay a separate step.
    fn reflect_ambient(&mut self) {
        let now = unix_now();
        let ambient = (
            self.state.connected(),
            self.state.worst_health_excluding_muted(now),
            self.state.troubled_count(now),
        );
        if self.last_ambient != Some(ambient) {
            self.last_ambient = Some(ambient);
            let (connected, worst, troubled) = ambient;
            crate::platform::reflect_ambient_health(worst, troubled, connected);
        }
    }

    fn copy_summary(&mut self, cx: &mut Context<Self>) {
        self.flash("Copied Environment summary", cx, 2);
        let text = self.state.summary_text(unix_now());
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
    }

    /// Copies the redacted agent-context JSON (see `agent_context_json`).
    fn copy_agent_context(&mut self, cx: &mut Context<Self>) {
        self.flash("Copied agent context (JSON)", cx, 2);
        let text = self.state.agent_context_json(unix_now());
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
    }

    /// Footer flash with its own text, cleared after `secs`.
    fn flash(&mut self, text: &str, cx: &mut Context<Self>, secs: u64) {
        self.copied_flash = Some(text.to_owned());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(secs))
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.copied_flash = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Exports the selected Environment to
    /// `~/.daku/exports/<id>-<unix>/`: `snapshots.json`, `trends.csv`, and
    /// `summary.md` (the copy text). No dialog — the footer flashes the
    /// directory, which keeps the flow headless-testable.
    fn export_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.state.selected_id().map(str::to_owned) else {
            return;
        };
        let now = unix_now();
        let dir = export_directory(&id, now);
        let step = |name: &str| format!("{} ({})", dir.join(name).display(), name);
        let result = std::fs::create_dir_all(&dir)
            .map_err(|error| format!("mkdir {}: {error:#}", dir.display()))
            .and_then(|_| {
                std::fs::write(
                    dir.join("snapshots.json"),
                    self.state.export_snapshots_json(),
                )
                .map_err(|error| format!("write {}: {error:#}", step("snapshots.json")))
            })
            .and_then(|_| {
                std::fs::write(dir.join("trends.csv"), self.state.export_trends_csv())
                    .map_err(|error| format!("write {}: {error:#}", step("trends.csv")))
            })
            .and_then(|_| {
                std::fs::write(dir.join("summary.md"), self.state.summary_text(now))
                    .map_err(|error| format!("write {}: {error:#}", step("summary.md")))
            });
        match result {
            Ok(()) => {
                self.exported_path = Some(dir.display().to_string());
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(4))
                        .await;
                    let _ = this.update(cx, |this: &mut Self, cx| {
                        this.exported_path = None;
                        cx.notify();
                    });
                })
                .detach();
            }
            // A partial directory must not look complete: surface the failing
            // step in the footer and remove what was written.
            Err(message) => {
                eprintln!("daku export failed: {message}");
                let _ = std::fs::remove_dir_all(&dir);
                self.flash(&format!("Export failed: {message}"), cx, 6);
            }
        }
    }

    /// Filtered palette rows for the current query. Pure over state plus
    /// the input text, so the overlay is one read during render.
    fn palette_matches(&self, cx: &App) -> Vec<crate::palette::PaletteEntry> {
        let query = self.palette_input.read(cx).value().to_string();
        let selected = self.state.selected_id().map(str::to_owned);
        let envs: Vec<crate::palette::EnvRef<'_>> = self
            .state
            .environments()
            .iter()
            .map(|env| crate::palette::EnvRef {
                id: &env.id,
                label: &env.label,
                health: env.health.as_str(),
                platform: &env.platform_id,
            })
            .collect();
        let entries = crate::palette::entries_for(&envs, selected.as_deref());
        crate::palette::filter_entries(&entries, &query)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Runs one palette entry by id; unknown ids are ignored. The sheet
    /// needs a window and runs here; the Enter key runs
    /// `run_palette_command`, which covers everything else.
    fn run_palette_entry(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        if !self.run_palette_command(id, cx) {
            match id {
                "add-env" => self.open_add_sheet(window, cx),
                "detach" => self.detach_selected(cx),
                _ => {}
            }
        }
        cx.notify();
    }

    /// Window-free palette entries. Returns false for entries the caller
    /// must handle with a window.
    fn run_palette_command(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        if let Some(env_id) = id.strip_prefix("switch:") {
            self.select_environment(env_id, false, cx);
            return true;
        }
        match id {
            "reload" => {
                if let Some(supervisor) = self.supervisor.as_ref().filter(|s| s.is_local()) {
                    let _ = supervisor.reload();
                }
            }
            "copy" => self.copy_summary(cx),
            "copy-context" => self.copy_agent_context(cx),
            "export" => self.export_snapshot(cx),
            "toggle-notifications" => {
                if let Ok(mut settings) = self.settings.lock() {
                    settings.notifications_enabled = !settings.notifications_enabled;
                }
                self.persist_settings();
                self.refresh_menus(cx);
            }
            "toggle-digest" => {
                if let Ok(mut settings) = self.settings.lock() {
                    settings.digest_weekly = !settings.digest_weekly;
                }
                self.persist_settings();
                self.refresh_menus(cx);
            }
            "mute-1h" => self.mute_selected(3600, cx),
            "mute-4h" => self.mute_selected(14_400, cx),
            "mute-24h" => self.mute_selected(86_400, cx),
            "unmute" => self.unmute_selected(cx),
            "open-snow" => {
                if let Some(url) = self.state.selected().map(|env| env.instance_url.clone())
                    && is_supported_instance_url(&url)
                {
                    cx.open_url(&url);
                }
            }
            _ => return false,
        }
        true
    }

    /// The palette overlay: filter input plus matching rows under the
    /// Environment header. `None` when closed.
    fn palette_overlay(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.palette_open {
            return None;
        }
        let matches = self.palette_matches(cx);
        Some(
            v_flex()
                .mx(px(22.0))
                .mb(px(8.0))
                .p(px(10.0))
                .gap(px(4.0))
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(Input::new(&self.palette_input).small())
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key.as_str() == "enter" {
                        let first = this.palette_matches(cx).into_iter().next();
                        // Enter runs the same dispatch as a click, including
                        // window-scoped entries (add-env, detach).
                        if let Some(entry) = first {
                            let id = entry.id.clone();
                            this.run_palette_entry(&id, window, cx);
                        }
                    }
                }))
                .children(matches.into_iter().map(|entry| {
                    let id = entry.id.clone();
                    div()
                        .id(SharedString::from(format!("palette-{id}")))
                        .flex()
                        .flex_row()
                        .justify_between()
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(cx.theme().radius)
                        .text_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().muted))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_palette_entry(&id, window, cx);
                        }))
                        .child(entry.title.clone())
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(entry.hint.clone()),
                        )
                }))
                .into_any_element(),
        )
    }
}

/// Export root for one Environment at one moment. Pure so tests pin the
/// layout without touching the home directory. The id grammar is enforced
/// at save time; this also verifies containment so a path-unsafe id can
/// never escape the export root even if it arrives from a hand-edited file
/// or a remote daemon.
fn export_directory(environment_id: &str, now: i64) -> std::path::PathBuf {
    let root = crate::persistence::export_root();
    if daku_protocol::environment_id_error(environment_id).is_some() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        environment_id.hash(&mut hasher);
        return root.join(format!("export-{:x}-{now}", hasher.finish()));
    }
    let candidate = root.join(format!("{environment_id}-{now}"));
    // Lexical containment as a second net; grammar above is authoritative.
    if candidate.starts_with(&root) {
        candidate
    } else {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        environment_id.hash(&mut hasher);
        root.join(format!("export-{:x}-{now}", hasher.finish()))
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Renders only happen on `cx.notify()`, so a stalled daemon would freeze the
/// "polled … ago" label; re-render on a slow tick instead.
fn tick_freshness(cx: &mut Context<Daku>) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(30))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
    .detach();
}

/// Forwards notification clicks to the shared SelectEnvironment path: a
/// click takes the Operator to that Environment without opening drift.
fn pump_notification_clicks(clicks: Arc<Mutex<mpsc::Receiver<String>>>, cx: &mut Context<Daku>) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            let ids: Vec<String> = clicks
                .lock()
                .expect("notification click channel")
                .try_iter()
                .collect();
            if ids.is_empty() {
                if this.update(cx, |_, _| {}).is_err() {
                    break;
                }
                continue;
            }
            if this
                .update(cx, |this, cx| {
                    for id in &ids {
                        this.select_environment(id, false, cx);
                    }
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

fn listen_dashboard(supervisor: &DaemonSupervisor, pinned: Option<String>, cx: &mut Context<Daku>) {
    // DaemonSupervisor clients have already completed Hello.
    let supervisor = supervisor.clone();
    cx.spawn(async move |this, cx| {
        let clients = supervisor.subscribe_clients();
        loop {
            let Ok(client) = cx
                .background_executor()
                .spawn({
                    let clients = clients.clone();
                    async move { clients.recv() }
                })
                .await
            else {
                break;
            };
            let _ = this.update(cx, |this, cx| {
                this.state.set_connected(true);
                cx.notify();
            });
            let dashboard = client.subscribe_dashboard();
            loop {
                match cx
                    .background_executor()
                    .spawn({
                        let dashboard = dashboard.clone();
                        async move { dashboard.recv() }
                    })
                    .await
                {
                    Ok(message) => {
                        let pinned = pinned.clone();
                        let _ = this.update(cx, |this, cx| {
                            if let ServerMessage::HealthEventsUpdated {
                                environment_id,
                                events,
                            } = &message
                            {
                                this.note_health_events(environment_id, events);
                            }
                            this.state.apply(&message);
                            if matches!(message, ServerMessage::EnvironmentsUpdated { .. }) {
                                this.maybe_send_weekly_digest(cx);
                            }
                            // A detached window re-pins after every publish:
                            // the dashboard auto-selects the first Environment
                            // when the selection goes stale.
                            if let Some(pinned) = pinned.as_deref() {
                                this.state.select(pinned);
                            }
                            cx.notify();
                        });
                    }
                    Err(_) => {
                        let _ = this.update(cx, |this, cx| {
                            this.state.set_connected(false);
                            cx.notify();
                        });
                        break;
                    }
                }
            }
        }
    })
    .detach();
}

impl Render for Daku {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_shared_settings();
        self.reflect_ambient();
        // A detached window whose Environment was deleted shows a tombstone
        // instead of silently following the dashboard's auto-reselect.
        let removed = self
            .detached
            .as_deref()
            .is_some_and(|pinned| !self.state.environments().iter().any(|env| env.id == pinned));
        let sidebar = self.detached.is_none().then(|| self.render_sidebar(cx));
        // Cards and the Drill-in carry click listeners, so they are built here
        // (where `Context<Self>` is available) and handed to the `&App` detail
        // render. Voting Signals render as full cards; the rest (last_clone,
        // sessions, table_growth) render as a compact strip.
        let (voting, context): (Vec<SignalCard>, Vec<SignalCard>) =
            if self.state.selected().is_some() {
                self.state
                    .cards(unix_now())
                    .into_iter()
                    .partition(|card| is_voting_signal(card.signal_id))
            } else {
                (Vec::new(), Vec::new())
            };
        let voting_cards: Vec<gpui::AnyElement> = voting
            .into_iter()
            .map(|card| self.signal_card(card, cx))
            .collect();
        let context_cards: Vec<gpui::AnyElement> = context
            .into_iter()
            .map(|card| self.context_chip(card, cx))
            .collect();
        let drill_in = self
            .state
            .selected_card()
            .map(|signal_id| self.drill_in_region(signal_id, cx));
        // Mute buttons and compare rows carry click listeners like the
        // cards do, so they are built here and handed to the detail render.
        let mute_controls = self.mute_controls(cx);
        let compare = self.compare_strip(cx);
        let sheet = self.env_sheet_view(cx);
        let detail = if removed {
            div()
                .flex_1()
                .p(px(22.0))
                .text_color(cx.theme().muted_foreground)
                .child("This Environment was removed. Close this window with ⌘W.")
                .into_any_element()
        } else {
            self.render_detail(
                voting_cards,
                context_cards,
                drill_in,
                mute_controls,
                compare,
                cx,
            )
        };
        let title: SharedString = match &self.detached {
            Some(pinned) => self
                .state
                .environments()
                .iter()
                .find(|env| &env.id == pinned)
                .map(|env| env.label.clone().into())
                .unwrap_or_else(|| pinned.clone().into()),
            None => self
                .state
                .selected()
                .map(|environment| environment.label.clone().into())
                .unwrap_or_else(|| "daku".into()),
        };
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            // One flat, slightly translucent surface: sidebar, title bar and
            // detail all share the sidebar colour over the blurred backdrop.
            .bg(cx.theme().sidebar.opacity(0.92))
            .flex()
            .flex_col()
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &CloseWindow, window, _cx| {
                // Detached windows destroy on close; the main window hides so
                // the session (and its daemon child) survives ⌘W.
                if this.detached.is_some() {
                    window.remove_window();
                } else {
                    crate::platform::hide_window(window);
                }
            }))
            .on_action(cx.listener(|this, _: &ReloadDaemon, _, cx| {
                // Local-only: `DaemonSupervisor::reload` refuses remote daemons
                // so a reload can never kill a daemon it cannot respawn.
                // Failures surface as a disconnected banner via the existing
                // dashboard listener; the daemon log holds the detail.
                if let Some(supervisor) = this.supervisor.as_ref().filter(|s| s.is_local()) {
                    let _ = supervisor.reload();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &SelectEnvironment, _, cx| {
                this.select_environment(&action.env_id, action.open_drift, cx);
            }))
            .on_action(cx.listener(|this, action: &SelectEnvironmentSlot, _, cx| {
                this.select_slot(action.slot, cx);
            }))
            .on_action(cx.listener(|this, _: &CopySummary, _, cx| {
                this.copy_summary(cx);
            }))
            .on_action(cx.listener(|this, _: &CopyAgentContext, _, cx| {
                this.copy_agent_context(cx);
            }))
            .on_action(cx.listener(|this, _: &ExportSnapshot, _, cx| {
                this.export_snapshot(cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePalette, _, cx| {
                this.palette_open = !this.palette_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClosePalette, _, cx| {
                if this.palette_open {
                    this.palette_open = false;
                    cx.notify();
                } else if this.note_target.is_some() {
                    // Escape backs out of the note editor without saving;
                    // the sheet keeps its own explicit Cancel so typed
                    // secrets are never lost to a stray keypress.
                    this.note_target = None;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleNotifications, _, cx| {
                if let Ok(mut settings) = this.settings.lock() {
                    settings.notifications_enabled = !settings.notifications_enabled;
                }
                this.persist_settings();
                this.refresh_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleWeeklyDigest, _, cx| {
                if let Ok(mut settings) = this.settings.lock() {
                    settings.digest_weekly = !settings.digest_weekly;
                }
                this.persist_settings();
                this.refresh_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &ToggleSignalNotify, _, cx| {
                if let Ok(mut settings) = this.settings.lock() {
                    let id = action.signal_id.to_string();
                    let enabled = !settings.signal_notify_enabled(&id);
                    settings.set_signal_notify(&id, enabled);
                }
                this.persist_settings();
                this.refresh_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &SetQuietHours, _, cx| {
                if let Ok(mut settings) = this.settings.lock() {
                    settings.quiet_hours = match (action.start_hour, action.end_hour) {
                        (Some(start), Some(end)) => Some(crate::persistence::QuietHours {
                            start_hour: start,
                            end_hour: end,
                        }),
                        _ => None,
                    };
                }
                this.persist_settings();
                this.refresh_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AddEnvironment, window, cx| {
                this.open_add_sheet(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DetachSelectedEnvironment, _, cx| {
                this.detach_selected(cx);
            }))
            .child(
                TitleBar::new()
                    .bg(gpui::transparent_black())
                    .border_b_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(title),
                    ),
            )
            .when(!self.state.connected(), |element| {
                element.child(disconnected_banner(cx))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .children(sidebar)
                    .child(detail),
            )
            .children(sheet)
    }
}

/// Explicit "Edit environment" button for the Environment sheet (`106`). A
/// free function so both mute-controls branches share it. Bordered like a
/// button on purpose: the bare grey word read as a fourth mute duration.
fn edit_button(cx: &mut Context<Daku>) -> gpui::AnyElement {
    div()
        .id("env-edit")
        .px(px(10.0))
        .py(px(2.0))
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(cx.theme().foreground)
        .cursor_pointer()
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.open_edit_sheet(window, cx);
        }))
        .child("Edit environment")
        .into_any_element()
}

/// Toggle meanings in sheet option rows.
#[derive(Clone, Copy, PartialEq)]
enum SheetToggle {
    Auth(daku_protocol::AuthMethod),
    Platform(daku_protocol::Platform),
    CloneSource(bool),
}

/// One sheet action button.
fn sheet_button(
    cx: &mut Context<Daku>,
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&mut Daku, &ClickEvent, &mut Context<Daku>) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .px(px(12.0))
        .py(px(6.0))
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .cursor_pointer()
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| on_click(this, event, cx)))
        .child(div().text_sm().child(label))
        .into_any_element()
}

/// One sheet section heading. Groups the ~25 sheet inputs under
/// Environment, Credential, Thresholds and Expected drift.
fn sheet_section(caption: &'static str, cx: &App) -> gpui::AnyElement {
    div()
        .pt(px(8.0))
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().foreground)
        .child(caption)
        .into_any_element()
}

impl Daku {
    fn sidebar_menu_item(
        &self,
        row: crate::dashboard_state::SidebarRow,
        slot: Option<usize>,
        selected_id: &Option<String>,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        let selected = selected_id.as_deref() == Some(row.id.as_str());
        let id = row.id.clone();
        let label: SharedString = match slot {
            Some(number) => format!("{number} · {}", row.label).into(),
            None => row.label.clone().into(),
        };
        let muted = row.muted;
        let dimmed = row.dimmed;
        let dot = if dimmed || muted {
            cx.theme().muted_foreground
        } else {
            health_color(row.health, cx)
        };
        // Hollow dot = stale or blind (never polled, disconnected): the tool
        // has nothing to say. Solid grey dot plus the word "Muted" = the
        // Operator chose silence. The two must never look alike.
        let hollow = dimmed && !muted;
        SidebarMenuItem::new(label)
            .active(selected)
            .suffix(move |_, cx| {
                h_flex()
                    .items_center()
                    .gap(px(4.0))
                    .when(muted, |element| {
                        element.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Muted"),
                        )
                    })
                    .child(status_dot(dot, hollow))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.select(&id);
                cx.notify();
            }))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected_id = self.state.selected_id().map(str::to_owned);
        let groups = self.state.sidebar_platforms(unix_now());
        // Single platform keeps the flat list; more platforms group up.
        let header_label: SharedString = if groups.len() > 1 {
            "Platforms".into()
        } else {
            groups
                .first()
                .map(|group| group.label.clone().into())
                .unwrap_or("ServiceNow".into())
        };
        // Roll-up: the worst unmuted Environment colours the header dot.
        // Muted Environments stay quiet on every attention surface.
        let roll_up = self
            .state
            .worst_health_excluding_muted(unix_now())
            .map_or(cx.theme().muted_foreground, |health| {
                health_color(health, cx)
            });
        let footer = if let Some(flashed) = self.copied_flash.as_deref() {
            flashed.to_owned()
        } else if let Some(path) = self.exported_path.as_deref() {
            format!("Exported to {}", short_export_path(path))
        } else {
            format!(
                "{} \u{b7} v{}",
                if self.state.connected() {
                    "daemon connected"
                } else {
                    "daemon disconnected"
                },
                env!("CARGO_PKG_VERSION")
            )
        };

        let mut sidebar = Sidebar::new("daku-sidebar")
            .collapsible(SidebarCollapsible::None)
            .w(px(SIDEBAR_WIDTH))
            .bg(gpui::transparent_black())
            .border_r_0()
            .header(
                SidebarHeader::new().child(
                    h_flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().size(px(10.0)).rounded_full().bg(roll_up))
                        .child(div().text_sm().child(header_label)),
                ),
            );
        if groups.len() > 1 {
            // Slot numbers follow the flat sidebar order (⌘1–9 switch in
            // that order), so one running counter spans every group.
            let mut slot: usize = 0;
            for group in groups {
                let mut items: Vec<SidebarMenuItem> = Vec::new();
                for row in group.rows {
                    slot += 1;
                    let number = if slot <= 9 { Some(slot) } else { None };
                    items.push(self.sidebar_menu_item(row, number, &selected_id, cx));
                }
                sidebar = sidebar.child(
                    SidebarGroup::new(group.label).child(SidebarMenu::new().children(items)),
                );
            }
        } else {
            let mut items: Vec<SidebarMenuItem> = Vec::new();
            for (index, row) in groups.into_iter().flat_map(|group| group.rows).enumerate() {
                let number = if index < 9 { Some(index + 1) } else { None };
                items.push(self.sidebar_menu_item(row, number, &selected_id, cx));
            }
            sidebar = sidebar
                .child(SidebarGroup::new("Environments").child(SidebarMenu::new().children(items)));
        }
        sidebar
            .footer(
                SidebarFooter::new().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(footer),
                ),
            )
            .into_any_element()
    }

    /// Mute / unmute controls for the selected Environment header: the
    /// mute deadline plus Unmute when muted, else 1 h / 4 h / 24 h options.
    /// The durations mute notifications.
    fn mute_controls(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let id = self.state.selected_id()?.to_owned();
        let now = unix_now();
        let base = h_flex().items_center().gap(px(8.0)).text_xs();
        if let Some(until) = self
            .state
            .muted_until(&id)
            .filter(|_| self.state.is_muted(&id, now))
        {
            Some(
                base.child(
                    div()
                        .text_color(cx.theme().warning)
                        .child(mute_remaining_label(until, now)),
                )
                .child(
                    div()
                        .id("mute-unmute")
                        .px(px(10.0))
                        .py(px(2.0))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_color(cx.theme().foreground)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.unmute_selected(cx);
                        }))
                        .child("Unmute"),
                )
                .child(edit_button(cx))
                .into_any_element(),
            )
        } else {
            Some(
                base.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("Mute notifications:"),
                )
                .children(MUTE_OPTIONS.into_iter().map(|(secs, label)| {
                    div()
                        .id(SharedString::from(format!("mute-{label}")))
                        .text_color(cx.theme().muted_foreground)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.mute_selected(secs, cx);
                        }))
                        .child(label)
                }))
                .child(edit_button(cx))
                .into_any_element(),
            )
        }
    }

    /// Add / edit Environment overlay (`106`). A centred panel over a
    /// dimmed backdrop; backdrop clicks never close it, so typed secrets
    /// cannot be lost to a stray click.
    fn env_sheet_view(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use gpui_component::input::Input;

        let sheet = self.env_sheet.as_ref()?;
        let values = sheet.values(cx);
        let title: SharedString = match &sheet.editing_id {
            Some(id) => format!("Edit {id}").into(),
            None => "Add Environment".into(),
        };
        let (caption_a, caption_b) = credential_captions(sheet.auth);
        let overwrite_hint = sheet.editing_id.is_none()
            && self
                .state
                .environments()
                .iter()
                .any(|env| env.id == values.id.trim());
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(gpui::black().opacity(0.45))
                .flex()
                .items_start()
                .justify_center()
                .pt(px(64.0))
                .child(
                    div()
                        .id("env-sheet-panel")
                        .w(px(520.0))
                        .max_h(px(640.0))
                        .overflow_y_scroll()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .p(px(20.0))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .when(sheet.editing_id.is_none(), |element| {
                            element.child(EnvSheet::field_row("ID", &sheet.id_field, cx))
                        })
                        .child(sheet_section("Environment", cx))
                        .child(EnvSheet::field_row("Label", &sheet.label_field, cx))
                        .child(EnvSheet::field_row(url_caption(sheet.platform), &sheet.url_field, cx))
                        .child(self.sheet_toggle_row(
                            "Platform",
                            &[
                                (
                                    SheetToggle::Platform(
                                        daku_protocol::Platform::Servicenow,
                                    ),
                                    "ServiceNow",
                                ),
                                (
                                    SheetToggle::Platform(daku_protocol::Platform::Http),
                                    "HTTP probe",
                                ),
                                (
                                    SheetToggle::Platform(daku_protocol::Platform::Github),
                                    "GitHub Actions",
                                ),
                            ],
                            cx,
                        ))
                        .child(self.sheet_toggle_row(
                            "Clone source",
                            &[
                                (SheetToggle::CloneSource(false), "No"),
                                (SheetToggle::CloneSource(true), "Yes"),
                            ],
                            cx,
                        ))
                        .child(sheet_section("Credential", cx))
                        .child(self.sheet_toggle_row(
                            "Auth",
                            &[
                                (
                                    SheetToggle::Auth(
                                        daku_protocol::AuthMethod::OauthClientCredentials,
                                    ),
                                    auth_label(
                                        daku_protocol::AuthMethod::OauthClientCredentials,
                                    ),
                                ),
                                (
                                    SheetToggle::Auth(daku_protocol::AuthMethod::Basic),
                                    auth_label(daku_protocol::AuthMethod::Basic),
                                ),
                            ],
                            cx,
                        ))
                        .child(EnvSheet::field_row(caption_a, &sheet.secret_a, cx))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(caption_b),
                                )
                                .child(Input::new(&sheet.secret_b).mask_toggle()),
                        )
                        .when(sheet.editing_id.is_some(), |element| {
                            element.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Leave both Credential fields blank to keep the stored one.",
                                    ),
                            )
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Test checks reachability and build. It does not check table access.",
                                ),
                        )
                        .child(sheet_section("Thresholds", cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Thresholds — empty means default. Jobs errors, email, update sets, RTT, transaction avg and probe RTT accept off.",
                                ),
                        )
                        .child(EnvSheet::field_row(
                            "Jobs overdue ≥",
                            &sheet.threshold_jobs_overdue,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Jobs errors ≥ (off)",
                            &sheet.threshold_jobs_error,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Syslog errors ≥",
                            &sheet.threshold_syslog,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Outbound failures ≥",
                            &sheet.threshold_outbound,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Flow errors ≥",
                            &sheet.threshold_flow,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Email failures ≥ (off)",
                            &sheet.threshold_email,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Failed upgrades ≥",
                            &sheet.threshold_upgrade,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Transaction avg ms (off)",
                            &sheet.threshold_txn,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Open update sets ≥ (off)",
                            &sheet.threshold_updates,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Scan P1 findings ≥",
                            &sheet.threshold_scan,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Failed Actions runs ≥",
                            &sheet.threshold_actions,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Probe RTT ms (off)",
                            &sheet.threshold_probe_rtt,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Unhealthy MIDs ≥",
                            &sheet.threshold_mid,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "ECC errors ≥",
                            &sheet.threshold_ecc_error,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "ECC queue ≥",
                            &sheet.threshold_ecc_queue,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "Drift mismatches ≥",
                            &sheet.threshold_drift,
                            cx,
                        ))
                        .child(EnvSheet::field_row(
                            "RTT ceiling ms (off)",
                            &sheet.threshold_rtt,
                            cx,
                        ))
                        .child(match sheet.thresholds_result(cx) {
                            Ok(thresholds) => div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Effective: {}", thresholds.summary())),
                            Err(error) => div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(error),
                        })
                        .child(sheet_section("Expected drift", cx))
                        .child(EnvSheet::field_row(
                            "Expected drift ids (comma-separated)",
                            &sheet.expected_drift_field,
                            cx,
                        ))
                        .when(overwrite_hint, |element| {
                            element.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child("This id already exists — Save will overwrite it."),
                            )
                        })
                        .when_some(sheet.notice.clone(), |element, (is_error, text)| {
                            element.child(
                                div()
                                    .text_sm()
                                    .text_color(if is_error {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(text),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.0))
                                .when(sheet.busy, |element| element.opacity(0.5))
                                .child(sheet_button(cx, "sheet-cancel", "Cancel", |this, _, cx| {
                                    this.env_sheet = None;
                                    cx.notify();
                                }))
                                .child(sheet_button(cx, "sheet-test", "Test", |this, _, cx| {
                                    this.test_sheet(cx);
                                }))
                                .when(sheet.editing_id.is_some(), |element| {
                                    let label = if sheet.delete_armed {
                                        "Confirm delete"
                                    } else {
                                        "Delete"
                                    };
                                    element.child(sheet_button(
                                        cx,
                                        "sheet-delete",
                                        label,
                                        |this, _, cx| {
                                            this.delete_sheet(cx);
                                        },
                                    ))
                                })
                                .child(sheet_button(cx, "sheet-save", "Save", |this, _, cx| {
                                    this.save_sheet(cx);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// One toggle row in the sheet. Options carry their meaning so the
    /// handler stays a single match.
    fn sheet_toggle_row(
        &self,
        caption: &'static str,
        options: &[(SheetToggle, &'static str)],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(caption),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .children(options.iter().map(|(pick, label)| {
                        let pick = *pick;
                        let selected = match pick {
                            SheetToggle::Auth(auth) => self
                                .env_sheet
                                .as_ref()
                                .is_some_and(|sheet| sheet.auth == auth),
                            SheetToggle::Platform(platform) => self
                                .env_sheet
                                .as_ref()
                                .is_some_and(|sheet| sheet.platform == platform),
                            SheetToggle::CloneSource(flag) => self
                                .env_sheet
                                .as_ref()
                                .is_some_and(|sheet| sheet.clone_source == flag),
                        };
                        div()
                            .id(SharedString::from(format!("sheet-pick-{label}")))
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(if selected {
                                cx.theme().primary
                            } else {
                                cx.theme().border
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(sheet) = this.env_sheet.as_mut() {
                                    match pick {
                                        SheetToggle::Auth(auth) => sheet.auth = auth,
                                        SheetToggle::Platform(platform) => {
                                            sheet.platform = platform
                                        }
                                        SheetToggle::CloneSource(flag) => sheet.clone_source = flag,
                                    }
                                    sheet.delete_armed = false;
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if selected {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(*label),
                            )
                    })),
            )
            .into_any_element()
    }

    /// Compare strip: build, drift and last-clone across Environments. A
    /// row click selects that Environment and opens its drift drill-in via
    /// the shared `SelectEnvironment` action.
    fn compare_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.detached.is_some() {
            return None;
        }
        let strip = self.state.compare_strip();
        if !strip.visible {
            return None;
        }
        let rows = self.state.compare_rows();
        Some(
            v_flex()
                .mx(px(22.0))
                .mb(px(16.0))
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted)
                .child(
                    compare_row_cells(
                        ["Environment", "Build", "Drift", "Last clone"].map(str::to_owned),
                    )
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
                )
                .child(Separator::horizontal().color(cx.theme().border))
                .children(rows.into_iter().map(|row| {
                    let action = SelectEnvironment {
                        env_id: SharedString::from(row.id.clone()),
                        open_drift: true,
                    };
                    compare_row_cells([
                        row.label.clone(),
                        row.build.clone().unwrap_or_else(|| "\u{2014}".to_owned()),
                        row.drift.clone(),
                        row.last_clone.clone(),
                    ])
                    .text_sm()
                    .text_color(if row.mismatch {
                        cx.theme().warning
                    } else {
                        cx.theme().muted_foreground
                    })
                    .id(SharedString::from(format!("compare-{}", row.id)))
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(action.clone()), cx);
                    })
                }))
                .when(strip.has_mismatch, |element| {
                    element.child(
                        div()
                            .px(px(14.0))
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child("build / drift mismatch"),
                    )
                })
                .child(
                    div()
                        .px(px(14.0))
                        .pb(px(10.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select a row to open its drift"),
                )
                .into_any_element(),
        )
    }

    /// Recent health/build transitions under the Signal cards. Omitted
    /// entirely without events — a fresh Environment shows no empty box.
    fn recent_block(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let now = unix_now();
        let query = self.timeline_filter.read(cx).value().to_string();
        let entries = self.state.timeline(now, 20, &query);
        // `entries` already answers emptiness for the unfiltered, note-less
        // case; a second `timeline(…, 1, "")` call would re-merge, re-format,
        // and re-sort up to 200 events every frame just for this boolean.
        let has_history = !entries.is_empty() || !query.trim().is_empty();
        if !has_history && query.trim().is_empty() && self.note_target.is_none() {
            return None;
        }
        let mut block = v_flex().mx(px(22.0)).mb(px(16.0)).gap(px(2.0)).child(
            h_flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Recent"),
                )
                .child(
                    div()
                        .w(px(180.0))
                        .child(Input::new(&self.timeline_filter).small()),
                ),
        );
        for entry in entries {
            let line = div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(entry.text.clone());
            match entry.note_key.clone() {
                Some((observed_at, kind)) => {
                    let env_id = self.state.selected_id().unwrap_or_default().to_owned();
                    let target = (env_id, observed_at, kind);
                    block = block.child(
                        div()
                            .id(SharedString::from(format!(
                                "timeline-{}-{}",
                                observed_at, target.2
                            )))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.note_target = Some(target.clone());
                                cx.notify();
                            }))
                            // The row takes a note on click.
                            .child(
                                h_flex().items_center().gap(px(8.0)).child(line).child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .hover(|style| style.text_decoration_1())
                                        .child("Add note"),
                                ),
                            ),
                    );
                }
                None => {
                    block = block.child(line);
                }
            }
        }
        if let Some((env_id, observed_at, kind)) = self.note_target.clone() {
            let label = format!(
                "Note for the {kind} event, {} ago:",
                age_phrase(now.saturating_sub(observed_at))
            );
            block = block
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(Input::new(&self.note_input).small())
                .child(
                    h_flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .id("note-save")
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.save_note(
                                        env_id.clone(),
                                        observed_at,
                                        kind.clone(),
                                        window,
                                        cx,
                                    );
                                }))
                                .child("Save note"),
                        )
                        .child(
                            div()
                                .id("note-cancel")
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.note_target = None;
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        ),
                );
        }
        Some(block.into_any_element())
    }

    /// Saves the note editor as an annotation over the loopback RPC, then
    /// clears the editor. Empty saves clear the annotation.
    fn save_note(
        &mut self,
        env_id: String,
        observed_at: i64,
        kind: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let note = self.note_input.read(cx).value().to_string();
        // The editor clears optimistically: rebuilding it needs the window,
        // which does not outlive this call. An RPC failure surfaces in the
        // footer and the next publish restores the un-annotated row.
        self.note_target = None;
        self.note_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Add context for the next operator…")
                .default_value("")
        });
        cx.notify();
        let Some(client) = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.client())
        else {
            return;
        };
        let kind_parse = match kind.as_str() {
            "health" => daku_protocol::HealthEventKind::Health,
            _ => daku_protocol::HealthEventKind::Build,
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(daku_protocol::Command::AddHealthEventNote {
                        environment_id: env_id,
                        observed_at,
                        kind: kind_parse,
                        note,
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    eprintln!("daku note save failed: {error:#}");
                    this.flash(&format!("Note save failed: {error:#}"), cx, 6);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_detail(
        &self,
        voting_cards: Vec<gpui::AnyElement>,
        context_cards: Vec<gpui::AnyElement>,
        drill_in: Option<gpui::AnyElement>,
        mute_controls: Option<gpui::AnyElement>,
        compare: Option<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = self.state.selected().cloned();
        div()
            .id("detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .when_some(selected, |element, environment| {
                // Disconnected or never polled: the label, the URL and the
                // freshness line stay, but stale colours would contradict them.
                let observed = self.state.connected() && environment.last_observed_at.is_some();
                let fresh = freshness(environment.last_observed_at, unix_now());
                let fresh_color = if fresh.critical {
                    cx.theme().danger
                } else if fresh.stale {
                    cx.theme().warning
                } else {
                    cx.theme().muted_foreground
                };
                // One verdict block answers "why is this degraded": the
                // voting lines first, the clone/upgrade correlation with
                // them, metadata after. Healthy Environments show no block.
                let explain = self.state.health_explain();
                let correlation = self.state.correlation_build(unix_now());
                let has_verdict = !explain.is_empty() || correlation.is_some();
                let shown = if self.verdict_expanded {
                    explain.len()
                } else {
                    explain.len().min(2)
                };
                let build_age = self.state.build_age().map(|(_, since)| {
                    format!(
                        "on this build {} ago",
                        age_phrase(unix_now().saturating_sub(since))
                    )
                });
                element
                    .child(
                        v_flex()
                            .px(px(22.0))
                            .pt(px(18.0))
                            .pb(px(12.0))
                            .gap(px(8.0))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    // The state is the headline: a dot the
                                    // title's own size, not a pill after it.
                                    .child(div().size(px(12.0)).rounded_full().bg(if observed {
                                        health_color(environment.health, cx)
                                    } else {
                                        cx.theme().muted_foreground
                                    }))
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(environment.label.clone()),
                                    )
                                    .when(observed, |element| {
                                        element
                                            .child(health_tag(environment.health))
                                            // Reachable is implied by any other
                                            // number on screen; only the bad
                                            // states earn a pill.
                                            .when(
                                                environment.reachability != Reachability::Reachable,
                                                |element| {
                                                    element.child(reachability_tag(
                                                        environment.reachability,
                                                    ))
                                                },
                                            )
                                    })
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(fresh_color)
                                            .child(fresh.label),
                                    ),
                            )
                            .when(has_verdict, |element| {
                                element.child(
                                    v_flex()
                                        .gap(px(2.0))
                                        .p(px(10.0))
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(cx.theme().warning.opacity(0.45))
                                        .bg(cx.theme().warning.opacity(0.08))
                                        .children(explain.iter().take(shown).map(|line| {
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(cx.theme().foreground)
                                                .child(line.clone())
                                        }))
                                        .when_some(correlation, |element, build| {
                                            element.child(
                                                div()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "Started after build {build} — likely clone/upgrade fallout"
                                                    )),
                                            )
                                        })
                                        .when(explain.len() > 2, |element| {
                                            let remaining = explain.len() - shown;
                                            let label: SharedString = if self.verdict_expanded {
                                                "Show less".into()
                                            } else {
                                                format!("+{remaining} more").into()
                                            };
                                            element.child(
                                                div()
                                                    .id("verdict-toggle")
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .cursor_pointer()
                                                    .hover(|style| style.text_decoration_1())
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, _, cx| {
                                                            this.verdict_expanded =
                                                                !this.verdict_expanded;
                                                            cx.notify();
                                                        },
                                                    ))
                                                    .child(label),
                                            )
                                        }),
                                )
                            })
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                environment
                                                    .instance_url
                                                    .trim_start_matches("https://")
                                                    .to_owned(),
                                            ),
                                    )
                                    .when_some(build_age, |element, age| {
                                        element.child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(age),
                                        )
                                    }),
                            )
                            .children(mute_controls),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(12.0))
                            .p(px(22.0))
                            .children(voting_cards),
                    )
                    .when(!context_cards.is_empty(), |element| {
                        element.child(
                            v_flex()
                                .mx(px(22.0))
                                .mb(px(12.0))
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Other signals — never vote toward health"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .flex_wrap()
                                        .gap(px(8.0))
                                        .children(context_cards),
                                ),
                        )
                    })
                    .children(self.recent_block(cx))
                    .children(self.palette_overlay(cx))
                    .children(drill_in)
                    .children(compare)
            })
            .when(self.state.selected().is_none(), |element| {
                let message = if self.state.connected() && !self.state.has_environments() {
                    "No Environments configured — add one from the app menu (daku → Add Environment…), or copy environments.example.json to ~/.daku/environments.json and press ⌘R. Daemon diagnostics: ~/.daku/daemon.log"
                } else {
                    "No Environment selected — pick one in the sidebar."
                };
                element.child(
                    div()
                        .p(px(22.0))
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
            })
            .into_any_element()
    }

    fn signal_card(&self, card: SignalCard, cx: &mut Context<Self>) -> gpui::AnyElement {
        let signal_id = card.signal_id;
        let selected = self.state.selected_card() == Some(signal_id);
        let url = self.state.signal_url(signal_id);
        let summary = self.state.card_summary(card.signal_id);
        let detail = self.state.card_detail(card.signal_id);
        let hint = self.state.card_hint(card.signal_id);
        let mismatch_lines = if card.signal_id == "drift" {
            self.state.drift_mismatch_lines(5)
        } else {
            Vec::new()
        };
        let waiting = card.status == crate::dashboard_state::WAITING;
        let skipped = card.status == "skipped";
        let unknown = card.status == "unknown";
        // Disconnected cards are stale; muted cards are quiet on purpose.
        // Both paint grey and never carry attention colour. Hollow dots mark
        // "no reading" (waiting, skipped, unknown, disconnected) so they
        // never read as chosen silence, which stays solid grey.
        let quiet = card.dimmed || card.muted;
        let attention = !quiet && matches!(card.status.as_str(), "degraded" | "down");
        let hollow = !card.muted && (card.dimmed || waiting || skipped || unknown);
        let hover_border = cx.theme().border;
        let color = if quiet {
            cx.theme().muted_foreground
        } else {
            status_color(&card.status, cx)
        };
        // A skipped probe has no metric: the reason is the headline, and any
        // configuration hint the context line.
        let (value, context) = if skipped {
            (capitalize(&detail), hint.to_owned())
        } else if summary.is_empty() {
            (card.status.clone(), String::new())
        } else {
            split_summary(&summary)
        };
        div()
            .id(SharedString::from(format!("card-{}", card.signal_id)))
            .flex_1()
            .min_w(px(250.0))
            .max_w(px(300.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(14.0))
            .rounded(cx.theme().radius)
            .border_2()
            .border_color(if selected {
                cx.theme().primary
            } else if attention {
                color.opacity(0.75)
            } else {
                gpui::transparent_black()
            })
            // Cards that need attention carry their colour, not just a dot.
            .bg(if attention {
                color.opacity(0.15)
            } else {
                cx.theme().secondary
            })
            .hover(|style| {
                if selected || attention {
                    style
                } else {
                    style.border_color(hover_border)
                }
            })
            .text_color(cx.theme().secondary_foreground)
            .when(skipped, |element| element.opacity(0.7))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.select_card(signal_id);
                cx.notify();
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(status_dot(color, hollow))
                    // The title is the deep link; it underlines on hover.
                    .child(match url {
                        Some(url) => open_link(
                            format!("open-{signal_id}").into(),
                            url,
                            signal_label(card.signal_id),
                        )
                        .into_any_element(),
                        None => div().child(signal_label(card.signal_id)).into_any_element(),
                    }),
            )
            .child(if waiting {
                Skeleton::new()
                    .w(px(96.0))
                    .h(px(22.0))
                    .rounded(cx.theme().radius)
                    .into_any_element()
            } else if skipped {
                clipped_line(format!("value-{signal_id}").into(), value.clone())
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element()
            } else {
                clipped_line(format!("value-{signal_id}").into(), value.clone())
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if attention {
                        color
                    } else {
                        cx.theme().foreground
                    })
                    .into_any_element()
            })
            .when(!context.is_empty(), |element| {
                element.child(
                    clipped_line(format!("context-{signal_id}").into(), context.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .when(!detail.is_empty() && !skipped, |element| {
                element.child(
                    div()
                        .text_xs()
                        .text_color(if card.status == "down" && !quiet {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(detail),
                )
            })
            .when(!mismatch_lines.is_empty(), |element| {
                element.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .children(mismatch_lines.into_iter().map(|line| div().child(line))),
                )
            })
            .when(card.sparkline.len() >= 2, |element| {
                element.child(sparkline_with_scale(
                    &card.sparkline,
                    color,
                    px(28.0),
                    unit_suffix(signal_id),
                    TREND_WINDOW_LABEL,
                    cx,
                ))
            })
            .into_any_element()
    }

    /// Compact chip for a non-voting Signal (last_clone, sessions,
    /// table_growth): one line of dot, label and value. Clicking opens the
    /// Drill-in like a full card does.
    fn context_chip(&self, card: SignalCard, cx: &mut Context<Self>) -> gpui::AnyElement {
        let signal_id = card.signal_id;
        let selected = self.state.selected_card() == Some(signal_id);
        let summary = self.state.card_summary(signal_id);
        let detail = self.state.card_detail(signal_id);
        let value = if card.status == crate::dashboard_state::WAITING {
            "waiting".to_owned()
        } else if !summary.is_empty() {
            summary
        } else if !detail.is_empty() {
            detail
        } else {
            card.status.clone()
        };
        let quiet = card.dimmed || card.muted;
        let hollow = !card.muted && (card.dimmed || card.status == "skipped");
        let color = if quiet {
            cx.theme().muted_foreground
        } else {
            status_color(&card.status, cx)
        };
        h_flex()
            .id(SharedString::from(format!("context-{signal_id}")))
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded_full()
            .border_1()
            .border_color(if selected {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .bg(cx.theme().secondary)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.select_card(signal_id);
                cx.notify();
            }))
            .child(status_dot(color, hollow))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(signal_label(signal_id)),
            )
            .child(
                div()
                    .max_w(px(260.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(value),
            )
            .into_any_element()
    }

    /// The Drill-in: a bounded region under the cards showing the rows, trend
    /// 24 h / 7 d / 30 d switch for trend Signals. The 24 h view reads raw
    /// samples; 7 d / 30 d read hourly roll-ups. Other Signals get no switch.
    fn trend_switch(
        &self,
        signal_id: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // Windowed rows are not trends: the switch only shows while a trend
        // draws, so it never sits inert above row lists or text.
        if !is_trend_signal(signal_id)
            || !matches!(
                self.state.drill_in(signal_id, unix_now()),
                DrillIn::Trend(_)
            )
        {
            return None;
        }
        let active = self.state.trend_window();
        Some(
            h_flex()
                .items_center()
                .gap(px(8.0))
                .children(TrendWindow::ALL.into_iter().map(|(window, label)| {
                    let selected = window == active;
                    div()
                        .id(SharedString::from(format!("trend-{label}")))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(if selected {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        })
                        .hover(|style| style.text_decoration_1())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.set_trend_window(window);
                            cx.notify();
                        }))
                        .child(label)
                }))
                .when_some(
                    self.state.week_delta_label(signal_id, unix_now()),
                    |element, delta| {
                        element.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(delta),
                        )
                    },
                )
                .into_any_element(),
        )
    }

    /// or text the selected Signal's snapshot already carries.
    fn drill_in_region(&self, signal_id: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let content = self.state.drill_in(signal_id, unix_now());
        let url = self.state.signal_url(signal_id);
        let platform_id = self
            .state
            .selected()
            .map(|env| env.platform_id.as_str())
            .unwrap_or("servicenow");
        let status = self
            .state
            .cards(unix_now())
            .into_iter()
            .find(|card| card.signal_id == signal_id)
            .map(|card| card.status)
            .unwrap_or_default();
        let color = if self.state.connected() {
            status_color(&status, cx)
        } else {
            cx.theme().muted_foreground
        };
        v_flex()
            .mx(px(22.0))
            .mb(px(16.0))
            .pb(px(10.0))
            .rounded(cx.theme().radius)
            .border_2()
            .border_color(cx.theme().primary)
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .child(div().size(px(8.0)).rounded_full().bg(color))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(signal_label(signal_id)),
                    )
                    .when_some(url, |element, url| {
                        element.child(
                            open_link(
                                format!("drill-open-{signal_id}").into(),
                                url,
                                drill_open_label(platform_id),
                            )
                            .text_xs(),
                        )
                    })
                    .children(self.trend_switch(signal_id, cx))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("drill-close")
                            .px(px(6.0))
                            .rounded(cx.theme().radius)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().muted))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.state.select_card(signal_id);
                                cx.notify();
                            }))
                            .child("\u{2715}"),
                    ),
            )
            .map(|element| match content {
                DrillIn::Rows {
                    headers,
                    rows,
                    truncated,
                } => element
                    .child(
                        compare_row_cells(headers.into_iter().map(str::to_owned))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(Separator::horizontal().color(cx.theme().border))
                    .children(rows.into_iter().map(|row| {
                        drill_in_row_cells(row)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                    }))
                    .when(truncated, |element| {
                        element.child(
                            div()
                                .px(px(14.0))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("… more rows on the instance — open with the link above"),
                        )
                    }),
                DrillIn::Trend(points) => element
                    .when_some(
                        self.state.anomaly_note(signal_id, unix_now()),
                        |element, note| {
                            element.child(
                                div()
                                    .px(px(14.0))
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child(note),
                            )
                        },
                    )
                    .child(div().px(px(14.0)).child(sparkline_with_scale(
                        &points,
                        color,
                        px(80.0),
                        unit_suffix(signal_id),
                        self.state.trend_window().caption(),
                        cx,
                    ))),
                DrillIn::Text(text) => element.child(
                    div()
                        .px(px(14.0))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                ),
                DrillIn::Empty => element.child(
                    div()
                        .px(px(14.0))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Nothing recorded yet."),
                ),
            })
            .into_any_element()
    }
}

/// A deep link that looks like the text it wraps and underlines on hover;
/// gpui-component's `Link` is always link-blue and underlined, which is
/// noise seven times over on a card grid. Stops the mouse-down so opening the
/// instance does not also toggle the Drill-in.
fn open_link(id: SharedString, url: String, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .cursor_pointer()
        .hover(|style| style.text_decoration_1())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(label)
}

/// Status dot: solid for a live reading or chosen silence, hollow for
/// "no reading" (waiting, skipped, unknown, disconnected).
fn status_dot(color: gpui::Hsla, hollow: bool) -> gpui::Div {
    let dot = div().size(px(8.0)).rounded_full();
    if hollow {
        dot.border_1().border_color(color)
    } else {
        dot.bg(color)
    }
}

/// One line that clips instead of wrapping; the full text is on hover. The id
/// names the slot, not the text — an id that changes with the value resets
/// hover and tooltip state on every poll.
fn clipped_line(id: SharedString, text: String) -> gpui::Stateful<gpui::Div> {
    let tip = SharedString::from(text.clone());
    div()
        .id(id)
        .w_full()
        .overflow_hidden()
        .text_ellipsis()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(text)
}

/// Splits a card summary into a prominent value and a muted context line on
/// the summary's "\u{b7}" separator. Summaries put number and unit together
/// before it ("71 errors · last hour"), so nothing else is split.
fn split_summary(summary: &str) -> (String, String) {
    match summary.split_once(" \u{b7} ") {
        Some((value, context)) => (value.to_owned(), context.to_owned()),
        None => (summary.to_owned(), String::new()),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Unit printed after sparkline scale labels.
fn unit_suffix(signal_id: &str) -> &'static str {
    match signal_id {
        "availability" => " ms",
        _ => "",
    }
}

fn disconnected_banner(cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .px(px(14.0))
        .py(px(8.0))
        .bg(cx.theme().danger.opacity(0.22))
        .text_color(cx.theme().danger)
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child("Disconnected — showing last known data")
}

/// Drill-in deep-link label per platform. The destination is the probe
/// target or the repository on non-ServiceNow platforms, never ServiceNow.
fn drill_open_label(platform_id: &str) -> &'static str {
    match platform_id {
        "github" => "Open repository \u{2197}",
        "http" => "Open probe target \u{2197}",
        _ => "Open in ServiceNow \u{2197}",
    }
}

/// Short footer form of an export directory: the last two path components.
/// The full path clips in the 220 px sidebar with no copy action.
fn short_export_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() >= 2 {
        format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        path.to_owned()
    }
}

/// Card/sidebar status dot. Known words share `health_color`'s mapping so
/// a theme change lands once; anything else (including `unknown` and
/// `waiting`) dims instead of guessing a severity.
fn status_color(status: &str, cx: &App) -> gpui::Hsla {
    match status {
        "healthy" => health_color(EnvironmentHealth::Healthy, cx),
        "degraded" => health_color(EnvironmentHealth::Degraded, cx),
        "down" => health_color(EnvironmentHealth::Down, cx),
        _ => cx.theme().muted_foreground,
    }
}

fn health_tag(health: EnvironmentHealth) -> Tag {
    match health {
        EnvironmentHealth::Healthy => Tag::success(),
        EnvironmentHealth::Degraded => Tag::warning(),
        EnvironmentHealth::Down => Tag::danger(),
        EnvironmentHealth::Waiting => Tag::secondary(),
    }
    .outline()
    .small()
    .rounded_full()
    .child(match health {
        EnvironmentHealth::Healthy => "healthy",
        EnvironmentHealth::Degraded => "degraded",
        EnvironmentHealth::Down => "down",
        EnvironmentHealth::Waiting => "waiting",
    })
}

fn reachability_tag(reachability: Reachability) -> Tag {
    match reachability {
        Reachability::Reachable => Tag::success().outline(),
        Reachability::Unreachable => Tag::danger().outline(),
        Reachability::Asleep => Tag::secondary(),
    }
    .small()
    .rounded_full()
    .child(match reachability {
        Reachability::Reachable => "reachable",
        Reachability::Unreachable => "unreachable",
        Reachability::Asleep => "asleep",
    })
}

fn health_color(health: EnvironmentHealth, cx: &App) -> gpui::Hsla {
    match health {
        EnvironmentHealth::Healthy => cx.theme().success,
        EnvironmentHealth::Degraded => cx.theme().warning,
        EnvironmentHealth::Down => cx.theme().danger,
        EnvironmentHealth::Waiting => cx.theme().muted_foreground,
    }
}

/// gpui-component's `Table` needs a delegate `Entity`, which `render_detail`
/// (a `&App` render with no entity context) cannot build, so the strip is a
/// bordered grid with a `Separator` under the header row.
/// One drill-in table row. The first cell is a deep link when the row
/// carries one (job rows link their ServiceNow records); opening it must not
/// toggle the Drill-in, like the header link.
fn drill_in_row_cells(row: crate::dashboard_state::DrillInRow) -> gpui::Div {
    h_flex()
        .w_full()
        .px(px(14.0))
        .py(px(8.0))
        .gap(px(12.0))
        .children(row.cells.into_iter().enumerate().map(|(index, cell)| {
            let body = div().flex_1().min_w_0().overflow_hidden().text_ellipsis();
            match (&row.link, index) {
                (Some(url), 0) => body
                    .child(
                        div()
                            .id(SharedString::from(format!("drill-row-{url}")))
                            .cursor_pointer()
                            .hover(|style| style.text_decoration_1())
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .on_click({
                                let url = url.clone();
                                move |_, _, cx| cx.open_url(&url)
                            })
                            .child(cell),
                    )
                    .into_any_element(),
                _ => body.child(cell).into_any_element(),
            }
        }))
}

fn compare_row_cells(cells: impl IntoIterator<Item = String>) -> gpui::Div {
    h_flex()
        .w_full()
        .px(px(14.0))
        .py(px(8.0))
        .gap(px(12.0))
        .children(cells.into_iter().map(|cell| {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(cell)
        }))
}

/// A sparkline with its scale: max at the top right, min at the bottom right,
/// and the window it spans underneath — a line with no scale cannot say
/// whether 186 is a lot.
fn sparkline_with_scale(
    points: &[Option<f64>],
    color: gpui::Hsla,
    height: Pixels,
    unit: &'static str,
    window_label: &str,
    cx: &App,
) -> impl IntoElement {
    let values: Vec<f64> = points.iter().filter_map(|p| *p).collect();
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    v_flex()
        .w_full()
        .mt(px(8.0))
        .gap(px(2.0))
        .child(
            h_flex()
                .w_full()
                .items_stretch()
                .gap(px(6.0))
                .child(sparkline(points, color, height))
                .child(
                    v_flex()
                        .justify_between()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{max:.0}{unit}"))
                        .child(format!("{min:.0}{unit}")),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(window_label.to_owned()),
        )
}

fn sparkline(points: &[Option<f64>], color: gpui::Hsla, height: Pixels) -> impl IntoElement {
    let points = points.to_vec();
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| paint_sparkline(bounds, &points, color, window),
    )
    .h(height)
    .flex_1()
}

fn paint_sparkline(
    bounds: Bounds<Pixels>,
    points: &[Option<f64>],
    color: gpui::Hsla,
    window: &mut Window,
) {
    let values: Vec<f64> = points.iter().filter_map(|p| *p).collect();
    if values.len() < 2 {
        return;
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1.0);
    let mut path = PathBuilder::stroke(px(1.5));
    let last = (points.len() - 1) as f32;
    let mut pen_down = false;
    for (index, value) in points.iter().enumerate() {
        let Some(value) = value else {
            // Gap: break the line so missing intervals render as a break,
            // not a connection across unobserved time.
            pen_down = false;
            continue;
        };
        let x = bounds.left() + bounds.size.width * (index as f32 / last);
        let y = bounds.bottom() - bounds.size.height * (((value - min) / span) as f32);
        let point: Point<Pixels> = point(x, y);
        if !pen_down {
            path.move_to(point);
            pen_down = true;
        } else {
            path.line_to(point);
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::short_export_path;
    use super::split_summary;

    #[test]
    fn split_summary_splits_value_from_context() {
        assert_eq!(
            split_summary("142 ms \u{b7} glide-zurich-patch3"),
            ("142 ms".to_owned(), "glide-zurich-patch3".to_owned())
        );
        assert_eq!(
            split_summary("38 errors \u{b7} last hour"),
            ("38 errors".to_owned(), "last hour".to_owned())
        );
        assert_eq!(
            split_summary("source of truth"),
            ("source of truth".to_owned(), String::new())
        );
        // A build-only availability summary has no separator: it stays whole
        // on the value line, which clips rather than wraps.
        assert_eq!(
            split_summary("glide-zurich-patch3"),
            ("glide-zurich-patch3".to_owned(), String::new())
        );
        // No splitting after a leading number: "142 ms" is one value.
        assert_eq!(
            split_summary("142 ms"),
            ("142 ms".to_owned(), String::new())
        );
    }

    #[test]
    fn capitalize_uppercases_the_first_char_only() {
        assert_eq!(
            super::capitalize("no clone source configured"),
            "No clone source configured"
        );
        assert_eq!(super::capitalize(""), "");
    }

    #[test]
    fn short_export_path_keeps_the_last_two_components() {
        assert_eq!(
            short_export_path("/Users/op/.daku/exports/prod-1700000000"),
            "…/exports/prod-1700000000"
        );
        assert_eq!(short_export_path("prod-1"), "prod-1");
        assert_eq!(short_export_path(""), "");
    }
}
