# Plan 016: Pin GPUI to the locked zed commit, drop `test-support` from release builds, remove unused root dependencies

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- Cargo.toml Cargo.lock README.md`
> If `Cargo.toml` changed since this plan was written, compare the "Current
> state" excerpts against the live file before proceeding; on a mismatch,
> treat it as a STOP condition. (`Cargo.lock` churn alone is expected — but
> re-read the resolved gpui commit in Step 1 rather than trusting this plan.)

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate only)
- **Category**: migration / dx
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/39

## Why this matters

- `gpui` and `gpui_platform` are git dependencies on `zed-industries/zed` with **no `rev`/`tag`** (`Cargo.toml:28-31`). Only `Cargo.lock` pins them (to zed `main` HEAD of 2026-08-17). Any `cargo update`, `cargo update -p gpui`, or a build from a tree without the lockfile silently moves ~12 `gpui_*` crates to whatever zed merged that hour. GPUI's API churn is high; the whole `src/` (6 files) breaks with no bisect point. Pinning `rev` to the already-locked commit costs nothing and makes the pin explicit.
- `gpui` is built with `features = ["test-support"]` in `[dependencies]` — i.e. in **release** builds. At the locked commit that feature pulls in `proptest` (itself a git-pinned crate), leak-detection backtraces, and platform extras; nothing in `src/` uses `TestAppContext`/`VisualTestContext`. Longer builds, larger `Daku.app`, more supply chain for zero use.
- The root `Cargo.toml` declares `dirs`, `serde` (derive), `uuid`, and `objc2-quartz-core` that no file in `src/` imports (verified by grep at HEAD; the `uuid` hits in `src/lib.rs:68-71` are a closure variable name).

## Current state

`Cargo.toml` (root) at HEAD:

```toml
# :25-33
[dependencies]
anyhow = "1.0"
dirs = "6.0"
# Prefer upstream Zed GPUI after browser/WKWebView strip (plan 001).
gpui = { git = "https://github.com/zed-industries/zed", features = ["test-support"] }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = [
    "font-kit",
] }
libc = "0.2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
uuid = { version = "1.18", features = ["v4", "serde"] }
daku-client = { path = "crates/daku-client" }
daku-protocol = { path = "crates/daku-protocol" }
rust-i18n = "4"

# :44-46 (macOS block; objc2-app-kit's feature list at :47-69 includes "objc2-quartz-core"
#          and "objc2-core-graphics" — those are FEATURES of objc2-app-kit, not this crate's deps)
[target.'cfg(target_os = "macos")'.dependencies]
block2 = "0.6"
objc2 = "0.6"
objc2-app-kit = { version = "0.3", default-features = false, features = [ … "objc2-core-graphics", "objc2-quartz-core", ] }
objc2-foundation = { version = "0.3", default-features = false, features = [ … ] }
objc2-quartz-core = { version = "0.3", default-features = false, features = ["std", "CALayer", "objc2-core-graphics"] }
raw-window-handle = "0.6"
```

`Cargo.lock:2244-2246`:

```
name = "gpui"
version = "0.2.2"
source = "git+https://github.com/zed-industries/zed#db7c1d38c8e17e9d4f01c35179c847fcd5bfa09b"
```

Root-crate usage at HEAD (grep over `src/`): `anyhow` (daemon.rs, assets.rs, platform.rs), `libc` (daemon.rs:45, updater.rs:627 — **keep**), `serde_json` (dashboard_state.rs — keep), `rust_i18n` (lib.rs — keep), `block2`/`objc2`/`objc2_foundation`/`objc2_app_kit`/`raw_window_handle` (updater.rs, platform.rs — keep), `daku_client`/`daku_protocol` (keep). **No** imports of `dirs`, `serde`, `uuid`, `objc2_quartz_core`.

`README.md:11-24` documents the toolchain and build; there is no note on how the GPUI pin is bumped.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Resolved gpui commit | `grep -n -A2 '^name = "gpui"$' Cargo.lock` | one `source = "git+…zed#<40-hex>"` line |
| Check | `cargo check --workspace --all-targets` | exit 0 |
| Proptest gone | `cargo tree --workspace -i proptest` | `error: package ID specification … did not match any packages` (i.e. absent) |
| Lock unchanged for gpui | `git diff Cargo.lock \| grep -c '^[-+]source = "git+https://github.com/zed-industries/zed'` | `0` after Step 1 |
| Gate | `bun run check` | exit 0 |

First `cargo check` after a manifest change re-resolves; expect a Cargo.lock diff that only **removes** entries (Step 2/3) — never one that changes the zed source line.

## Scope

**In scope**:
- `Cargo.toml` (root)
- `Cargo.lock` (regenerated by cargo; review the diff)
- `README.md` (one sentence on bumping the pin)
- `plans/README.md` (status row)

**Out of scope**:
- `crates/*/Cargo.toml` — no changes.
- The `[patch.crates-io] block` fork (`Cargo.toml:96-101`) — leave it; removing it is a separate experiment (see backlog).
- Upgrading GPUI to a newer commit — this plan pins, it does not bump.
- Any `src/` change. If a dep removal needs a code change, that dep is *used* — STOP and report.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Pin gpui to the locked zed commit and trim unused root dependencies.`

## Steps

### Step 1: Pin `rev` to the locked commit

Read the commit: `grep -n -A2 '^name = "gpui"$' Cargo.lock` → note the 40-hex after `zed#` (at HEAD: `db7c1d38c8e17e9d4f01c35179c847fcd5bfa09b`; use what the lockfile says).

Edit `Cargo.toml`:

```toml
# Prefer upstream Zed GPUI after browser/WKWebView strip (plan 001).
# Pinned to the commit Cargo.lock resolved; bump `rev` on both lines
# together and re-run `bun run check`.
gpui = { git = "https://github.com/zed-industries/zed", rev = "<40-hex>" }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "<40-hex>", features = [
    "font-kit",
] }
```

(This also performs Step 2 — `features = ["test-support"]` is dropped from `gpui`.)

**Verify**: `cargo check --workspace --all-targets` → exit 0. `git diff Cargo.lock | grep -c '^[-+]source = "git+https://github.com/zed-industries/zed'` → `0` (the zed source lines are unchanged; only `proptest`/other removals appear). `cargo tree --workspace -i proptest` → not found.

### Step 2: Confirm nothing needed `test-support`

**Verify**: `cargo test --workspace --no-fail-fast` → 0 failed (the root crate's 11 tests do not use GPUI test contexts).

### Step 3: Remove unused root dependencies

Delete these lines from `Cargo.toml` `[dependencies]`: `dirs = "6.0"`, `serde = { version = "1.0", features = ["derive"] }`, `uuid = { version = "1.18", features = ["v4", "serde"] }`; and from the macOS block: the `objc2-quartz-core = { … }` line. Keep the `"objc2-quartz-core"` **feature** inside `objc2-app-kit`'s feature list.

**Verify**: `cargo check --workspace --all-targets` → exit 0. If it fails with `unresolved import`/`use of undeclared crate` for one of the four, restore that one line and note it in the status row (it was used after all — do not edit `src/`).

### Step 4: README note

In `README.md`, under `## Build`, after the code block, add: `GPUI is pinned by \`rev\` in \`Cargo.toml\` (both \`gpui\` and \`gpui_platform\`); bump both together and run \`bun run check\`. Do not run \`cargo update\` casually — it re-resolves every zed crate.`

**Verify**: `grep -n 'pinned by' README.md` → 1 match.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- No new tests; the change is manifest-only. Verification = `cargo check --workspace --all-targets`, `cargo test --workspace --no-fail-fast`, `cargo tree -i proptest` absent, and a reviewed `Cargo.lock` diff (removals only; zed source lines untouched).

## Done criteria

- [ ] `grep -c 'rev = "' Cargo.toml` → `3` (two gpui lines + the existing `block` patch)
- [ ] `grep -n 'test-support' Cargo.toml` → no matches
- [ ] `grep -n '^dirs\|^serde = \|^uuid\|^objc2-quartz-core' Cargo.toml` → no matches (unless one was restored in Step 3 with a note)
- [ ] `cargo tree --workspace -i proptest` reports no such package
- [ ] `git diff Cargo.lock | grep -c '^[-+]source = "git+https://github.com/zed-industries/zed'` → `0`
- [ ] `bun run check` exits 0
- [ ] `git status` shows only `Cargo.toml`, `Cargo.lock`, `README.md`, `plans/README.md` modified
- [ ] `plans/README.md` status row for 016 updated

## STOP conditions

- `Cargo.lock` has more than one distinct zed source commit across `gpui*` entries (already inconsistent) — report before pinning.
- After Step 1 cargo tries to fetch a different zed commit or `Cargo.lock`'s zed source lines change.
- Removing `test-support` breaks `cargo check` (a root test now uses GPUI test contexts) — restore the feature under `[dev-dependencies]` instead: `gpui = { git = …, rev = …, features = ["test-support"] }` there, and note it.
- Network unavailable and cargo needs to re-fetch — report.

## Maintenance notes

- Bumping GPUI is now a deliberate two-line `rev` change; expect API breakage in `src/app.rs`/`platform.rs` and budget for it.
- Reviewers: the `Cargo.lock` diff must be removals only (proptest, its deps, and possibly transitive `dirs`/`uuid` duplicates); anything else means resolution moved.
- Deferred to backlog: the personal-fork `[patch.crates-io] block`; `ureq` 2→3 / native roots; `rusqlite` bump; a `rust-toolchain.toml`.
