# Plan 001: Import pinned waku trees and strip agent domain until `cargo` workspace builds

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md` — unless a reviewer dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat b670982..HEAD -- plans/ docs/spec/v1.md docs/adr/`
> If those paths changed in a way that contradicts this plan, STOP and report.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: [waku-fork-inventory](https://github.com/martinthommesen/daku/blob/research/waku-fork-inventory/docs/research/waku-fork-inventory.md) (issue #18)
- **Category**: direction
- **Planned at**: commit `b670982`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/20

## Why this matters

daku v1 is a one-time partial fork of egoist/waku (GPUI + Rust daemon), not a clean rewrite (ADR-0001, ADR-0003, spec §4). Until the workspace builds after copy+strip+rename, no Signal or UI work can land. This plan produces a hollow but compiling macOS workspace named `daku`.

## Current state

- This repo (`martinthommesen/daku` @ `b670982`) has **docs only**: `docs/spec/v1.md`, `docs/adr/*`, `CONTEXT.md`, `plans/`, prototype HTML. **No** `Cargo.toml` / `src/` / `crates/` yet.
- Upstream to copy: **egoist/waku** at pin **`4c483bc282faf4ce9296390887f09b44abb34f27`** (2026-08-17). Full keep/strip/rename tables: inventory note above — treat it as normative; do not invent paths.
- Product locks (do not relitigate):
  - Native GPUI, macOS-only, Rust daemon+protocol (ADR-0001).
  - Partial fork, one-time inheritance (ADR-0003).
  - GPL-3.0-only public (ADR-0002).
- Glossary: use **daku** / **Environment** / **Signal** in new names — never leave user-facing `"Waku"` strings.

Upstream daemon entry (for orientation after copy) looks like:

```rust
// egoist/waku crates/waku-daemon/src/main.rs (pinned SHA) — will become daku-daemon
fn main() -> anyhow::Result<()> {
    let auth = std::env::var(DAEMON_AUTH_ENV)...; // rename from upstream *TOKEN* env
    let listener = TcpListener::bind(&arguments.bind)?;
    // ...
    waku_core::serve(...);
}
```

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Pin check | `git -C /tmp/waku-ref rev-parse HEAD` after clone | prints `4c483bc282faf4ce9296390887f09b44abb34f27` |
| Toolchain | `rustc --version` | ≥ 1.96 |
| Bun | `bun --version` | exits 0 (any recent) |
| Check workspace | `cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client` | exit 0 |
| No agent leftovers | `rg -n 'waku_js_repl|computer_use|Waku Debug' src crates 2>/dev/null \|\| true` | no hits in kept trees (allow comments in this plan only) |

## Suggested executor toolkit

- Read inventory end-to-end before copying: `docs/research/waku-fork-inventory.md` on branch `research/waku-fork-inventory` (or fetch that blob).
- Spec §4 + ADR-0001 / ADR-0003 for boundaries.
- Optional: local mirror `/Users/t979259/.local/src/waku` if present at the same SHA — still verify `rev-parse`.

## Scope

**In scope**

- Adding workspace root files from the pin: `Cargo.toml`, `Cargo.lock`, `LICENSE`, slim `package.json`, `drizzle.config.ts`, `db/` (schema placeholder), `scripts/dev.ts`, `scripts/delete-debug-app.ts`, `.cargo/` if present.
- Copying `src/` (then deleting agent modules per inventory §5.2), `assets/`, selected `resources/`, `locales/`.
- Copying `crates/waku-{core,protocol,client,daemon}/` then renaming to `daku-*` and stripping agent modules (§5.3–5.4).
- Mechanical renames (§6): package names, `APP_NAME`, data dir `daku`, env prefix `DAKU_`.
- Minimal **hollow** `Backend` so `serve` links (stub collector — no ServiceNow yet).
- Minimal GPUI window (theme + chrome) that compiles; agent UI deleted.
- Short root `README.md` for build instructions (replace waku agent docs).

**Out of scope**

- Implementing any Signal poller (plan 003+).
- Real SQLite schema for Environments/Signals (plan 002 may refine; 001 may ship empty/placeholder migration only).
- Sparkle / DMG / Homebrew (plan 010).
- `apps/web`, `packages/*`, `website`, Linux bundles (delete, never port).
- Merging further waku commits after the pin.
- Putting hostnames or credentials anywhere in the repo.

## Git workflow

- Branch: `plan/001-import-waku-strip-agent`
- Commits: imperative, e.g. `Import waku pin and rename crates to daku`
- Do not push/PR unless asked.

## Steps

### Step 1: Fetch the pinned tree

```sh
git clone --depth 1 https://github.com/egoist/waku /tmp/waku-ref
cd /tmp/waku-ref && git fetch --depth 1 origin 4c483bc282faf4ce9296390887f09b44abb34f27 && git checkout 4c483bc282faf4ce9296390887f09b44abb34f27
git rev-parse HEAD   # must equal 4c483bc282faf4ce9296390887f09b44abb34f27
```

Record the pin in `README.md` (Upstream pin section).

**Verify**: `git rev-parse HEAD` → exact SHA above.

### Step 2: Copy keep-set into the daku repo

From the daku repo root, copy inventory §4 paths from `/tmp/waku-ref` into place (do **not** copy §5.1 trees). Prefer `git archive` or `rsync` of listed paths over copying the whole repo.

**Verify**: `test -f Cargo.toml && test -d crates/waku-core && test -d src && test ! -d apps/web`

### Step 3: Delete strip-set

Delete every path in inventory §5.1–5.4 (top-level trees, GPUI agent modules, core/protocol/client agent modules). Drop Cargo deps that only served browser/terminal (`wry`, `objc2-web-kit`, `rquickjs`, `alacritty_terminal`, …) once references are gone.

**Verify**: `test ! -e src/browser.rs && test ! -e src/terminal.rs && test ! -d apps/web`

### Step 4: Rename crates and identity

Apply inventory §6: directory renames `waku-*` → `daku-*`, workspace members, path deps, binary names, `ActiveWakuTheme` → `ActiveDakuTheme` (or `Theme`), `APP_NAME` / `APP_ID` (`app.daku` / `app.daku.dev`), `DATA_DIRECTORY_NAME` → `daku`, env `DAKU_*`. Update `scripts/dev.ts` binary paths to `daku Debug.app`.

**Verify**: `rg -n 'name = "waku"' Cargo.toml crates/*/Cargo.toml` → no matches; `rg -n 'waku-core' Cargo.toml` → no matches.

### Step 5: Hollow backend + green build

Replace deleted `WakuBackend` with a stub that satisfies whatever traits `daku_core::serve` still requires (empty command handlers / TODO for Signals). Prefer switching `gpui` to upstream `zed-industries/zed` **after** browser deletion; if that fails once, STOP and report before permanently keeping `egoist/zed` `waku-webview` — inventory allows egoist pin only as fallback.

Run:

```sh
bun install
cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client
```

Fix compile errors only within in-scope files (usually missing mods in `lib.rs`, unused imports, renamed paths).

**Verify**: `cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client` → exit 0.

### Step 6: Document build

Root `README.md`: how to check out, Rust/Bun versions, `cargo check` packages, pointer to `docs/spec/v1.md`, pin SHA, GPL-3.0-only.

**Verify**: `rg -n '4c483bc282faf4ce9296390887f09b44abb34f27' README.md` → ≥1 hit.

## Test plan

- No product tests required yet.
- Add `crates/daku-daemon` unit test that still refuses non-loopback bind without flag (port the existing test from waku-daemon if present after rename).

**Verify**: `cargo test -p daku-daemon` → pass (or skip with STOP if the test file was deleted accidentally — restore it).

## Done criteria

- [ ] Workspace exists with packages `daku`, `daku-core`, `daku-daemon`, `daku-protocol`, `daku-client`
- [ ] `cargo check` on those packages exits 0
- [ ] No `apps/web`, `src/browser.rs`, `src/terminal.rs`
- [ ] Pin SHA documented in `README.md`
- [ ] `plans/README.md` row 001 → `done` (or `in_progress` only while PR open)
- [ ] No files containing live instance hostnames/secrets added

## STOP conditions

- Pinned SHA is missing or `rev-parse` mismatches.
- `cargo check` still fails after two focused fix passes on rename/strip fallout.
- Green build appears to require keeping large agent modules (`composer`, `transcript`, provider sessions).
- Upstream Zed GPUI migration fails **and** egoist pin is unclear — report; do not silently vendor unrelated forks.
- Need to edit files outside Scope.

## Maintenance notes

- Plan 002 owns real `~/.daku` SQLite schema; 001 may leave placeholder migrations.
- Reviewers: inspect `Cargo.toml` GPUI source (upstream vs egoist) and that GPL `LICENSE` remains.
- Deferred: Sparkle scripts until plan 010; Signal protocol payloads until 003+.
