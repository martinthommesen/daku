//! In-app updates via Sparkle.
//!
//! `scripts/bundle.sh` embeds Sparkle.framework at Contents/Frameworks, and
//! this module loads it at runtime instead of linking it, so a bare `cargo
//! run` binary simply runs without an updater. Sparkle still owns update
//! discovery, download, signature verification, installation, and relaunch.
//! Sparkle's standard controller drives scheduled and manual checks, so its
//! own windows are the only update UI.
//!
//! Debug builds stay dormant so the dev watcher's app never offers to replace
//! itself with a production build; `DAKU_FORCE_UPDATER=1` enables the updater
//! in a debug bundle.
//!
//! Homebrew cask builds must not run Sparkle (`DAKU_CHANNEL=homebrew` or
//! `--features channel-homebrew`).

use gpui::Global;

/// Which update channel this process should honour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdaterChannel {
    Sparkle,
    Homebrew,
}

/// Parse `DAKU_CHANNEL`. Only the exact value `homebrew` disables Sparkle.
pub fn channel_from_env(value: Option<&str>) -> UpdaterChannel {
    match value {
        Some("homebrew") => UpdaterChannel::Homebrew,
        _ => UpdaterChannel::Sparkle,
    }
}

pub fn current_channel() -> UpdaterChannel {
    if cfg!(feature = "channel-homebrew") {
        return UpdaterChannel::Homebrew;
    }
    channel_from_env(std::env::var("DAKU_CHANNEL").ok().as_deref())
}

pub fn schedules_update_checks(channel: UpdaterChannel) -> bool {
    matches!(channel, UpdaterChannel::Sparkle)
}

/// App-wide handle to the updater, if this build can update itself.
pub struct UpdaterState(pub Option<Updater>);

impl Global for UpdaterState {}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{MainThreadMarker, msg_send};

    pub struct Updater {
        controller: Retained<AnyObject>,
    }

    impl Updater {
        /// Load Sparkle and start its standard updater. `None` when this
        /// build cannot update itself: Homebrew channel, debug builds unless
        /// `DAKU_FORCE_UPDATER=1`, or no embedded framework next to the
        /// binary.
        pub fn init() -> Option<Self> {
            if !super::schedules_update_checks(super::current_channel()) {
                return None;
            }
            let forced = std::env::var_os("DAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
            if cfg!(debug_assertions) && !forced {
                return None;
            }

            let _mtm = MainThreadMarker::new()?;
            let library = sparkle_library_path()?;
            let library_c =
                std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()))
                    .ok()?;
            let handle = unsafe { libc::dlopen(library_c.as_ptr(), libc::RTLD_NOW) };
            if handle.is_null() {
                let reason = unsafe { libc::dlerror() };
                let reason = if reason.is_null() {
                    "unknown dlopen failure".into()
                } else {
                    unsafe { std::ffi::CStr::from_ptr(reason) }
                        .to_string_lossy()
                        .into_owned()
                };
                eprintln!("Daku updater: failed to load Sparkle: {reason}");
                return None;
            }

            let controller_class = AnyClass::get(c"SPUStandardUpdaterController")?;
            let controller: Retained<AnyObject> = unsafe {
                let allocated: *mut AnyObject = msg_send![controller_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithStartingUpdater: true,
                    updaterDelegate: std::ptr::null_mut::<AnyObject>(),
                    userDriverDelegate: std::ptr::null_mut::<AnyObject>()
                ];
                Retained::from_raw(initialized)?
            };

            // Starting only arms the scheduled checker, which stays quiet
            // until its interval has elapsed since the last check. Force one
            // silent check per launch once the Operator has consented;
            // results are presented by Sparkle's standard driver.
            let sparkle: *mut AnyObject = unsafe { msg_send![&*controller, updater] };
            if !sparkle.is_null() {
                let automatic: bool = unsafe { msg_send![sparkle, automaticallyChecksForUpdates] };
                if automatic {
                    let _: () = unsafe { msg_send![sparkle, checkForUpdatesInBackground] };
                }
            }

            Some(Self { controller })
        }

        /// User-initiated check with Sparkle's standard windows.
        pub fn check_for_updates(&self) {
            let _: () = unsafe {
                msg_send![&*self.controller, checkForUpdates: std::ptr::null_mut::<AnyObject>()]
            };
        }
    }

    /// The embedded framework's dylib next to the running executable
    /// (Contents/MacOS/Daku → Contents/Frameworks/Sparkle.framework/Sparkle).
    fn sparkle_library_path() -> Option<std::path::PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let contents = executable.parent()?.parent()?;
        let library = contents.join("Frameworks/Sparkle.framework/Sparkle");
        library.exists().then_some(library)
    }
}

#[cfg(target_os = "macos")]
pub use macos::Updater;

/// Non-macOS builds have no updater yet. This stub is the seam where a
/// platform implementation slots in (WinSparkle consumes the same appcast
/// format on Windows); callers already treat `None` as "no updater".
#[cfg(not(target_os = "macos"))]
pub struct Updater;

#[cfg(not(target_os = "macos"))]
impl Updater {
    pub fn init() -> Option<Self> {
        None
    }

    pub fn check_for_updates(&self) {}
}

#[cfg(test)]
mod updater_channel_tests {
    use super::{UpdaterChannel, channel_from_env, schedules_update_checks};

    #[test]
    fn updater_channel_homebrew_does_not_schedule_checks() {
        assert!(!schedules_update_checks(UpdaterChannel::Homebrew));
    }

    #[test]
    fn updater_channel_sparkle_schedules_checks() {
        assert!(schedules_update_checks(UpdaterChannel::Sparkle));
    }

    #[test]
    fn updater_channel_from_env_homebrew() {
        assert_eq!(channel_from_env(Some("homebrew")), UpdaterChannel::Homebrew);
        assert!(!schedules_update_checks(channel_from_env(Some("homebrew"))));
    }
}
