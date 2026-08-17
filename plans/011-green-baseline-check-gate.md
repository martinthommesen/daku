# Plan 011: Make the local verification gate one command and green

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol/src/settings.rs crates/daku-core/src/settings.rs crates/daku-core/src/collector.rs scripts/release.ts scripts/delete-debug-app.ts package.json README.md CLAUDE.md AGENTS.md docs/agents/git-workflow.md`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx (+ bug: one failing test)
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/34

## Why this matters

This repo has **no CI and no pull requests by decision** (`docs/agents/git-workflow.md`): "Local verification … is the quality gate." Today that gate is red on `main` and is not a single command:

- `cargo test --workspace` fails: `daku-core settings::tests::legacy_combined_settings_keep_only_daemon_fields` (1 of 100 tests). Root cause: `DaemonSettings.extra` is a plain field, not `#[serde(flatten)]`, so unknown top-level keys in `~/.daku/settings.json` are silently dropped instead of captured. The README's only tuning knob (`poll_interval_secs`) is read from that map, so an Operator writing the natural `{"poll_interval_secs": 60}` is silently ignored.
- `cargo fmt --all --check` fails (7 hunks in 5 files).
- `bun run lint` (oxlint) fails with 4 `anti-slop/require-safety-comment-for-type-assertion` errors.
- Nothing runs all three; every executor plan so far only ran scoped `cargo test -p … <name>`.

After this plan: `bun run check` runs fmt-check + workspace tests + oxlint, exits 0 on `main`, and is named as the gate in `CLAUDE.md`/`AGENTS.md` and `docs/agents/git-workflow.md`. Every later plan in this index uses it as a done criterion.

## Current state

Files and roles:

- `crates/daku-protocol/src/settings.rs` — wire/disk shape of the daemon settings (`DaemonSettings`).
- `crates/daku-core/src/settings.rs` — `DaemonSettingsStore` (load/quarantine/atomic write) + the failing test.
- `crates/daku-core/src/collector.rs` — `poll_interval_secs(&DaemonSettings)` reads `settings.extra["poll_interval_secs"]`.
- `scripts/release.ts`, `scripts/delete-debug-app.ts` — Bun scripts with the 4 lint errors.
- `package.json` — Bun scripts (`dev`, `release`, `db:generate`, `db:push`, `lint`).
- `README.md:30` — documents `poll_interval_secs`.
- `CLAUDE.md` and `AGENTS.md` — byte-identical (verify: `diff CLAUDE.md AGENTS.md` prints nothing). Keep them identical.
- `docs/agents/git-workflow.md:7` — rule 1 names local verification as the gate.

`crates/daku-protocol/src/settings.rs:10-17` today:

```rust
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    pub theme: ThemePreference,
    pub language: AppLanguage,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}
```

The failing test, `crates/daku-core/src/settings.rs:108-127`, writes `{"theme":"dark","analytics_enabled":false,"future":42}` to a temp file, opens the store, and asserts `settings.extra.get("future") == Some(42)` and that `analytics_enabled` is dropped (by `discard_legacy_app_keys`, `crates/daku-protocol/src/settings.rs:37-41`), then that a `replace()` round-trip keeps `future` at top level. That is exactly the `#[serde(flatten)]` contract.

`crates/daku-core/src/collector.rs:27-37` today:

```rust
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;
pub const POLL_INTERVAL_SECS_KEY: &str = "poll_interval_secs";

pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    settings
        .extra
        .get(POLL_INTERVAL_SECS_KEY)
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
}
```

The only existing assertion on it is `assert_eq!(poll_interval_secs(&DaemonSettings::default()), 120);` at `collector.rs:272` inside `collector_loop_tick_writes_availability_snapshot`.

`README.md:30` today: `Optional \`poll_interval_secs\` in \`~/.daku/settings.json\` \`extra\` (default **120**). …` — after the flatten fix the supported shape is a top-level key.

Lint failures (`bun run lint` at HEAD):

```
scripts/release.ts:88:18  anti-slop(require-safety-comment-for-type-assertion)
scripts/release.ts:129:18 anti-slop(require-safety-comment-for-type-assertion)
scripts/delete-debug-app.ts:41:10 anti-slop(require-safety-comment-for-type-assertion)
scripts/delete-debug-app.ts:64:12 anti-slop(require-safety-comment-for-type-assertion)
```

The rule (`tools/oxlint/anti-slop/rules/require-safety-comment-for-type-assertion.ts:29`) accepts any comment containing `SAFETY:` that ends before the assertion's statement. The four sites:

```ts
// scripts/release.ts:88-90
const metadata = JSON.parse(
  await $`cargo metadata --no-deps --format-version 1`.quiet().text(),
) as CargoMetadata;

// scripts/release.ts:129
  const result = JSON.parse(resultText) as {
    id?: string;
    message?: string;
    status?: string;
  };

// scripts/delete-debug-app.ts:41
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;

// scripts/delete-debug-app.ts:64
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
```

fmt: `cargo fmt --all --check` reports diffs in `crates/daku-client/src/lib.rs`, `crates/daku-core/src/config.rs`, `crates/daku-core/src/drift.rs`, `crates/daku-core/src/server.rs`, `crates/daku-core/src/settings.rs` (import ordering / wrapping only).

Conventions: imperative commit summaries as on `main` (e.g. `Add outbound Signal on the shared collector loop (#29).`); tests live in `#[cfg(test)] mod tests` at the bottom of each file; temp files use `std::env::temp_dir().join(format!("daku-…-{}", Uuid::new_v4()))`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Workspace tests | `cargo test --workspace --no-fail-fast` | exit 0, all pass |
| Settings test only | `cargo test -p daku-core legacy_combined_settings` | 1 passed |
| Poll interval tests | `cargo test -p daku-core poll_interval` | all pass |
| Format check | `cargo fmt --all --check` | exit 0, no output |
| Format write | `cargo fmt --all` | exit 0 |
| Lint | `bun run lint` | exit 0 |
| Gate (after step 6) | `bun run check` | exit 0 |

Note: `clippy` is deliberately **not** in the gate yet — `main` has ~30 dead-code warnings inherited from the waku fork; a later dead-code plan owns them.

## Scope

**In scope** (the only files you should modify):
- `crates/daku-protocol/src/settings.rs`
- `crates/daku-core/src/collector.rs` (tests only)
- `scripts/release.ts`, `scripts/delete-debug-app.ts` (comments only)
- `package.json`
- `README.md` (line 30 only)
- `CLAUDE.md`, `AGENTS.md` (append one section, identical in both)
- `docs/agents/git-workflow.md` (rule 1 sentence)
- Files touched by `cargo fmt --all` (formatting only): `crates/daku-client/src/lib.rs`, `crates/daku-core/src/config.rs`, `crates/daku-core/src/drift.rs`, `crates/daku-core/src/server.rs`, `crates/daku-core/src/settings.rs`
- `plans/README.md` (status row)

**Out of scope** (do NOT touch):
- `crates/daku-core/src/settings.rs` logic (only fmt may reformat it). Do not "fix" the test by changing its assertions — the test encodes the intended contract.
- Any change to what `poll_interval_secs` means (min/max clamping is a separate plan).
- `clippy` warnings, dead code, `oxlint.config.ts` rules (do not weaken the lint config to make it pass).
- `.github/`, CI of any kind — forbidden by `docs/agents/git-workflow.md`.

## Git workflow

- Trunk-based on `main`; commit directly on `main` (or a disposable local branch merged locally). Do NOT push unless the operator asked.
- Suggested commits: (1) `Flatten DaemonSettings.extra and document poll_interval_secs shape.` (2) `Add bun run check as the local verification gate; fix fmt and lint.`

## Steps

### Step 1: Flatten `DaemonSettings.extra`

In `crates/daku-protocol/src/settings.rs`, change the `extra` field attributes to:

```rust
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
```

Nothing else in the file changes.

**Verify**: `cargo test -p daku-core legacy_combined_settings` → `test result: ok. 1 passed`.
**Verify**: `cargo test -p daku-protocol` → all pass (13 tests at HEAD; none of them serialise `DaemonSettings`, but confirm).

### Step 2: Pin the `poll_interval_secs` contract with tests

In `crates/daku-core/src/collector.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn poll_interval_secs_reads_top_level_json_key() {
        let settings: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs": 30}"#).unwrap();
        assert_eq!(poll_interval_secs(&settings), 30);
    }

    #[test]
    fn poll_interval_secs_falls_back_to_default_for_zero_or_non_number() {
        let zero: DaemonSettings = serde_json::from_str(r#"{"poll_interval_secs": 0}"#).unwrap();
        assert_eq!(poll_interval_secs(&zero), DEFAULT_POLL_INTERVAL_SECS);
        let text: DaemonSettings =
            serde_json::from_str(r#"{"poll_interval_secs": "fast"}"#).unwrap();
        assert_eq!(poll_interval_secs(&text), DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(poll_interval_secs(&DaemonSettings::default()), DEFAULT_POLL_INTERVAL_SECS);
    }
```

(`serde_json` and `DaemonSettings` are already imported in the module; `DEFAULT_POLL_INTERVAL_SECS` is `pub const` in the same file.)

**Verify**: `cargo test -p daku-core poll_interval` → `2 passed`.

### Step 3: Fix the README instruction

Replace `README.md` line 30's first sentence (`Optional \`poll_interval_secs\` in \`~/.daku/settings.json\` \`extra\` (default **120**).`) with:

```markdown
Optional poll cadence: put a top-level `"poll_interval_secs"` in `~/.daku/settings.json`, e.g. `{"poll_interval_secs": 60}` (default **120**; the daemon reads it at start — relaunch after editing).
```

Keep the rest of the paragraph unchanged.

**Verify**: `grep -n 'poll_interval_secs' README.md` → exactly one line, containing `{"poll_interval_secs": 60}`.

### Step 4: Add `SAFETY:` comments to the four type assertions

Add one comment line immediately above each statement (wording may vary; it must contain `SAFETY:` and state the checked invariant):

- `scripts/release.ts` before `const metadata = JSON.parse(`: `// SAFETY: \`cargo metadata --format-version 1\` is a stable schema; only \`packages[].name/version\` are read.`
- `scripts/release.ts` before `const result = JSON.parse(resultText) as {`: `// SAFETY: notarytool \`--output-format json\` documents \`id\`/\`message\`/\`status\`; all are read as optional.`
- `scripts/delete-debug-app.ts` before `if ((error as NodeJS.ErrnoException).code === "ENOENT") return;`: `// SAFETY: readdir rejects with a Node errno error; only \`.code\` is read.`
- `scripts/delete-debug-app.ts` before `if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;`: `// SAFETY: fs errors carry \`.code\`; any other error is rethrown.`

Do not change any code, only add comments. Do not edit `oxlint.config.ts`.

**Verify**: `bun run lint` → exit 0.

### Step 5: Format

Run `cargo fmt --all`.

**Verify**: `cargo fmt --all --check` → exit 0. `git diff --stat` shows only the five files listed under "Current state → fmt" (plus your earlier edits). If fmt touches other files, STOP (drift).

### Step 6: Add the `check` script and name it as the gate

`package.json` scripts — add one entry (keep the others):

```json
    "check": "cargo fmt --all --check && cargo test --workspace --no-fail-fast && oxlint -c oxlint.config.ts ."
```

Append the following section to **both** `CLAUDE.md` and `AGENTS.md` (identical text):

```markdown
### Verification gate

There is no CI. Before committing to `main`, run `bun run check` (fmt check + `cargo test --workspace` + oxlint) and require exit 0. Plans under `plans/` use it as a done criterion.
```

In `docs/agents/git-workflow.md`, rule 1, replace `Local verification (see each \`plans/*\` Done criteria / \`cargo test\` / \`cargo check\`) is the quality gate.` with `Local verification is the quality gate: \`bun run check\` (fmt check + \`cargo test --workspace\` + oxlint) must exit 0, plus each \`plans/*\` Done criteria.`

**Verify**: `bun run check` → exit 0. `diff CLAUDE.md AGENTS.md` → no output.

## Test plan

- New tests: `crates/daku-core/src/collector.rs` — `poll_interval_secs_reads_top_level_json_key`, `poll_interval_secs_falls_back_to_default_for_zero_or_non_number` (Step 2). Model after the existing assertion style in `collector.rs:272`.
- Existing test made green: `settings::tests::legacy_combined_settings_keep_only_daemon_fields`.
- Verification: `cargo test --workspace --no-fail-fast` → 0 failed (≥102 tests).

## Done criteria

- [ ] `bun run check` exits 0
- [ ] `cargo test --workspace --no-fail-fast` shows `0 failed` in every crate
- [ ] `grep -n 'flatten' crates/daku-protocol/src/settings.rs` → 1 match on the `extra` field
- [ ] `grep -c 'SAFETY:' scripts/release.ts scripts/delete-debug-app.ts` → `2` and `2`
- [ ] `grep -n '"check"' package.json` → 1 match
- [ ] `diff CLAUDE.md AGENTS.md` prints nothing; both contain `bun run check`
- [ ] `git status` shows no modified files outside the in-scope list
- [ ] `plans/README.md` status row for 011 updated

## STOP conditions

- `cargo test -p daku-core legacy_combined_settings` still fails after Step 1 (the contract has changed since this plan; report the assertion diff).
- `cargo fmt --all` modifies files other than the five listed.
- `bun run lint` reports errors from rules other than `require-safety-comment-for-type-assertion`, or at other locations.
- `bun` or `oxlint` are not installed and cannot be installed with `bun install` (report; do not swap the linter).
- Step 1 breaks a `daku-protocol` test (a serialisation test on `DaemonSettings` was added since this plan).

## Maintenance notes

- Any new user-facing daemon setting should be a **typed field** on `DaemonSettings`, not another `extra` key; `extra` exists for forward-compat only. (A follow-up may replace `extra` with `poll_interval_secs: u64`; then this README line and the two tests move with it.)
- Reviewers: check that the gate was not "made green" by weakening `oxlint.config.ts` or by editing test assertions.
- Deferred: `cargo clippy --workspace -- -D warnings` in the gate — add it once the dead-code plan lands and the warning count is 0.
