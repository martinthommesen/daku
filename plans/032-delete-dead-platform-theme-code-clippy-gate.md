# Plan 032: Delete rustc-flagged dead platform/theme code, fix the sidebar tint width, and add clippy to the gate

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- src/platform.rs src/theme.rs src/daemon.rs src/app.rs src/lib.rs Cargo.toml package.json crates/daku-client/src/process.rs crates/daku-core/src/persistence.rs crates/daku-core/src/servicenow.rs crates/daku-core/src/server.rs`
> Plans 016, 020, 021, 029, 030 are expected to have touched some of these. Re-run
> `cargo clippy --workspace --all-targets` FIRST and work from the live warning
> list; the list below is from `f7fdbe7` and tells you what each warning is.
> If a warning listed below is already gone, skip it. If a warning NOT listed
> below appears in a file outside Scope, STOP and report.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW (deletions of code with zero callers; one 32 px layout fix)
- **Depends on**: plans/011 (gate), plans/016 (root deps trimmed), plans/020 (settings cleanup — owns `apply_theme_preference`'s fate), plans/021 (updater — owns the `src/updater.rs` warnings), plans/029, plans/030 (their deletions remove other warnings). Land this **last** among the debt plans so the clippy gate goes green in one step.
- **Category**: tech-debt / dx
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/55

## Why this matters

`cargo clippy --workspace` at `f7fdbe7` reports 29 warnings; 21 are in the root `daku` crate and almost all are `dead_code`: unsafe AppKit helpers with no caller (`load_app_icon_for_bundle_id`, `set_window_appearance`, `set_sidebar_material_width`, …), a Linux icon loader that references an `image` crate that is not a dependency and a `website/public/app-icon.png` that does not exist (the crate cannot build on Linux), 14 palette fields no view reads, and `daemon::local_hostname`. Warnings mask real ones and the "local verification gate" (`bun run check`, plan 011) deliberately left clippy out because of them. One of the unused helpers hides a real bug: the macOS sidebar tint layer is 252 px wide (`platform.rs:239`) while the GPUI sidebar is 220 px (`app.rs:144`), so a 32 px band of tint bleeds under the detail pane.

After this plan: zero clippy warnings on `main`, `bun run check` runs `cargo clippy --workspace --all-targets -- -D warnings`, and the tint width is shared with the sidebar width.

## Current state

### Clippy at `f7fdbe7` (`cargo clippy --workspace`, lib targets; test targets add duplicates only)

Root crate `daku` (`src/`):

| Warning | Location | Owner |
|---|---|---|
| constant `SYMBOLS_FONT_FAMILY` is never used | `src/assets.rs:216` | plan 030 |
| function `show_task_notification` is never used | `src/platform.rs:121` | **this plan** |
| function `load_app_icon_for_bundle_id` is never used | `src/platform.rs:124` | this plan |
| function `reveal_in_file_manager` is never used | `src/platform.rs:156` | this plan |
| function `open_with_default_app` is never used | `src/platform.rs:161` | this plan |
| function `primary_shortcut` is never used | `src/platform.rs:180` | this plan |
| function `start_window_move` is never used | `src/platform.rs:241` | this plan |
| function `titlebar_double_click` is never used | `src/platform.rs:248` | this plan |
| function `set_sidebar_material_width` is never used | `src/platform.rs:348` | this plan |
| function `set_window_appearance` is never used | `src/platform.rs:390` | this plan |
| function `native_override` is never used | `src/theme.rs:16` | this plan |
| function `apply_theme_preference` is never used | `src/theme.rs:206` | plan 020 (else this plan) |
| multiple fields are never read (`Theme`) | `src/theme.rs:33` | this plan |
| variant `Checking` never constructed; fields `0`/`events` never read; methods `install_available_update`/`status`/`events`/`set_automatically_checks_for_updates` never used; docs for unsafe trait missing `# Safety` ×2 | `src/updater.rs:58,66,68,92,97,586,603,728` | plan 021 |

Other crates:

| Warning | Location | Owner |
|---|---|---|
| called `unwrap_err` on `restore` after checking its variant with `is_ok` | `crates/daku-client/src/process.rs:450` | this plan |
| the `Err`-variant returned from this closure/function is very large (`ErrorResponse`) | `crates/daku-core/src/server.rs:283`, `:447` | this plan (allow) |
| this `if` statement can be collapsed | `crates/daku-core/src/persistence.rs:96`, `crates/daku-core/src/server.rs:423`, `crates/daku-core/src/servicenow.rs:150` | this plan (server.rs:423 is deleted by plan 029) |
| this `impl` can be derived | `crates/daku-protocol/src/i18n.rs:67` (`Default for AppLanguage`), `crates/daku-protocol/src/settings.rs:19` (`Default for DaemonSettings`) | plans 030 / 020 (else this plan) |

### `src/platform.rs` (444 lines) — what stays vs goes

Callers (grep at `f7fdbe7`): `src/lib.rs:133` `init_reduce_motion`, `:143` `show_about_panel`, `:175` `configure_main_window_close_behavior`, `:183` `configure_sidebar_material`; `src/app.rs:29,114` `hide_window`; `src/theme.rs:207` `set_window_appearance` (from the itself-unused `apply_theme_preference`), `:217` `configure_sidebar_material`.

To delete (line ranges at `f7fdbe7`):

```
:120-121  pub fn show_task_notification(_tag, _title, _body, _)  {}       // "Notifications return in Signal UI plans"
:123-152  pub fn load_app_icon_for_bundle_id (macOS body + non-macOS stub)
:154-158  pub fn reveal_in_file_manager
:160-163  pub fn open_with_default_app
:165-177  #[cfg(target_os = "linux")] pub fn linux_app_icon   — uses `image::` (not a dep) and include_bytes!("../website/public/app-icon.png") (missing)
:179-186  pub const fn primary_shortcut
:241-243  pub fn start_window_move
:245-256  pub fn titlebar_double_click
:347-385  pub fn set_sidebar_material_width (macOS + stub)
:387-425  pub fn set_window_appearance (macOS + stub)   — only caller is theme.rs apply_theme_preference (dead)
:438-443  test embedded_linux_icon_decodes_at_desktop_size (references linux_app_icon)
```

To keep: `show_about_panel` (`:3-14`), `init_reduce_motion` + `linux_reduce_motion_enabled` + `parse_boolean_setting` (`:68-118`) and its Linux test (`:427-437`), `configure_main_window_close_behavior` (`:188-199`), `hide_window` (`:201-230`), `SIDEBAR_TINT_VIEW` (`:232-236`), `SIDEBAR_WIDTH` (`:238-239`, **value changes**), `configure_sidebar_material` (`:258-345`). (`register_fonts_with_coretext` `:16-66` is deleted by plan 030.)

```rust
// :238-239
#[cfg(target_os = "macos")]
const SIDEBAR_WIDTH: f64 = 252.0;
// :324-325 (inside configure_sidebar_material)
                let mut frame = content_view.bounds();
                frame.size.width = SIDEBAR_WIDTH;
```

`src/app.rs:144` (inside `render_sidebar`): `.w(px(220.0))`.

### `src/theme.rs`

```rust
// :3
pub use daku_client::theme::ThemePreference;
// :5-14 fn resolves_to_dark(preference, system_appearance) -> bool   (used by init at :198)
// :16-22 fn native_override(preference) -> Option<bool>              (used only by apply_theme_preference)
// :30-77 pub struct Theme { is_dark, canvas, sidebar, sidebar_drag_background, sidebar_item_background, surface, raised,
//        composer, inset, terminal, overlay, overlay_strong, border, border_strong, sidebar_border, text, text_secondary,
//        text_tertiary, text_ghost, accent, resize_handle, gauge, selection, code_text, code_wash, inverse, on_inverse,
//        warning, success, favorite, danger, danger_soft }
// :89-134 pub fn dark() -> Self { … every field … }   :136-185 pub fn light() -> Self { … }
// :196-204 pub fn init(cx)   — always ThemePreference::System
// :206-219 pub fn apply_theme_preference(preference, window, cx)   — calls set_window_appearance + configure_sidebar_material
```

Field usage in `src/app.rs`/`src/lib.rs` (grep `theme\.<field>` at `f7fdbe7`): **used** — `is_dark`, `canvas`, `sidebar`, `sidebar_item_background`, `raised`, `inset`, `border`, `border_strong`, `sidebar_border`, `text`, `text_secondary`, `text_tertiary`, `text_ghost`, `accent`, `warning`, `success`, `danger`, `danger_soft`; **unused (14)** — `sidebar_drag_background`, `surface`, `composer`, `terminal`, `overlay`, `overlay_strong`, `resize_handle`, `gauge`, `selection`, `code_text`, `code_wash`, `inverse`, `on_inverse`, `favorite`. Re-grep before deleting — plans 019/038–043 may add UI that reads some.

### `src/daemon.rs:38-61`

```rust
/// Resolve the local host name once during app construction. Settings can
/// then show a useful LAN URL without touching the OS from a render frame.
pub fn local_hostname() -> Option<String> { … libc::gethostname … std::env::var("HOSTNAME") … }
```

No callers (grep `local_hostname` → only its definition). `libc` stays in root `Cargo.toml` — `src/updater.rs:627` uses `libc::dlopen` outside tests.

### `src/app.rs:28-31` vs `src/lib.rs:175`

```rust
// src/app.rs:28-31 (inside Daku::new)
        window.on_window_should_close(cx, |window, _cx| {
            crate::platform::hide_window(window);
            false
        });
// src/lib.rs:174-176 (open_window callback, runs before Daku::new)
                    move |window, cx| {
                        crate::platform::configure_main_window_close_behavior(window, cx);
                        Daku::new(window, cx, daemon)
```

`configure_main_window_close_behavior` (`platform.rs:191-196`) registers the same handler. Two registrations; keep the platform one.

### Root `Cargo.toml` `objc2-app-kit` features (`:46-69`)

`"std", "NSAccessibility", "NSAppearance", "NSApplication", "NSBitmapImageRep", "NSCell", "NSColor", "NSEvent", "NSGraphics", "NSGraphicsContext", "NSImage", "NSImageRep", "NSMenu", "NSMenuItem", "NSResponder", "NSView", "NSVisualEffectView", "NSWindow", "NSWorkspace", "objc2-core-graphics", "objc2-quartz-core"`. AppKit types named in `src/` at `f7fdbe7`: `NSApplication`, `NSBitmapImageFileType`, `NSBitmapImageRep`, `NSView`, `NSWorkspace`, plus (inside `configure_sidebar_material`/`set_window_appearance`) `NSColor`, `NSVisualEffectView`, `NSAutoresizingMaskOptions`, `NSWindowOrderingMode`, `NSAppearance*`. objc2 features also gate **methods** on kept classes (e.g. `NSView::layer()` needs `objc2-quartz-core`, `NSView::window()` needs `NSWindow`, `NSResponder` is a superclass), so trimming is trial-by-compile.

### `package.json` after plan 011

`"check": "cargo fmt --all --check && cargo test --workspace --no-fail-fast && oxlint -c oxlint.config.ts ."`

### Other crates' warning sites

```rust
// crates/daku-client/src/process.rs:450 (inside a test or fn — read the surrounding lines): `if restore.is_ok() { … } … restore.unwrap_err()` → clippy wants `match`/`if let Err(e) = restore`
// crates/daku-core/src/persistence.rs:96-100
        if let Ok(path) = std::env::var(DAKU_DB_PATH_ENV) {
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
// crates/daku-core/src/servicenow.rs:149-153
            if let Some(cached) = cache.get(&environment.id) {
                if self.clock.now() < cached.valid_until {
                    return Ok(cached.access_token.clone());
                }
            }
// crates/daku-core/src/server.rs:283 closure and :447 fn validate_handshake return Result<HandshakeResponse, ErrorResponse> (large Err)
```

Conventions: edition 2024 (let-chains are stable: `if let Ok(path) = … && !path.is_empty()`); imperative commit summaries.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Warning list | `cargo clippy --workspace --all-targets 2>&1 \| grep -E '^warning' \| sort \| uniq -c` | after Step 6: no lines except none |
| Strict clippy | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Compile | `cargo check --workspace --all-targets` | exit 0 |
| Root tests | `cargo test -p daku` | all pass |
| Gate | `bun run check` | exit 0 (now including clippy) |
| Manual smoke | `DAKU_UI_FIXTURE=1 bun run dev` | sidebar tint ends exactly at the sidebar edge (macOS) |

## Scope

**In scope**:
- `src/platform.rs`, `src/theme.rs`, `src/daemon.rs`, `src/app.rs` (delete duplicate close handler; export `SIDEBAR_WIDTH`), `src/lib.rs` (only if an import breaks)
- `Cargo.toml` (root, `objc2-app-kit`/`objc2-foundation` feature lists — optional Step 5)
- `crates/daku-client/src/process.rs:450` area, `crates/daku-core/src/persistence.rs:96`, `crates/daku-core/src/servicenow.rs:150`, `crates/daku-core/src/server.rs` (two `#[allow(clippy::result_large_err)]` attributes only)
- `crates/daku-protocol/src/i18n.rs` / `settings.rs` `Default` impls — only if plans 030/020 left them
- `package.json` (`check` script)
- `plans/README.md` (status row)

**Out of scope**:
- `src/updater.rs` — plan 021 owns every warning there. If 021 is not DONE, STOP at Step 6 (do not `#[allow]` your way past it).
- `src/assets.rs` — plan 030.
- Any behaviour change in `configure_sidebar_material`, `hide_window`, `init_reduce_motion`, `show_about_panel`.
- Deleting `resolves_to_dark`/`init` or changing the palette values of kept fields.

## Git workflow

- Trunk-based on `main`; commit directly; do NOT push unless asked.
- Suggested commits: (1) `Delete unused platform/theme helpers; share the sidebar width.` (2) `Fix remaining clippy lints and gate on clippy -D warnings.`

## Steps

### Step 0: Take the live warning list

Run `cargo clippy --workspace --all-targets 2>&1 | grep -E '^warning|-->' | paste - - > /tmp/daku-clippy-before.txt` (temp file outside the repo) and compare with the tables above. Anything in `src/updater.rs` still present → plan 021 not landed → you may proceed with Steps 1–5 but must STOP before Step 6.

### Step 1: `src/platform.rs` deletions + shared sidebar width

1. Delete the functions listed under "To delete" (`show_task_notification`, `load_app_icon_for_bundle_id` both cfgs, `reveal_in_file_manager`, `open_with_default_app`, `linux_app_icon`, `primary_shortcut`, `start_window_move`, `titlebar_double_click`, `set_sidebar_material_width` both cfgs, `set_window_appearance` both cfgs) and the test `embedded_linux_icon_decodes_at_desktop_size`. Keep the Linux `mod tests` with its one remaining test.
2. In `src/app.rs`, add near the top (after imports): `pub const SIDEBAR_WIDTH: f32 = 220.0;` and change `.w(px(220.0))` in `render_sidebar` (`:144`) to `.w(px(SIDEBAR_WIDTH))`.
3. In `src/platform.rs`, replace `const SIDEBAR_WIDTH: f64 = 252.0;` (`:238-239`, with its `#[cfg]`) — delete it — and change `frame.size.width = SIDEBAR_WIDTH;` (`:325`) to `frame.size.width = f64::from(crate::app::SIDEBAR_WIDTH);`.
4. Delete the duplicate close handler in `src/app.rs:28-31` (`window.on_window_should_close(cx, |window, _cx| { crate::platform::hide_window(window); false });`). `configure_main_window_close_behavior` in `src/lib.rs:175` already registers it.

**Verify**: `cargo check -p daku --all-targets` → exit 0; `cargo clippy -p daku 2>&1 | grep -c 'src/platform.rs'` → `0`. `DAKU_UI_FIXTURE=1 bun run dev` on macOS: the sidebar tint band ends at the sidebar's right edge (no lighter/darker strip under the detail pane); Cmd-W hides the window once and Dock re-activation restores it.

### Step 2: `src/theme.rs`

1. Delete `native_override` (`:16-22`).
2. If plan 020 has not removed it, delete `apply_theme_preference` (`:206-219`) — it is unused and calls the now-deleted `set_window_appearance`.
3. Re-grep field usage: `for f in sidebar_drag_background surface composer terminal overlay overlay_strong resize_handle gauge selection code_text code_wash inverse on_inverse favorite; do echo "$f $(grep -rn "\.$f\b" src --include='*.rs' | grep -v 'src/theme.rs' | wc -l)"; done` — every count must be `0`; delete those fields from `struct Theme` **and** from both `dark()` and `light()` initialisers. Any field with a non-zero count stays (a plan landed a consumer).
4. If `ThemePreference` (`:3` re-export) becomes unused after 020/030, delete the `pub use` too.

**Verify**: `cargo clippy -p daku 2>&1 | grep -c 'src/theme.rs'` → `0`; `cargo test -p daku` → all pass.

### Step 3: `src/daemon.rs`

Delete `local_hostname` (`:38-61`, including its doc comment). If `libc` is then unused in `src/daemon.rs`, remove that file's `libc` usage only — keep the crate dependency (`src/updater.rs` uses it).

**Verify**: `grep -rn 'local_hostname' src` → 0; `cargo check -p daku` → exit 0.

### Step 4: Mechanical lints in the other crates

- `crates/daku-core/src/persistence.rs:96-100` → `if let Ok(path) = std::env::var(DAKU_DB_PATH_ENV) && !path.is_empty() { return PathBuf::from(path); }`.
- `crates/daku-core/src/servicenow.rs:149-153` → `if let Some(cached) = cache.get(&environment.id) && self.clock.now() < cached.valid_until { return Ok(cached.access_token.clone()); }`.
- `crates/daku-core/src/server.rs:423` — gone if plan 029 landed (`dispatch_request` rewritten); otherwise collapse the nested `if matches!(…)` into one condition.
- `crates/daku-core/src/server.rs` `validate_handshake` (and the closure that calls it): add `#[allow(clippy::result_large_err)]` on `fn validate_handshake` and on `fn handle_connection` (the closure lives inside it), with the comment `// tungstenite's ErrorResponse is a full http::Response; boxing it buys nothing here.`
- `crates/daku-client/src/process.rs:450`: rewrite the `is_ok()` + `unwrap_err()` pair as `match restore { Ok(..) => …, Err(error) => … }` (read the surrounding function first; keep semantics).
- `crates/daku-protocol/src/settings.rs:19` `impl Default for DaemonSettings` → `#[derive(Default)]` — only if plan 020 has not already replaced the struct; `crates/daku-protocol/src/i18n.rs:67` `impl Default for AppLanguage` → `#[derive(Default)]` + `#[default]` on `System` — only if plan 030 left the enum.

**Verify**: `cargo clippy --workspace --all-targets -- -D warnings` → exit 0 **or** the only remaining warnings are in `src/updater.rs` (021 pending → STOP before Step 6).

### Step 5 (optional, bounded): trim `objc2-app-kit` / `objc2-foundation` features

Only if Steps 1–4 are green. Remove **one** feature at a time from the root `Cargo.toml` list, run `cargo check -p daku`; if it fails, put it back and move on. Candidates likely removable after Step 1: `NSBitmapImageRep`, `NSImage`, `NSImageRep`, `NSCell`, `NSEvent`, `NSGraphicsContext`, `NSMenu`, `NSMenuItem`, `NSAppearance` (only if `set_window_appearance` is gone) in `objc2-app-kit`; `NSBundle`, `NSData`, `NSDictionary`, `NSError`, `NSFileManager`, `NSKeyValueObserving`, `NSLocale`, `NSProcessInfo`, `NSURL` in `objc2-foundation` (`src/updater.rs` may need some — the compile decides). Stop after one pass; do not chase transitive method gates.

**Verify**: `cargo check --workspace --all-targets` → exit 0 after each removal; `cargo test -p daku` → all pass at the end.

### Step 6: Gate on clippy

Only when `cargo clippy --workspace --all-targets -- -D warnings` exits 0. In `package.json` change the `check` script to:

```json
"check": "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast && oxlint -c oxlint.config.ts ."
```

and add one sentence to the "Verification gate" section in **both** `CLAUDE.md` and `AGENTS.md` (added by plan 011): `The gate includes \`cargo clippy -- -D warnings\`; do not add \`#[allow]\` to pass it without a comment saying why.`

**Verify**: `bun run check` → exit 0; `diff CLAUDE.md AGENTS.md` → no output.

## Test plan

- No new behaviour; the only test change is deleting `embedded_linux_icon_decodes_at_desktop_size` (its subject is deleted). `cargo test --workspace --no-fail-fast` → 0 failed.
- Manual: macOS sidebar tint width (Step 1) and Cmd-W hide/restore.

## Done criteria

- [ ] `grep -n 'fn show_task_notification\|fn load_app_icon_for_bundle_id\|fn reveal_in_file_manager\|fn open_with_default_app\|fn linux_app_icon\|fn primary_shortcut\|fn start_window_move\|fn titlebar_double_click\|fn set_sidebar_material_width\|fn set_window_appearance' src/platform.rs` → 0 matches
- [ ] `grep -n 'fn native_override\|fn apply_theme_preference\|sidebar_drag_background\|resize_handle\|code_wash' src/theme.rs` → 0 matches
- [ ] `grep -rn 'local_hostname' src` → 0; `grep -c 'on_window_should_close' src/app.rs` → `0`
- [ ] `grep -n 'SIDEBAR_WIDTH' src/app.rs src/platform.rs` → definition in `app.rs` (220.0) + one use in each file; `grep -n '252' src/platform.rs` → 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `grep -n 'clippy' package.json` → 1 match inside `"check"`
- [ ] `bun run check` exits 0; `diff CLAUDE.md AGENTS.md` prints nothing
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 032 updated

## STOP conditions

- Step 0 shows warnings in `src/updater.rs` and plan 021 is not DONE — do Steps 1–5, then STOP before Step 6 and report "clippy gate blocked on 021".
- Step 2's field re-grep shows a non-zero count for a field this plan meant to delete — keep that field, note it, continue.
- Deleting `set_window_appearance` breaks compilation because a plan added a caller — keep it and report.
- Step 5: more than one `cargo check` failure per feature attempt (do not iterate on transitive method gates).
- Any clippy warning outside the Scope list appears at Step 6.

## Maintenance notes

- With `-D warnings` in the gate, every new dead helper fails `bun run check` immediately — that is the point; delete or use, never `#[allow(dead_code)]` without a comment.
- `SIDEBAR_WIDTH` is now the single source for both the GPUI sidebar and the AppKit tint layer; if the sidebar becomes resizable, `configure_sidebar_material` must be re-run (or the deleted `set_sidebar_material_width` restored) on resize.
- Reviewers: the diff should be deletions plus ≤ 10 lines of lint fixes; confirm no palette **values** of kept fields changed.
