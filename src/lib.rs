#![recursion_limit = "256"]

mod app;
pub mod daemon;
mod dashboard_state;
mod env_sheet;
mod notifications;
mod platform;
mod updater;

pub use daku_client::{identity, persistence};

use gpui::{
    App, AppContext as _, Application, Bounds, KeyBinding, Menu, MenuItem, SharedString,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, px, size,
};

use crate::app::Daku;
use crate::identity::{APP_ID, APP_NAME};

actions!(
    daku,
    [
        Quit,
        About,
        CloseWindow,
        CheckForUpdates,
        ReloadDaemon,
        CopySummary,
        ToggleNotifications,
        AddEnvironment,
        DetachSelectedEnvironment
    ]
);

/// Selects an Environment and optionally opens its drift drill-in. The one
/// "take me there" primitive: compare-row clicks dispatch it, and the
/// health-notification (079) and menu-bar (081) clicks will too.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, gpui::Action)]
#[action(namespace = daku, no_json)]
pub struct SelectEnvironment {
    pub env_id: SharedString,
    pub open_drift: bool,
}

/// Keyboard slot for ⌘1–9 in sidebar order. Slots are positional because
/// keybindings are static while the Environment list is not; the handler
/// resolves the slot against the current sidebar order.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, gpui::Action)]
#[action(namespace = daku, no_json)]
pub struct SelectEnvironmentSlot {
    pub slot: usize,
}

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
    // Desktop preferences (mutes). Missing file mints defaults; a corrupt
    // file is fatal here so a mute is never silently dropped.
    // Shared across windows so a mute set in one window appears in the other.
    let settings = std::sync::Arc::new(std::sync::Mutex::new(
        crate::persistence::load_or_create_app_settings()
            .unwrap_or_else(|error| panic!("failed to load daku app settings: {error:#}")),
    ));
    // Notification click router (105): the delegate lives process-wide, the
    // shell pumps the receiver for Environment ids.
    let notify_clicks = crate::notifications::install_click_router();

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
                KeyBinding::new("secondary-r", ReloadDaemon, None),
                KeyBinding::new("secondary-shift-c", CopySummary, None),
                KeyBinding::new("secondary-1", SelectEnvironmentSlot { slot: 0 }, None),
                KeyBinding::new("secondary-2", SelectEnvironmentSlot { slot: 1 }, None),
                KeyBinding::new("secondary-3", SelectEnvironmentSlot { slot: 2 }, None),
                KeyBinding::new("secondary-4", SelectEnvironmentSlot { slot: 3 }, None),
                KeyBinding::new("secondary-5", SelectEnvironmentSlot { slot: 4 }, None),
                KeyBinding::new("secondary-6", SelectEnvironmentSlot { slot: 5 }, None),
                KeyBinding::new("secondary-7", SelectEnvironmentSlot { slot: 6 }, None),
                KeyBinding::new("secondary-8", SelectEnvironmentSlot { slot: 7 }, None),
                KeyBinding::new("secondary-9", SelectEnvironmentSlot { slot: 8 }, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());

            open_daku_window(
                cx,
                daemon,
                settings.clone(),
                notify_clicks.clone(),
                None,
                WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
                    cx,
                )),
            )
            .expect("failed to open daku window");

            set_app_menus(cx, updater_available);
        });
}

/// Opens a daku window: the main sidebar+detail shell, or a detached
/// single-Environment view (`108`) that pins one Environment, drops the
/// sidebar, and destroys itself on close instead of hiding.
pub(crate) fn open_daku_window(
    cx: &mut App,
    supervisor: Option<daku_client::DaemonSupervisor>,
    settings: std::sync::Arc<std::sync::Mutex<crate::persistence::AppSettings>>,
    notify_clicks: Option<std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<String>>>>,
    detached: Option<String>,
    window_bounds: WindowBounds,
) -> anyhow::Result<gpui::WindowHandle<gpui_component::Root>> {
    let hide_on_close = detached.is_none();
    let window = cx.open_window(
        WindowOptions {
            titlebar: Some(gpui_component::TitleBar::title_bar_options()),
            is_movable: true,
            app_owns_titlebar_drag: cfg!(target_os = "macos"),
            window_background: WindowBackgroundAppearance::Blurred,
            app_id: Some(APP_ID.to_owned()),
            window_bounds: Some(window_bounds),
            display_id: None,
            window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
            ..Default::default()
        },
        move |window, cx| {
            if hide_on_close {
                crate::platform::configure_main_window_close_behavior(window, cx);
            }
            window
                .observe_window_appearance(|window, cx| {
                    gpui_component::Theme::sync_system_appearance(Some(window), cx);
                })
                .detach();
            let view = Daku::new(
                window,
                cx,
                supervisor,
                settings.clone(),
                notify_clicks,
                detached,
            );
            // Root paints an opaque `background`; clear it so the
            // window's blurred backdrop shows through `Daku`'s tint.
            cx.new(|cx| {
                gpui::Styled::bg(
                    gpui_component::Root::new(view, window, cx),
                    gpui::transparent_black(),
                )
            })
        },
    )?;
    window
        .update(cx, |_, window, cx| {
            crate::platform::enable_backdrop_blur(window);
            gpui_component::Theme::sync_system_appearance(Some(window), cx);
            cx.activate(true);
        })
        .ok();
    Ok(window)
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
                items.push(MenuItem::action("Copy Environment Summary", CopySummary));
                items.push(MenuItem::action(
                    "Toggle Health Notifications",
                    ToggleNotifications,
                ));
                items.push(MenuItem::action("Add Environment…", AddEnvironment));
                items.push(MenuItem::separator());
                items.push(MenuItem::action(format!("Quit {APP_NAME}"), Quit));
                items
            },
        },
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Close Window", CloseWindow),
                MenuItem::action("Detach Selected Environment", DetachSelectedEnvironment),
                MenuItem::separator(),
                MenuItem::action("Reload Daemon", ReloadDaemon),
            ],
        },
    ]);
}
