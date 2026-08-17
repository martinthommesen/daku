# Plan 001: Import pinned waku trees and strip agent domain until `cargo` workspace builds

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- Cargo.toml Cargo.lock src crates db scripts package.json drizzle.config.ts LICENSE README.md`
> If any in-scope path already diverges from “Current state” (docs-only → unexpected half-fork), STOP and report.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: none (inventory paths are inlined below; remote note is reference only)
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/24
- **Done**: workspace green on upstream Zed GPUI; hollow backend + empty shell

## Why this matters

daku v1 is a one-time partial fork of egoist/waku (GPUI + Rust daemon), not a clean rewrite (ADR-0001, ADR-0003, spec §4). Until the workspace builds after copy+strip+rename, no Signal or UI work can land. This plan produces a hollow but compiling macOS workspace named `daku`.

## Current state

- This repo (`martinthommesen/daku` @ `567179a`) has **docs + plans** only: `docs/spec/v1.md`, `docs/adr/*`, `CONTEXT.md`, `plans/`, prototype HTML. **No** `Cargo.toml` / `src/` / `crates/` yet.
- Upstream pin: **egoist/waku** SHA **`4c483bc282faf4ce9296390887f09b44abb34f27`** (2026-08-17). Rust ≥ 1.96, edition 2024; Bun for `db:generate` / `scripts/dev.ts`.
- GPUI in upstream `Cargo.toml`: `git = "https://github.com/egoist/zed"`, branch `waku-webview`, lock rev `f9bad8941ea813982d6dfb10c0377ebf7716b3e7`. After deleting the browser, **prefer** upstream `zed-industries/zed`; keep egoist pin only if one green build against upstream fails — then STOP and report before locking egoist permanently.
- Product locks: native GPUI, macOS-only, Rust daemon+protocol (ADR-0001); one-time fork (ADR-0003); GPL-3.0-only (ADR-0002).
- Glossary: **daku** / **Environment** / **Signal** — no user-facing `"Waku"`.

### Inlined keep set (copy from pin)

**Scaffold:** `Cargo.toml`, `Cargo.lock`, `LICENSE`, slim `package.json` (keep `db:generate` / `db:push` only), `drizzle.config.ts`, `db/schema.ts` (replace tables later), `db/migrations/`, `scripts/dev.ts`, `scripts/bundle.sh`, `scripts/delete-debug-app.ts`, `.cargo/` if present.

**GPUI (copy trees, then delete strip list):** `src/main.rs`, `src/lib.rs`, `src/app.rs`, `src/theme.rs`, `src/assets.rs`, `src/platform.rs`, `src/input.rs`, `src/daemon.rs`, `src/ui/`, `src/md/`, `src/app/render.rs`, `src/app/window_chrome.rs`, `src/app/sidebar.rs`, `src/app/command_palette.rs`, `src/app/settings.rs` (keep General/Appearance/Daemon; strip Providers/Skills/Computer Use), `src/app/usage_page.rs` (chart helpers only), `src/app/components.rs`, `assets/fonts/`, `assets/icons/`, `resources/AppIcon.icns`, `resources/AppIconDev.icns`, `resources/Info.plist`, `locales/*.yml`.

**Crates (copy then rename/strip):**

| Upstream path | Keep |
|---|---|
| `crates/waku-daemon/` | `main.rs` (bind, auth from env, `serve`) |
| `crates/waku-protocol/` | `protocol.rs` envelope (`PROTOCOL_VERSION`, Hello/Rejected, `MAX_WIRE_MESSAGE_BYTES`), `bin/export_types.rs` optional, `lib.rs` / i18n / theme / settings / identity skeletons |
| `crates/waku-core/` | `server.rs` (Hub, handshake, replay), migration runner + path helpers from `persistence.rs`, `build.rs`, `settings.rs`, `lib.rs` re-export `serve` |
| `crates/waku-client/` | `client.rs`, `process.rs` (`DaemonSupervisor`), desktop prefs `persistence.rs`, `lib.rs` |

### Inlined strip set (delete; do not inherit)

**Top-level:** `apps/web/`, `packages/waku-client/`, `website/`, `scripts/seed-mock-sessions.ts`, `scripts/bundle-linux.sh`, `resources/computer-use/`, `resources/linux/`, `src/bin/` (e.g. `waku_js_repl.rs`), `src/driver/`.

**GPUI agent files:** `src/browser.rs`, `src/terminal.rs`, `src/js_repl.rs`, `src/js_repl_bootstrap.js`, `src/computer_use.rs`, `src/review_diff.rs`, `src/query.rs`, `src/analytics.rs`, `src/app/composer.rs`, `src/app/transcript.rs`, `src/app/transcript_view.rs`, `src/app/right_panel.rs`, `src/app/runtime.rs`, `src/app/streaming.rs`, `src/app/sessions.rs`, `src/app/drafts.rs`, `src/app/autocomplete.rs`, `src/app/skills_page.rs`, `src/app/commit_dialog.rs`, `src/app/branches.rs`, `src/app/activity_diff.rs`, `src/app/file_search.rs`, `src/app/image_preview.rs`, `src/app/usage_meter.rs`, `src/app/background_work.rs`.

**Drop Cargo deps once unused:** `wry`, `objc2-web-kit`, `rquickjs`, `alacritty_terminal`.

**`waku-core`:** delete everything under `crates/waku-core/src/` except the keep set — including `driver/`, `*_session.rs`, attachments, blob_store, checkpoint, composer_complete, computer_use, git_*, model*, projectless, skills, terminal, usage*, workspace, worktree, command_env, thick `daemon::WakuBackend`. **Do not delete `persistence.rs` wholesale** — extract `apply_migrations`, WAL pragmas, path helpers; discard session/message SQL.

**`waku-protocol` / `waku-client`:** delete attachments, blob, checkpoint, composer, computer_use, driver_wire, git, model*, projectless, provider_session, skills, usage*, workspace modules and matching `Command` / `ResponsePayload` variants; client: `driver`, `composer_complete`, `computer_use`, `workspace_client` (keep `command_env` only if daemon spawn needs it).

### Inlined rename map (apply mechanically)

| Upstream | daku |
|---|---|
| package/bin `waku` | `daku` |
| `crates/waku-core` | `crates/daku-core` |
| `crates/waku-protocol` | `crates/daku-protocol` |
| `crates/waku-client` | `crates/daku-client` |
| `crates/waku-daemon` | `crates/daku-daemon` (bins `daku-daemon`, `daku-debug-daemon` if present) |
| `Waku` / `ActiveWakuTheme` | `Daku` / `ActiveDakuTheme` (or `App` / `Theme`) |
| `APP_NAME` `"Waku"` / `"Waku Debug"` | `"daku"` / `"daku Debug"` |
| `APP_ID` `sh.waku` / `sh.waku.dev` | `app.daku` / `app.daku.dev` |
| `DATA_DIRECTORY_NAME` `"Waku"` | `"daku"`; Operator config/db under **`~/.daku/`** (ADR-0007) |
| Env `WAKU_*` / upstream daemon auth env | Prefix **`DAKU_`**. Lock daemon shared-secret env name to **`DAKU_DAEMON_TOKEN`** (Hello auth). |
| `Waku Debug.app` | `daku Debug.app` |

Reference (optional): [docs/research/waku-fork-inventory.md](../docs/research/waku-fork-inventory.md) — must not contradict the tables above; if it does, prefer this plan and STOP to report drift.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Pin check | `git -C /tmp/waku-ref rev-parse HEAD` | `4c483bc282faf4ce9296390887f09b44abb34f27` |
| Toolchain | `rustc --version` | ≥ 1.96 |
| Bun | `bun --version` | exit 0 |
| Check workspace | `cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client` | exit 0 |
| No strip leftovers | `test ! -e src/browser.rs && test ! -e src/terminal.rs && test ! -d apps/web` | all true |

## Scope

**In scope:** paths in the keep set; deletions in the strip set; renames in the rename map; hollow `Backend` stub; minimal GPUI window; root `README.md` build docs.

**Out of scope:** Signal pollers (003+); real Environments/Signals schema (002); Sparkle/DMG (010); `apps/web` / Linux; further upstream merges; hostnames/credentials in repo.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Fetch the pinned tree

```sh
git clone --depth 1 https://github.com/egoist/waku /tmp/waku-ref
cd /tmp/waku-ref && git fetch --depth 1 origin 4c483bc282faf4ce9296390887f09b44abb34f27 && git checkout 4c483bc282faf4ce9296390887f09b44abb34f27
git rev-parse HEAD
```

**Verify**: stdout is exactly `4c483bc282faf4ce9296390887f09b44abb34f27`.

### Step 2: Copy keep-set into the daku repo

From the daku repo root, copy **only** the inlined keep-set paths from `/tmp/waku-ref` (rsync/cp listed paths). Do **not** copy strip-set trees.

**Verify**: `test -f Cargo.toml && test -d crates/waku-core && test -d src && test ! -d apps/web`

### Step 3: Delete strip-set

Delete every path in the inlined strip set. Remove unused browser/terminal Cargo deps.

**Verify**: `test ! -e src/browser.rs && test ! -e src/terminal.rs && test ! -d apps/web && test ! -d src/driver`

### Step 4: Rename crates and identity

Apply the inlined rename map (directories, workspace members, path deps, binaries, theme types, `APP_NAME` / `APP_ID`, data dir, **`DAKU_DAEMON_TOKEN`**, `scripts/dev.ts` → `daku Debug.app`).

**Verify**: `rg -n 'name = "waku"' Cargo.toml crates/*/Cargo.toml` → no matches; `rg -n 'waku-core' Cargo.toml` → no matches; `rg -n 'DAKU_DAEMON_TOKEN' crates/daku-daemon` → ≥1 hit.

### Step 5: Hollow backend + green build

Stub Backend for `daku_core::serve`. Prefer upstream Zed GPUI after browser deletion; if that fails once, STOP and report (egoist fallback only with operator approval).

```sh
bun install
cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client
```

**Verify**: command exits 0.

### Step 6: Document build

Root `README.md`: Rust/Bun versions, `cargo check` packages, pin SHA, GPL-3.0-only, pointer to `docs/spec/v1.md`.

**Verify**: `rg -n '4c483bc282faf4ce9296390887f09b44abb34f27' README.md` → ≥1 hit.

## Test plan

- Port refuse-non-loopback bind test from upstream daemon if present.

**Verify**: `cargo test -p daku-daemon` → exit 0 (if the test file was deleted, restore from pin and re-run).

## Done criteria

- [ ] `test -f Cargo.toml && test -d crates/daku-core && test -d crates/daku-daemon && test -d crates/daku-protocol && test -d crates/daku-client`
- [ ] `cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client` exits 0
- [ ] `test ! -d apps/web && test ! -e src/browser.rs && test ! -e src/terminal.rs`
- [ ] `rg -n '4c483bc282faf4ce9296390887f09b44abb34f27' README.md` → ≥1 hit
- [ ] `rg -n 'DAKU_DAEMON_TOKEN' crates/daku-daemon` → ≥1 hit
- [ ] `plans/README.md` row 001 Status = `DONE`

## STOP conditions

- `rev-parse` ≠ pinned SHA.
- `cargo check` still fails after two focused rename/strip fix passes.
- Green build requires keeping `composer` / `transcript` / provider sessions.
- Upstream Zed GPUI fails and egoist pin would be kept without reporting.
- Need to edit files outside Scope.

## Maintenance notes

- Plan 002 owns `~/.daku` SQLite schema + `environments.json`.
- Daemon Hello auth env is **`DAKU_DAEMON_TOKEN`** for all later plans.
- Reviewers: GPUI source (upstream vs egoist) and `LICENSE` retained.
