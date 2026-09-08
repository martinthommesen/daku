//! Environments overview — sidebar + detail (variant C).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, mpsc};

use daku_client::DaemonSupervisor;
use daku_protocol::{EnvironmentHealth, HealthEventDto, Reachability, ServerMessage};
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight,
    IntoElement, PathBuilder, Pixels, Point, SharedString, Window, canvas, div, point, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, TitleBar, h_flex,
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
use crate::CloseWindow;
use crate::CopySummary;
use crate::DetachSelectedEnvironment;
use crate::ReloadDaemon;
use crate::SelectEnvironment;
use crate::SelectEnvironmentSlot;
use crate::ToggleNotifications;
use crate::dashboard_state::{
    DashboardState, DrillIn, SignalCard, TREND_WINDOW_LABEL, TrendWindow, age_phrase,
    fixture_events, format_health_event, freshness, is_trend_signal, mute_remaining_label,
    signal_label, ui_fixture_enabled,
};
use crate::env_sheet::{EnvSheet, auth_label, build_config, credential_blob, credential_captions};
use crate::notifications::{
    notification_body, notification_title, post_health_notification, select_notification,
};
use crate::persistence::{AppSettings, save_app_settings};

const SIDEBAR_WIDTH: f32 = 220.0;

pub struct Daku {
    state: DashboardState,
    supervisor: Option<DaemonSupervisor>,
    settings: Arc<Mutex<AppSettings>>,
    /// Copy-summary confirmation, cleared ~2 s after ⌘⇧C.
    copied_flash: bool,
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
                copied_flash: false,
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
    /// replays, muted Environments and the global off switch stay silent.
    /// Detached windows never post: the main window owns notifications.
    fn note_health_events(&mut self, env_id: &str, events: &[HealthEventDto]) {
        if self.detached.is_some() {
            return;
        }
        let (record, fire) = select_notification(env_id, events, &self.notify_seen, self.boot_now);
        self.notify_seen.extend(record);
        let Some(event) = fire else { return };
        let notifications_enabled = self
            .settings
            .lock()
            .map(|settings| settings.notifications_enabled)
            .unwrap_or(true);
        if !notifications_enabled {
            return;
        }
        if self.state.is_muted(env_id, unix_now()) {
            return;
        }
        let label = self
            .state
            .environment_label(env_id)
            .unwrap_or(env_id)
            .to_owned();
        let (health, headline) = match self.state.headline_for(env_id) {
            Some((health, line)) => (health, Some(line)),
            None => (event.to_health, None),
        };
        post_health_notification(
            env_id,
            &notification_title(&label),
            &notification_body(health, headline.as_deref()),
        );
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
        cx.notify();
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
            thresholds: summary.thresholds.clone(),
            expected_drift: summary.expected_drift.clone(),
        });
        let config = build_config(
            id,
            values.label.trim().to_owned(),
            values.url.trim().to_owned(),
            sheet.auth,
            sheet.clone_source,
            sort_order,
            existing_config.as_ref(),
        );
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
    /// entered, plus threshold and expected-drift parsing. Shows the first
    /// problem in the sheet.
    fn validate_sheet(
        &mut self,
        cx: &App,
    ) -> Option<(daku_protocol::EnvironmentConfig, Option<String>)> {
        let (mut config, blob) = self.read_sheet(cx)?;
        if let Err(error) =
            crate::env_sheet::validate_fields(&config.id, &config.label, &config.instance_url)
        {
            self.set_sheet_notice(true, error);
            return None;
        }
        let sheet = self.env_sheet.as_ref()?;
        match sheet.thresholds_result(cx) {
            Ok(thresholds) => config.thresholds = thresholds,
            Err(error) => {
                self.set_sheet_notice(true, error);
                return None;
            }
        }
        match sheet.expected_drift_result(cx) {
            Ok(expected_drift) => config.expected_drift = expected_drift,
            Err(error) => {
                self.set_sheet_notice(true, error);
                return None;
            }
        }
        if let Some(blob) = &blob
            && let Err(error) = daku_protocol::validate_credential(config.auth_method, blob)
        {
            self.set_sheet_notice(true, error);
            return None;
        }
        Some((config, blob))
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
        let text = self.state.summary_text(unix_now());
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied_flash = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.copied_flash = false;
                cx.notify();
            });
        })
        .detach();
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
        // render.
        let cards: Vec<gpui::AnyElement> = if self.state.selected().is_some() {
            self.state
                .cards(unix_now())
                .into_iter()
                .map(|card| self.signal_card(card, cx))
                .collect()
        } else {
            Vec::new()
        };
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
                .child("This Environment was removed.")
                .into_any_element()
        } else {
            self.render_detail(cards, drill_in, mute_controls, compare, cx)
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
            .on_action(cx.listener(|this, _: &ToggleNotifications, _, cx| {
                if let Ok(mut settings) = this.settings.lock() {
                    settings.notifications_enabled = !settings.notifications_enabled;
                }
                this.persist_settings();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AddEnvironment, window, cx| {
                this.open_add_sheet(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DetachSelectedEnvironment, _, cx| {
                let Some(id) = this.state.selected_id().map(str::to_owned) else {
                    return;
                };
                let supervisor = this.supervisor.clone();
                let settings = this.settings.clone();
                let bounds = gpui::WindowBounds::Windowed(gpui::Bounds::centered(
                    None,
                    gpui::size(gpui::px(1100.0), gpui::px(800.0)),
                    cx,
                ));
                let _ = crate::open_daku_window(cx, supervisor, settings, None, Some(id), bounds);
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

/// Small "Edit" opener for the Environment sheet (`106`). A free function
/// so both mute-controls branches share it.
fn edit_button(cx: &mut Context<Daku>) -> gpui::AnyElement {
    div()
        .id("env-edit")
        .text_color(cx.theme().muted_foreground)
        .cursor_pointer()
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.open_edit_sheet(window, cx);
        }))
        .child("Edit")
        .into_any_element()
}

/// Toggle meanings in sheet option rows.
#[derive(Clone, Copy, PartialEq)]
enum SheetToggle {
    Auth(daku_protocol::AuthMethod),
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

impl Daku {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected_id = self.state.selected_id().map(str::to_owned);
        let items: Vec<SidebarMenuItem> = self
            .state
            .sidebar(unix_now())
            .into_iter()
            .map(|row| {
                let selected = selected_id.as_deref() == Some(row.id.as_str());
                let id = row.id.clone();
                let quiet = row.dimmed || row.muted;
                let color = if quiet {
                    cx.theme().muted_foreground
                } else {
                    health_color(row.health, cx)
                };
                SidebarMenuItem::new(row.label.clone())
                    .active(selected)
                    .suffix(move |_, _| div().size(px(8.0)).rounded_full().bg(color))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.select(&id);
                        cx.notify();
                    }))
            })
            .collect();
        // Roll-up: the worst unmuted Environment colours the header dot.
        // Muted Environments stay quiet on every attention surface.
        let roll_up = self
            .state
            .worst_health_excluding_muted(unix_now())
            .map_or(cx.theme().muted_foreground, |health| {
                health_color(health, cx)
            });
        let footer = if self.copied_flash {
            "Copied Environment summary".to_owned()
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

        Sidebar::new("daku-sidebar")
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
                        .child(div().text_sm().child("ServiceNow")),
                ),
            )
            .child(SidebarGroup::new("Environments").child(SidebarMenu::new().children(items)))
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
                        .text_color(cx.theme().muted_foreground)
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
                base.child(div().text_color(cx.theme().muted_foreground).child("Mute"))
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
                        .child(EnvSheet::field_row("Label", &sheet.label_field, cx))
                        .child(EnvSheet::field_row("Instance URL", &sheet.url_field, cx))
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
                        .child(self.sheet_toggle_row(
                            "Clone source",
                            &[
                                (SheetToggle::CloneSource(false), "No"),
                                (SheetToggle::CloneSource(true), "Yes"),
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
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Thresholds — empty means default. Jobs errors, email, update sets, RTT and transaction avg accept off.",
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
                        .child(EnvSheet::field_row(
                            "Expected drift ids (comma-separated)",
                            &sheet.expected_drift_field,
                            cx,
                        ))
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
                            .pb(px(10.0))
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child("build / drift mismatch"),
                    )
                })
                .into_any_element(),
        )
    }

    /// Recent health/build transitions under the Signal cards. Omitted
    /// entirely without events — a fresh Environment shows no empty box.
    fn recent_block(&self, cx: &App) -> Option<gpui::AnyElement> {
        let now = unix_now();
        let lines: Vec<String> = self
            .state
            .recent_events(5)
            .iter()
            .map(|event| format_health_event(event, now))
            .collect();
        if lines.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .mx(px(22.0))
                .mb(px(16.0))
                .gap(px(2.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Recent"),
                )
                .children(lines.into_iter().map(|line| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(line)
                }))
                .into_any_element(),
        )
    }

    fn render_detail(
        &self,
        cards: Vec<gpui::AnyElement>,
        drill_in: Option<gpui::AnyElement>,
        mute_controls: Option<gpui::AnyElement>,
        compare: Option<gpui::AnyElement>,
        cx: &App,
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
                element
                    .child(
                        v_flex()
                            .px(px(22.0))
                            .pt(px(18.0))
                            .pb(px(12.0))
                            .gap(px(6.0))
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
                                    )
                                    .children(mute_controls),
                            )
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
                            .when_some(self.state.build_age(), |element, (_, since)| {
                                element.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "on this build {} ago",
                                            age_phrase(unix_now().saturating_sub(since))
                                        )),
                                )
                            })
                            .children(self.state.health_explain().into_iter().map(|line| {
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().warning)
                                    .child(line)
                            }))
                            .when_some(
                                self.state.correlation_build(unix_now()),
                                |element, build| {
                                    element.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "Started after build {build} — likely clone/upgrade fallout"
                                            )),
                                    )
                                },
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(12.0))
                            .p(px(22.0))
                            .children(cards),
                    )
                    .children(self.recent_block(cx))
                    .children(drill_in)
                    .children(compare)
            })
            .when(self.state.selected().is_none(), |element| {
                let message = if self.state.connected() && !self.state.has_environments() {
                    "No Environments configured — add one from the app menu (daku → Add Environment…), or copy environments.example.json to ~/.daku/environments.json and press ⌘R. Daemon diagnostics: ~/.daku/daemon.log"
                } else {
                    "No Environment selected."
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
        // Disconnected cards are stale; muted cards are quiet on purpose.
        // Both paint grey and never carry attention colour.
        let quiet = card.dimmed || card.muted;
        let attention = !quiet && matches!(card.status.as_str(), "degraded" | "down");
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
                color.opacity(0.4)
            } else {
                gpui::transparent_black()
            })
            // Cards that need attention carry their colour, not just a dot.
            .bg(if attention {
                color.opacity(0.10)
            } else {
                cx.theme().secondary
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
                    .child(div().size(px(8.0)).rounded_full().bg(color))
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
                    cx,
                ))
            })
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
                .into_any_element(),
        )
    }

    /// or text the selected Signal's snapshot already carries.
    fn drill_in_region(&self, signal_id: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let content = self.state.drill_in(signal_id, unix_now());
        let url = self.state.signal_url(signal_id);
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
                                "Open in ServiceNow \u{2197}",
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
                                .child("\u{2026} more on the instance"),
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
        .bg(cx.theme().danger.opacity(0.15))
        .text_color(cx.theme().danger)
        .text_size(px(12.0))
        .child("Disconnected")
}

fn status_color(status: &str, cx: &App) -> gpui::Hsla {
    match status {
        "healthy" => cx.theme().success,
        "degraded" => cx.theme().warning,
        "down" => cx.theme().danger,
        _ => cx.theme().muted_foreground,
    }
}

fn health_tag(health: EnvironmentHealth) -> Tag {
    match health {
        EnvironmentHealth::Healthy => Tag::success(),
        EnvironmentHealth::Degraded => Tag::warning(),
        EnvironmentHealth::Down => Tag::danger(),
    }
    .outline()
    .small()
    .rounded_full()
    .child(match health {
        EnvironmentHealth::Healthy => "healthy",
        EnvironmentHealth::Degraded => "degraded",
        EnvironmentHealth::Down => "down",
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
    points: &[f64],
    color: gpui::Hsla,
    height: Pixels,
    unit: &'static str,
    cx: &App,
) -> impl IntoElement {
    let min = points.iter().copied().fold(f64::INFINITY, f64::min);
    let max = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
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
                .child(TREND_WINDOW_LABEL),
        )
}

fn sparkline(points: &[f64], color: gpui::Hsla, height: Pixels) -> impl IntoElement {
    let points = points.to_vec();
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| paint_sparkline(bounds, &points, color, window),
    )
    .h(height)
    .flex_1()
}

fn paint_sparkline(bounds: Bounds<Pixels>, points: &[f64], color: gpui::Hsla, window: &mut Window) {
    if points.len() < 2 {
        return;
    }
    let min = points.iter().copied().fold(f64::INFINITY, f64::min);
    let max = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1.0);
    let mut path = PathBuilder::stroke(px(1.5));
    let last = (points.len() - 1) as f32;
    for (index, value) in points.iter().enumerate() {
        let x = bounds.left() + bounds.size.width * (index as f32 / last);
        let y = bounds.bottom() - bounds.size.height * (((value - min) / span) as f32);
        let point: Point<Pixels> = point(x, y);
        if index == 0 {
            path.move_to(point);
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
}
