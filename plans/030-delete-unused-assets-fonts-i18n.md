# Plan 030: Delete the unreferenced waku assets (icons, fonts, CoreText FFI) and shrink the i18n catalog to what daku uses

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- src/assets.rs src/platform.rs src/lib.rs assets locales crates/daku-protocol/src/lib.rs crates/daku-protocol/src/i18n.rs crates/daku-protocol/src/theme.rs crates/daku-core/build.rs`
> If any of those changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. (Plan 020 may have already removed
> `AppLanguage`/`ThemePreference` — see Step 4's branch.)

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate). Soft: plans/020-settings-cleanup-typed-poll-interval.md (owns removing `theme`/`language` from the settings structs — Step 4 branches on whether it landed).
- **Category**: tech-debt
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/49

## Why this matters

The waku fork (ADR-0003) left three piles of assets in the binary that daku never asks for:

- **Icons**: `src/assets.rs` embeds 183 SVGs (`include_bytes!`) — 86 top-level (`bot`, `terminal`, `provider-*`, …) plus 101 `file-types/*` (angular, docker, …). `grep -rn -e svg -e 'icons/' src` outside `assets.rs` returns nothing: no view ever calls `AssetSource::load`. `assets/icons` is 764 KB on disk.
- **Fonts**: four JetBrainsMono faces (1.1 MB) are registered via `add_fonts` and a 2.5 MB Nerd Symbols font is registered through 45 lines of `unsafe` CoreText FFI in `src/platform.rs` — yet no `font_family(`/`FontFallbacks` call exists in `src/`; rustc already flags `SYMBOLS_FONT_FAMILY` unused.
- **i18n**: `locales/app.yml` (2037 lines), `ja.yml` (1044), `zh-CN.yml` (992) — daku uses **six** `menu.*` keys (`src/lib.rs:201-218`). The catalog is compiled in **twice** (`rust_i18n::i18n!` in both `src/lib.rs:3` and `crates/daku-protocol/src/lib.rs:5`), the protocol crate's `i18n.rs` tests pin waku strings ("New Task", "session.rewound", "computer_use.allow_control"), and `set_language` has no caller so the locale is always `en`.

Cost: ~4.4 MB in every `Daku.app` and Sparkle download, an unsafe FFI path with zero payoff, 4 k lines of YAML that will drift, and a proc-macro dependency in the wire crate. This plan deletes what nothing references and keeps the six menu strings.

## Current state

### `src/assets.rs` (243 lines)

```rust
// :1-7
use std::borrow::Cow;
use anyhow::Result;
use gpui::{App, AssetSource, SharedString};
/// Icons embedded in the binary so the app stays a single artifact.
pub struct Assets;
// :9-16  macro_rules! icons { … concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")) … }
// :18-202 const ICONS: &[(&str, &[u8])] = icons![ "alert", "appearance", … "file-types/angular", … "zap", ];
// :204-209 const TEXT_FONTS: &[&[u8]] = &[ include_bytes!("../assets/fonts/JetBrainsMono-{Regular,Bold,Italic,BoldItalic}.ttf") ];
// :211-216
const SYMBOLS_FONT: &[u8] = include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");
pub const SYMBOLS_FONT_FAMILY: &str = "Symbols Nerd Font Mono";
// :218-226
pub fn register_fonts(cx: &App) -> Result<()> {
    cx.text_system().add_fonts(TEXT_FONTS.iter().map(|font| Cow::Borrowed(*font)).collect::<Vec<_>>())?;
    crate::platform::register_fonts_with_coretext(&[SYMBOLS_FONT])
}
// :228-243 impl AssetSource for Assets { fn load(..) → ICONS lookup; fn list(..) → ICONS prefix filter }
```

Callers: `src/lib.rs:127` `.with_assets(crate::assets::Assets)` and `src/lib.rs:131` `crate::assets::register_fonts(cx).expect("failed to register bundled fonts");`. Nothing else in `src/` references `assets::`, `.svg`, `icons/`, `JetBrains`, `font_family`, or `SYMBOLS_FONT_FAMILY` (grep at `f7fdbe7`).

`src/platform.rs:16-61` — `register_fonts_with_coretext(fonts: &[&'static [u8]])` (macOS: `#[link] extern "C"` CoreGraphics/CoreText FFI; `:63-66` non-macOS no-op). Only caller: `assets.rs:225`.

`assets/` tracked files: 193 (`assets/icons/*.svg` ×86, `assets/icons/file-types/*.svg` ×101 + `SOURCE.md`, `assets/fonts/{JetBrainsMono-Regular,Bold,Italic,BoldItalic}.ttf`, `SymbolsNerdFontMono-Regular.ttf`, `LICENSE-nerd-fonts.txt`, `OFL.txt`).

### i18n

`src/lib.rs`:

```rust
// :3
rust_i18n::i18n!("locales", fallback = "en");
// :5-9
const _LOCALE_SOURCES: [&str; 3] = [
    include_str!("../locales/app.yml"),
    include_str!("../locales/zh-CN.yml"),
    include_str!("../locales/ja.yml"),
];
// :11-18
macro_rules! tr {
    ($key:expr) => { crate::i18n::translate($key) };
    ($key:expr, $($args:tt)*) => { rust_i18n::t!($key, $($args)*).into_owned() };
}
// :28  pub use daku_client::{i18n, identity, persistence};
// :201-218 the only tr! uses: "menu.about" (app = APP_NAME), "menu.check_for_updates", "menu.quit" (app = APP_NAME),
//          "menu.window", "menu.toggle_fps_counter", "menu.close_window"
```

`crates/daku-protocol/src/lib.rs`:

```rust
// :5
rust_i18n::i18n!("../../locales", fallback = "en");
// :7-11  const _LOCALE_SOURCES: [&str; 3] = [ include_str!("../../../locales/{app,zh-CN,ja}.yml") ];
// :13-20 macro_rules! _tr { … }        ← zero uses
// :22-25 pub mod i18n; pub mod identity; pub mod settings; pub mod theme;
```

`crates/daku-protocol/src/i18n.rs` (222 lines): `AppLanguage` enum (`:9-14`, `System|English|SimplifiedChinese|Japanese`), `locale()`, `label()` (uses `translate("language.system")`), `resolved()/from_system()/from_locale_id()`, `set_language` (`:73-75`, **no callers**), `translate` (`:77-79`, used by `tr!` in `src/lib.rs` and by `theme.rs:19-21`), `uses_east_asian_date_format` (`:81-87`, no callers outside tests), `system_locale()` (`:89-109`, macOS `NSLocale::preferredLanguages()` — the reason `objc2-foundation` is in the protocol manifest), tests `:111-221` including waku-string assertions at `:178-219` (`settings.daemon`, `daemon.expose_title`, `settings.general` zh-CN/ja, `computer_use.allow_control`, `session.rewound`, `menu.new_task`, `command_palette.new_task`, `providers.disabled_for_new_tasks`).

`crates/daku-protocol/src/theme.rs` (24 lines): `ThemePreference { System, Light, Dark }` + `label()` translating `settings.theme_{system,light,dark}`. `label()` has no callers. `ThemePreference` itself is used by `crates/daku-protocol/src/settings.rs:13`, `crates/daku-client/src/persistence.rs:37`, and `src/theme.rs:3-20,198,206` (`resolves_to_dark`, `native_override`, `apply_theme_preference` — the last two are rustc-flagged unused; plan 032 deletes them; `init` at `:196` always passes `ThemePreference::System`).

`AppLanguage` is used by `crates/daku-protocol/src/settings.rs:7,14,23` and `crates/daku-client/src/persistence.rs:14,38,47` (persisted, never applied) — plan 020 removes those fields.

`locales/app.yml` — the six used keys are at lines 3, 5, 9, 45, 47, 49 (`menu.about`, `menu.check_for_updates`, `menu.quit`, `menu.window`, `menu.toggle_fps_counter`, `menu.close_window`), format:

```yaml
_version: 2

menu.about:
  en: About %{app}
menu.check_for_updates:
  en: Check for Updates…
```

`crates/daku-core/build.rs:16-19` prints `cargo:rerun-if-changed=<repo>/locales` — daku-core has no i18n usage at all (grep `rust_i18n|locales` in `crates/daku-core/src` → 0).

`Cargo.toml` (root) `rust-i18n = "4"`; `crates/daku-protocol/Cargo.toml` `rust-i18n = "4"` + macOS `objc2-foundation` (`NSArray`, `NSLocale`, `NSString`).

Conventions: imperative commit summaries; `bun run check` is the gate.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check --workspace --all-targets` | exit 0 |
| Root tests | `cargo test -p daku` | all pass |
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Gate | `bun run check` | exit 0 |
| Size before/after (optional) | `cargo build --release -p daku && ls -l target/release/daku` | binary shrinks by ≈4 MB |
| Manual smoke | `DAKU_UI_FIXTURE=1 bun run dev` | window renders; menu shows About/Check for Updates/Quit/Window items |

## Scope

**In scope**:
- `src/assets.rs`, `src/platform.rs` (only `register_fonts_with_coretext`), `src/lib.rs` (`register_fonts` call, `_LOCALE_SOURCES`)
- `assets/icons/**`, `assets/fonts/**` (deletions)
- `locales/app.yml` (shrink), `locales/ja.yml`, `locales/zh-CN.yml` (delete)
- `crates/daku-protocol/src/lib.rs`, `crates/daku-protocol/src/i18n.rs`, `crates/daku-protocol/src/theme.rs`, `crates/daku-protocol/Cargo.toml`
- `crates/daku-core/build.rs` (one `println!`)
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-protocol/src/settings.rs`, `crates/daku-client/src/persistence.rs` field removal (`theme`/`language`) — plan 020. If 020 has not landed, `AppLanguage`/`ThemePreference` **stay** (Step 4 branch B).
- `src/theme.rs` palette / `apply_theme_preference` / `native_override` — plan 032.
- Any UI change; any new font.
- `resources/AppIcon*.icns`, `resources/Info.plist`.

## Git workflow

- Trunk-based on `main`; commit directly; do NOT push unless asked.
- Two commits suggested: `Drop unreferenced icons, fonts, and CoreText font registration.` then `Shrink the i18n catalog to the six menu strings; embed it once.`

## Steps

### Step 1: Confirm nothing references the assets (read-only)

```sh
grep -rn -e '\.svg' -e 'icons/' -e 'JetBrains' -e 'font_family' -e 'FontFallbacks' -e 'SYMBOLS_FONT' -e 'assets::' src | grep -v '^src/assets.rs'
```

**Verify**: exactly two hits, both in `src/lib.rs` (`.with_assets(crate::assets::Assets)` and `crate::assets::register_fonts(cx)`), plus the doc comment mention of `FontFallbacks` in `src/platform.rs:20`. Anything else → STOP (a view started using an icon/font).

### Step 2: Delete icons, fonts and the FFI

1. `git rm -r assets/icons assets/fonts` (removes 193 tracked files including `LICENSE-nerd-fonts.txt` and `OFL.txt` — they license the deleted fonts only). Confirm `assets/` is then empty and remove the directory.
2. Rewrite `src/assets.rs` to the minimum GPUI still needs (an `AssetSource` with nothing in it — `gpui_platform::application().with_assets(..)` requires some `AssetSource`; keep the type, drop the data):

```rust
use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// daku ships no bundled assets; GPUI still needs an `AssetSource`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, _path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
```

3. `src/lib.rs:131`: delete the line `crate::assets::register_fonts(cx).expect("failed to register bundled fonts");`.
4. `src/platform.rs`: delete `register_fonts_with_coretext` — both the macOS body (`:16-61`, including its doc comment) and the non-macOS stub (`:63-66`).

**Verify**: `cargo check -p daku --all-targets` → exit 0 with no `unused` warning mentioning `assets`/`SYMBOLS`. `git status --short | grep -c '^D  assets/'` → `193`. `DAKU_UI_FIXTURE=1 bun run dev` renders the window with system fonts (text visible, sidebar + cards).

### Step 3: Shrink the locale catalog and embed it once

1. Replace `locales/app.yml` with exactly:

```yaml
_version: 2

menu.about:
  en: About %{app}
menu.check_for_updates:
  en: Check for Updates…
menu.quit:
  en: Quit %{app}
menu.window:
  en: Window
menu.toggle_fps_counter:
  en: <copy the current en value from locales/app.yml line 48>
menu.close_window:
  en: <copy the current en value from locales/app.yml line 50>
```

(Copy the six `en:` values verbatim from the current file — `sed -n '3,10p;45,50p' locales/app.yml` — so the menu text does not change.)

2. `git rm locales/ja.yml locales/zh-CN.yml`.
3. `src/lib.rs`: delete `_LOCALE_SOURCES` (`:5-9`). Keep `rust_i18n::i18n!("locales", fallback = "en");` and the `tr!` macro. Change `tr!`'s single-arg arm to `rust_i18n::t!($key).into_owned()` so it no longer depends on `daku_client::i18n::translate` (which Step 4 may delete); if plan 020/033 has not yet removed `pub use daku_client::{i18n, identity, persistence};` (`:28`), drop `i18n` from that list only if nothing else in `src/` uses `crate::i18n::` (grep).
4. `crates/daku-core/build.rs:16-19`: delete the `rerun-if-changed … locales` `println!` (daku-core never reads locales).

**Verify**: `cargo test -p daku` → all pass; `wc -l locales/app.yml` → ≤ 14; `ls locales` → `app.yml` only.

### Step 4: Remove i18n from the protocol crate

**Branch A — plan 020 is DONE** (`grep -n 'AppLanguage' crates/daku-protocol/src/settings.rs crates/daku-client/src/persistence.rs` → 0 matches):

1. Delete `crates/daku-protocol/src/i18n.rs` entirely and `pub mod i18n;` from `crates/daku-protocol/src/lib.rs`.
2. `crates/daku-protocol/src/lib.rs`: delete the `rust_i18n::i18n!(…)` line, `_LOCALE_SOURCES`, and the `_tr!` macro (`:5-20`).
3. `crates/daku-protocol/src/theme.rs`: delete `impl ThemePreference { … label() … }` (`:14-24`) so the file no longer needs `translate`; keep the enum only if plan 020 kept `ThemePreference` (check `settings.rs`/`persistence.rs`); if nothing references `ThemePreference` any more except `src/theme.rs`, move the enum into `src/theme.rs` (it is a UI concern) and delete `crates/daku-protocol/src/theme.rs` + `pub mod theme;`; update `src/theme.rs:3` (`pub use daku_client::theme::ThemePreference;`) accordingly.
4. `crates/daku-protocol/Cargo.toml`: remove `rust-i18n = "4"` and the `[target.'cfg(target_os = "macos")'.dependencies] objc2-foundation` block (only `system_locale()` used it — grep `objc2_foundation` in `crates/daku-protocol/src` → 0 after step 1).

**Branch B — plan 020 is NOT done** (`AppLanguage`/`ThemePreference` still in the settings structs):

1. In `crates/daku-protocol/src/i18n.rs` keep `AppLanguage` + its `Default`, `locale()` (make it return `"en"` for every variant), delete `label()`, `resolved()`, `from_system()`, `from_locale_id()`, `set_language`, `translate`, `uses_east_asian_date_format`, `locale_uses_east_asian_date_format`, `system_locale()` (both cfg variants), and every test that references them or waku strings (`:111-221`); keep only a serde round-trip test for `AppLanguage::System` → `"system"`.
2. `crates/daku-protocol/src/theme.rs`: delete `impl ThemePreference` (`:14-24`).
3. `crates/daku-protocol/src/lib.rs`: delete `rust_i18n::i18n!`, `_LOCALE_SOURCES`, `_tr!` (`:5-20`).
4. `crates/daku-protocol/Cargo.toml`: remove `rust-i18n` and the macOS `objc2-foundation` block.
5. Leave a one-line note in the plan-020 status row that `AppLanguage` is now a dead enum awaiting 020.

Either branch — **Verify**: `cargo test -p daku-protocol` → all pass; `grep -rn 'rust_i18n\|objc2_foundation' crates/daku-protocol` → 0 matches; `cargo tree -p daku-protocol -e normal | grep -c 'rust-i18n\|objc2-foundation'` → `0`.

### Step 5: Gate

**Verify**: `bun run check` → exit 0. `cargo build --release -p daku` (optional) — `ls -l target/release/daku` is ≈4 MB smaller than before.

## Test plan

- No new behaviour; deletions only. Existing tests: `cargo test -p daku` (11), `cargo test -p daku-protocol` (13 minus the deleted i18n waku-string tests, plus the kept `AppLanguage` serde test in branch B).
- Manual: `DAKU_UI_FIXTURE=1 bun run dev` — app menu shows the six items with the same text as before.

## Done criteria

- [ ] `git ls-files assets | wc -l` → `0`; `ls locales` → `app.yml` only; `wc -l < locales/app.yml` ≤ 14
- [ ] `grep -rn 'register_fonts\|SYMBOLS_FONT\|TEXT_FONTS\|include_bytes!("../assets' src` → 0 matches
- [ ] `grep -rn 'CTFontManagerRegisterGraphicsFont\|CGFontCreateWithDataProvider' src` → 0 matches
- [ ] `grep -rn 'rust_i18n::i18n!' src crates` → exactly 1 match (`src/lib.rs`)
- [ ] `grep -rn 'rust_i18n\|_LOCALE_SOURCES\|_tr!' crates/daku-protocol` → 0 matches; `grep -n 'rust-i18n\|objc2-foundation' crates/daku-protocol/Cargo.toml` → 0 matches
- [ ] `grep -rn 'locales' crates/daku-core/build.rs` → 0 matches
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope paths modified/deleted
- [ ] `plans/README.md` status row for 030 updated

## STOP conditions

- Step 1 grep finds a real consumer of an icon or a font family (a view added since `f7fdbe7`).
- `gpui_platform::application()` no longer accepts an `AssetSource` with empty `load`/`list` (GPUI pin moved — check `Cargo.toml` `rev`).
- The six menu strings' current `en:` values cannot be located at the quoted lines (catalog reordered) — find them by key; if a key is missing, STOP.
- Branch A chosen but `cargo check` reports `AppLanguage`/`ThemePreference` still referenced from `settings.rs`/`persistence.rs` (020 was partial) — switch to Branch B.

## Maintenance notes

- If daku ever needs an icon, add it back one file at a time with a call site in the same commit; do not restore the waku set.
- The root `rust-i18n` dependency stays for six strings; when a Settings view or a second language actually appears, revisit; otherwise it is also a candidate for deletion (hard-code the six strings, drop `rust-i18n` and `locales/`).
- Reviewers: the diff should be almost entirely deletions; the only additions are the empty `AssetSource` and (branch B) the trimmed `AppLanguage`.
