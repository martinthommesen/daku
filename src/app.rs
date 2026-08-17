//! Hollow GPUI shell — theme canvas only until Signal UI (plan 009).

use gpui::{
    App, Context, Entity, IntoElement, Render, Window, div, prelude::*, px,
};

use crate::theme::Theme;
use crate::{CloseWindow, ToggleFpsCounter};

pub struct Daku {
    show_fps: bool,
}

impl Daku {
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        window.on_window_should_close(cx, |window, _cx| {
            crate::platform::hide_window(window);
            false
        });
        cx.new(|_| Self { show_fps: false })
    }
}

impl Render for Daku {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        div()
            .size_full()
            .bg(theme.canvas)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .text_color(theme.text)
            .on_action(cx.listener(|this, _: &ToggleFpsCounter, window, cx| {
                this.show_fps = !this.show_fps;
                window.refresh();
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| {
                crate::platform::hide_window(window);
            }))
            .child(
                div()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(crate::identity::APP_NAME),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme.text_secondary)
                    .child("Hollow shell — Environments and Signals land in later plans."),
            )
            .when(self.show_fps, |element| {
                element.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child("FPS counter toggle is wired; meter UI comes later."),
                )
            })
    }
}
