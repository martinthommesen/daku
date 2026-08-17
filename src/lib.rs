#![recursion_limit = "256"]

rust_i18n::i18n!("locales", fallback = "en");

macro_rules! tr {
    ($key:expr) => {
        rust_i18n::t!($key).into_owned()
    };
    ($key:expr, $($args:tt)*) => {
        rust_i18n::t!($key, $($args)*).into_owned()
    };
}

mod app;
mod assets;
pub mod daemon;
mod dashboard_state;
mod platform;
mod theme;
mod updater;

pub use daku_client::{identity, persistence};

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::app::Daku;
use crate::identity::{APP_ID, APP_NAME};

actions!(
    daku,
    [Quit, About, CloseWindow, CheckForUpdates, ToggleFpsCounter]
);

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
        .with_assets(crate::assets::Assets)
        .with_main_window_reopen()
        .run(move |cx: &mut App| {
            cx.set_app_identity(APP_ID, APP_NAME);
            crate::theme::init(cx);
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
                KeyBinding::new("secondary-alt-shift-f", ToggleFpsCounter, None),
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
                        titlebar: Some(TitlebarOptions {
                            title: Some(APP_NAME.into()),
                            appears_transparent: cfg!(target_os = "macos"),
                            traffic_light_position: cfg!(target_os = "macos")
                                .then(|| point(px(16.0), px(17.0))),
                        }),
                        is_movable: true,
                        app_owns_titlebar_drag: cfg!(target_os = "macos"),
                        window_background: if cfg!(target_os = "macos") {
                            WindowBackgroundAppearance::Blurred
                        } else {
                            WindowBackgroundAppearance::Opaque
                        },
                        app_id: Some(APP_ID.to_owned()),
                        window_bounds: Some(window_bounds),
                        display_id: None,
                        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                        ..Default::default()
                    },
                    move |window, cx| {
                        crate::platform::configure_main_window_close_behavior(window, cx);
                        Daku::new(window, cx, daemon)
                    },
                )
                .expect("failed to open daku window");

            window
                .update(cx, |_, window, cx| {
                    crate::platform::configure_sidebar_material(
                        window,
                        crate::theme::Theme::current(cx).is_dark,
                    );
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
                let mut items = vec![MenuItem::action(tr!("menu.about", app = APP_NAME), About)];
                if updater_available {
                    items.push(MenuItem::action(
                        tr!("menu.check_for_updates"),
                        CheckForUpdates,
                    ));
                }
                items.push(MenuItem::separator());
                items.push(MenuItem::action(tr!("menu.quit", app = APP_NAME), Quit));
                items
            },
        },
        Menu {
            name: tr!("menu.window").into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("menu.toggle_fps_counter"), ToggleFpsCounter),
                MenuItem::action(tr!("menu.close_window"), CloseWindow),
            ],
        },
    ]);
}
