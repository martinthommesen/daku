# GPUI shell on gpui-component

The desktop shell is built from [gpui-component](https://github.com/longbridge/gpui-component) (Apache-2.0): `gpui_component::init` + `Root` at the window root, its `TitleBar`, `Sidebar`, theme tokens (`cx.theme()`, JSON light/dark themes following system appearance) and Lucide icons via `gpui-component-assets`. The hand-rolled waku shell (`src/theme.rs` palette, custom sidebar/titlebar drag, `NSVisualEffectView` sidebar vibrancy) is deleted, not styled. ADR-0005’s layout stays; ADR-0001 still holds (native GPUI, macOS-only). Window chrome is the library `TitleBar` (traffic lights cleared by construction) with the sidebar below it, not a full-height sidebar.

**Pin discipline.** gpui-component `main` depends on `gpui = { git = zed }` with no rev and moves daily; crates.io releases lag by months and require a registry `gpui` we cannot use. Cargo treats `git+zed?rev=X` and `git+zed` as different sources, so daku's `gpui`/`gpui_platform` lines carry **no `rev`**; the zed commit is pinned only in `Cargo.lock` (`cargo update -p gpui --precise <sha>` — one command cascades all zed crates), and `gpui-component`/`gpui-component-assets` are pinned by `rev`. Bumps move both together, taking the zed sha from gpui-component's own lockfile at the new rev. This supersedes plan 016's "rev on both lines" note.

Accepted costs: +~140 crates and a slower cold build; `gpui` gains the `profiler` feature and `gpui_platform` gains `runtime_shaders` through feature unification; the app's only `AssetSource` is the library's icon bundle (daku ships no assets of its own — plan 030); no sparkline in the library, so the ~30-line custom one stays.

## Considered options

- **Keep hand-rolling on raw GPUI** — the shell was rough and every widget (badge, table, tooltip, drill-in) would be bespoke; rejected.
- **Style the library to look native-macOS** (vibrancy sidebar, full-height sidebar, inline traffic lights) — more AppKit coupling for a look nobody asked for; revisit only if the default chrome looks wrong on macOS.
- **crates.io `gpui-component 0.5.1`** — pins registry `gpui 0.2.2` (Oct 2025, no `gpui_platform`); unusable with daku's git gpui.
- **Fork gpui-component to add rev pins** — keeps plan 016 literally at the cost of a fork; the `Cargo.lock` pin gives the same reproducibility.
