# Plan 044: Put the shell on gpui-component — Root, TitleBar, Sidebar, theme tokens — and switch the gpui pin discipline

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 826a636..HEAD -- Cargo.toml Cargo.lock src/lib.rs src/app.rs src/theme.rs src/platform.rs src/assets.rs README.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED (dependency graph + window chrome change; UI-only, daemon untouched)
- **Depends on**: none (spike `spike/gpui-component` proved compile + render at these revs)
- **Category**: direction / tech-debt
- **Planned at**: commit `826a636`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/68

## Why this matters

The shell is hand-rolled waku scaffolding: a private `Theme` palette, a custom titlebar-less window whose title draws under the traffic lights, an `NSVisualEffectView` sidebar layer, bespoke rows/badges. ADR-0008 decides to build the shell from **gpui-component** instead. This plan lands the structural half — the dependency + pin discipline, `Root`/`TitleBar`/`Sidebar`, system light/dark — with the Environment detail pane converted only as far as compiling on library tokens. Plan 045 restyles the detail pane; plan 046 adds the drill-in.

## Current state

- `Cargo.toml:27-31` pins gpui by rev (plan 016):
  ```toml
  gpui = { git = "https://github.com/zed-industries/zed", rev = "db7c1d38c8e17e9d4f01c35179c847fcd5bfa09b" }
  gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "db7c1d38…", features = ["font-kit"] }
  ```
  with the comment "bump `rev` on both lines together". `README.md` has a matching pin note ("Bumping the GPUI pin" — grep `rev` in README).
- gpui-component `main` (rev `972a3ebfd01afca7da6d8b6f31c9a51288ea5565`, 2026-08-17) depends on `gpui = { git = zed }` **with no rev**; Cargo treats `git+zed?rev=X` and `git+zed` as different sources → two `gpui` crates unless our `rev` goes. Its own lockfile uses zed `e0931d5a9dbf4f781b336fdf448739e74a2ac0b5` (parent of `db7c1d3`; only a CI-workflow commit apart). crates.io `gpui-component 0.5.1` is unusable (registry gpui 0.2.2, no `gpui_platform`).
- `src/lib.rs:59-140` `run()`: `gpui_platform::application().with_assets(crate::assets::Assets)`; `crate::theme::init(cx)`; `open_window(WindowOptions { titlebar: Some(TitlebarOptions { title, appears_transparent, traffic_light_position: point(16,17) }), app_owns_titlebar_drag, window_background: Blurred on macOS, … }, |window, cx| { configure_main_window_close_behavior; Daku::new(window, cx, daemon) })`, then `window.update(.., |_, window, cx| { crate::platform::configure_sidebar_material(window, Theme::current(cx).is_dark); cx.activate(true) })`.
- `src/app.rs` (588 lines): `SIDEBAR_WIDTH = 220.0` (`:18`, also read by `platform.rs:186`); `render` (`:124`) = column [ disconnected_banner? , row [ render_sidebar, render_detail ] ]; `render_sidebar` (`:166`) draws "daku · ServiceNow", `section_label("Platforms")`, `platform_row`, `section_label("Environments")`, `environment_rows` (`:190`, `environment_row` `:365`); `render_detail` (`:199`), `signal_card` (`:305`), `disconnected_banner` (`:400`), `health_dot`/`status_dot`/`health_badge`/`reachability_badge`/`badge`/`health_color` (`:432-494`), `compare_strip` (`:495`), `sparkline` + `paint_sparkline` (`:555-588`). All colours come from `crate::theme::Theme` (`src/theme.rs:22-45`: canvas, sidebar_item_background, raised, inset, border, border_strong, sidebar_border, text, text_secondary, text_tertiary, text_ghost, accent, warning, success, danger, danger_soft; `Theme::current(cx)`, `dark()`, `light()`, `init`).
- `src/platform.rs`: `configure_sidebar_material` (`:124-206`, NSVisualEffectView layer sized by `SIDEBAR_WIDTH`), `configure_main_window_close_behavior`, `hide_window`, `init_reduce_motion`, `show_about_panel` — the last four stay.
- `src/assets.rs`: an empty `AssetSource` (plan 030 deleted all assets).
- `src/dashboard_state.rs` is the model: `sidebar() -> Vec<SidebarRow{id,label,health,muted}>`, `selected_id()`, `select(id)`, `cards()`, `compare_rows()`, `card_summary/card_detail`, `freshness`. **Do not change it** in this plan.
- Vocabulary (`CONTEXT.md` › Screen): **Environment detail**, **Signal card**, **Compare strip**, **Drill-in**. ADR-0008 fixes the pin discipline and the "TitleBar on top, sidebar below" chrome; ADR-0005 amendment drops the Platforms group.

### Spike findings (branch `spike/gpui-component`, commit `10e6585` — read `git show 10e6585 -- src/lib.rs src/app.rs Cargo.toml` for a working reference; it is disposable, do not merge it)

- Pin procedure that worked: drop `rev` from both zed lines; add `gpui-component` **and** `gpui-component-assets` (both `{ git = "https://github.com/longbridge/gpui-component", rev = "972a3ebfd01afca7da6d8b6f31c9a51288ea5565" }`, default features — the crate has no default features); `cargo fetch`; `cargo update -p gpui --precise e0931d5a9dbf4f781b336fdf448739e74a2ac0b5` cascades every zed crate. Verify one gpui: `grep -c 'name = "gpui"' Cargo.lock` → 1; `grep -o 'zed-industries/zed#[a-f0-9]*' Cargo.lock | sort -u` → one line. +141 crates; warm full build ~2 min, incremental ~4 s.
- APIs: `gpui_component::init(cx)` before any component; window first layer `cx.new(|cx| gpui_component::Root::new(view, window, cx))` (`Root::new(impl Into<AnyView>, &mut Window, &mut Context<Root>)`; needs `use gpui::AppContext as _`); `gpui_component::TitleBar::title_bar_options()` → `TitlebarOptions{ title: None, appears_transparent: true, traffic_light_position: (9,9) }`; `TitleBar::new().child(..)` as the first child of the root column; `gpui_component::ActiveTheme as _` gives `cx.theme()` with tokens `background foreground border muted muted_foreground secondary secondary_foreground success warning danger accent radius radius_lg sidebar* popover*` (**no** `card`); `gpui_component::Theme::sync_system_appearance(Some(window), cx)` after open (default is Light — without it the app is light); `Sidebar::new(id).collapsible(SidebarCollapsible::None).w(px).header(SidebarHeader::new()...).child(SidebarGroup::new("Environments").child(SidebarMenu::new().children(items)))`; `SidebarMenuItem::new(label).active(bool).suffix(move |_, _| element).on_click(cx.listener(..))`; icons render only with `.with_assets(gpui_component_assets::Assets)` (single slot → `src/assets.rs` becomes dead). `gpui_component::badge::Badge` is an **overlay** (dot/count on a child), not a labelled pill. Feature unification adds `profiler` to gpui and `runtime_shaders` to gpui_platform — harmless, document it. Name clash: never import `gpui_component::Theme` by name while `crate::theme::Theme` exists.
- Borrow gotchas: return `gpui::AnyElement` from `render_sidebar`/`render_detail`/`signal_card` and bind them to locals before the builder chain; take `cx: &App` where no listener is needed.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Resolve | `cargo fetch` then `cargo update -p gpui --precise e0931d5a9dbf4f781b336fdf448739e74a2ac0b5` | exit 0 |
| One gpui | `grep -c 'name = "gpui"' Cargo.lock` | `1` |
| Build | `cargo build -p daku -p daku-daemon` | exit 0 |
| Fixture run | `HOME=$(mktemp -d) DAKU_UI_FIXTURE=1 DAKU_DAEMON_PATH=$PWD/target/debug/daku-daemon ./target/debug/daku` | window opens, no stderr, alive after 10 s |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**: `Cargo.toml`, `Cargo.lock`, `README.md` (pin note + one sentence on features), `src/lib.rs`, `src/app.rs`, `src/theme.rs` (delete), `src/platform.rs` (delete `configure_sidebar_material` and its cfg stubs), `src/assets.rs` (delete) , `docs/packaging.md` only if it mentions the pin, `plans/README.md` (status row).

**Out of scope**: `src/dashboard_state.rs` and every crate under `crates/` (no model/daemon change); the detail-pane redesign (plan 045); drill-in/deep links (plan 046); `src/updater.rs`; any second window.

## Git workflow

Trunk-based on `main` (`docs/agents/git-workflow.md`); commit on `main` or a disposable local branch; do not push unless asked. Suggested commits: (1) `Adopt gpui-component: pin discipline, Root, TitleBar, system theme (#NN).` (2) `Rebuild the sidebar on gpui-component and delete the waku theme and vibrancy layer (#NN).`

## Steps

### Step 1: Dependencies and pin
Edit `Cargo.toml` per the spike findings (drop `rev` on the two zed lines, add the two gpui-component lines, rewrite the comment: "zed sha is pinned in Cargo.lock only — see ADR-0008; bump with `cargo update -p gpui-component --precise <rev>` then `cargo update -p gpui --precise <zed sha from gpui-component's Cargo.lock at that rev>`"). Run the resolve commands.
**Verify**: one gpui; `cargo tree -p gpui -e features -i gpui 2>/dev/null | head -3` shows `profiler` (expected, note it in README).

### Step 2: Root + TitleBar + system theme (`src/lib.rs`)
`.with_assets(gpui_component_assets::Assets)`; `gpui_component::init(cx)` right after `set_app_identity`; `titlebar: Some(gpui_component::TitleBar::title_bar_options())`; wrap `Daku::new` in `Root::new`; replace the `configure_sidebar_material` call with `gpui_component::Theme::sync_system_appearance(Some(window), cx)`; also observe appearance changes: inside the open_window closure, `window.observe_window_appearance(|window, cx| { gpui_component::Theme::sync_system_appearance(Some(window), cx); }).detach();` (or the equivalent GPUI API — check `gpui::Window` for `observe_window_appearance`; if absent, STOP and report). Set `window_background: WindowBackgroundAppearance::Opaque` (the Blurred value is inert once surfaces are opaque). Drop the `crate::theme::init(cx)` call once Step 4 deletes the module.
**Verify**: `cargo build -p daku` → exit 0.

### Step 3: Sidebar + shell on library tokens (`src/app.rs`)
`render`: column [ `TitleBar::new().child(div().text_sm().child("daku"))`, `Alert`-style disconnected banner if `!state.connected()` (a plain `div` on `cx.theme().danger`/`muted` tokens is fine), `h_flex()[ sidebar, detail ]` ]. `render_sidebar` → `Sidebar` (Environments group only — delete `section_label`, `platform_row`, `environment_row`, `health_dot`; keep the health dot as the item `.suffix`; header `SidebarHeader::new().child("ServiceNow")` or the app name — pick one, no icon needed). Convert every remaining `theme.*` read in `render_detail`, `signal_card`, `health_badge`/`reachability_badge`/`badge`, `status_dot`, `compare_strip`, `sparkline`, `disconnected_banner` to `cx.theme()` tokens (mapping: canvas→background, raised→secondary, border→border, text→foreground, text_secondary/tertiary/ghost→muted_foreground, success/warning/danger→same, accent→accent, danger_soft→danger with `.opacity(0.15)`), then delete `src/theme.rs` and its `mod theme;`. Keep the pill badges as our own `div`s for now (Badge is an overlay; plan 045 owns the badge design). Selection state stays in `DashboardState`.
**Verify**: `cargo build -p daku` → 0 errors; `grep -n 'theme::Theme\|crate::theme' src/*.rs` → 0.

### Step 4: Delete the vibrancy layer and the empty asset source
Delete `configure_sidebar_material` (+ non-macOS stub) from `src/platform.rs` and its imports (`NSVisualEffect*` etc.); if `SIDEBAR_WIDTH` is now only read by `app.rs`, make it private. Delete `src/assets.rs` and `mod assets;`.
**Verify**: `cargo clippy --workspace --all-targets -- -D warnings` → 0 warnings.

### Step 5: Docs
README pin note → the two-command bump from ADR-0008; one sentence that `gpui` gains `profiler`/`runtime_shaders` via gpui-component. Update the "Toolchain" mention if the cold build is now longer (say "first build compiles GPUI + gpui-component, expect several minutes").
**Verify**: `grep -n 'rev' README.md` shows the new procedure and no "both lines" wording.

### Step 6: Gate + fixture launch
`bun run check` → 0. Launch the fixture command; confirm alive after 10 s, no stderr; capture `screencapture -x -D 1 /tmp/claude-501/044.png` if possible.

## Test plan
No new model tests (model unchanged). `cargo test -p daku` must stay green (18 dashboard_state tests). Manual: fixture launch + PDI launch by the Operator (dark and light system appearance both render; traffic lights not overlapped; sidebar shows Environments only).

## Done criteria
- [ ] `bun run check` exits 0 (fmt + clippy -D warnings + tests + oxlint)
- [ ] `grep -c 'name = "gpui"' Cargo.lock` → 1; `grep -c 'rev = ' Cargo.toml` → 2 (the two gpui-component lines only)
- [ ] `ls src/theme.rs src/assets.rs` → both gone; `grep -rn 'NSVisualEffect' src` → 0
- [ ] `grep -n 'gpui_component::init\|Root::new\|title_bar_options\|sync_system_appearance' src/lib.rs` → 4 hits
- [ ] `grep -n 'Platforms' src/app.rs` → 0
- [ ] fixture launch alive after 10 s with empty stderr
- [ ] `plans/README.md` status row updated

## STOP conditions
- Two `gpui` entries remain in Cargo.lock after Step 1 (report the sources).
- gpui-component at rev `972a3eb` fails to compile against zed `e0931d5` (report the first error; do not bump revs on your own).
- `Root::update` panics at runtime ("window first layer should be a gpui_component::Root") — a second window exists somewhere; report where.
- `observe_window_appearance` (or equivalent) does not exist on the pinned gpui.

## Maintenance notes
- Bumping: `cargo update -p gpui-component --precise <rev>` (also `-p gpui-component-assets`), then `cargo update -p gpui --precise <sha from their Cargo.lock>`; run the gate; launch the fixture.
- Any new asset daku ships must be merged into one `AssetSource` with the Lucide bundle (single `with_assets` slot).
