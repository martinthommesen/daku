# Plan 037: Document fresh-clone prerequisites and every environment variable daku reads

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- README.md docs/packaging.md Cargo.toml src/daemon.rs src/updater.rs src/dashboard_state.rs crates/daku-core/src/persistence.rs scripts/`
> `README.md`, `docs/packaging.md`, `Cargo.toml` and `scripts/*` are expected
> to have changed via plans 011/015/016/019/035 — re-read them and merge
> your additions; do not overwrite those plans' text. Then re-run the env-var
> grep in "Current state" and use the live results, not this plan's list.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW (docs + one manifest field)
- **Depends on**: plans/011-green-baseline-check-gate.md, plans/016-pin-gpui-and-trim-root-deps.md (README pin note), soft: plans/019-daemon-log-file-and-empty-state.md (log path), plans/035 (`.cargo/config.toml` outcome)
- **Category**: docs / dx
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/60

## Why this matters

A fresh clone on a new Mac fails or stalls for reasons the README does not name: GPUI's build script shells out to `xcrun metal`, which on Xcode 26+ is a separately downloaded Metal toolchain; the first `cargo check` clones zed's git repository (~370 MB db + ~90 MB checkout per pinned commit on this machine, several minutes); no `rust-version` means an old rustc dies with edition/feature errors instead of "requires 1.96"; `bun install` is listed as a build step but the Rust build only needs committed `db/migrations/*.sql` and `locales/`. Separately, half of the environment variables daku reads are documented nowhere — attaching the GUI to a hand-run daemon (`DAKU_DAEMON_ADDRESS`), a sandboxed dev loop (`DAKU_DB_PATH`, `DAKU_UI_FIXTURE`), and every release-time knob require reading source or `--help`.

Docs-only plan (plus `rust-version`), so every done criterion is a `grep`.

## Current state

`README.md:11-24` at HEAD:

```markdown
## Toolchain

| Tool | Version |
|------|---------|
| Rust | **≥ 1.96** (edition 2024) |
| Bun | current (schema / `scripts/dev.ts`) |
| Xcode / clang | macOS builds |

## Build

```sh
bun install
cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client
```
```

`README.md:26` documents `DAKU_DAEMON_TOKEN` and `DAKU_DB_PATH`; `:30` mentions `DAKU_UI_FIXTURE=1`; `:71` mentions `DAKU_CHANNEL=homebrew`. Nothing else. `docs/packaging.md` mentions `SKIP_CODESIGN=1` (`:15`), `SPARKLE_PRIVATE_KEY` (`:39`), `DAKU_CHANNEL` (`:43-45`), `DAKU_NOTARY_PROFILE` (`:59`). Root `Cargo.toml` has `edition = "2024"` (`:4`) and `resolver = "2"` (`:22`), no `rust-version`. `crates/daku-core/build.rs:13-19` reads only `db/migrations` and `locales` from the repo — no `node_modules`.

Every env var read by code at HEAD (re-run: `git grep -n -E 'DAKU_[A-Z_]+|SPARKLE_[A-Z_]+|SKIP_CODESIGN' -- src crates scripts homebrew`):

| Var | Read at | Meaning |
|---|---|---|
| `DAKU_DAEMON_TOKEN` | `crates/daku-daemon/src/main.rs:15` (server), `src/daemon.rs:11` (client, remote mode), constant `crates/daku-protocol/src/protocol.rs:10` | daemon Hello bearer token |
| `DAKU_DAEMON_ADDRESS` | `src/daemon.rs:8-17` (with token: attach to an external daemon instead of spawning), constant `protocol.rs:11` | remote daemon `host:port` / `ws://` URL |
| `DAKU_DAEMON_PATH` | `src/daemon.rs:64` (set by `scripts/dev.ts:84`) | path to the `daku-daemon` binary the app spawns |
| `DAKU_APP_EXECUTABLE` | set by `crates/daku-client/src/process.rs:161` for the child (constant `protocol.rs:12`) — internal, document as "set by the app, not by the Operator" |
| `DAKU_DB_PATH` | `crates/daku-core/src/persistence.rs:22,96` | SQLite path override (default `~/.daku/app.db`) |
| `DAKU_UI_FIXTURE` | `src/dashboard_state.rs:373` (`=1`) | load fixture dashboard events, no ServiceNow |
| `DAKU_CHANNEL` | `src/updater.rs:16,28,40` (`homebrew` disables Sparkle at runtime), `scripts/bundle.sh:117` (`homebrew` build), `homebrew/daku.rb:5` | update channel |
| `DAKU_PREVIEW_UPDATE` | `src/updater.rs:616` (`=1`, debug builds only) | fake an available update in the UI |
| `DAKU_FORCE_UPDATER` | `src/updater.rs:617` (`=1`, debug builds) | run the real Sparkle flow from a debug bundle |
| `DAKU_REDUCE_MOTION` | `src/platform.rs:77` (`cfg(target_os = "linux")` only) | not reachable on macOS — omit from the table or mark linux-only |
| `DAKU_SKIP_CARGO_BUILD` | `scripts/bundle.sh:120`, `scripts/release.ts:115` | reuse existing `target/release` binaries |
| `SKIP_CODESIGN` | `scripts/bundle.sh:26` | same as `--unsigned` |
| `DAKU_CODESIGN_IDENTITY` | `scripts/bundle.sh:62-63`, `scripts/release.ts:64,117` | Developer ID Application identity |
| `DAKU_NOTARY_PROFILE` | `scripts/release.ts:67` | notarytool keychain profile **name** |
| `DAKU_DOWNLOAD_URL_PREFIX` | `scripts/appcast.ts:16,88`, `scripts/release.ts:159` | appcast enclosure URL prefix |
| `SPARKLE_BIN` | `scripts/appcast.ts:26-59`, `scripts/release.ts:165` | dir with Sparkle tools (`generate_appcast`) |
| `SPARKLE_PRIVATE_KEY` | `scripts/appcast.ts:10-15,63` | EdDSA private key (release secrets only — never in git) |

`.gitignore:8` whitelists `.env.example` but none is needed: secrets live in the macOS Keychain (README `:28`, ADR-0004), not in env files. Plan 019 (if DONE) adds `~/.daku/daemon.log`; plan 016 adds a README sentence about the GPUI `rev` pin; plan 035 may delete `.cargo/config.toml` (Metal env). Merge, don't duplicate.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Env-var inventory | `git grep -n -E 'DAKU_[A-Z_]+\|SPARKLE_[A-Z_]+\|SKIP_CODESIGN' -- src crates scripts homebrew` | list to reconcile with the tables |
| rust-version accepted | `cargo check --workspace` | exit 0 (no "package requires rustc" error) |
| Rust-only build without bun | `rm -rf node_modules && cargo check -p daku-core` then `bun install` | exit 0 (proves `bun install` is optional for Rust) |
| Metal toolchain present | `xcrun -f metal` | prints a path |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `README.md` (Toolchain table, Build section, a new "Environment variables" table, dev-loop note)
- `docs/packaging.md` (a "Release-time environment variables" table)
- `Cargo.toml` (root: `rust-version = "1.96"`)
- `plans/README.md` (status row)

**Out of scope**:
- Any code change; `crates/*/README.md`; `.env.example` (explicitly not created); `.cargo/config.toml` (plan 035).
- Re-documenting Keychain setup (README already has it; plan SEC-05 backlog covers the `security` CLI history issue).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Document fresh-clone prerequisites and every DAKU_* / SPARKLE_* variable.`

## Steps

### Step 1: `rust-version`

Root `Cargo.toml` `[package]`: add `rust-version = "1.96"` after `edition = "2024"`. (`resolver = "2"` is unaffected; MSRV-aware resolution is resolver 3 and not required.)

**Verify**: `cargo check --workspace` → exit 0; `grep -n 'rust-version' Cargo.toml` → 1 match.

### Step 2: README Toolchain + Build

Replace the Toolchain table with:

```markdown
| Tool | Version / note |
|------|----------------|
| Rust | **≥ 1.96** (edition 2024; `rust-version` in `Cargo.toml`) |
| Xcode + Command Line Tools | macOS builds |
| Metal toolchain | GPUI compiles shaders with `xcrun metal`. On Xcode 26+ install it once: `xcodebuild -downloadComponent MetalToolchain`; check with `xcrun -f metal`. |
| Bun | only for `scripts/dev.ts`, `bun run release`, `bun run lint`, `bun run db:generate` — **not** needed for `cargo` builds |
```

Below the Build code block add: `The first \`cargo\` build clones the pinned zed repository for GPUI (~0.5 GB, several minutes); later builds reuse it. \`bun install\` is only needed for the Bun scripts.` (Keep plan 016's pin sentence next to it if present.) Change the Build code block to list `cargo check --workspace` first and `bun install` second with the comment `# optional: Bun scripts / lint`.

Add a "Dev loop" paragraph after the `bun run dev` block: `\`DAKU_UI_FIXTURE=1 bun run dev\` renders fixture data without ServiceNow; \`DAKU_DB_PATH=/tmp/daku-dev.db\` keeps a dev daemon's SQLite away from \`~/.daku/app.db\` (the dev Debug.app otherwise polls the same Environments as an installed Daku.app).`

**Verify**: `grep -n 'MetalToolchain' README.md` → 1; `grep -n 'DAKU_UI_FIXTURE=1 bun run dev' README.md` → 1; `grep -n 'not\*\* needed for' README.md` → 1.

### Step 3: README environment-variable table

Add a section `## Environment variables` (runtime/dev only) with one row per var from the "Current state" table **except** the release-time ones and `DAKU_REDUCE_MOTION` (linux-only, unreachable). Columns: Variable | Read by | Effect. Include `DAKU_APP_EXECUTABLE` with the note "set by the app for its daemon child; not for Operators". End with the sentence: `Secrets never go in env files — Credentials live in the macOS Keychain (service \`daku\`); there is deliberately no \`.env.example\`.` If plan 019 is DONE, add `Daemon stderr: \`~/.daku/daemon.log\`.` here.

**Verify**: for each of `DAKU_DAEMON_TOKEN DAKU_DAEMON_ADDRESS DAKU_DAEMON_PATH DAKU_DB_PATH DAKU_UI_FIXTURE DAKU_CHANNEL DAKU_PREVIEW_UPDATE DAKU_FORCE_UPDATER`: `grep -c '<VAR>' README.md` → ≥1; `grep -n 'no \`.env.example\`' README.md` → 1.

### Step 4: `docs/packaging.md` release-time table

Add a section `## Release-time environment variables` (before "Homebrew cask") with rows for `DAKU_SKIP_CARGO_BUILD`, `SKIP_CODESIGN`, `DAKU_CODESIGN_IDENTITY`, `DAKU_NOTARY_PROFILE`, `DAKU_DOWNLOAD_URL_PREFIX`, `SPARKLE_BIN`, `SPARKLE_PRIVATE_KEY`, `DAKU_CHANNEL` (build-time meaning), each with the script that reads it. Mark `SPARKLE_PRIVATE_KEY` and `DAKU_CODESIGN_IDENTITY` as "release secrets / local keychain only — never commit". Cross-link from README's env-var section (`See docs/packaging.md for release-time variables.`).

**Verify**: for each of those 8 vars: `grep -c '<VAR>' docs/packaging.md` → ≥1; `grep -n 'release-time variables' README.md` → 1.

### Step 5: Reconcile against the live grep

Run the env-var inventory command; every `DAKU_*`/`SPARKLE_*`/`SKIP_CODESIGN` name it prints (excluding `DAKU_REDUCE_MOTION` and internal constants like `DAKU_DB_PATH_ENV`) must appear in README or packaging.md. If a new variable appeared since this plan (e.g. from plan 019/035), add it.

**Verify**: `for v in $(git grep -h -o -E 'DAKU_[A-Z_]+|SPARKLE_[A-Z_]+|SKIP_CODESIGN' -- src crates scripts homebrew | sort -u | grep -v -E '_ENV$|DAKU_REDUCE_MOTION'); do grep -q "$v" README.md docs/packaging.md || echo "MISSING $v"; done` → prints nothing.

### Step 6: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- Docs-only; verification is the grep loop in Step 5 plus `cargo check --workspace` for `rust-version`.

## Done criteria

- [ ] `grep -n 'rust-version = "1.96"' Cargo.toml` → 1 match; `cargo check --workspace` exits 0
- [ ] `grep -n 'MetalToolchain' README.md` → 1; README states the first build clones zed and that `bun install` is optional for cargo
- [ ] README has an `## Environment variables` section; the Step 5 loop prints nothing
- [ ] `docs/packaging.md` has a `## Release-time environment variables` section covering the 8 release vars
- [ ] `grep -n '.env.example' README.md` → 1 (the "deliberately none" sentence); no `.env.example` file created
- [ ] `bun run check` exits 0
- [ ] `git status` shows only `README.md`, `docs/packaging.md`, `Cargo.toml`, `plans/README.md`
- [ ] `plans/README.md` status row for 037 updated

## STOP conditions

- `cargo check --workspace` rejects `rust-version = "1.96"` (installed rustc older) — report the version; do not lower the field.
- README/packaging.md sections from plans 011/015/016/019 are missing although those plans are marked DONE (index drift) — report.
- The Step 5 loop reports a variable whose meaning you cannot determine from its read site — list it and STOP rather than guessing.

## Maintenance notes

- Any new `std::env::var("DAKU_…")` must be added to the README table; the Step 5 one-liner is a cheap review check — consider adding it to `bun run check` later if the table drifts.
- If plan 035 deletes `.cargo/config.toml`, the Metal row is the only remaining mention of the toolchain — keep it accurate for the Xcode version in use.
