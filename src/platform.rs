use gpui::Window;

#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(main_thread).orderFrontStandardAboutPanel(None);
}

#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

#[cfg(target_os = "macos")]
pub fn init_reduce_motion(cx: &mut gpui::App) {
    use objc2_app_kit::NSWorkspace;

    cx.set_reduce_motion(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion());
}

#[cfg(target_os = "linux")]
pub fn init_reduce_motion(cx: &mut gpui::App) {
    if let Ok(value) = std::env::var("DAKU_REDUCE_MOTION")
        && let Some(enabled) = parse_boolean_setting(&value)
    {
        cx.set_reduce_motion(enabled);
        return;
    }

    // GNOME exposes its animation preference through GSettings. Resolve it
    // once off the UI thread; frames only read GPUI's in-memory flag.
    cx.spawn(async move |cx| {
        let enabled = cx
            .background_executor()
            .spawn(async move { linux_reduce_motion_enabled() })
            .await;
        cx.update(|cx| cx.set_reduce_motion(enabled));
    })
    .detach();
}

#[cfg(target_os = "linux")]
fn linux_reduce_motion_enabled() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| parse_boolean_setting(&value))
        .is_some_and(|animations_enabled| !animations_enabled)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn init_reduce_motion(_: &mut gpui::App) {}

#[cfg(target_os = "linux")]
fn parse_boolean_setting(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Keep daku's single main window alive when the user closes it. This preserves
/// the current session and lets a Dock activation reveal the same GPUI window.
#[cfg(target_os = "macos")]
pub fn configure_main_window_close_behavior(window: &Window, cx: &gpui::App) {
    window.on_window_should_close(cx, |window, _| {
        hide_window(window);
        false
    });
}

#[cfg(not(target_os = "macos"))]
pub fn configure_main_window_close_behavior(_: &Window, _: &gpui::App) {}

#[cfg(target_os = "macos")]
pub fn hide_window(window: &mut Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };

    // GPUI owns this view and its NSWindow. AppKit access stays on the main
    // thread, and orderOut hides without triggering GPUI's close callback.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        if let Some(native_window) = view.window() {
            native_window.orderOut(None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_window(window: &mut Window) {
    window.remove_window();
}

/// Always-on attention surfaces (`107`): a coloured dot in the menu bar and
/// a troubled-Environment count on the Dock icon. Both derive from the
/// existing worst-health roll-up with muted Environments excluded, cost no
/// new data, and clear when disconnected.
///
/// Deliberately passive: no menu-bar dropdown and no Dock menu here. Both
/// need an Objective-C target object that would collide with GPUI's own
/// application delegate, so they are a separate, abandonable step.
#[cfg(target_os = "macos")]
pub fn reflect_ambient_health(
    worst: Option<daku_protocol::EnvironmentHealth>,
    troubled: usize,
    connected: bool,
) {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSColor, NSSquareStatusItemLength, NSStatusBar, NSStatusItem,
    };
    use objc2_foundation::{
        NSAttributedString, NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSString,
    };

    // NSStatusItem is main-thread-only: thread-local retention is honest, and
    // GPUI drives renders on the main thread on macOS.
    thread_local! {
        static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> =
            const { RefCell::new(None) };
    }

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    STATUS_ITEM.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot =
                Some(NSStatusBar::systemStatusBar().statusItemWithLength(NSSquareStatusItemLength));
        }
        let Some(item) = slot.as_deref() else {
            return;
        };
        let color = match (connected, worst) {
            (false, _) | (_, None) => NSColor::systemGrayColor(),
            (_, Some(daku_protocol::EnvironmentHealth::Healthy)) => NSColor::systemGreenColor(),
            (_, Some(daku_protocol::EnvironmentHealth::Degraded)) => NSColor::systemYellowColor(),
            (_, Some(daku_protocol::EnvironmentHealth::Down)) => NSColor::systemRedColor(),
            (_, Some(daku_protocol::EnvironmentHealth::Waiting)) => NSColor::systemGrayColor(),
        };
        // A bare title cannot carry colour; one foreground-color attribute does.
        let attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
            let dict: Retained<NSMutableDictionary<NSAttributedStringKey, NSColor>> =
                NSMutableDictionary::new();
            dict.setObject_forKey(
                &color,
                ProtocolObject::from_ref(objc2_app_kit::NSForegroundColorAttributeName),
            );
            Retained::cast_unchecked(dict)
        };
        let dot = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str("\u{25cf}"),
                Some(&attrs),
            )
        };
        if let Some(button) = item.button(mtm) {
            button.setAttributedTitle(&dot);
        }
        let badge = (connected && troubled > 0).then(|| NSString::from_str(&troubled.to_string()));
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(badge.as_deref());
    });
}

#[cfg(not(target_os = "macos"))]
pub fn reflect_ambient_health(
    _worst: Option<daku_protocol::EnvironmentHealth>,
    _troubled: usize,
    _connected: bool,
) {
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::parse_boolean_setting;

    #[test]
    fn boolean_desktop_settings_are_parsed_case_insensitively() {
        assert_eq!(parse_boolean_setting(" true\n"), Some(true));
        assert_eq!(parse_boolean_setting("OFF"), Some(false));
        assert_eq!(parse_boolean_setting("default"), None);
    }
}

/// GPUI's `WindowBackgroundAppearance::Blurred` inserts an `NSVisualEffectView`
/// with the colourless `Selection` material, which on macOS 26+ no longer
/// creates a backdrop layer — the window ends up plain transparent. Swap it to
/// `UnderWindowBackground`, the material meant for exactly this, so the app
/// gets a real behind-window blur under its translucent tint.
#[cfg(target_os = "macos")]
pub fn enable_backdrop_blur(window: &mut Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSView, NSVisualEffectMaterial, NSVisualEffectView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };

    // GPUI owns this view and its NSWindow; AppKit access stays on the main
    // thread. The blur view is a sibling of GPUI's view under the content view.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(content_view) = view.window().and_then(|w| w.contentView()) else {
            return;
        };
        for sub in content_view.subviews().iter() {
            if let Ok(effect) = sub.downcast::<NSVisualEffectView>() {
                effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn enable_backdrop_blur(_: &mut Window) {}
