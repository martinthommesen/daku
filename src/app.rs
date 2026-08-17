//! Environments overview — sidebar + detail (variant C).

use daku_client::DaemonSupervisor;
use daku_protocol::{EnvironmentHealth, Reachability};
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight,
    IntoElement, PathBuilder, Pixels, Point, SharedString, Window, canvas, div, point, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, TitleBar, h_flex,
    separator::Separator,
    sidebar::{
        Sidebar, SidebarCollapsible, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
    },
    skeleton::Skeleton,
    tag::Tag,
    tooltip::Tooltip,
    v_flex,
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
                        v_flex()
                            .px(px(22.0))
                            .pt(px(18.0))
                            .pb(px(12.0))
                            .gap(px(6.0))
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(environment.label.clone()),
                                    )
                                    .child(health_tag(environment.health))
                                    .child(reachability_tag(environment.reachability)),
                            )
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_sm()
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
                            .gap(px(12.0))
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
        let color = status_color(&card.status, cx);
        let (value, context) = split_summary(if summary.is_empty() {
            &card.status
        } else {
            &summary
        });
        div()
            .id(SharedString::from(format!("card-{}", card.signal_id)))
            .w(px(300.0))
            .min_h(px(120.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(14.0))
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().size(px(8.0)).rounded_full().bg(color))
                    .child(signal_label(card.signal_id)),
            )
            .child(if waiting {
                Skeleton::new()
                    .w(px(96.0))
                    .h(px(22.0))
                    .rounded(cx.theme().radius)
                    .into_any_element()
            } else {
                clipped_line(value.clone())
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .into_any_element()
            })
            .when(!context.is_empty(), |element| {
                element.child(
                    clipped_line(context.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .when(!detail.is_empty(), |element| {
                element.child(
                    div()
                        .text_xs()
                        .text_color(if card.status == "down" {
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
                element.child(sparkline(&card.sparkline, color))
            })
            .into_any_element()
    }
}

/// One line that clips instead of wrapping; the full text is on hover.
fn clipped_line(text: String) -> gpui::Stateful<gpui::Div> {
    let tip = SharedString::from(text.clone());
    div()
        .id(SharedString::from(format!("line-{text}")))
        .w_full()
        .overflow_hidden()
        .text_ellipsis()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(text)
}

/// Splits a card summary into a prominent value and a muted context line:
/// on the summary's "\u{b7}" separator when there is one, else after a leading
/// numeric token ("38 errors / h"). Anything else stays whole as the value.
fn split_summary(summary: &str) -> (String, String) {
    if let Some((value, context)) = summary.split_once(" \u{b7} ") {
        return (value.to_owned(), context.to_owned());
    }
    if let Some((first, rest)) = summary.split_once(' ')
        && first.starts_with(|c: char| c.is_ascii_digit())
    {
        return (first.to_owned(), rest.to_owned());
    }
    (summary.to_owned(), String::new())
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
fn compare_strip(
    has_mismatch: bool,
    selected_id: &str,
    rows: &[CompareRow],
    cx: &App,
) -> impl IntoElement {
    let selected_build = rows
        .iter()
        .find(|row| row.id == selected_id)
        .and_then(|row| row.build.clone());
    v_flex()
        .mx(px(22.0))
        .mb(px(16.0))
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .child(
            compare_row_cells(["Environment", "Build", "Drift", "Last clone"].map(str::to_owned))
                .text_xs()
                .text_color(cx.theme().muted_foreground),
        )
        .child(Separator::horizontal().color(cx.theme().border))
        .children(rows.iter().map(|row| {
            let mismatch = selected_build.is_some() && row.build != selected_build;
            compare_row_cells([
                row.label.clone(),
                row.build.clone().unwrap_or_else(|| "\u{2014}".to_owned()),
                row.drift.clone(),
                row.last_clone.clone(),
            ])
            .text_sm()
            .text_color(if mismatch {
                cx.theme().warning
            } else {
                cx.theme().muted_foreground
            })
        }))
        .when(has_mismatch, |element| {
            element.child(
                div()
                    .px(px(14.0))
                    .pb(px(10.0))
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child("build / drift mismatch"),
            )
        })
}

fn compare_row_cells(cells: [String; 4]) -> gpui::Div {
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
            split_summary("38 errors / h"),
            ("38".to_owned(), "errors / h".to_owned())
        );
        assert_eq!(
            split_summary("source of truth"),
            ("source of truth".to_owned(), String::new())
        );
        // A build-only availability summary has no numeric head: it stays whole
        // on the value line, which clips rather than wraps.
        assert_eq!(
            split_summary("glide-zurich-patch3"),
            ("glide-zurich-patch3".to_owned(), String::new())
        );
    }
}
