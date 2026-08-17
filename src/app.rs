//! Environments overview — sidebar + detail (variant C).

use daku_client::DaemonSupervisor;
use daku_protocol::{EnvironmentHealth, Reachability};
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight,
    IntoElement, PathBuilder, Pixels, Point, SharedString, Window, canvas, div, point, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme as _, TitleBar, h_flex,
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
    },
};

use crate::dashboard_state::{
    CompareRow, DashboardState, SignalCard, fixture_events, freshness, signal_label,
    ui_fixture_enabled,
};
use crate::{CloseWindow, ToggleFpsCounter};

const SIDEBAR_WIDTH: f32 = 220.0;

pub struct Daku {
    state: DashboardState,
    _supervisor: Option<DaemonSupervisor>,
    show_fps: bool,
    /// `Root` owns the window's root dispatch node, so the shell only receives
    /// menu- and keystroke-dispatched actions while this handle is focused.
    focus_handle: FocusHandle,
}

impl Daku {
    pub fn new(
        window: &mut Window,
        cx: &mut App,
        supervisor: Option<DaemonSupervisor>,
    ) -> Entity<Self> {
        let focus_handle = cx.focus_handle();
        let entity = cx.new(|cx| {
            let mut state = DashboardState::new();
            if ui_fixture_enabled() {
                state.set_connected(true);
                state.apply_all(&fixture_events());
            } else if let Some(supervisor) = supervisor.as_ref() {
                listen_dashboard(supervisor, cx);
            }
            tick_freshness(cx);
            Self {
                state,
                _supervisor: supervisor,
                show_fps: false,
                focus_handle: focus_handle.clone(),
            }
        });
        window.focus(&focus_handle, cx);
        entity
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

fn listen_dashboard(supervisor: &DaemonSupervisor, cx: &mut Context<Daku>) {
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
                        let _ = this.update(cx, |this, cx| {
                            this.state.apply(&message);
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
        let sidebar = self.render_sidebar(cx);
        let detail = self.render_detail(cx);
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .flex()
            .flex_col()
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &ToggleFpsCounter, window, cx| {
                this.show_fps = !this.show_fps;
                window.refresh();
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| {
                crate::platform::hide_window(window);
            }))
            .child(TitleBar::new().child(div().text_sm().child("daku")))
            .when(!self.state.connected(), |element| {
                element.child(disconnected_banner(cx))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(sidebar)
                    .child(detail),
            )
            .when(self.show_fps, |element| {
                element.child(
                    div()
                        .px(px(12.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .text_color(cx.theme().muted_foreground)
                        .child("FPS"),
                )
            })
    }
}

impl Daku {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected_id = self.state.selected_id().map(str::to_owned);
        let items: Vec<SidebarMenuItem> = self
            .state
            .sidebar()
            .into_iter()
            .map(|row| {
                let selected = selected_id.as_deref() == Some(row.id.as_str());
                let id = row.id.clone();
                let color = if row.muted {
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

        Sidebar::new("daku-sidebar")
            .collapsible(SidebarCollapsible::None)
            .w(px(SIDEBAR_WIDTH))
            .header(SidebarHeader::new().child(div().text_sm().child("ServiceNow")))
            .child(SidebarGroup::new("Environments").child(SidebarMenu::new().children(items)))
            .into_any_element()
    }

    fn render_detail(&self, cx: &App) -> gpui::AnyElement {
        let selected = self.state.selected().cloned();
        let cards = selected
            .as_ref()
            .map(|_| self.state.cards())
            .unwrap_or_default();
        let strip = self.state.compare_strip();
        let rows = self.state.compare_rows();
        div()
            .id("detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .when_some(selected, |element, environment| {
                let selected_id = environment.id.clone();
                element
                    .child(
                        div()
                            .px(px(22.0))
                            .pt(px(18.0))
                            .pb(px(10.0))
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_size(px(20.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(environment.label.clone()),
                            )
                            .child(
                                div()
                                    .mt(px(6.0))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(health_badge(environment.health, cx))
                                    .child(reachability_badge(environment.reachability, cx)),
                            )
                            .child(
                                div()
                                    .mt(px(6.0))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_size(px(12.0))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        environment
                                            .instance_url
                                            .trim_start_matches("https://")
                                            .to_owned(),
                                    )
                                    .when_some(
                                        freshness(environment.last_observed_at, unix_now()),
                                        |element, fresh| {
                                            element.child("\u{b7}").child(
                                                div()
                                                    .text_color(if fresh.stale {
                                                        cx.theme().warning
                                                    } else {
                                                        cx.theme().muted_foreground
                                                    })
                                                    .child(fresh.label),
                                            )
                                        },
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(10.0))
                            .p(px(22.0))
                            .children(cards.into_iter().map(|card| self.signal_card(card, cx))),
                    )
                    .when(strip.visible, |element| {
                        element.child(compare_strip(
                            strip.has_mismatch,
                            &selected_id,
                            &rows,
                            cx,
                        ))
                    })
            })
            .when(self.state.selected().is_none(), |element| {
                let message = if self.state.connected() && !self.state.has_environments() {
                    "No Environments configured \u{2014} copy environments.example.json to ~/.daku/environments.json, then relaunch daku. Daemon diagnostics: ~/.daku/daemon.log"
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

    fn signal_card(&self, card: SignalCard, cx: &App) -> gpui::AnyElement {
        let summary = self.state.card_summary(card.signal_id);
        let detail = self.state.card_detail(card.signal_id);
        let mismatch_lines = if card.signal_id == "drift" {
            self.state.drift_mismatch_lines(5)
        } else {
            Vec::new()
        };
        let waiting = card.status == crate::dashboard_state::WAITING;
        div()
            .id(SharedString::from(format!("card-{}", card.signal_id)))
            .w(px(220.0))
            .flex_grow(1.0)
            .p(px(12.0))
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(status_dot(&card.status, cx))
                    .child(signal_label(card.signal_id)),
            )
            .child(div().mt(px(6.0)).text_size(px(15.0)).child(if waiting {
                crate::dashboard_state::WAITING.to_owned()
            } else if summary.is_empty() {
                card.status.clone()
            } else {
                summary
            }))
            .when(!detail.is_empty(), |element| {
                element.child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(11.0))
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
            })
            .when(!mismatch_lines.is_empty(), |element| {
                element.child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(11.0))
                        .text_color(cx.theme().muted_foreground)
                        .children(mismatch_lines.into_iter().map(|line| div().child(line))),
                )
            })
            .when(card.sparkline.len() >= 2, |element| {
                element.child(sparkline(&card.sparkline, cx.theme().accent))
            })
            .into_any_element()
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

fn status_dot(status: &str, cx: &App) -> impl IntoElement {
    let color = match status {
        "healthy" => cx.theme().success,
        "degraded" => cx.theme().warning,
        "down" => cx.theme().danger,
        _ => cx.theme().muted_foreground,
    };
    div().size(px(8.0)).rounded_full().bg(color)
}

fn health_badge(health: EnvironmentHealth, cx: &App) -> impl IntoElement {
    badge(
        match health {
            EnvironmentHealth::Healthy => "healthy",
            EnvironmentHealth::Degraded => "degraded",
            EnvironmentHealth::Down => "down",
        },
        health_color(health, cx),
        cx,
    )
}

fn reachability_badge(reachability: Reachability, cx: &App) -> impl IntoElement {
    let (label, color) = match reachability {
        Reachability::Reachable => ("reachable", cx.theme().success),
        Reachability::Unreachable => ("unreachable", cx.theme().danger),
        Reachability::Asleep => ("asleep", cx.theme().muted_foreground),
    };
    badge(label, color, cx)
}

/// gpui-component's `Badge` is a count/dot *overlay*, not a label pill, so the
/// pill stays local until plan 045 owns the badge design.
fn badge(label: &'static str, color: gpui::Hsla, cx: &App) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(999.0))
        .bg(cx.theme().muted)
        .text_size(px(12.0))
        .text_color(cx.theme().muted_foreground)
        .child(div().size(px(8.0)).rounded_full().bg(color))
        .child(label)
}

fn health_color(health: EnvironmentHealth, cx: &App) -> gpui::Hsla {
    match health {
        EnvironmentHealth::Healthy => cx.theme().success,
        EnvironmentHealth::Degraded => cx.theme().warning,
        EnvironmentHealth::Down => cx.theme().danger,
    }
}

fn compare_strip(
    has_mismatch: bool,
    selected_id: &str,
    rows: &[CompareRow],
    cx: &App,
) -> impl IntoElement {
    div()
        .mx(px(22.0))
        .mb(px(16.0))
        .p(px(14.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .child(
            div()
                .mb(px(8.0))
                .text_size(px(12.0))
                .text_color(cx.theme().muted_foreground)
                .child("vs clone source"),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(16.0))
                .text_color(cx.theme().muted_foreground)
                .children(
                    rows.iter()
                        .filter(|row| row.id != selected_id)
                        .map(|row| div().child(compare_row_text(row))),
                ),
        )
        .when(has_mismatch, |element| {
            element.child(
                div()
                    .mt(px(8.0))
                    .text_color(cx.theme().warning)
                    .text_size(px(12.0))
                    .child("build / drift mismatch"),
            )
        })
}

fn compare_row_text(row: &CompareRow) -> String {
    let mut text = format!(
        "{}: {}",
        row.label,
        row.build.as_deref().unwrap_or("\u{2014}")
    );
    if !row.drift.is_empty() {
        text.push_str(&format!(" \u{b7} drift {}", row.drift));
    }
    if !row.last_clone.is_empty() {
        text.push_str(&format!(" \u{b7} clone {}", row.last_clone));
    }
    text
}

fn sparkline(points: &[f64], color: gpui::Hsla) -> impl IntoElement {
    let points = points.to_vec();
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| paint_sparkline(bounds, &points, color, window),
    )
    .h(px(28.0))
    .w_full()
    .mt(px(8.0))
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
