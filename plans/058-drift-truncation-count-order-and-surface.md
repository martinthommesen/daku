# Plan 058: Drift knows when it only saw part of the inventory, and says so

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/drift.rs src/dashboard_state.rs src/app.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED
- **Depends on**: `plans/049-drift-build-tri-state-skip-reason-and-asleep-gate.md`
- **Category**: bug
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Drift compares two 1000-row plugin pages. Three things go wrong when an
Environment actually has that many rows.

1. **The truncation flag under-counts.** `truncated` is computed from
   `records.len()`, which is the count **after** filtering — `plugin_record`
   drops every row with a missing or empty `id`/`scope`. A full 1000-row page
   containing three unusable rows yields `records.len() == 997`, the
   `>= PLUGIN_PAGE_LIMIT` test fails, and if `X-Total-Count` is absent (it is
   not guaranteed on Table API responses) the truncation is never noticed.
2. **The two pages are not the same slice.** Neither `SYS_PLUGINS_PATH` nor
   `SYS_STORE_APP_PATH` carries an `ORDERBY`, so once a page is capped, the
   source's 1000 rows and the target's 1000 rows are arbitrary and different
   subsets. `diff_plugin_inventory` then reports plugins present on *both*
   sides as one-sided mismatches.
3. **When it does know, it tells nobody.** `truncated` reaches the payload and
   stops there. Nothing in `src/` reads it — `src/dashboard_state.rs` reads
   `mismatch_list_truncated`, which is a *different* flag about the 50-row
   drill-in bound. So a partial diff renders as an exact one.

Together: an instance with ≥1000 `sys_plugins` rows can show a large phantom
mismatch count, hold the Environment at Degraded, and give the Operator no way
to tell that the number is not real.

**Honest scoping**: the miscount at `drift.rs:235` is certain from the code.
Whether any real Environment exceeds 1000 `sys_plugins` rows is **not verified**
— Step 1 settles that before the rest of the work is worth doing.

## Current state

**`crates/daku-core/src/drift.rs:19-22`** — the paths, with no `ORDERBY`:

```rust
pub const PLUGIN_PAGE_LIMIT: usize = 1000;
pub const SYS_PLUGINS_PATH: &str =
    ...
pub const SYS_STORE_APP_PATH: &str = "/api/now/table/sys_store_app?sysparm_fields=scope,id,version,latest_version,active&sysparm_limit=1000";
```

**`crates/daku-core/src/drift.rs:225-238`** — the post-filter count:

```rust
    let records = parse_plugin_records(response.body.as_bytes())?;
    let total = response
        .header("X-Total-Count")
        .and_then(|value| value.parse::<u64>().ok());
    let truncated = records.len() >= PLUGIN_PAGE_LIMIT
        || total.is_some_and(|count| count > records.len() as u64);
    Ok((records, truncated))
```

**`crates/daku-core/src/drift.rs`** — the filter that makes the count wrong:

```rust
fn parse_plugin_records(body: &[u8]) -> anyhow::Result<Vec<PluginRecord>> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let rows = value
        .get("result")
        .and_then(|result| result.as_array())
        .ok_or_else(|| anyhow!("plugin response missing result array"))?;
    Ok(rows.iter().filter_map(plugin_record).collect())
}
```

```rust
fn plugin_record(row: &serde_json::Value) -> Option<PluginRecord> {
    let id = row
        .get("id")
        .and_then(|value| value.as_str())
        .or_else(|| row.get("scope").and_then(|value| value.as_str()))
        .filter(|id| !id.is_empty())?
        .to_owned();
```

**`crates/daku-core/src/drift.rs:341-350`** — `truncated` in the payload,
alongside the *unrelated* `mismatch_list_truncated`:

```rust
    let payload = serde_json::json!({
        "mismatches": mismatches,
        "build_matches": build_matches,
        "truncated": source.truncated || other.truncated,
        "mismatch_list": &mismatch_list[..mismatch_list.len().min(MISMATCH_LIST_LIMIT)],
        "mismatch_list_truncated": mismatch_list.len() > MISMATCH_LIST_LIMIT,
    });
```

**`src/dashboard_state.rs:294`** — the only `truncated` the client reads is the
drill-in one:

```rust
                    truncated: value.get("mismatch_list_truncated")
```

### Constraints you must honor

- **Plan 023's cache is 30 minutes and must not cache failures.** Adding
  requests inside `env_inventory` multiplies against that cache, not against
  every tick — but real pagination would still double or triple the request
  count on a cache miss. That is why pagination is **out of scope** here.
- `plans/README.md` records splitting `drift.rs` as considered and rejected.
- **`CONTEXT.md`**: the **Signal card** shows "its state, a one-line summary,
  and any diagnostic detail the daemon persisted". A truncation caveat belongs
  in that detail line, not as a new card.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Drift tests | `cargo test -p daku-core drift` | all pass |
| Client tests | `cargo test -p daku dashboard_state` | all pass |

## Scope

**In scope**:
- `crates/daku-core/src/drift.rs`
- `src/dashboard_state.rs`
- `src/app.rs` (only if the detail line needs a render change)

**Out of scope** (do NOT touch):
- **Real pagination.** Following `Link` headers or looping `sysparm_offset` is
  a genuine option but it doubles request volume against a 30-minute cache and
  is a separate decision. This plan makes truncation *correctly detected and
  visible*, not impossible.
- `MISMATCH_LIST_LIMIT` and `mismatch_list_truncated` — plan 043's bound is
  correct and rendered.
- `build_matches` — plan 049 owns it.
- `diff_plugin_inventory`'s algorithm.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Count raw plugin rows for truncation, order the page, and surface a partial diff (#83).`

## Steps

### Step 1: Find out whether this fires at all

**Operator-run — you cannot do this.** Ask the Operator to run, against each
Environment:

```
GET /api/now/stats/sys_plugins?sysparm_count=true
GET /api/now/stats/sys_store_app?sysparm_count=true
```

- If every count is comfortably under 1000 → **report that and stop after
  Step 2.** Steps 3 and 4 are not worth their risk for a case that cannot
  occur; record the counts in `docs/research/servicenow-signals.md` and mark
  this plan DONE with that note. Step 2 alone is a correct, cheap fix.
- If any count is at or near 1000 → do the whole plan.
- If the Operator cannot run it → do the whole plan (the cheap correctness fix
  plus the ordering and the caveat), and say in your report that the trigger is
  unverified.

### Step 2: Count raw rows, not surviving records

Change `parse_plugin_records` to return both the records and the raw row count,
and compute `truncated` from the raw count:

```rust
fn parse_plugin_records(body: &[u8]) -> anyhow::Result<(Vec<PluginRecord>, usize)> {
    ...
    let raw_rows = rows.len();
    Ok((rows.iter().filter_map(plugin_record).collect(), raw_rows))
}
```

```rust
    let truncated = raw_rows >= PLUGIN_PAGE_LIMIT
        || total.is_some_and(|count| count > raw_rows as u64);
```

**Verify**: `cargo test -p daku-core drift` → all pass.

### Step 3: Make the page deterministic

Add `^ORDERBYid` to `SYS_PLUGINS_PATH` and `^ORDERBYscope` (or `^ORDERBYid`,
matching whichever field that table actually exposes — check
`SYS_STORE_APP_PATH`'s `sysparm_fields`) to `SYS_STORE_APP_PATH`, so a capped
page is the *same* slice on every Environment. Two capped-but-aligned pages
diff meaningfully; two capped-and-arbitrary pages do not.

The existing tests assert on the request URL (the `HttpTransport` stubs check
`request.url.contains(...)`) — update those assertions to include the new
clause, and add one asserting the `ORDERBY` is present so it cannot be dropped
silently.

**Verify**: `cargo test -p daku-core drift` → all pass.

### Step 4: Say so on the card

The daemon already persists `truncated`. Make the client read it: in
`src/dashboard_state.rs`'s `detail_from_payload` (or the drift arm of
`summarize_payload` — pick the one that already carries diagnostics; read both
first), append a caveat when `truncated` is `true`, in the register of the
existing phrases, e.g. "partial inventory — plugin counts may be incomplete".

**Verify**: `cargo test -p daku dashboard_state` → all pass.
`bun run check` → exit 0.

## Test plan

New tests in `crates/daku-core/src/drift.rs` `mod tests`, using `TempDb` and the
existing scripted transports:

1. `drift_truncation_counts_unusable_rows` — a response with exactly
   `PLUGIN_PAGE_LIMIT` rows of which several have an empty `id` and no `scope`;
   assert the persisted payload has `truncated == true`. **This test fails
   before Step 2** — confirm that, and say so in your report.
2. `drift_short_page_is_not_truncated` — a small response; assert
   `truncated == false` (guards against Step 2 over-reporting).
3. `drift_total_count_header_marks_truncation` — a short page plus
   `X-Total-Count` greater than the rows returned.
4. `drift_requests_a_deterministic_order` — assert both request URLs contain
   the `ORDERBY` clause.

One test in `src/dashboard_state.rs` `mod tests`: a drift payload with
`"truncated": true` produces a detail line containing the caveat, and one with
`"truncated": false` does not.

**Verification**: `cargo test -p daku-core drift` → all pass, +4 tests;
`cargo test -p daku dashboard_state` → all pass, +1 test.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "records.len() >= PLUGIN_PAGE_LIMIT" crates/daku-core/src/drift.rs`
      → no matches (replaced by the raw count)
- [ ] `grep -n "ORDERBY" crates/daku-core/src/drift.rs` → matches in both path
      constants — **unless** Step 1 showed the counts are far under the limit and
      you stopped after Step 2, which your report must then state
- [ ] `grep -n '"truncated"' src/dashboard_state.rs` → at least one match
      (distinct from `mismatch_list_truncated`)
- [ ] Your report states the outcome of Step 1 and confirms test 1 fails before
      Step 2
- [ ] `cargo test -p daku-core drift` → all pass
- [ ] `git diff --name-only` lists only the in-scope files and `plans/README.md`
- [ ] `plans/README.md` status row for 058 updated to DONE, with a one-line note
      if you stopped after Step 2

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- Test 1 passes *before* Step 2 — then the miscount is not what this plan
  describes and the analysis is wrong.
- Adding `ORDERBY` changes what the existing fixtures produce in a way that is
  not just a URL assertion.
- You find yourself implementing pagination. It is out of scope; report it as a
  follow-up with the row counts from Step 1 as justification.

## Maintenance notes

- **Two different `truncated` flags now reach the client**: `truncated` (the
  inventory page was capped) and `mismatch_list_truncated` (the 50-row drill-in
  bound, plan 043). They mean different things and must not be merged. That is
  the confusion to catch in review.
- If Step 1's counts ever approach 1000, pagination becomes the right answer and
  this plan's caveat becomes a stopgap. Record the counts so the next reader has
  the number.
- `PLUGIN_PAGE_LIMIT` and the `sysparm_limit=1000` in both path constants must
  agree. Consider building the paths from the constant, as plan 057 does for
  `CLONE_PAGE_LIMIT`, if you are editing them anyway.
