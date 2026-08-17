# Plan 021: Sparkle uses its standard updater controller — scheduled updates are shown, custom driver deleted

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- src/updater.rs src/lib.rs Cargo.toml`
> If `src/updater.rs` or `src/lib.rs` changed since this plan was written,
> compare the "Current state" excerpts against the live code before
> proceeding; on a mismatch, treat it as a STOP condition. (`Cargo.toml`
> changes from plan 016 are expected.)

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (ObjC dynamic loading; only verifiable on macOS with a bundled Sparkle)
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/015-release-pipeline-sparkle-fixes.md (a bundle whose `CFBundleVersion` is real, so a manual end-to-end check can actually see an update)
- **Category**: bug / tech-debt
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/44

## Why this matters

`src/updater.rs` (902 lines) carries a ~500-line custom `SPUUserDriver` (`DakuSparkleUserDriver`) inherited from waku. Its purpose was to keep **scheduled** update results inside waku's sidebar (a status pill + `UpdaterEvent` stream) and only hand **manual** checks to Sparkle's standard windows. Daku's GPUI shell renders none of that: `Updater::status()`, `events()`, `install_available_update()`, `set_automatically_checks_for_updates()` and `UpdateStatus::Checking` have no callers (rustc flags them). Net effect today: `init()` forces one silent background check per launch, Sparkle finds an update, the custom driver parks it in `pending_update` and sets a status nobody reads — the Operator learns about updates only by choosing "Check for Updates" by hand. That contradicts ADR-0006 ("Sparkle primary") and the module's own doc comment.

The lazy, correct end state: use Sparkle's own `SPUStandardUpdaterController` (standard driver for scheduled **and** manual checks, standard permission prompt), keep the dynamic-load bootstrap and the channel gating, delete the rest. `Updater` shrinks to `init()` + `check_for_updates()` — exactly what `src/lib.rs` uses.

## Current state

### Callers — `src/lib.rs:135-142, 191-207`

```rust
            let updater = crate::updater::Updater::init();
            let updater_available = updater.is_some();
            cx.set_global(crate::updater::UpdaterState(updater));
            cx.on_action(|_: &CheckForUpdates, cx| {
                if let Some(updater) = &cx.global::<crate::updater::UpdaterState>().0 {
                    updater.check_for_updates();
                }
            });
            …
            set_app_menus(cx, updater_available);   // adds "Check for Updates" when true
```

Nothing else in `src/` or `crates/` references the module (grep `updater` → only `src/lib.rs` + the module).

### `src/updater.rs` — keep

- Module doc (`:1-17`) — rewrite (drop the sidebar/preview sentences).
- `UpdaterChannel`, `channel_from_env`, `current_channel`, `schedules_update_checks` (`:20-45`); `UpdaterState` global (`:47-50`).
- In `mod macos`: `sparkle_library_path()` (`:798-805`), the `dlopen` bootstrap in `Updater::init()` (`:611-640`: channel gate, `DAKU_FORCE_UPDATER=1` override for debug builds, `MainThreadMarker`, `dlopen`, `NSBundle mainBundle`), and the "force one background check per launch" idea (`:698-707`).
- `updater_channel_tests` (`:884-902`).
- Non-macOS stub `Updater` (`:846-881`) — trimmed to `init() -> None` and `check_for_updates()`.

### `src/updater.rs` — delete

- `UpdateStatus` (`:52-60`), `UpdaterEvent` (`:62-68`), `PendingUpdate` (`:102-106`), `UserDriverIvars` (`:108-119`), the whole `define_class!(… struct UserDriver …)` block with `SPUUserDriver`/`SPUUpdaterDelegate` impls (`:121-447`), `impl UserDriver { new, uses_standard_presentation, begin_standard_presentation, schedule_requested_standard_check, show_update_found_with_standard_driver, present_pending_update, request_standard_check, send, set_status, clear_update, install_available_update }` (`:449-597`), the two `extern_protocol!` declarations (`:87-95`), constants `USER_UPDATE_CHOICE_INSTALL`, `UPDATE_CHECK_USER_INITIATED`, `UPDATE_CHECK_IN_BACKGROUND`, `MANUAL_CHECK_MAX_RETRIES` (`:82-85`), `error_description` (`:789-795`), `Updater::{install_available_update, status, events, set_automatically_checks_for_updates, preview, set_preview_status}` and the `DAKU_PREVIEW_UPDATE` mode (`:611-620, 728-787`), tests `routing_user_driver_satisfies_sparkle_protocols` (vacuous — returns early unless a Debug.app with Sparkle exists) and `preview_update_switches_from_available_to_spinner` (`:807-846`).

Today's `init()` core (`:641-696`) allocates `SPUStandardUserDriver` (`initWithHostBundle:delegate:`), wraps it in `UserDriver`, allocates `SPUUpdater` (`initWithHostBundle:applicationBundle:userDriver:delegate:`), calls `startUpdater:` and then `checkForUpdatesInBackground` when `automaticallyChecksForUpdates` is true. `check_for_updates()` (`:713-726`) routes through `UserDriver::request_standard_check`.

Sparkle 2.x API you will use instead (public, in the embedded framework — `scripts/bundle.sh` pins Sparkle 2.9.4): `SPUStandardUpdaterController` — `- (instancetype)initWithStartingUpdater:(BOOL)startUpdater updaterDelegate:(id<SPUUpdaterDelegate>)updaterDelegate userDriverDelegate:(id<SPUStandardUserDriverDelegate>)userDriverDelegate;`, `- (IBAction)checkForUpdates:(id)sender;`, property `updater` (`SPUUpdater`) with `automaticallyChecksForUpdates` and `- (void)checkForUpdatesInBackground;`. With `startingUpdater:YES` the controller calls `startUpdater` itself and logs failures.

Dependencies: `block2` is used **only** by `updater.rs` (`RcBlock`/`DynBlock` in the deleted code) — root `Cargo.toml:45` `block2 = "0.6"`. `objc2` (`msg_send`, `AnyClass`, `Retained`) and `libc` (`dlopen`) stay in use.

Conventions: `unsafe { msg_send![…] }` with typed returns; `eprintln!("Daku updater: …")` for bootstrap failures; imperative commit summaries; tests at file bottom.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Root check | `cargo check -p daku` | exit 0 |
| Root tests | `cargo test -p daku updater_channel` | 3 passed |
| Dead-code count | `cargo clippy -p daku 2>&1 \| grep -c 'never used'` | lower than before (≈8 fewer) |
| Line count | `wc -l src/updater.rs` | well under 300 |
| Gate | `bun run check` | exit 0 |
| Manual (macOS) | `DAKU_SKIP_CARGO_BUILD=0 ./scripts/bundle.sh --unsigned` then `DAKU_FORCE_UPDATER=1 open dist/Daku.app` | see Step 4 |

## Scope

**In scope**:
- `src/updater.rs`
- `Cargo.toml` (remove `block2` if unused after the change — verify with `cargo check`)
- `plans/README.md` (status row)

**Out of scope**:
- `src/lib.rs` — the two call sites (`init`, `check_for_updates`) keep their signatures; no change.
- `scripts/bundle.sh`, `resources/Info.plist`, `SUPublicEDKey`, appcast generation (plan 015 / human steps).
- A GPUI "update available" affordance — Sparkle's standard windows are the affordance now.
- `src/platform.rs`, `objc2*` feature lists (dead-code plan).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Use Sparkle's standard updater controller; delete the custom user driver.`

## Steps

### Step 1: Replace the macOS `Updater` with a controller wrapper

Inside `mod macos`, after deleting everything listed above, the module contains: imports (`objc2::rc::Retained`, `objc2::runtime::{AnyClass, AnyObject}`, `objc2::{MainThreadMarker, msg_send}`), `sparkle_library_path`, and:

```rust
    pub struct Updater {
        controller: Retained<AnyObject>,
    }

    impl Updater {
        /// Load Sparkle and start its standard updater. `None` when this
        /// build cannot update itself: Homebrew channel, debug builds unless
        /// `DAKU_FORCE_UPDATER=1`, or no embedded framework next to the binary.
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
            … (existing dlopen block verbatim: CString, libc::dlopen RTLD_NOW, dlerror → eprintln + None) …
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
            // Starting only arms the scheduled checker; force one silent check
            // per launch once the Operator has consented. Results (if any) are
            // presented by Sparkle's standard driver.
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
```

Trim the non-macOS stub to `init() -> None` and `check_for_updates(&self) {}`. Rewrite the module doc: keep the "embedded at runtime, dlopen, bare cargo run has no updater" and Homebrew paragraphs; replace the sidebar/preview text with "Sparkle's standard controller drives scheduled and manual checks; `DAKU_FORCE_UPDATER=1` enables the updater in a debug bundle." Delete the `DAKU_PREVIEW_UPDATE` sentence.

**Verify**: `cargo check -p daku` → exit 0 with no `unused import` warnings in `updater.rs`; `wc -l src/updater.rs` → < 300.

### Step 2: Tests

Delete `mod tests` inside `mod macos` (both tests). Keep `updater_channel_tests` unchanged.

**Verify**: `cargo test -p daku updater_channel` → 3 passed; `cargo test -p daku` → all pass.

### Step 3: Drop `block2` if unused

`grep -rn 'block2' src` → if only the deleted code used it, remove `block2 = "0.6"` from root `Cargo.toml` `[target.'cfg(target_os = "macos")'.dependencies]`.

**Verify**: `cargo check --workspace` → exit 0; `git diff Cargo.lock` shows only removals (block2 and its exclusive deps).

### Step 4: Manual verification on macOS (record the outcome)

1. `./scripts/bundle.sh --unsigned` (needs network for Sparkle 2.9.4 on first run) → `dist/Daku.app` with `Contents/Frameworks/Sparkle.framework`.
2. `DAKU_FORCE_UPDATER=1 open dist/Daku.app` — expected: the app opens; on first run Sparkle's standard permission prompt ("Check for updates automatically?") appears; the menu has "Check for Updates"; choosing it opens Sparkle's standard "Checking for updates…" window, which then reports "You're up to date" or an appcast/feed error (the unsigned build has no `SUPublicEDKey`, so a signature error is acceptable proof that Sparkle ran) — no crash, no silent no-op.
3. `open dist/Daku.app` without the env var (release build) behaves the same. `DAKU_CHANNEL=homebrew open dist/Daku.app` → no "Check for Updates" menu item.
4. Optional end-to-end (needs plan 015 + a signed appcast): serve an appcast with a higher `sparkle:version` and confirm the standard "A new version is available" window appears without any menu action.

Write results in the plan's status note.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- Automated: `updater_channel_tests` (unchanged, 3 tests). No ObjC-level test — the previous one was vacuous; Sparkle presence is a bundle property, checked manually.
- Manual: Step 4.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'UserDriver\|UpdaterEvent\|UpdateStatus\|PendingUpdate\|DAKU_PREVIEW_UPDATE\|extern_protocol' src/updater.rs` → no matches
- [ ] `grep -n 'SPUStandardUpdaterController' src/updater.rs` → ≥1 match; `grep -n 'checkForUpdatesInBackground' src/updater.rs` → 1 match
- [ ] `grep -n 'pub fn init\|pub fn check_for_updates' src/updater.rs` → 2 each (macOS + stub)
- [ ] `wc -l src/updater.rs` < 300
- [ ] `grep -n block2 src/updater.rs Cargo.toml` → no matches (or a note why it stayed)
- [ ] `cargo test -p daku` passes; `bun run check` exits 0
- [ ] Manual Step 4 items 1–3 recorded
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 021 updated

## STOP conditions

- `src/lib.rs` calls any `Updater` method other than `init`/`check_for_updates` (a UI grew) — report.
- `AnyClass::get(c"SPUStandardUpdaterController")` returns `None` in the manual check (framework layout changed) — report the Sparkle version in `.daku-cache/sparkle`.
- The manual check shows a crash on `initWithStartingUpdater:` (selector/ABI mismatch with objc2 `msg_send!` bool/nil arguments) — report with the crash log; do not reintroduce the custom driver.
- Not on macOS — Steps 1–3 and 5 are still executable; note that Step 4 was not run.

## Maintenance notes

- If Daku ever wants an in-app update indicator, implement `SPUStandardUserDriverDelegate`/`SPUUpdaterDelegate` on a small delegate object and pass it to the controller — do not resurrect a full `SPUUserDriver`.
- Bumping Sparkle: `scripts/bundle.sh` `sparkle_version`/`sparkle_sha256`; the selectors used here are stable across Sparkle 2.x.
- Reviewers: check that `init()` still refuses to run in debug builds without `DAKU_FORCE_UPDATER=1` (the dev watcher's app must never offer to replace itself) and that the Homebrew gate precedes any `dlopen`.
