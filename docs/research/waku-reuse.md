# What in waku is reusable for daku

Research note for issue #4 (map: #1). Question: which parts of
[egoist/waku](https://github.com/egoist/waku) can daku inherit for a monitoring
dashboard whose first domain is ServiceNow prod/test/dev.

Source of truth: shallow clone of `egoist/waku` at commit
`4c483bc282faf4ce9296390887f09b44abb34f27` (2026-08-17), read locally at
`/tmp/waku-ref`. Paths below are relative to that checkout. LOC figures are
`wc -l` over `*.rs *.ts *.tsx *.css *.sql`, excluding `node_modules`, build
output and `packages/waku-client/src/generated`.

## Repo shape (LOC)

| Area | LOC | What it is |
|---|---:|---|
| `src/` | 62,559 | GPUI native desktop (`src/app/` 38.9k, `src/md/` 5.6k, `src/ui/` 2.7k, top-level files 15k) |
| `crates/waku-core` | 36,923 | Daemon implementation: provider drivers, SQLite persistence, workspace/git, WS server |
| `crates/waku-protocol` | 7,076 | Wire types (`model.rs` alone 4,342) + `bin/export_types.rs` (ts-rs) |
| `crates/waku-client` | 2,848 | Rust WS client + daemon process supervisor + desktop prefs |
| `crates/waku-daemon` | 180 | `main.rs`: arg parsing, token from env, bind, `waku_core::serve` |
| `apps/web` | 21,316 | React/TanStack Start browser client (incl. ~2,140 LOC of tests) |
| `packages/waku-client` | 642 (+686 generated) | TS `WakuClient` + generated types |
| `website` | 1,144 | Marketing site (Geist lives here, not in the app) |
| `scripts` | 2,537 | `dev.ts` watcher, `bundle.sh` (mac sign), `release.ts`, `appcast.ts`, `seed-mock-sessions.ts` |
| `db` | 138 | `schema.ts` + 3 drizzle migrations |
| `locales` | 4,073 (yml) | rust-i18n catalog shared by native + protocol; web has its own `lib/i18n*.ts` |

Total ≈ 135k LOC. Roughly 100k of it is coding-agent domain (transcript,
composer, providers, git, terminal, browser, skills, usage).

## 1. The two UIs

### 1a. Native GPUI desktop (`src/`)

Structure. One god-object `pub struct Waku` (`src/app.rs:1019`, 249 fields)
with `impl Waku` blocks spread across `src/app/*.rs`. There is no component
tree to lift; "the sidebar" is `Waku::render_sidebar` (`src/app/sidebar.rs:664`),
"settings" is `Waku::render_settings` (`src/app/settings.rs:100`), etc.
Reusing the shell means extracting methods off that struct, not copying files.

Shell layout (`src/app/render.rs:260-420`): root `div().flex()` →
`[sidebar_pane (cached, slide-tweened width)] [main column: header, transcript
or empty state, permission strip, queued messages, composer, workspace footer,
toast overlay] [right_panel_pane (absolute, pinned right)]`, then command
palette / commit dialog / image preview layered on top, all wrapped in
`render_window_frame` (`src/app/window_chrome.rs`, 374 LOC — traffic-light
inset, drag region). Panels resize via `render_panel_resize_handle`
(`render.rs:10`) and animate via `motion::WidthTween` (`src/ui/motion.rs:217`).
Sidebar is a virtualized `list()` of `SidebarRow`s grouped by date
(Today/Yesterday/This week/...; `sidebar.rs:22`), with inline rename,
context menus, updater button and footer.

What makes it feel the way it does:
- Colour: `src/theme.rs` (219 LOC). Two hand-tuned palettes, "neutral graphite
  in the spirit of Cursor — colour reserved for meaning" (comment at
  `theme.rs:24`). Dark canvas `#1A1A1A`, raised `#232323`, text `#E2E2E2`,
  brand coral accent `#E2795B`, 6% neutral overlays for hover/selected. Light:
  canvas `#F6F5F6`, text `#242424`, accent `#C85F44`. Published as a GPUI
  `Global` (`ActiveWakuTheme`) and read via `Theme::current(cx)`. On macOS the
  sidebar is `transparent_black()` over native NSVisualEffectView vibrancy
  (`src/platform.rs`, `configure_sidebar_material`).
- Typography: **not Geist**. UI font is `.SystemUIFont` (`render.rs:300`);
  bundled fonts are only JetBrains Mono + Nerd symbols (`assets/fonts/`).
  Sizes are small (11.5–13.5px), line-heights explicit.
- Icons: 86 monochrome SVGs in `assets/icons/` tinted through `ui::icon()`
  (`src/ui/mod.rs:17`); polychrome file-type icons via `img()`.
- Motion: pulse clock, spinners, width tweens (`src/ui/motion.rs`); honours
  reduce-motion (AGENTS.md "Accessibility").
- Command palette (`src/app/command_palette.rs`, 1,359 LOC): Cmd/Ctrl-K,
  `nucleo-matcher` fuzzy scoring, sections Suggested/Tasks/Commands/Settings,
  debounced background SQLite search for transcript matches. The list/keys
  scaffold is generic; item sources are all sessions/tasks.
- Settings (`src/app/settings.rs`, 2,522 LOC): full-window page with its own
  sidebar; pages General, Appearance, Daemon, Providers, Skills, Computer Use.
  General/Appearance/Daemon (~1,200 LOC) are generic; the rest is agent domain.
- Primitives (`src/ui/`, 2,741 LOC): menus/popovers/context menus
  (`menu.rs` 1,391), overlay scrollbar, tooltip, text field, `icon_button`.
  Genuinely reusable.
- Markdown renderer for GPUI (`src/md/`, 5,593 LOC) and the text-editing
  widget (`src/input.rs`, 3,177 LOC) are generic infrastructure a GPUI app
  would otherwise have to write.
- Charts exist natively: `src/app/usage_page.rs` (2,970 LOC) draws area/line
  plots and meters with `canvas` + `PathBuilder` (`usage_page.rs:689-939`).
  That is direct precedent for dashboard tiles in GPUI.

Domain-specific vs generic (native): generic shell + infra ≈ 13–15k LOC
(theme, ui/, md/, input.rs, platform.rs, assets.rs, render.rs, window_chrome,
command palette scaffold, generic settings pages, updater, analytics, i18n
plumbing, usage chart drawing). The remaining ~48k (composer 3.6k, transcript
~4k, right_panel 4.6k, runtime 3.4k, terminal 2k, browser 1.6k, js_repl 1.3k,
review_diff, sessions, skills, file_search, commit_dialog, branches, drafts,
autocomplete, streaming, driver/, query.rs, computer_use) is coding-agent.

### 1b. Browser client (`apps/web`)

Structure. TanStack Start (file routes `src/routes/__root.tsx`, `index.tsx`,
`settings.$page.tsx`, `settings.index.tsx`) + React 19 + React Compiler +
Tailwind v4 + shadcn tokens + `@base-ui/react` (menus, popover, context menu,
dialog) + `sonner` toasts + `react-virtuoso` lists + `@tanstack/react-query`
for daemon data. Entry: `WakuShell` (`src/components/waku-shell.tsx`) shows
`ConnectionPanel` until `useDaemon().phase === 'connected'`, then lazy-loads
`WakuApp` (`src/components/waku-app.tsx`, 1,643 LOC).

Shell layout (`waku-app.tsx:936-1100`): `<div class="flex h-dvh">
[<Sidebar>] <main class="flex flex-col"> <TaskHeader/> {NewTaskCanvas |
Transcript + Composer} </main> [<RightPanel>]</div>`, `<CommandPalette>` and
`<CommitDialog>` portalled. Sidebar (`components/sidebar.tsx`, 475 LOC) is a
`Virtuoso` list of grouped rows with the same date groups as native, resize
handle (`panel-resize-handle.tsx`, 103 LOC), mobile drawer.

What makes it feel the way it does:
- Colour: `src/styles.css` (389 LOC). shadcn CSS-variable tokens
  (`--background`, `--sidebar`, `--raised`, `--inset`, `--text-secondary/
  tertiary/ghost`, `--success/--warning/--danger-soft`, `--code-text/wash`)
  with **the same hex values as `src/theme.rs`** (`#1a1a1a`, `#232323`,
  `#e2795b`, `#f6f5f6`, ...). Dark via `.dark` class + `prefers-color-scheme`
  fallback; theme + language decided by an inline `<script>` in `__root.tsx`
  before paint (`localStorage 'waku.theme'`).
- Typography: **not Geist either**. `--font-sans: -apple-system,
  BlinkMacSystemFont, 'SF Pro Text', 'Segoe UI'`; `--font-mono: ui-monospace,
  SFMono-Regular, Menlo…`; base `font-size: 14px`. `@fontsource-variable/geist`
  is a dependency in `apps/web/package.json` but is only `@import`ed in
  `website/src/styles.css:4`. Menu items are 11.5px/15px; sidebar 12.5px.
- Chrome: `.waku-menu-surface`, `.waku-popover-surface`, `.waku-menu-item`
  in `styles.css` — 9px radius, 1px `--input` border, `--raised` bg, soft
  shadow. Icons are an inline SVG set in `components/waku-icon.tsx` (386 LOC).
- Command palette (`components/command-palette.tsx`, 456 LOC): custom
  listbox (no cmdk), fuzzy score in `lib/palette-search.ts`, sections mirror
  native. Settings (`components/settings-view.tsx`, 597 LOC): pages general,
  appearance, providers, skills, usage, daemon; `SETTINGS_PAGES` metadata
  drives palette + routing.
- Charts: `components/usage-chart.tsx` (152 LOC) with `@tanstack/charts`
  (`areaY`, `lineY`, `crosshair`, `tooltip`) + `d3-scale`.
- Reduced motion honoured globally (`styles.css` `@media (prefers-reduced-motion)`).
- Deploy: `wrangler.jsonc` (Worker serves the SSR app only; the daemon token
  never touches the Worker — `apps/web/README.md`), `vite.config.ts` with
  `@cloudflare/vite-plugin`, `tanstackStart()`, `tailwindcss()`,
  babel React Compiler.

Domain-specific vs generic (web): generic ≈ 3–3.5k LOC (styles.css, `ui/`
five shadcn primitives, waku-icon (swap glyphs), connection-panel +
`lib/connection.ts` + `lib/daemon-context.tsx` (~400), appearance/platform/
i18n libs (~300), sidebar scaffold, command palette scaffold, settings shell,
panel resize handle, routes, waku-shell/startup-screen, usage-chart, vite/
wrangler/tailwind config). Domain ≈ 17–18k (transcript 1,955, composer 1,982,
right-panel 1,726, waku-app most of 1,643, runtime-context 1,474, event-reducer,
daemon-api 714, skills, model-picker, commit-dialog, daemon-file-picker,
code-surfaces, and their tests). 41 of 53 non-test source files import
`@waku/client` types.

## 2. Daemon / `waku-protocol` / `waku-client` split

What it provides (all in `crates/waku-protocol/src/protocol.rs`, 564 LOC):
- Versioned envelope: `PROTOCOL_VERSION = 3`, `MAX_WIRE_MESSAGE_BYTES` 48 MiB.
- Auth handshake: `ClientMessage::Hello { protocol_version, token, client_id,
  resume_from: Vec<ReplayCursor> }` → `ServerMessage::Hello | Rejected`.
  Server side (`crates/waku-core/src/server.rs`): bearer token compared with
  `subtle::ConstantTimeEq`, browser `Origin` allow-list checked at the WS
  upgrade (`validate_handshake`, `server.rs:638-654`; empty set = native
  clients only), `MAX_CONNECTIONS = 64`, non-loopback bind gated by
  `--allow-non-loopback` (`crates/waku-daemon/src/main.rs`). Token comes from
  `WAKU_DAEMON_TOKEN` env and is scrubbed before spawning children.
- Request/response: `Request { request_id, session_id, runtime_id, command }`
  → `ServerMessage::Response { request_id, outcome: Ok{payload} | Error{RpcError} }`.
- Subscriptions + sequence dedup + replay: `SequencedEvent { session_id,
  runtime_id, epoch, sequence, event }`; the Hub keeps a per-runtime journal
  (`MAX_REPLAY_EVENTS_PER_SESSION = 4096`) and a cached-response ring
  (`MAX_CACHED_RESPONSES = 2048`) so a reconnecting client passing
  `resume_from` cursors gets missed events; `epoch` changes on daemon restart
  so sequences cannot be confused. `EventSink::send_ephemeral` skips the
  journal for PTY-style firehoses. `TaskStateChanged { revision }` is a
  coarse "invalidate your snapshot" signal for multi-client edits.
- Both clients implement the same state machine: Rust `DaemonClient`
  (`crates/waku-client/src/client.rs`, 435 LOC; tungstenite, thread + channels)
  and TS `WakuClient` (`packages/waku-client/src/client.ts`, 372 LOC; pending
  map, per-runtime listener sets, buffered events, `LastSequence` per
  `(session, runtime)`, `replayCursors()` on reconnect).
- Generated TS types: `crates/waku-protocol/src/bin/export_types.rs` runs
  ts-rs `export_all` for `ClientMessage`, `ServerMessage`, `DaemonReady`,
  writes `packages/waku-client/src/generated/*.ts` + `constants.ts`;
  `bun run protocol:generate` / `protocol:check` (`package.json`).
- Process supervision: `crates/waku-client/src/process.rs` (643 LOC) spawns
  the daemon, reads `DaemonReady` JSON from stdout, monitors it, hot-swaps
  the debug daemon, and manages "exposure" (fixed port, origins, token
  regeneration) that Settings → Daemon edits.
- `Backend` trait (`server.rs:53`): `fn handle(&self, Request, EventSink) ->
  Result<ResponsePayload>` — the transport is one trait away from the domain.
  `WakuBackend` (`crates/waku-core/src/daemon.rs`, 1,926 LOC) is the domain.

Cost of keeping the split with a different domain:
- The envelope is generic and small (~150 LOC of `protocol.rs` once `Command`
  and `ResponsePayload` are emptied, ~2k LOC `server.rs`, ~1.1k Rust client,
  ~630 TS client incl. tests, ~140 LOC exporter). But the Hub in `server.rs`
  is not fully domain-free: `HubState` keeps `catalog_projects` /
  `catalog_sessions` typed as `Project` / `AgentSession` /
  `ProviderKind` / `SessionStatus` (`server.rs:20, 82-130`) to diff and emit
  `TaskStateChanged`. Expect ~300 LOC of surgery there.
- Every payload type is coding-agent: `Command` has ~45 variants (Start,
  Prompt, Steer, Fork, Rollback, ProbeProvider, LoadSkills, OpenTerminal…),
  `ResponsePayload` ~25. `model.rs` (4,342 LOC) is `AgentSession`, turns,
  activities, provider probes. All of it gets replaced with e.g.
  `Command::Subscribe { instance }`, `Command::LoadSnapshot`,
  `ResponsePayload::InstanceHealth {..}`.
- Session/runtime addressing (`session_id`, `runtime_id` UUID pair on every
  request and event) is agent-shaped; for monitoring the natural key is
  `(instance, signal)`. Either alias it or rename — renaming touches both
  clients and the Hub.
- The rust-i18n catalog is compiled into `waku-protocol` (`lib.rs:9-18`, reads
  `../../locales/*.yml`); drop it or keep the 3 yml files.
- Net: 2–4 days to hollow out and re-type, then the pipeline (Rust types →
  `bun run protocol:generate` → typed TS client) works unchanged. The Rust
  daemon must exist for this to pay off; a Bun/TS-only backend makes ts-rs
  moot.

## 3. Data layer

- Schema authored in TypeScript: `db/schema.ts` (drizzle `sqliteTable`:
  `projects`, `sessions`, `messages`, `session_details`; narrow list rows,
  wide JSON detail row split out on purpose — see file header comment).
- `bun run db:generate` (`drizzle-kit generate`, `drizzle.config.ts`,
  `migrations: { prefix: "index" }`) writes plain SQL to `db/migrations/
  000N_*.sql`. drizzle-orm never ships in the binary (`schema.ts:4-8`).
- `crates/waku-core/build.rs` embeds every `*.sql` in prefix order into a
  `MIGRATIONS: &[(&str,&str)]` table; `persistence::apply_migrations`
  (`crates/waku-core/src/persistence.rs:802`) creates a `migrations(tag,
  applied_at)` table and applies each unapplied file in its own transaction.
- SQLite via `rusqlite` bundled (`waku-core/Cargo.toml`), WAL +
  `synchronous=NORMAL` (`persistence.rs:958-961`); read-only side connections
  for search (`persistence.rs:723-727`).
- Ownership: the **daemon** owns `app.db` (`StateStore::daemon`,
  `waku-daemon/src/main.rs:52`); default path is `dirs::data_local_dir()/
  <DATA_DIRECTORY_NAME>/app.db` in release, `<repo>/temp/app.db` in debug
  (`persistence.rs:870-884`). Desktop keeps only prefs (`~/.waku/app.json`,
  `crates/waku-client/src/persistence.rs`) and proxies task state over RPC.
  Daemon settings live in `~/.waku/settings.json` (`DaemonSettingsStore`).
- Reuse: the pipeline (schema.ts → SQL → build.rs → apply_migrations) is
  ~250 LOC and domain-free. The schema itself is replaced wholesale.

## 4. Build / tooling

- Bun workspace (`package.json`: `website`, `apps/*`, `packages/*`); scripts
  `dev` (`scripts/dev.ts` — cargo watcher that rebuilds+signs `Waku Debug.app`
  and hot-swaps `waku-debug-daemon`), `release`, `protocol:generate|check`,
  `db:generate|push`.
- Cargo workspace (`Cargo.toml`): root crate `waku` (desktop) + 4 crates,
  edition 2024, Rust ≥ 1.96 (README). `profile.dev` opt-level 1 / deps 2.
- GPUI pin: `gpui`/`gpui_platform` from `git = "https://github.com/egoist/zed",
  branch = "waku-webview"` (lock rev `f9bad894…`); comment says it is upstream
  main + zed PR #61945 (layered scene rendering) so menus can composite above
  the WKWebView browser pane. Without the embedded browser, upstream
  `zed-industries/zed` gpui suffices. Also `[patch.crates-io] block` fork.
  macOS deps: `wry`, `objc2-*` (webview, notifications, vibrancy); Linux needs
  Vulkan + Wayland/X11 (CONTRIBUTING.md). No Windows.
- Release: `scripts/bundle.sh` (mac bundle, Sparkle framework, signing),
  `scripts/bundle-linux.sh`, `scripts/release.ts` + `appcast.ts` (Sparkle
  appcast), `src/updater.rs` (850 LOC, loads Sparkle at runtime).
- Web: `apps/web/package.json` `dev`/`build`/`deploy` (`vite build && wrangler
  deploy`), `wrangler.jsonc` (`nodejs_compat`, observability on, TanStack
  Start server entry). Tests: `bun test` (`*.test.ts` colocated).

## 5. Fork surface (rough)

Numbers are "keep" = code you'd retain and adapt; "delete" = remove on day 1.
Everything kept still needs domain re-typing.

| Option | Keep (≈LOC) | Delete (≈LOC) | Notes |
|---|---:|---:|---|
| **A. Native GPUI only** | 17–18k: `src/` shell+infra 13–15k, protocol envelope 0.6k, Rust client+supervisor 1.1k, `server.rs`+migration runner ~2k, daemon 0.2k, `scripts/dev.ts`+`bundle*.sh` ~0.5k, db pipeline | ~115k: `src/` domain ~48k, `waku-core` domain ~35k, all of `apps/web`+`packages/waku-client`+`website` ~24k, `locales` | Must un-god-object `Waku` (249 fields). Keep macOS/Linux, no Windows; drop the egoist/zed fork if no webview; Sparkle/signing only if distributed. |
| **B. Web client only** | ~7–8k: `apps/web` shell 3–3.5k, TS client 0.6k, protocol envelope 0.6k, `server.rs`+migration runner ~2k, daemon 0.2k, db pipeline, wrangler/vite config | ~125k: all of `src/` 62.5k, `waku-core` domain ~35k, `apps/web` domain ~18k, Rust `waku-client` (or keep `process.rs` if a desktop supervisor is wanted later), `website` | Rust daemon stays (needed for ts-rs to matter). Runs anywhere with a browser; deploy = one Worker + one daemon per site. |
| **C. Both** | ~25k (A ∪ B) | ~105k | Two UI taxes; the shared token values (theme.rs ↔ styles.css) mean the look stays in sync cheaply, but every feature is built twice. |

Either way the daemon core to keep is: `server.rs` (Hub, handshake, replay),
`protocol.rs` envelope, `export_types.rs`, `build.rs` + `apply_migrations`,
`waku-daemon/src/main.rs`. Everything else in `waku-core` (drivers, git,
checkpoints, skills, usage, workspace, terminal) goes.

## 6. Licence: GPL-3.0-only

`LICENSE` is GPLv3 verbatim; every crate declares `license = "GPL-3.0-only"`
(`Cargo.toml`, `crates/*/Cargo.toml`). "only" means the "or any later
version" option (LICENSE §14) is not granted; daku's derived work stays on v3.

- Internal tool (used only inside the organisation, including served to
  colleagues over the network): no source-disclosure obligation. GPLv3 §0:
  "Mere interaction with a user through a computer network, with no transfer
  of a copy, is not conveying." FSF GPL FAQ `#InternalDistribution`: making and
  using copies within one organisation is not distribution; `#UnreleasedMods`
  and `#GPLRequireSourcePostedPublic`: a company may run a modified version
  internally without releasing sources (that changes only under AGPL, which
  waku is not; GPLv3 §13 applies only if AGPL code is combined in). Caveat
  from `#InternalDistribution`: giving copies to contractors for off-site use
  is distribution.
- Distributed tool (binaries or the web bundle conveyed to other
  organisations, customers, or the public): the whole derived work must be
  licensed GPL-3.0 (§5c), with corresponding source (§6); no relicensing to
  MIT/proprietary; modification notices required (§5a). Note the FAQ's
  JavaScript point (`#UnreleasedMods`): a web page that ships GPL'd JS to a
  visitor's browser is conveying that JS, so an externally reachable Cloudflare
  deployment of a forked `apps/web` puts the client bundle under GPL source
  obligations even if the daemon never leaves the building.
- Practical: for an internal ServiceNow dashboard, GPL costs nothing legally;
  keep `LICENSE` and copyright notices in the fork. If daku might ever be
  offered outside the org (or open-sourced under a permissive licence), do
  not build on waku's code — take only ideas/tokens, which are not
  copyrightable expression to the same degree. Corporate policy may still
  restrict GPL code in-house; flag to whoever owns OSS policy.

## Recommendation (monitoring dashboard, first domain ServiceNow prod/test/dev)

Inherit **Option B: the web client shell + the daemon/protocol skeleton**;
do not inherit the GPUI desktop.

Why:
- A monitoring dashboard is read-mostly, poll-driven, multi-viewer, and wants
  to live on any laptop and a wall display. Browser delivery (one Worker + one
  daemon) fits; a native app that is macOS/Linux-only, needs Vulkan/Metal, has
  no screen-reader tree yet (AGENTS.md "Accessibility"), and pins a personal
  fork of zed's GPUI is the wrong cost curve for a ServiceNow-shop audience.
- ~90% of `src/` is transcript/composer/terminal/browser; the reusable native
  parts (theme, ui primitives, md renderer, panel shell) sit on a 249-field
  struct and need extraction before they are useful. The web shell's reusable
  parts are already files (`styles.css`, `ui/`, `sidebar.tsx` scaffold,
  `command-palette.tsx`, `settings-view.tsx`, `connection-panel.tsx`,
  `daemon-context.tsx`, `usage-chart.tsx`).
- The daemon split maps cleanly: the daemon becomes the ServiceNow collector
  (owns credentials, polling, SQLite history), clients subscribe per
  `(instance, signal)` and get replay after reconnect for free. The ts-rs
  pipeline keeps the browser typed against Rust. Keep `server.rs`,
  `protocol.rs` envelope, `export_types.rs`, `build.rs`/`apply_migrations`,
  `waku-daemon/main.rs`, `packages/waku-client/src/client.ts`; rewrite
  `Command`/`ResponsePayload`/`model.rs` and `db/schema.ts` for instances,
  checks, samples, incidents.
- Keep the visual language (the graphite palette and 11.5–14px type scale are
  identical in `theme.rs` and `styles.css`), so a native GPUI client can be
  added later without a redesign — `crates/waku-client/src/client.rs` +
  `process.rs` (1.1k LOC) are the transport it would need.
- Licence: fine for internal use; if external distribution is on the table,
  decide before writing code on top of waku.

Open question for the parent decision: whether the first iteration even needs
the Rust daemon, or whether a Bun/TS server that polls ServiceNow and serves
the same web shell is enough. Waku's protocol machinery only earns its keep if
Rust stays on the server side.
