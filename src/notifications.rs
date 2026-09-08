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

use std::collections::{HashMap, HashSet};

use daku_protocol::{EnvironmentHealth, HealthEventDto, HealthEventKind};

use crate::persistence::QuietHours;

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

/// Grouped body over up to three explainer lines: the health word plus every
/// voting Signal, so one banner carries the whole story instead of just the
/// worst line. Empty lines read as recovery.
pub fn notification_body_grouped(health: EnvironmentHealth, lines: &[String]) -> String {
    let lines: Vec<&str> = lines.iter().take(3).map(String::as_str).collect();
    match lines.as_slice() {
        [] => notification_body(health, None),
        [single] => notification_body(health, Some(single)),
        _ => {
            let word = match health {
                EnvironmentHealth::Healthy => "healthy",
                EnvironmentHealth::Degraded => "degraded",
                EnvironmentHealth::Down => "down",
            };
            format!("{word}: {}", lines.join("; "))
        }
    }
}

/// Local hour (0–23) of a unix timestamp, system timezone. Falls back to 0
/// when the value is out of range for the platform clock.
pub fn local_hour(unix_secs: i64) -> u8 {
    local_tm(unix_secs).tm_hour.clamp(0, 23) as u8
}

/// Local weekday of a unix timestamp, system timezone: 0 is Sunday, 1 is
/// Monday, per `tm_wday`.
pub fn local_weekday(unix_secs: i64) -> u8 {
    local_tm(unix_secs).tm_wday.clamp(0, 6) as u8
}

fn local_tm(unix_secs: i64) -> libc::tm {
    let timestamp = unix_secs as libc::time_t;
    let mut broken: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&timestamp, &mut broken).is_null() };
    if !ok {
        unsafe { std::mem::zeroed() }
    } else {
        broken
    }
}

/// True on Mondays at/after 09:00 local: the weekly digest slot. The caller
/// also checks the last-sent mark, so this staying true all Monday only
/// sends once.
pub fn is_digest_slot(now: i64) -> bool {
    local_weekday(now) == 1 && local_hour(now) >= 9
}

/// One-line digest notification body: the first two content lines of the
/// Markdown (past the `#` title and blanks), joined and capped. Never empty:
/// a digest with no content lines still names itself a digest.
pub fn digest_notification_body(markdown: &str) -> String {
    const CAP: usize = 220;
    let mut lines = markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .take(2);
    let body = format!(
        "{} {}",
        lines.next().unwrap_or("weekly review"),
        lines.next().unwrap_or_default()
    );
    let body = body.trim_end().to_owned();
    if body.len() <= CAP {
        body
    } else {
        format!("{}…", body.chars().take(CAP - 1).collect::<String>())
    }
}

/// One gating decision for a fireable transition: master switch, mute,
/// headline-signal switch, and quiet hours must all allow it.
pub struct NotifyGate<'a> {
    pub master: bool,
    pub muted: bool,
    pub headline_signal: Option<&'a str>,
    pub notify_signals: &'a HashMap<String, bool>,
    pub quiet_hours: Option<QuietHours>,
}

pub fn gate_allows(gate: &NotifyGate<'_>, now: i64) -> bool {
    if !gate.master || gate.muted {
        return false;
    }
    if let Some(signal_id) = gate.headline_signal
        && !gate.notify_signals.get(signal_id).copied().unwrap_or(true)
    {
        return false;
    }
    if gate
        .quiet_hours
        .is_some_and(|window| window.contains(local_hour(now)))
    {
        return false;
    }
    true
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

    #[test]
    fn grouped_bodies_carry_up_to_three_lines() {
        assert_eq!(
            notification_body_grouped(EnvironmentHealth::Healthy, &[]),
            "back to healthy"
        );
        assert_eq!(
            notification_body_grouped(
                EnvironmentHealth::Degraded,
                &["Scheduled jobs: 2 overdue".to_owned()]
            ),
            "degraded: Scheduled jobs: 2 overdue"
        );
        assert_eq!(
            notification_body_grouped(
                EnvironmentHealth::Degraded,
                &[
                    "Scheduled jobs: 2 overdue".to_owned(),
                    "Syslog errors: 4 errors".to_owned(),
                    "Outbound: 3 failures".to_owned(),
                    "Flow errors: 1 error".to_owned(),
                ]
            ),
            "degraded: Scheduled jobs: 2 overdue; Syslog errors: 4 errors; Outbound: 3 failures"
        );
    }

    static TZ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs `body` with `TZ` set, restored afterwards. Serialised: the
    /// timezone is process-global. Both platform clocks re-read `TZ` on
    /// every `localtime` call, so no `tzset` binding is needed.
    fn with_tz(name: &str, body: impl FnOnce()) {
        let _guard = TZ_LOCK.lock().expect("tz lock");
        let previous = std::env::var("TZ").ok();
        // SAFETY: serialised by TZ_LOCK; restored before returning.
        unsafe {
            std::env::set_var("TZ", name);
        }
        body();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("TZ", value),
                None => std::env::remove_var("TZ"),
            }
        }
    }

    #[test]
    fn local_hour_follows_the_system_timezone() {
        // 1970-01-01 00:00 UTC.
        with_tz("UTC", || assert_eq!(local_hour(0), 0));
        with_tz("America/New_York", || assert_eq!(local_hour(0), 19));
    }

    #[test]
    fn digest_slot_is_monday_from_nine_local() {
        // Monday 2026-01-05 09:00 UTC; minus an hour is 08:00; plus a day is
        // Tuesday 09:00.
        const MONDAY_9AM: i64 = 1_767_603_600;
        with_tz("UTC", || {
            assert_eq!(local_weekday(MONDAY_9AM), 1);
            assert!(is_digest_slot(MONDAY_9AM));
            assert!(!is_digest_slot(MONDAY_9AM - 3600));
            assert!(!is_digest_slot(MONDAY_9AM + 24 * 3600));
        });
    }

    #[test]
    fn digest_bodies_summarise_markdown() {
        let markdown = "# Daku digest: Production (prod)\n\nLast 7 days · health degraded · reachable\n\n## Health transitions\n- 6d ago: healthy → degraded\n";
        assert_eq!(
            digest_notification_body(markdown),
            "Last 7 days · health degraded · reachable - 6d ago: healthy → degraded"
        );
        assert_eq!(
            digest_notification_body("# Only a title\n"),
            "weekly review"
        );
        let long = format!("# T\n\n{}\n", "x".repeat(500));
        assert!(digest_notification_body(&long).len() <= 223);
    }

    #[test]
    fn quiet_hours_wrap_midnight_and_reject_garbage() {
        use crate::persistence::QuietHours;
        let nights = QuietHours {
            start_hour: 22,
            end_hour: 7,
        };
        assert!(nights.contains(22));
        assert!(nights.contains(3));
        assert!(!nights.contains(7));
        assert!(!nights.contains(12));
        let days = QuietHours {
            start_hour: 9,
            end_hour: 17,
        };
        assert!(days.contains(9));
        assert!(!days.contains(17));
        assert!(!days.contains(8));
        let garbage = QuietHours {
            start_hour: 25,
            end_hour: 7,
        };
        assert!(!garbage.contains(3), "out-of-range reads as unset");
    }

    #[test]
    fn gate_blocks_master_mute_signal_and_quiet() {
        use crate::persistence::QuietHours;
        // 2023-11-14 22:13 UTC; minus 20 h is 02:13 UTC.
        const NIGHT: i64 = 1_700_000_000;
        const DAWN: i64 = 1_700_000_000 - 20 * 3600;
        let allenabled: HashMap<String, bool> = HashMap::new();
        let mut signals = HashMap::new();
        signals.insert("jobs".to_owned(), false);
        fn gate<'a>(
            muted: bool,
            headline: Option<&'a str>,
            map: &'a HashMap<String, bool>,
            quiet: Option<QuietHours>,
        ) -> NotifyGate<'a> {
            NotifyGate {
                master: true,
                muted,
                headline_signal: headline,
                notify_signals: map,
                quiet_hours: quiet,
            }
        }
        assert!(gate_allows(&gate(false, None, &allenabled, None), NIGHT));
        assert!(!gate_allows(
            &NotifyGate {
                master: false,
                ..gate(false, None, &allenabled, None)
            },
            NIGHT
        ));
        assert!(!gate_allows(&gate(true, None, &allenabled, None), NIGHT));
        assert!(!gate_allows(
            &gate(false, Some("jobs"), &signals, None),
            NIGHT
        ));
        assert!(gate_allows(
            &gate(false, Some("syslog"), &signals, None),
            NIGHT
        ));
        let nights = Some(QuietHours {
            start_hour: 21,
            end_hour: 23,
        });
        with_tz("UTC", || {
            assert!(!gate_allows(&gate(false, None, &allenabled, nights), NIGHT));
            assert!(gate_allows(&gate(false, None, &allenabled, nights), DAWN));
        });
    }
}
