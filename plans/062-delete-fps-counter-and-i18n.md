# Plan 062: Delete the FPS counter that renders the word "FPS", and the i18n framework serving four English strings

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- src/lib.rs src/app.rs Cargo.toml locales`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: tech-debt
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Two pieces of scaffolding that never grew into anything.

1. **The FPS counter is a menu item, a keybinding, an action, a struct field, a
   listener, a locale key — and it renders the literal string `"FPS"`.** No
   frame timing exists anywhere in the repo. It is in the Window menu and bound
   to `secondary-alt-shift-f`, so it is discoverable UI, not a hidden dev
   toggle: an Operator can find it and it does nothing.
2. **`rust-i18n` 4 plus a `locales/` catalog plus a `tr!` macro serve six keys,
   `en` only, all in one file.** Nothing else in `src/` or `crates/` calls
   `t!`/`tr!`. Plan 030 already deleted `ja.yml` and `zh-CN.yml`; what is left is
   a build-script-backed framework resolving `"About Daku"` to `"About Daku"`.

Neither is a bug. Both are the kind of thing an audit exists to remove: less
code, one fewer dependency, one fewer directory, and a menu that means what it
says.

## Current state

**`src/app.rs:183-192`** — the entire feature:

```rust
            .when(self.show_fps, |element| {
                element.child(
                    div()
                        .px(px(12.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .text_color(cx.theme().muted_foreground)
                        .child("FPS"),
                )
            })
```

Its supporting cast: `src/app.rs:27` (`use crate::{CloseWindow, ToggleFpsCounter}`),
`:34` (`show_fps: bool`), `:59` (`show_fps: false`), `:163-165` (the listener),
`src/lib.rs:32` (the action list), `src/lib.rs:88` (the keybinding),
`src/lib.rs:156` (the menu item).

**`src/lib.rs:1-11`** — the i18n plumbing:

```rust
rust_i18n::i18n!("locales", fallback = "en");

macro_rules! tr {
    ($key:expr) => {
        rust_i18n::t!($key).into_owned()
    };
    ($key:expr, $($args:tt)*) => {
        rust_i18n::t!($key, $($args)*).into_owned()
    };
}
```

**`src/lib.rs:138-160`** — every call site:

```rust
                let mut items = vec![MenuItem::action(tr!("menu.about", app = APP_NAME), About)];
                if updater_available {
                    items.push(MenuItem::action(
                        tr!("menu.check_for_updates"),
                        CheckForUpdates,
                    ));
                }
                items.push(MenuItem::separator());
                items.push(MenuItem::action(tr!("menu.quit", app = APP_NAME), Quit));
                ...
            name: tr!("menu.window").into(),
            ...
                MenuItem::action(tr!("menu.toggle_fps_counter"), ToggleFpsCounter),
                MenuItem::action(tr!("menu.close_window"), CloseWindow),
```

**`locales/app.yml`** — the whole catalog:

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
  en: Toggle FPS Counter
menu.close_window:
  en: Close Window
```

**`Cargo.toml:44`** — `rust-i18n = "4"`.

### Constraints you must honor

- **`crates/daku-core/build.rs` is unrelated** — it embeds the drizzle
  migrations, not locales. Do not touch it. (`rust_i18n::i18n!` is a macro, not
  a build script; deleting `locales/` affects only that macro.)
- The **menu strings must not change**. `About Daku`, `Check for Updates…`,
  `Quit Daku`, `Window`, `Close Window` — same text, same ellipsis character
  (`…`, U+2026, not three dots), same `APP_NAME` interpolation.
- **`CONTEXT.md`** is the vocabulary source for *domain* terms; menu labels are
  platform conventions and are not domain vocabulary. No `CONTEXT.md` change.
- ADR-0008: the shell is gpui-component's `TitleBar`/`Sidebar`. Nothing here
  touches that.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Build | `cargo build -p daku` | exit 0 |
| Visual check | `DAKU_UI_FIXTURE=1 bun run dev` | Operator confirms the menus read the same |

## Scope

**In scope**:
- `src/app.rs`
- `src/lib.rs`
- `Cargo.toml`
- `locales/` (delete)

**Out of scope** (do NOT touch):
- `Cargo.lock` beyond what removing one dependency does automatically.
- `crates/daku-core/build.rs` and `db/migrations/`.
- Any other action, keybinding or menu item (`Quit`, `CloseWindow`,
  `CheckForUpdates`, `About` all stay).
- `src/updater.rs` — `CheckForUpdates` is conditionally present
  (`updater_available`); preserve that condition exactly.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative. Two independent deletions — two commits preferred,
  e.g. `Delete the FPS counter: it rendered the string "FPS" (#87).`

## Steps

### Step 1: Delete the FPS counter

Remove, in this order so the tree compiles at each stage:

1. `src/lib.rs:156` — the `MenuItem::action(tr!("menu.toggle_fps_counter"), ToggleFpsCounter)` entry.
2. `src/lib.rs:88` — the `KeyBinding::new("secondary-alt-shift-f", …)` entry.
3. `src/lib.rs:32` — `ToggleFpsCounter` from the action list.
4. `src/app.rs` — the `.when(self.show_fps, …)` block, the listener at
   `:163-165`, the `show_fps` field and its initialiser, and the import.
5. `locales/app.yml` — the `menu.toggle_fps_counter` key.

**Verify**: `grep -rni "fps" src/ locales/` → no matches (`locales/` still exists at this step; Step 2 deletes it).
`cargo build -p daku` → exit 0. `bun run check` → exit 0.

### Step 2: Inline the five remaining menu strings

Replace each `tr!(...)` with the literal it resolves to, keeping `APP_NAME`
interpolation via `format!`:

```rust
let mut items = vec![MenuItem::action(format!("About {APP_NAME}"), About)];
...
items.push(MenuItem::action(format!("Quit {APP_NAME}"), Quit));
```

and `"Check for Updates…"`, `"Window"`, `"Close Window"` as plain literals.
**Copy the strings from `locales/app.yml` character for character** — the
ellipsis is U+2026.

Then delete the `tr!` macro and the `rust_i18n::i18n!` line from `src/lib.rs`,
delete the `locales/` directory, and remove `rust-i18n = "4"` from `Cargo.toml`.

**Verify**: `grep -rn "rust_i18n\|tr!" src/ crates/` → no matches.
`ls locales` → no such directory. `cargo build -p daku` → exit 0.
`bun run check` → exit 0.

### Step 3: Confirm the menus are unchanged

This is the one thing a compiler cannot check. Ask the Operator to launch
`DAKU_UI_FIXTURE=1 bun run dev` and confirm the app and Window menus read
exactly as before, with `Toggle FPS Counter` gone. Record their answer.

**Verify**: recorded in your report.

## Test plan

No new tests. Menu construction is not testable in this setup (it needs a GPUI
`App`), and adding a harness for five string literals would be more machinery
than the code it guards — which is the same reasoning that makes this deletion
worth doing.

The regression surface is covered by: `cargo build` (the strings must compile),
`grep` (nothing references the removed symbols), and the Operator's visual
check in Step 3.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -rni "fps" src/ crates/` → no matches (`locales/` is gone by then)
- [ ] `grep -rn "rust_i18n\|tr!(" src/ crates/` → no matches
- [ ] `grep -n "rust-i18n" Cargo.toml` → no matches
- [ ] `ls locales 2>/dev/null` → no such directory
- [ ] `grep -c "rust-i18n" Cargo.lock` → `0`
- [ ] `grep -n "Check for Updates" src/lib.rs` → present, with U+2026
- [ ] Your report records the Operator's Step 3 confirmation
- [ ] `plans/README.md` status row for 062 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- `grep -rn "tr!\|rust_i18n"` finds a call site outside `src/lib.rs` — then i18n
  is more load-bearing than this plan assumes.
- Removing `rust-i18n` from `Cargo.toml` cascades into unrelated `Cargo.lock`
  churn beyond that crate and its own dependencies.
- The Operator reports any menu label changed.
- Anything suggests the FPS counter was a deliberate placeholder for work in
  flight. Report rather than delete.

## Maintenance notes

- **If a real frame counter is ever wanted**, it is a few lines: an `Instant`
  delta in `render`. Deleting the dead scaffolding does not block that — it just
  stops shipping a menu item that lies. Plan 056 (render cost) is where a real
  counter would actually be useful.
- **If a second locale is ever shipped**, re-add `rust-i18n` then. Five literals
  is not a reason to carry a framework; a real translation is.
- Reviewers: check the ellipsis and the `APP_NAME` interpolation. Those are the
  two ways a mechanical string inline goes subtly wrong.
