//! Environments overview — sidebar + detail (variant C).

use daku_client::DaemonSupervisor;
use daku_protocol::{EnvironmentHealth, Reachability};
use gpui::{
    App, Bounds, ClickEvent, Context, Entity, FontWeight, IntoElement, PathBuilder, Pixels, Point,
    SharedString, Window, canvas, div, point, prelude::*, px,
};

use crate::dashboard_state::{
    CompareRow, DashboardState, SidebarRow, SignalCard, fixture_events, freshness, signal_label,
    ui_fixture_enabled,
};
use crate::theme::Theme;
use crate::{CloseWindow, ToggleFpsCounter};

pub struct Daku {
    state: DashboardState,
    _supervisor: Option<DaemonSupervisor>,
    show_fps: bool,
}

impl Daku {
    pub fn new(
        window: &mut Window,
        cx: &mut App,
        supervisor: Option<DaemonSupervisor>,
    ) -> Entity<Self> {
        window.on_window_should_close(cx, |window, _cx| {
            crate::platform::hide_window(window);
            false
        });
        cx.new(|cx| {
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
            }
        })
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
        let theme = Theme::current(cx);
        div()
            .size_full()
            .bg(theme.canvas)
            .flex()
            .flex_col()
            .text_color(theme.text)
            .on_action(cx.listener(|this, _: &ToggleFpsCounter, window, cx| {
                this.show_fps = !this.show_fps;
                window.refresh();
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| {
                crate::platform::hide_window(window);
            }))
            .when(!self.state.connected(), |element| {
                element.child(disconnected_banner(&theme))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&theme, cx))
                    .child(self.render_detail(&theme)),
            )
            .when(self.show_fps, |element| {
                element.child(
                    div()
                        .px(px(12.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child("FPS"),
                )
            })
    }
}

impl Daku {
    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(220.0))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .px(px(10.0))
            .py(px(12.0))
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .px(px(8.0))
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("daku · ServiceNow"),
            )
            .child(section_label("Platforms", theme))
            .child(platform_row(theme))
            .child(section_label("Environments", theme))
            .children(self.environment_rows(theme, cx))
    }

    fn environment_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let selected_id = self.state.selected_id().map(str::to_owned);
        self.state
            .sidebar()
            .into_iter()
            .map(|row| environment_row(row, selected_id.as_deref(), theme, cx))
            .collect()
    }

    fn render_detail(&self, theme: &Theme) -> impl IntoElement {
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
                            .border_color(theme.border)
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
                                    .child(health_badge(environment.health, theme))
                                    .child(reachability_badge(environment.reachability, theme)),
                            )
                            .child(
                                div()
                                    .mt(px(6.0))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_tertiary)
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
                                                        theme.warning
                                                    } else {
                                                        theme.text_tertiary
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
                            .children(cards.into_iter().map(|card| self.signal_card(card, theme))),
                    )
                    .when(strip.visible, |element| {
                        element.child(compare_strip(
                            strip.has_mismatch,
                            &selected_id,
                            &rows,
                            theme,
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
                        .text_color(theme.text_tertiary)
                        .child(message),
                )
            })
    }

    fn signal_card(&self, card: SignalCard, theme: &Theme) -> impl IntoElement {
        let summary = self.state.card_summary(card.signal_id);
        let detail = self.state.card_detail(card.signal_id);
        let waiting = card.status == crate::dashboard_state::WAITING;
        div()
            .id(SharedString::from(format!("card-{}", card.signal_id)))
            .w(px(220.0))
            .flex_grow(1.0)
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_tertiary)
                    .child(status_dot(&card.status, theme))
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
                        .text_color(theme.text_tertiary)
                        .child(detail),
                )
            })
            .when(card.sparkline.len() >= 2, |element| {
                element.child(sparkline(&card.sparkline, theme.accent))
            })
    }
}

fn environment_row(
    row: SidebarRow,
    selected_id: Option<&str>,
    theme: &Theme,
    cx: &mut Context<Daku>,
) -> gpui::AnyElement {
    let selected = selected_id == Some(row.id.as_str());
    let id = row.id.clone();
    div()
        .id(SharedString::from(format!("env-{}", row.id)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .when(selected, |element| {
            element.bg(theme.sidebar_item_background)
        })
        .text_color(if selected {
            theme.text
        } else {
            theme.text_secondary
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.state.select(&id);
            cx.notify();
        }))
        .child(health_dot(row.health, row.muted, theme))
        .child(row.label)
        .into_any_element()
}

fn disconnected_banner(theme: &Theme) -> impl IntoElement {
    div()
        .w_full()
        .px(px(14.0))
        .py(px(8.0))
        .bg(theme.danger_soft)
        .text_color(theme.danger)
        .text_size(px(12.0))
        .child("Disconnected")
}

fn section_label(label: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .px(px(8.0))
        .text_size(px(10.0))
        .text_color(theme.text_ghost)
        .child(label.to_ascii_uppercase())
}

fn platform_row(theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .bg(theme.sidebar_item_background)
        .child("ServiceNow")
}

fn health_dot(health: EnvironmentHealth, muted: bool, theme: &Theme) -> impl IntoElement {
    div().size(px(8.0)).rounded_full().bg(if muted {
        theme.text_ghost
    } else {
        health_color(health, theme)
    })
}

fn status_dot(status: &str, theme: &Theme) -> impl IntoElement {
    let color = match status {
        "healthy" => theme.success,
        "degraded" => theme.warning,
        "down" => theme.danger,
        _ => theme.text_ghost,
    };
    div().size(px(8.0)).rounded_full().bg(color)
}

fn health_badge(health: EnvironmentHealth, theme: &Theme) -> impl IntoElement {
    badge(
        match health {
            EnvironmentHealth::Healthy => "healthy",
            EnvironmentHealth::Degraded => "degraded",
            EnvironmentHealth::Down => "down",
        },
        health_color(health, theme),
        theme,
    )
}

fn reachability_badge(reachability: Reachability, theme: &Theme) -> impl IntoElement {
    let (label, color) = match reachability {
        Reachability::Reachable => ("reachable", theme.success),
        Reachability::Unreachable => ("unreachable", theme.danger),
        Reachability::Asleep => ("asleep", theme.text_ghost),
    };
    badge(label, color, theme)
}

fn badge(label: &'static str, color: gpui::Hsla, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(999.0))
        .bg(theme.inset)
        .text_size(px(12.0))
        .text_color(theme.text_secondary)
        .child(div().size(px(8.0)).rounded_full().bg(color))
        .child(label)
}

fn health_color(health: EnvironmentHealth, theme: &Theme) -> gpui::Hsla {
    match health {
        EnvironmentHealth::Healthy => theme.success,
        EnvironmentHealth::Degraded => theme.warning,
        EnvironmentHealth::Down => theme.danger,
    }
}

fn compare_strip(
    has_mismatch: bool,
    selected_id: &str,
    rows: &[CompareRow],
    theme: &Theme,
) -> impl IntoElement {
    div()
        .mx(px(22.0))
        .mb(px(16.0))
        .p(px(14.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.inset)
        .child(
            div()
                .mb(px(8.0))
                .text_size(px(12.0))
                .text_color(theme.text_tertiary)
                .child("vs clone source"),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(16.0))
                .text_color(theme.text_secondary)
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
                    .text_color(theme.warning)
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
