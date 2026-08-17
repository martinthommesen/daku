# Plan 043: Drift says *which* plugins differ — bounded mismatch list in the payload, rendered under the card

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/drift.rs src/dashboard_state.rs src/app.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition (plan 038 adds `card_detail` to
> `dashboard_state.rs`/`app.rs` — that specific change is expected).

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW–MED (payload grows per tick; bounded)
- **Depends on**: plans/011-green-baseline-check-gate.md (gate); soft: plans/038-signal-detail-render-error-and-drill-in.md (its detail line under cards is where the list renders — if 038 is not landed, render the list as extra card children as described in Step 3), plans/031-collector-consolidation-typed-signal-state.md (if landed first, `persist_drift_compare` may have moved — the diff/payload change is the same)
- **Category**: direction
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/64

## Why this matters

Post-clone drift is the first pain in the spec's driving list (`docs/spec/v1.md` §5: "post-clone drift"; "drift … first-class"). Today the drift Signal fetches up to 2×1000 rows per Environment (`crates/daku-core/src/drift.rs:19-21`), diffs them, and persists only `{mismatches, build_matches, truncated}` (`:310-330`); the card prints "N plugins differ" (`src/dashboard_state.rs:353-360`). "3 plugins differ" without names sends the Operator to two `sys_plugins` lists to eyeball. The prototype's variant B (`prototypes/environments-overview/index.html:403`) showed a "Cross-Environment drift" callout list — exactly this, and cheaper than the deferred matrix view (ADR-0005: matrix "may return later as a secondary drift view, not the home screen"). A bounded list per snapshot stays inside ADR-0007 (latest snapshot only, no history) and needs no protocol bump (`payload_json` is free-form).

## Current state

### `crates/daku-core/src/drift.rs`

```rust
// :384-388
pub struct PluginRecord { pub id: String, pub version: String, pub active: bool }

// :390-413
pub fn diff_plugin_inventory(source: &[PluginRecord], other: &[PluginRecord]) -> u64 {
    let source_by_id: HashMap<&str, &PluginRecord> = …;
    let other_by_id: HashMap<&str, &PluginRecord> = …;
    let mut mismatches = 0;
    for (id, source_record) in &source_by_id {
        match other_by_id.get(id) {
            Some(other_record)
                if other_record.version == source_record.version
                    && other_record.active == source_record.active => {}
            _ => mismatches += 1,
        }
    }
    for id in other_by_id.keys() {
        if !source_by_id.contains_key(id) { mismatches += 1; }
    }
    mismatches
}

// :310-330
fn persist_drift_compare(connection, environment_id, source: &EnvInventory, other: &EnvInventory, observed_at) -> io::Result<()> {
    let mismatches = diff_plugin_inventory(&source.plugins, &other.plugins);
    let build_matches = source.build == other.build;
    let payload = serde_json::json!({
        "mismatches": mismatches,
        "build_matches": build_matches,
        "truncated": source.truncated || other.truncated,
    });
    persistence::persist_signal_snapshot(connection, environment_id, DRIFT_SIGNAL_ID, observed_at, drift_state(build_matches, mismatches), &payload.to_string())
}
```

`EnvInventory { build: Option<String>, plugins: Vec<PluginRecord>, truncated: bool }` (`:31-35`). Tests (`mod tests`): helper `plugin(id, version)` (active `true`); `diff_plugin_inventory_identical_is_empty` asserts `== 0`; `diff_plugin_inventory_version_mismatch_counts_one` asserts `== 1`; `collect_pair(source_json, other_json)` runs a `prod`(source)/`test` pair with `DriftTransport`; `drift_signal_version_mismatch_is_degraded` asserts `payload["mismatches"] == 1` using fixtures `tests/fixtures/drift/plugins_a.json` (`com.example.plugin_a` `1.0.0`) vs `plugins_a_v2.json` (`1.1.0`); `HashMap` is imported at `:3`.

### `src/dashboard_state.rs`

- `:353-360` `summarize_payload("drift")`: `"source of truth"` / `"{count} plugins differ"` / `""`.
- `:285-293` `drift_mismatch(payload_json)` used by the compare strip (`build_matches == false` or `mismatches > 0`).
- After plan 038: `card_detail(signal_id)` / `detail_from_payload` render `error`/`detail`/`skipped`.

### `src/app.rs:246-286` `signal_card`

Renders dot + label, summary line, optional sparkline; after plan 038 also a `text_tertiary` detail line. Theme tokens: `theme.text_tertiary`, `theme.text_secondary`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Drift tests | `cargo test -p daku-core drift` | all pass |
| Model tests | `cargo test -p daku dashboard_state` | all pass |
| Client build | `cargo check -p daku` | exit 0 |
| Fixture run | `DAKU_UI_FIXTURE=1 bun run dev` | select "Test": drift card lists the fixture mismatches |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/drift.rs` (diff returns records; payload gains `mismatch_list` + `mismatch_list_truncated`; tests)
- `src/dashboard_state.rs` (`drift_mismatch_lines`, fixture, tests)
- `src/app.rs` (render the lines under the drift card)
- `plans/README.md`

**Out of scope**:
- Protocol/DTO changes (none needed).
- The matrix / secondary drift view (ADR-0005 defers it; revisit only if the list proves insufficient).
- Store-app vs plugin distinction, `latest_version` (store apps), pagination beyond the existing `truncated` flag, drift fetch throttling (plan 023).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested: `Persist and render the drift mismatch list.`

## Steps

### Step 1: Diff returns the mismatched records

In `crates/daku-core/src/drift.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginMismatch {
    pub id: String,
    /// `None` when the plugin is absent on that side.
    pub source_version: Option<String>,
    pub other_version: Option<String>,
}

/// Bound on the list persisted per snapshot; the count stays exact.
// ponytail: 50 rows keeps a 3-Environment payload under ~10 KB per tick.
pub const MISMATCH_LIST_LIMIT: usize = 50;

pub fn diff_plugin_inventory(source: &[PluginRecord], other: &[PluginRecord]) -> Vec<PluginMismatch>
```

Same loop as today, but push a `PluginMismatch` instead of incrementing: for a source id missing/differing on the other side → `{ id, source_version: Some(v), other_version: other.map(version) }` (encode an `active` difference as a version string suffix ` (inactive)` on the inactive side, so one field carries both); for ids only on the other side → `{ id, source_version: None, other_version: Some(v) }`. **Sort by `id`** before returning (HashMap order is not stable; tests and the UI want determinism).

`serde` is a dependency of `daku-core` (`crates/daku-core/Cargo.toml`) with `derive`; if `use serde::Serialize` is not present, the fully-qualified derive above works.

Update `persist_drift_compare`:

```rust
    let mismatch_list = diff_plugin_inventory(&source.plugins, &other.plugins);
    let mismatches = mismatch_list.len() as u64;
    let build_matches = source.build == other.build;
    let payload = serde_json::json!({
        "mismatches": mismatches,
        "build_matches": build_matches,
        "truncated": source.truncated || other.truncated,
        "mismatch_list": &mismatch_list[..mismatch_list.len().min(MISMATCH_LIST_LIMIT)],
        "mismatch_list_truncated": mismatch_list.len() > MISMATCH_LIST_LIMIT,
    });
```

Update tests: the two `diff_plugin_inventory_*` tests assert `.len()` (0 / 1) and, for the mismatch case, the record `{ id: "com.example.plugin_a", source_version: Some("1.0.0"), other_version: Some("1.1.0") }`. Add `diff_plugin_inventory_reports_missing_both_ways_sorted` (one id only in source, one only in other, one differing → 3 rows sorted by id with the expected `None`s). Extend `drift_signal_version_mismatch_is_degraded` to assert `payload["mismatch_list"][0]["id"] == "com.example.plugin_a"`, `payload["mismatch_list_truncated"] == false`. Add `diff_plugin_inventory_list_is_bounded`: 60 differing ids → payload built via `persist_drift_compare` (or a small helper) has 50 entries, `mismatches == 60`, `mismatch_list_truncated == true`.

**Verify**: `cargo test -p daku-core drift` → all pass.

### Step 2: Model — lines for the card

In `src/dashboard_state.rs` add:

```rust
/// Up to `limit` human lines "id: 1.0.0 → 1.1.0" / "id: missing here" / "id: only here"
/// for the selected Environment's drift snapshot; empty for the source or when healthy.
pub fn drift_mismatch_lines(&self, limit: usize) -> Vec<String>
```

reading `payload["mismatch_list"]` and appending `"… and N more"` when `mismatches > lines shown` (covers both the 50-cap and the UI limit). Fixture: change the `test` drift snapshot (`:430-434`) to include `"mismatch_list":[{"id":"com.example.plugin_a","source_version":"1.0.0","other_version":"1.1.0"},{"id":"com.example.plugin_b","source_version":"2.0.0","other_version":null},{"id":"com.example.plugin_c","source_version":null,"other_version":"0.9.0"}],"mismatch_list_truncated":false` (keep `"mismatches":3`). Tests: `drift_mismatch_lines_formats_three_kinds` (after `select("test")`, `limit 10` → 3 lines: `com.example.plugin_a: 1.0.0 → 1.1.0`, `com.example.plugin_b: missing here`, `com.example.plugin_c: only here`); `drift_mismatch_lines_respects_limit` (`limit 2` → 2 lines + `… and 1 more`); `prod` (source) → empty. Confirm `dashboard_state_compare_strip_build_mismatch` still passes (it reads `mismatches`/`build_matches` only).

**Verify**: `cargo test -p daku dashboard_state` → pass.

### Step 3: Render

In `src/app.rs` `signal_card`, when `card.signal_id == "drift"`, add the lines (limit 5) as `text_size(px(11.0)).text_color(theme.text_secondary)` children after the summary/detail line — one `div` per line, `mt(px(4.0))` on the block. If plan 038 has landed, place them right after its detail child; the two are independent.

**Verify**: `cargo check -p daku` → exit 0; `DAKU_UI_FIXTURE=1 bun run dev` → select "Test": three lines under "3 plugins differ".

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `drift.rs`: updated `diff_plugin_inventory_identical_is_empty`, `diff_plugin_inventory_version_mismatch_counts_one`; new `diff_plugin_inventory_reports_missing_both_ways_sorted`, `diff_plugin_inventory_list_is_bounded`; extended `drift_signal_version_mismatch_is_degraded`.
- `dashboard_state.rs`: `drift_mismatch_lines_formats_three_kinds`, `drift_mismatch_lines_respects_limit`.
- Manual fixture run.

## Done criteria

- [ ] `grep -n 'pub struct PluginMismatch\|MISMATCH_LIST_LIMIT\|mismatch_list_truncated' crates/daku-core/src/drift.rs` → ≥3 matches
- [ ] `grep -n 'pub fn drift_mismatch_lines' src/dashboard_state.rs` → 1; `grep -n 'drift_mismatch_lines' src/app.rs` → 1
- [ ] `cargo test -p daku-core drift` and `cargo test -p daku dashboard_state` pass with the new tests
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 043 updated

## STOP conditions

- `diff_plugin_inventory` or `persist_drift_compare` no longer match the excerpts (e.g. plan 031 moved them) — apply the same change at the new location if mechanical; otherwise report.
- `serde` derive is unavailable in `daku-core` (it is a dependency at HEAD — if removed, build the JSON with `serde_json::json!` per record instead).
- The fixture change breaks a test other than the ones named — report rather than editing that assertion.

## Maintenance notes

- If real inventories routinely exceed 50 mismatches (fresh clones should be near 0; a stale dev may be large), raise `MISMATCH_LIST_LIMIT` modestly or add a "show more" in the drill-in pane (plan 038's decision) — do not remove the bound.
- Plan 023 (throttled inventory fetch) keeps the last inventory in memory; the list computation is unaffected.
- Reviewers: check the sort, the `Option` encoding for one-sided plugins, and that `mismatches` stays the exact count while the list is capped.
