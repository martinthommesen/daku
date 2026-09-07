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
        Sidebar, SidebarCollapsible, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu,
        SidebarMenuItem,
    },
    skeleton::Skeleton,
    tag::Tag,
    tooltip::Tooltip,
    v_flex,
};

use crate::CloseWindow;
use crate::ReloadDaemon;
use crate::dashboard_state::{
    CompareRow, DashboardState, DrillIn, SignalCard, TREND_WINDOW_LABEL, fixture_events, freshness,
    signal_label, ui_fixture_enabled,
};

const SIDEBAR_WIDTH: f32 = 220.0;

pub struct Daku {
    state: DashboardState,
    supervisor: Option<DaemonSupervisor>,
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
                supervisor,
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
        // Cards and the Drill-in carry click listeners, so they are built here
        // (where `Context<Self>` is available) and handed to the `&App` detail
        // render.
        let cards: Vec<gpui::AnyElement> = if self.state.selected().is_some() {
            self.state
                .cards()
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
        let detail = self.render_detail(cards, drill_in, cx);
        let title: SharedString = self
            .state
            .selected()
            .map(|environment| environment.label.clone().into())
            .unwrap_or_else(|| "daku".into());
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            // One flat, slightly translucent surface: sidebar, title bar and
            // detail all share the sidebar colour over the blurred backdrop.
            .bg(cx.theme().sidebar.opacity(0.92))
            .flex()
            .flex_col()
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| {
                crate::platform::hide_window(window);
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
                    .child(sidebar)
                    .child(detail),
            )
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
        // Roll-up: the worst observed Environment colours the header dot.
        let roll_up = self
            .state
            .worst_health()
            .map_or(cx.theme().muted_foreground, |health| {
                health_color(health, cx)
            });
        let footer = format!(
            "{} \u{b7} v{}",
            if self.state.connected() {
                "daemon connected"
            } else {
                "daemon disconnected"
            },
            env!("CARGO_PKG_VERSION")
        );

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

    fn render_detail(
        &self,
        cards: Vec<gpui::AnyElement>,
        drill_in: Option<gpui::AnyElement>,
        cx: &App,
    ) -> gpui::AnyElement {
        let selected = self.state.selected().cloned();
        let strip = self.state.compare_strip();
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
                                    ),
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
                    .children(drill_in)
                    // The rows are built only when the strip is on screen; a
                    // hidden strip must not cost a pass over every Environment.
                    .when(strip.visible, |element| {
                        element.child(compare_strip(
                            strip.has_mismatch,
                            &self.state.compare_rows(),
                            cx,
                        ))
                    })
            })
            .when(self.state.selected().is_none(), |element| {
                let message = if self.state.connected() && !self.state.has_environments() {
                    "No Environments configured — copy environments.example.json to ~/.daku/environments.json, then press ⌘R to reload. Daemon diagnostics: ~/.daku/daemon.log"
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
        let attention = !card.muted && matches!(card.status.as_str(), "degraded" | "down");
        let color = if card.muted {
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
                        .text_color(if card.status == "down" && !card.muted {
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
    /// or text the selected Signal's snapshot already carries.
    fn drill_in_region(&self, signal_id: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let content = self.state.drill_in(signal_id);
        let url = self.state.signal_url(signal_id);
        let status = self
            .state
            .cards()
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
                        compare_row_cells(row)
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
                DrillIn::Trend(points) => element.child(div().px(px(14.0)).child(
                    sparkline_with_scale(&points, color, px(80.0), unit_suffix(signal_id), cx),
                )),
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
fn compare_strip(has_mismatch: bool, rows: &[CompareRow], cx: &App) -> impl IntoElement {
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
