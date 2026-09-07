//! Health-change notifications (105).
//!
//! Decision logic is platform-neutral and unit-tested; delivery is macOS
//! UserNotifications, a no-op elsewhere.
//!
//! Failure modes from the issue, by design:
//! - Noise: only health-kind transitions fire (build events never do), and a
//!   batch carries at most one notification — the latest — which replaces the
//!   previous one per Environment (same request identifier) instead of
//!   stacking five.
//! - Replay storm: events observed before the desktop booted record as seen
//!   without firing, so launch replays, reconnect replays, and deliberate
//!   ⌘R reloads stay silent. Anything genuinely new still fires.
//! - Muted Environments stay silent, and the whole surface switches off via
//!   `AppSettings::notifications_enabled`.

use std::collections::HashSet;

use daku_protocol::{EnvironmentHealth, HealthEventDto, HealthEventKind};

/// Seen key: (environment id, event time, kind).
pub type SeenKey = (String, i64, String);

/// Splits a published batch into keys to record and the latest fireable
/// health transition. History (observed before boot) records without firing.
pub fn select_notification(
    env_id: &str,
    events: &[HealthEventDto],
    seen: &HashSet<SeenKey>,
    boot_now: i64,
) -> (Vec<SeenKey>, Option<HealthEventDto>) {
    let mut record = Vec::new();
    let mut fire = None;
    for event in events {
        let key = (
            env_id.to_owned(),
            event.observed_at,
            event.kind.as_str().to_owned(),
        );
        if seen.contains(&key) {
            continue;
        }
        record.push(key);
        if event.kind == HealthEventKind::Health && event.observed_at >= boot_now {
            fire = Some(event.clone());
        }
    }
    (record, fire)
}

pub fn notification_title(label: &str) -> String {
    format!("daku — {label}")
}

/// Recovery (no worst line) reads as "back to healthy"; otherwise the health
/// word plus the worst Signal line.
pub fn notification_body(health: EnvironmentHealth, headline: Option<&str>) -> String {
    let word = match health {
        EnvironmentHealth::Healthy => "healthy",
        EnvironmentHealth::Degraded => "degraded",
        EnvironmentHealth::Down => "down",
    };
    match headline {
        Some(line) => format!("{word}: {line}"),
        None => format!("back to {word}"),
    }
}

/// Posts a health notification, replacing the Environment's previous one.
/// Never prompts, never throws: delivery failures are silent by design (the
/// console itself still shows the state).
pub fn post_health_notification(env_id: &str, title: &str, body: &str) {
    delivery::post(env_id, title, body);
}

/// Installs the click router and returns the Environment-id receiver the
/// shell pumps. Clicking a notification selects that Environment (`102`'s
/// `SelectEnvironment` path). `None` where delivery is unavailable.
pub fn install_click_router()
-> Option<std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<String>>>> {
    delivery::install()
}

#[cfg(target_os = "macos")]
mod delivery {
    use std::sync::{Arc, Mutex, OnceLock, mpsc};

    use objc2::AnyThread;
    use objc2::ClassType;
    use objc2::MainThreadMarker;
    use objc2::define_class;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::runtime::{Bool, NSObjectProtocol};
    use objc2_foundation::NSString;
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    /// Request identifier prefix. One identifier per Environment means a new
    /// post replaces the previous banner instead of stacking.
    const ID_PREFIX: &str = "daku-health-";

    static CLICK_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();
    static AUTHORIZED: OnceLock<()> = OnceLock::new();

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; the class holds
        // no ivars (the click channel lives in `CLICK_TX`) and no Drop.
        #[unsafe(super(objc2::runtime::NSObject))]
        #[thread_kind = AnyThread]
        #[name = "DakuNotificationDelegate"]
        struct ClickDelegate;

        unsafe impl NSObjectProtocol for ClickDelegate {}

        unsafe impl UNUserNotificationCenterDelegate for ClickDelegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion_handler: &block2::DynBlock<dyn Fn()>,
            ) {
                if let Some(env_id) = response
                    .notification()
                    .request()
                    .identifier()
                    .to_string()
                    .strip_prefix(ID_PREFIX)
                {
                    if let Some(tx) = CLICK_TX.get() {
                        let _ = tx.send(env_id.to_owned());
                    }
                    // Bring daku forward; the shell pump selects the
                    // Environment through the shared SelectEnvironment path.
                    if let Some(mtm) = MainThreadMarker::new() {
                        objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
                    }
                }
                completion_handler.call(());
            }

            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &objc2_user_notifications::UNNotification,
                completion_handler: &block2::DynBlock<
                    dyn Fn(objc2_user_notifications::UNNotificationPresentationOptions),
                >,
            ) {
                use objc2_user_notifications::UNNotificationPresentationOptions;
                completion_handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            }
        }
    );

    static DELEGATE: OnceLock<Retained<ClickDelegate>> = OnceLock::new();

    pub fn install() -> Option<Arc<Mutex<mpsc::Receiver<String>>>> {
        let (tx, rx) = mpsc::channel();
        if CLICK_TX.set(tx).is_err() {
            return None;
        }
        let delegate: Retained<ClickDelegate> =
            unsafe { objc2::msg_send![ClickDelegate::class(), new] };
        let delegate_ref: &ClickDelegate = delegate.as_ref();
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(ProtocolObject::<
            dyn UNUserNotificationCenterDelegate,
        >::from_ref(
            delegate_ref
        )));
        DELEGATE.set(delegate).ok();
        Some(Arc::new(Mutex::new(rx)))
    }

    fn ensure_authorized() {
        AUTHORIZED.get_or_init(|| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            // The center runs the handler asynchronously: leak one
            // process-lifetime block rather than borrowing a local. It runs
            // once and ignores its result either way.
            let handler: block2::RcBlock<dyn Fn(Bool, *mut objc2_foundation::NSError)> =
                block2::RcBlock::new(|_granted, _error| {});
            let handler: &'static _ = Box::leak(Box::new(handler));
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert,
                handler,
            );
        });
    }

    pub fn post(env_id: &str, title: &str, body: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        ensure_authorized();
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&format!("{ID_PREFIX}{env_id}")),
            &content,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    }
}

#[cfg(not(target_os = "macos"))]
mod delivery {
    use std::sync::{Arc, Mutex, mpsc};

    pub fn install() -> Option<Arc<Mutex<mpsc::Receiver<String>>>> {
        None
    }

    pub fn post(_env_id: &str, _title: &str, _body: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(at: i64, from: EnvironmentHealth, to: EnvironmentHealth) -> HealthEventDto {
        HealthEventDto {
            observed_at: at,
            kind: HealthEventKind::Health,
            from_health: Some(from),
            to_health: to,
            build: None,
        }
    }

    fn build(at: i64) -> HealthEventDto {
        HealthEventDto {
            observed_at: at,
            kind: HealthEventKind::Build,
            from_health: None,
            to_health: EnvironmentHealth::Healthy,
            build: Some("glide-1".into()),
        }
    }

    #[test]
    fn history_records_without_firing() {
        let seen = HashSet::new();
        let events = vec![health(
            100,
            EnvironmentHealth::Healthy,
            EnvironmentHealth::Degraded,
        )];
        let (record, fire) = select_notification("prod", &events, &seen, 200);
        assert_eq!(record.len(), 1);
        assert!(fire.is_none(), "pre-boot history must stay silent");
    }

    #[test]
    fn novelty_fires_latest_only_and_builds_never() {
        let seen = HashSet::new();
        let events = vec![
            build(300),
            health(300, EnvironmentHealth::Healthy, EnvironmentHealth::Degraded),
            health(301, EnvironmentHealth::Degraded, EnvironmentHealth::Down),
        ];
        let (record, fire) = select_notification("prod", &events, &seen, 200);
        assert_eq!(record.len(), 3);
        let fire = fire.expect("novel transition fires");
        assert_eq!(fire.observed_at, 301);
        assert_eq!(fire.to_health, EnvironmentHealth::Down);
    }

    #[test]
    fn replayed_events_stay_silent() {
        let mut seen = HashSet::new();
        let events = vec![health(
            300,
            EnvironmentHealth::Healthy,
            EnvironmentHealth::Degraded,
        )];
        let (record, fire) = select_notification("prod", &events, &seen, 200);
        assert!(fire.is_some());
        seen.extend(record);
        let (record, fire) = select_notification("prod", &events, &seen, 200);
        assert!(record.is_empty());
        assert!(fire.is_none(), "reconnect replay must stay silent");
    }

    #[test]
    fn bodies_read_plainly() {
        assert_eq!(notification_title("Production"), "daku — Production");
        assert_eq!(
            notification_body(
                EnvironmentHealth::Degraded,
                Some("Scheduled jobs: 2 overdue")
            ),
            "degraded: Scheduled jobs: 2 overdue"
        );
        assert_eq!(
            notification_body(EnvironmentHealth::Healthy, None),
            "back to healthy"
        );
    }
}
