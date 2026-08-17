# Plan 035: Clean `.cargo/config.toml`, keep release symbols (`.dSYM`), and try dropping the personal-fork `block` patch

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- Cargo.toml .cargo/config.toml scripts/release.ts scripts/bundle.sh docs/packaging.md README.md`
> `scripts/release.ts`, `scripts/bundle.sh` and `docs/packaging.md` are
> **expected** to have changed via plan 015 (read `plans/015-release-pipeline-sparkle-fixes.md`
> and confirm its status is DONE). For `Cargo.toml` and `.cargo/config.toml`,
> compare the "Current state" excerpts against the live files; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/015-release-pipeline-sparkle-fixes.md (release.ts shape), plans/016-pin-gpui-and-trim-root-deps.md (Cargo.toml shape)
- **Category**: dx / migration
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/58

## Why this matters

Three small leftovers from the waku import, each with a concrete cost:

1. **`.cargo/config.toml`** sets `TS_RS_LARGE_INT` for a `ts-rs` crate that is not in the dependency tree (0 hits in `Cargo.lock`), forces `TOOLCHAINS = "Metal"` (on this machine `xcrun -f metal` resolves identically with or without it — the Metal toolchain is a separate download either way), and commits `[build] jobs = 4`, which throttles the very slow GPUI build on every contributor's 10–16-core Mac.
2. **Release builds are unsymbolicatable.** `[profile.release]` has `strip = true` and no `debug`/`split-debuginfo`, and `bundle.sh` never archives a `.dSYM`. macOS crash reports and Rust panics from Sparkle-updated builds come back as bare addresses; there is no App Store symbol server for self-distributed builds.
3. **`[patch.crates-io] block`** points every release at a one-person GitHub fork of a crate last released in 2018, reachable only via `cocoa` ← `gpui` (build-dependency path). The comment says it exists for a future-incompatibility fix; zed itself builds `block = "0.1"` unpatched. Whether daku still needs the patch on Rust 1.96 is an experiment, not a fact — this plan runs it and records the outcome either way.

## Current state

`.cargo/config.toml` (whole file, 10 lines):

```toml
[env]
# Xcode ships the Metal compiler as an optional toolchain. GPUI's build script
# invokes `xcrun metal`, so select that toolchain for precompiled shaders.
TOOLCHAINS = "Metal"
# JSON has no bigint literal, so generated browser bindings model the wire's
# integer fields as JavaScript numbers.
TS_RS_LARGE_INT = "number"

[build]
jobs = 4
```

`Cargo.toml` (root):

```toml
# :86-89
[profile.release]
lto = "thin"
codegen-units = 1
strip = true

# :96-101
# `block` 0.1.6 declares an extern static with an uninhabited type. This fork
# is the crates.io release plus the future-incompatibility fix only.
[patch.crates-io]
block = { git = "https://github.com/Dicklesworthstone/rust-block", rev = "b39ae859d1ee8e8cb5eef6a516471f1578d26b96" }
```

`cargo tree --workspace -i block` at HEAD: `block v0.1.6 (…rust-block…) └── cocoa v0.26.0 └── gpui v0.2.2 [build-dependencies] └── gpui_apple … └── gpui_platform └── daku`. `grep -c '^name = "ts-rs"' Cargo.lock` → `0`. `xcrun -f metal` with `TOOLCHAINS` unset, `=Metal`, and `=Bogus` all print the same `…/Metal.xctoolchain/usr/bin/metal` path on this machine (verified 2026-08-17).

`scripts/bundle.sh:120-127` builds with `cargo build --release … --package daku --bin daku --package daku-daemon --bin daku-daemon` (or `--features channel-homebrew`) unless `DAKU_SKIP_CARGO_BUILD=1`; the binaries are copied from `$cargo_target_dir/release/`. `scripts/release.ts` (after plan 015) zips the app to `dist/Daku-<v>.zip` around lines 149-151 (`ditto -c -k --keepParent ${appBundle} ${zipPath}`) and prints `App ready / DMG ready / ZIP ready`. `docs/packaging.md` "Sparkle (primary)" section lists the release assets (plan 015 adds the ZIP).

Conventions: POSIX `sh` in bundle.sh; Bun `$` in release.ts (oxlint `anti-slop` rules — run `bun run lint`); README toolchain table at `README.md:11-17`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Metal inert check | `unset TOOLCHAINS; xcrun -f metal; TOOLCHAINS=Metal xcrun -f metal` | both print the same path |
| Check | `cargo check --workspace --all-targets` | exit 0 |
| Release build | `cargo build --release --package daku --bin daku --package daku-daemon --bin daku-daemon` | exit 0; `target/release/daku.dSYM` and `daku-daemon.dSYM` exist |
| Symbols present | `dwarfdump --uuid target/release/daku.dSYM` | prints a UUID |
| Binary still stripped | `nm target/release/daku 2>&1 \| head -1` | `no symbols` (or empty) |
| Lint | `bun run lint` | exit 0 |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `.cargo/config.toml`
- `Cargo.toml` (`[profile.release]`, `[patch.crates-io]`)
- `Cargo.lock` (only if the patch is removed)
- `scripts/release.ts` (archive `.dSYM`s)
- `docs/packaging.md` (one bullet), `README.md` (one sentence)
- `plans/README.md` (status row)

**Out of scope**:
- `scripts/bundle.sh` — no change needed (cargo writes `.dSYM` next to the binaries; release.ts archives them).
- Any `src/` change. If removing the `block` patch produces a **hard error**, restore the patch — do not touch code.
- Adding a `rust-toolchain.toml` (plan 037 adds `rust-version`).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Trim .cargo config, keep release dSYMs, and drop/record the block patch.`

## Steps

### Step 1: `.cargo/config.toml`

Delete the `TS_RS_LARGE_INT` line and its comment, and the whole `[build] jobs = 4` block. For `TOOLCHAINS`: run the "Metal inert check" command. If both invocations print the same path, delete the `[env]` table entirely (file becomes empty → delete the file and the now-empty `.cargo/` directory). If they differ (the env var actually selects a different toolchain on this machine), keep only the `TOOLCHAINS` line + its comment and record that in the status row.

**Verify**: `cargo check --workspace --all-targets` → exit 0 (GPUI's shader build script still finds `metal`). `git status` shows `.cargo/config.toml` deleted (or reduced to the `TOOLCHAINS` lines).

### Step 2: Release symbols

In `Cargo.toml` `[profile.release]` add two lines (keep `strip = true`):

```toml
[profile.release]
lto = "thin"
codegen-units = 1
strip = true
debug = "line-tables-only"
split-debuginfo = "packed"
```

Then in `scripts/release.ts`, after the ZIP is created (after the `ditto -c -k --keepParent ${appBundle} ${zipPath}` line), add:

```ts
// Symbols for crash symbolication; the shipped binaries stay stripped.
const symbolsZip = resolve(projectRoot, "dist", `${appName}-${version}-dSYM.zip`);
await $`ditto -c -k --keepParent ${join(projectRoot, "target", "release", "daku.dSYM")} ${symbolsZip}`;
```

(If `daku-daemon.dSYM` also exists, zip both: create a temp dir, `ditto` both `.dSYM` bundles into it, then `ditto -c -k` the dir. Keep it to the two files bundle.sh builds.) Add `console.log(\`Symbols ready: ${symbolsZip}\`);` next to the other `ready` lines. Respect `DAKU_SKIP_CARGO_BUILD` — the `.dSYM` must exist from the release build; if it does not, throw with a message naming `target/release/daku.dSYM`.

`docs/packaging.md` "Sparkle (primary)" section: add the bullet `Keep \`dist/Daku-x.y.z-dSYM.zip\` with the release (crash symbolication); it is not a Sparkle asset.`

**Verify**: `cargo build --release --package daku --bin daku --package daku-daemon --bin daku-daemon` → `ls -d target/release/*.dSYM` lists `daku.dSYM` and `daku-daemon.dSYM`; `nm target/release/daku | head -1` → `no symbols`; `bun run lint` → exit 0; `DAKU_SKIP_CARGO_BUILD=1 bun run release --unsigned` → prints `Symbols ready: …-dSYM.zip` and the file exists.

### Step 3: `block` patch experiment

Delete the `[patch.crates-io]` table (both comment lines and the `block = …` line) from `Cargo.toml`. Run `cargo check --workspace --all-targets 2>&1 | tee /tmp/daku-block-check.log`.

- If exit 0 (warnings allowed — grep the log for `future-incompat`/`block` and paste any such warning text into the status row): keep the removal. `cargo tree --workspace -i block` must now show `block v0.1.6 (registry+…)`.
- If it **fails** with an error mentioning `block` / uninhabited static: `git checkout -- Cargo.toml Cargo.lock` for that hunk only (re-apply Step 2's profile lines), keep the patch, and paste the exact error into the status row and into a comment above the patch (`# Required on rustc <version>: <one-line error>`).

**Verify**: either `grep -c 'patch.crates-io' Cargo.toml` → `0` and `cargo tree --workspace -i block | head -1` shows the registry source, or the patch remains with the recorded error text.

### Step 4: README

`README.md` Toolchain section: add one sentence: `Release builds keep line-table debuginfo in a separate \`.dSYM\` (\`split-debuginfo = "packed"\`); the shipped binaries are stripped.`

**Verify**: `grep -n 'dSYM' README.md docs/packaging.md scripts/release.ts` → ≥1 match in each.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- No unit tests (manifest/config/script change). Verification is the release build producing `.dSYM`s, `nm` showing a stripped binary, and `bun run release --unsigned` archiving symbols.

## Done criteria

- [ ] `.cargo/config.toml` deleted, or contains only the `TOOLCHAINS` lines with a status-row note
- [ ] `grep -n 'split-debuginfo = "packed"' Cargo.toml` → 1 match; `grep -n 'line-tables-only' Cargo.toml` → 1 match
- [ ] `ls -d target/release/daku.dSYM` exists after a release build; `nm target/release/daku` shows no symbols
- [ ] `grep -n 'dSYM' scripts/release.ts` → ≥2 matches; `bun run lint` exits 0
- [ ] `block` patch removed with `cargo check` green, **or** kept with the recorded error text in a comment
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 035 updated (with the block/Metal outcomes)

## STOP conditions

- Plan 015 is not DONE (release.ts still has `--output`/old shape) — do 015 first.
- `cargo check` fails after Step 1 with a Metal/shader error — restore `TOOLCHAINS` and report.
- Removing the patch fails for a reason **other** than `block` (network, other crate) — report; do not iterate on the lockfile.
- `cargo build --release` does not produce a `.dSYM` with the two profile lines (toolchain difference) — report the cargo version.

## Maintenance notes

- If GPUI upstream drops `cocoa`/`block` (zed is migrating to `objc2`), the patch question disappears — re-check `cargo tree -i block` when bumping the GPUI `rev` (plan 016).
- Symbol zips must match the released binary UUID (`dwarfdump --uuid`); never re-build between zipping the app and zipping symbols.
- Reviewers: confirm `strip = true` is still present and the app binary has no symbol table.
