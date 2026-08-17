# Waku partial-fork inventory for daku

Research note for issue [#18](https://github.com/martinthommesen/daku/issues/18) (map: [#17](https://github.com/martinthommesen/daku/issues/17)). Answers ADR-0003: which commit of [egoist/waku](https://github.com/egoist/waku) to pin, and exactly which paths/crates to copy vs strip for a one-time inheritance of the GPUI client + Rust daemon/protocol + SQLite migration pipeline.

**Product override vs earlier research:** [`waku-reuse.md`](./waku-reuse.md) recommended the web shell. ADR-0001 / ADR-0003 / `docs/spec/v1.md` lock **native GPUI + Rust daemon, macOS-only**; this inventory follows that lock.

Primary evidence: a local clone of egoist/waku at the pinned SHA below (same tip as the shallow checkout used for `waku-reuse`). Paths are relative to that tree.

---

## 1. Pinned commit

| Field | Value |
|---|---|
| Repo | `https://github.com/egoist/waku` |
| **SHA** | **`4c483bc282faf4ce9296390887f09b44abb34f27`** |
| Date | **2026-08-17** (`2026-08-17 16:30:14 +0800`) |
| Subject | `tweaks` |

Pin this SHA (and its `Cargo.lock`) for the one-time copy. Do **not** track upstream after the copy; treat waku as a reference only (ADR-0003).

---

## 2. Known dependency pins (copy with the tree)

| Pin | Value | Source |
|---|---|---|
| Rust | **≥ 1.96**, edition **2024** | `README.md`, root `Cargo.toml` |
| GPUI / platform | `git = "https://github.com/egoist/zed"`, **branch `waku-webview`** | root `Cargo.toml` |
| GPUI lock rev | **`f9bad8941ea813982d6dfb10c0377ebf7716b3e7`** | `Cargo.lock` (`gpui`, `gpui_platform`, …) |
| Why that fork | Upstream Zed main + [zed PR #61945](https://github.com/zed-industries/zed/pull/61945) (layered scene rendering) so menus composite above a **WKWebView** child | comment above `gpui` in `Cargo.toml` |
| `block` crates.io patch | `git = "https://github.com/Dicklesworthstone/rust-block"`, rev **`b39ae859d1ee8e8cb5eef6a516471f1578d26b96`** | `[patch.crates-io]` in `Cargo.toml` |
| Licence | **GPL-3.0-only** on every crate | `LICENSE`, each `Cargo.toml` |

**daku decision on the egoist/zed fork:** v1 has no embedded browser (`src/browser.rs` / `wry` / WKWebView are agent-domain). After strip, prefer switching `gpui` / `gpui_platform` to **upstream `zed-industries/zed`** (or a rev that still builds) and drop macOS `wry` / `objc2-web-kit` deps. Keep the egoist pin **only** if a first green build against upstream fails; document that choice in the foundation plan.

---

## 3. Build prerequisites

| Tool | Required? | Why |
|---|---|---|
| **Rust ≥ 1.96** | Yes | Workspace edition 2024; README / CONTRIBUTING |
| **Bun** | Yes (dev / schema) | `bun run db:generate` (`drizzle-kit`), `scripts/dev.ts` watcher, root `package.json` scripts |
| **Xcode / clang** (macOS) | Yes for signed Debug.app | `scripts/dev.ts` + `scripts/bundle.sh` sign `Waku Debug.app` |
| Linux Vulkan / Wayland / X11 | **No for daku v1** | Upstream supports Linux; ADR-0001 is **macOS-only** |
| Windows | No | Upstream has none; daku neither |
| Agent CLIs | No | Strip entirely |

Minimal post-fork loop (adapted names):

```sh
bun install
bun run db:generate          # after rewriting db/schema.ts
cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client
bun run dev                  # after renaming scripts/dev.ts targets
```

`drizzle-orm` / `drizzle-kit` are **build-time only**; they never ship in the binary (`db/schema.ts` header). Rust applies embedded SQL via `crates/*/build.rs` → `persistence::apply_migrations`.

---

## 4. Copy into daku (keep / adapt)

Copy these paths from the pinned SHA, then rename crates/strings (section 6). Everything kept still needs domain re-typing for Environments / Signals.

### 4.1 Workspace / toolchain scaffold

| Path | Role |
|---|---|
| `Cargo.toml`, `Cargo.lock` | Workspace members + GPUI / patch pins (adapt package names) |
| `LICENSE` | GPL-3.0-only |
| `package.json` (slim) | Keep `db:generate` / `db:push`; drop `website` / `apps/*` / `packages/*` workspaces |
| `drizzle.config.ts` | SQLite + `migrations: { prefix: "index" }` |
| `db/schema.ts` | **Replace tables** (pipeline shape only) |
| `db/migrations/` | **Replace** `000N_*.sql`; keep empty starter or regenerate |
| `scripts/dev.ts` | Cargo watcher / Debug.app hot-swap (rename binary paths) |
| `scripts/bundle.sh` | macOS sign + Sparkle layout (packaging plan later) |
| `scripts/delete-debug-app.ts` | Dev hygiene |
| `.cargo/` (if present) | Config as needed |
| `CONTRIBUTING.md` / `README.md` | Rewrite for daku; do not keep agent-CLI docs |

Optional later (packaging plan, not day-1): `scripts/release.ts`, `scripts/appcast.ts`, `src/updater.rs`, Sparkle bits in `resources/`.

### 4.2 GPUI desktop (`src/` + assets)

**Copy whole trees, then delete agent modules listed in §5:**

| Path | Keep why |
|---|---|
| `src/main.rs`, `src/lib.rs`, `src/app.rs` | Entry + god-object shell (must be hollowed / renamed) |
| `src/theme.rs` | Graphite palette / `ActiveWakuTheme` → daku theme |
| `src/assets.rs` | Icon / font loading |
| `src/platform.rs` | macOS vibrancy / window chrome hooks |
| `src/input.rs` | Text-editing widget (~3.2k LOC) |
| `src/daemon.rs` | Desktop ↔ daemon connection glue |
| `src/ui/` | Menus, motion, scrollbar, tooltip, text field (~2.7k) |
| `src/md/` | Markdown renderer (~5.6k) |
| `src/app/render.rs` | Root layout scaffold |
| `src/app/window_chrome.rs` | Traffic lights / drag region |
| `src/app/sidebar.rs` | List / resize scaffold (strip session rows) |
| `src/app/command_palette.rs` | Cmd-K scaffold (swap item sources) |
| `src/app/settings.rs` | Keep General / Appearance / Daemon pages; strip Providers / Skills / Computer Use |
| `src/app/usage_page.rs` | **Chart drawing** precedent (`canvas` / `PathBuilder`) for Signal tiles — extract drawing helpers; drop agent usage domain |
| `src/app/components.rs` | Shared widgets (audit imports; drop agent-only) |
| `assets/fonts/` | JetBrains Mono + Nerd symbols |
| `assets/icons/` | Monochrome SVG set (subset as needed) |
| `resources/AppIcon.icns`, `AppIconDev.icns`, `Info.plist` | Bundle identity (rebrand) |
| `locales/*.yml` | rust-i18n plumbing — **rewrite strings**; keep file layout |

### 4.3 Crates (copy, then strip modules)

| Crate path | Keep |
|---|---|
| `crates/waku-daemon/` | `main.rs`: bind, token from env, `serve` — rename binary |
| `crates/waku-protocol/` | `src/protocol.rs` envelope (`PROTOCOL_VERSION`, Hello / Rejected, `MAX_WIRE_MESSAGE_BYTES`), `src/bin/export_types.rs` (ts-rs optional for native-only), `src/lib.rs` / `i18n` / `theme` / `settings` / `identity` skeletons |
| `crates/waku-core/` | `src/server.rs` (Hub, handshake, replay, origin allow-list), `src/persistence.rs` **migration runner + `StateStore` path helpers** (carve out of 3.5k mixed file), `build.rs` (embed `db/migrations/*.sql`), `src/settings.rs` / `DaemonSettingsStore`, `src/lib.rs` re-exports of `serve` |
| `crates/waku-client/` | `src/client.rs` (WS client), `src/process.rs` (`DaemonSupervisor`), `src/persistence.rs` (desktop prefs JSON), `src/lib.rs` |

Rough “skeleton” size from prior reuse note: protocol envelope ~0.6k, client+supervisor ~1.1k, `server.rs`+migration runner ~2k+, daemon ~0.2k — plus GPUI shell/infra ~13–15k before domain strip.

---

## 5. Strip / delete (do not inherit)

### 5.1 Top-level trees — delete entirely

| Path | Why |
|---|---|
| `apps/web/` | Browser client; ADR-0001 rejects as v1 UI |
| `packages/waku-client/` | TS client + generated types; native-only v1 |
| `website/` | Marketing |
| `scripts/seed-mock-sessions.ts` | Agent session fixtures |
| `scripts/bundle-linux.sh` | Linux packaging; v1 macOS-only |
| `resources/computer-use/` | Agent computer-use assets |
| `resources/linux/` | Linux desktop entry / icons |
| `src/bin/` (`waku_js_repl.rs`) | Agent REPL |
| `src/driver/` | Agent driver bridge |

### 5.2 GPUI agent domain — delete after copy

| Path | Domain |
|---|---|
| `src/browser.rs` | WKWebView embedded browser |
| `src/terminal.rs` | PTY |
| `src/js_repl.rs`, `src/js_repl_bootstrap.js` | JS REPL |
| `src/computer_use.rs` | Computer use |
| `src/review_diff.rs`, `src/query.rs` | Diff / agent query |
| `src/analytics.rs` | Product analytics (omit unless wanted) |
| `src/app/composer.rs` | Composer |
| `src/app/transcript.rs`, `transcript_view.rs` | Transcript |
| `src/app/right_panel.rs` | Agent right panel |
| `src/app/runtime.rs`, `streaming.rs` | Agent runtime |
| `src/app/sessions.rs`, `drafts.rs`, `autocomplete.rs` | Sessions |
| `src/app/skills_page.rs` | Skills |
| `src/app/commit_dialog.rs`, `branches.rs`, `activity_diff.rs` | Git agent UX |
| `src/app/file_search.rs`, `image_preview.rs` | Agent file/image |
| `src/app/usage_meter.rs` | Agent usage meter (keep chart helpers from `usage_page` only if extracted) |
| `src/app/background_work.rs` | Agent background jobs |

Also drop Cargo deps that only served the browser: `wry`, `objc2-web-kit`, `rquickjs`, `alacritty_terminal` on the desktop crate (and matching core deps once terminal/driver are gone).

### 5.3 `waku-core` modules — delete

Everything under `crates/waku-core/src/` **except** the keep set in §4.3, including:

`driver/`, `*_session.rs`, `amp_session`, `claude_session`, `cursor_session`, `deepseek_*`, `opencode_*`, `grok_session`, `attachments`, `blob_store`, `checkpoint`, `composer_complete`, `computer_use`, `git_branch`, `git_commit`, `model`, `model_catalog`, `projectless`, `skills`, `terminal`, `usage`, `usage_history`, `workspace`, `worktree`, `command_env`, and the thick agent `daemon::WakuBackend` — replace with a daku collector backend that implements the same `Backend` / `EventSink` traits `server::serve` expects (or thin those traits).

`persistence.rs`: **do not delete the file wholesale** — extract `apply_migrations`, WAL pragmas, and path helpers; discard session/message/project SQL.

### 5.4 `waku-protocol` / `waku-client` modules — delete

Protocol: `attachments`, `blob`, `checkpoint`, `composer`, `computer_use`, `driver_wire`, `git`, `model`, `model_catalog`, `projectless`, `provider_session`, `skills`, `usage`, `usage_history`, `workspace` (and matching wire variants inside `model.rs` / `protocol.rs` `Command` / `ResponsePayload`).

Client: `driver`, `composer_complete`, `computer_use`, `workspace_client`, `command_env` (unless reused for daemon spawn env).

---

## 6. How daku should rename crates

| Upstream | daku |
|---|---|
| package / bin `waku` | `daku` |
| `crates/waku-core` | `crates/daku-core` |
| `crates/waku-protocol` | `crates/daku-protocol` |
| `crates/waku-client` | `crates/daku-client` |
| `crates/waku-daemon` | `crates/daku-daemon` (bin e.g. `daku-daemon` / `daku-debug-daemon`) |
| Rust `Waku` / `ActiveWakuTheme` | `Daku` / `ActiveDakuTheme` (or neutral `App` / `Theme`) |
| `APP_NAME` `"Waku"` / `"Waku Debug"` | `"daku"` / `"daku Debug"` |
| `APP_ID` `sh.waku` / `sh.waku.dev` | choose a stable reverse-DNS id (e.g. `app.daku` / `app.daku.dev`) — set once in foundation plan |
| `DATA_DIRECTORY_NAME` `"Waku"` | **`"daku"`** → data under `~/Library/Application Support/daku/` (dirs crate) and prefs under **`~/.daku/`** per ADR-0007 / spec |
| Env vars `WAKU_*` / `DAEMON_*` as published | Prefix `DAKU_` (keep semantic names: token, address, protocol version) |
| Debug.app `Waku Debug.app` | `daku Debug.app` |
| Protocol crate name in ts-rs / export | Only if a web client returns later; v1 can drop `packages/*` |

Mechanical rename pass: `Cargo.toml` workspace members, every `waku_*` path dependency, `rust_i18n` / locale keys, bundle `Info.plist`, and `scripts/dev.ts` binary paths.

---

## 7. Risks

1. **GPUI pin** — Shipping against `egoist/zed` `waku-webview` (`f9bad894…`) couples daku to a personal fork meant for WKWebView layering. After deleting the browser, migrate to upstream Zed GPUI early or accept ongoing pin drift.
2. **macOS-only** — Spec / ADR-0001; upstream Linux paths and `bundle-linux.sh` are out of scope. No Windows.
3. **God-object `Waku`** — `src/app.rs` (~249 fields) with methods spread across `src/app/*.rs`. Reuse is “extract shell methods,” not file-level lift. Budget a foundation plan slice for hollowing the struct before Signal UI.
4. **Mixed persistence** — `persistence.rs` (~3.5k) interleaves migrations with agent tables. Copy carefully; regenerate schema from `db/schema.ts` for Environments / Signal snapshots (ADR-0007).
5. **Protocol rewrite** — Keep envelope + handshake + replay; replace `Command` / `ResponsePayload` / `model.rs` (~4.3k alone) for Signals. Bump or reset `PROTOCOL_VERSION` (currently `3`) for the new domain.
6. **GPL-3.0-only** — Matches ADR-0002 / public repo; keep LICENSE and notices. No permissive relicense of copied code.
7. **Bun still required** — Even without the web app, schema generation and the dev watcher are Bun scripts unless rewritten.
8. **One-time fork** — No upstream merge expectation; cherry-picks are manual reference only.

---

## 8. Suggested copy order (for the foundation plan — not this ticket)

1. Orphan/worktree copy of pinned SHA → rename crates → delete §5 trees.
2. Green `cargo check` with hollow backend + empty GPUI window (theme + chrome only).
3. Re-author `db/schema.ts` + one migration; confirm `build.rs` embed + `apply_migrations`.
4. Re-type protocol envelope payloads; wire `DaemonSupervisor` + Hello handshake.
5. Then collector / Signal work (map #17 slicing: collector-first).

---

## Sources

- Local `egoist/waku` at `4c483bc282faf4ce9296390887f09b44abb34f27` (`Cargo.toml`, `Cargo.lock`, `README.md`, `CONTRIBUTING.md`, `db/`, `crates/*/src/lib.rs`, `src/` tree)
- Prior note: `research/waku-reuse` → `docs/research/waku-reuse.md`
- ADR-0001, ADR-0003; `docs/spec/v1.md` §§4–7
