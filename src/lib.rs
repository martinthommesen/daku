#![recursion_limit = "256"]

mod app;
pub mod daemon;
mod dashboard_state;
mod platform;
mod updater;

pub use daku_client::{identity, persistence};

use gpui::{
    App, AppContext as _, Application, Bounds, KeyBinding, Menu, MenuItem,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, px, size,
};

use crate::app::Daku;
use crate::identity::{APP_ID, APP_NAME};

actions!(daku, [Quit, About, CloseWindow, CheckForUpdates]);

const DEFAULT_WINDOW_WIDTH: f32 = 1380.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 880.0;
const MIN_WINDOW_WIDTH: f32 = 980.0;
const MIN_WINDOW_HEIGHT: f32 = 680.0;
trait DakuApplicationExt {
    fn with_main_window_reopen(self) -> Self;
}

impl DakuApplicationExt for Application {
    fn with_main_window_reopen(self) -> Self {
        self.on_reopen(|cx| {
            if let Some(window) = cx.windows().into_iter().next() {
                window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
            cx.activate(true);
        });
        self
    }
}

pub fn run() {
    let fixture = crate::dashboard_state::ui_fixture_enabled();
    let daemon = if fixture {
        None
    } else {
        Some(
            crate::daemon::start_process()
                .unwrap_or_else(|error| panic!("failed to start daku daemon: {error:#}")),
        )
    };

    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .with_main_window_reopen()
        .run(move |cx: &mut App| {
            cx.set_app_identity(APP_ID, APP_NAME);
            gpui_component::init(cx);
            crate::platform::init_reduce_motion(cx);

            let updater = crate::updater::Updater::init();
            let updater_available = updater.is_some();
            cx.set_global(crate::updater::UpdaterState(updater));
            cx.on_action(|_: &CheckForUpdates, cx| {
                if let Some(updater) = &cx.global::<crate::updater::UpdaterState>().0 {
                    updater.check_for_updates();
                }
            });
            cx.on_action(|_: &About, _| crate::platform::show_about_panel());
            cx.bind_keys([
                KeyBinding::new("secondary-q", Quit, None),
                KeyBinding::new("secondary-w", CloseWindow, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let window_bounds = WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
                cx,
            ));
            let window = cx
                .open_window(
                    WindowOptions {
                        titlebar: Some(gpui_component::TitleBar::title_bar_options()),
                        is_movable: true,
                        app_owns_titlebar_drag: cfg!(target_os = "macos"),
                        window_background: WindowBackgroundAppearance::Opaque,
                        app_id: Some(APP_ID.to_owned()),
                        window_bounds: Some(window_bounds),
                        display_id: None,
                        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                        ..Default::default()
                    },
                    move |window, cx| {
                        crate::platform::configure_main_window_close_behavior(window, cx);
                        window
                            .observe_window_appearance(|window, cx| {
                                gpui_component::Theme::sync_system_appearance(Some(window), cx);
                            })
                            .detach();
                        let view = Daku::new(window, cx, daemon);
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    },
                )
                .expect("failed to open daku window");

            window
                .update(cx, |_, window, cx| {
                    gpui_component::Theme::sync_system_appearance(Some(window), cx);
                    cx.activate(true);
                })
                .ok();

            set_app_menus(cx, updater_available);
        });
}

pub(crate) fn set_app_menus(cx: &mut App, updater_available: bool) {
    cx.set_menus(vec![
        Menu {
            name: APP_NAME.into(),
            disabled: false,
            items: {
                let mut items = vec![MenuItem::action(format!("About {APP_NAME}"), About)];
                if updater_available {
                    items.push(MenuItem::action("Check for Updates…", CheckForUpdates));
                }
                items.push(MenuItem::separator());
                items.push(MenuItem::action(format!("Quit {APP_NAME}"), Quit));
                items
            },
        },
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![MenuItem::action("Close Window", CloseWindow)],
        },
    ]);
}
