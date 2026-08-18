# Plan 057: "No clone in the page I read" stops looking like "never cloned"

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/last_clone.rs src/dashboard_state.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: `plans/048-last-clone-persists-every-target-on-failure.md`
- **Category**: bug
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Last-clone answers one question: *is this Environment's copy of prod stale?*
Today it can answer "never cloned" about an Environment cloned last month, and
nothing distinguishes that from the truth.

The collector reads **one page of the ten newest completed clones across all
targets** and never checks whether there were more rows. A target that does not
appear in that page gets `{"supported": true, "completed": null}` — byte-identical
to the payload written when no clone has ever completed.

Ten rows sounds generous until the clone volumes are uneven: a nightly-refreshed
test plus a yearly-refreshed dev is enough for test's ten most recent clones to
push dev out of the page entirely. Then dev's card reads "never cloned",
permanently, and the Signal's whole purpose is inverted — the *most* stale
Environment is the one it silently gives up on.

There is a second cause with the same symptom, and it is not confirmed from
inside this repo: the query filters `state=Completed` against the raw field
value. `docs/research/servicenow-signals.md` records the `clone_instance` field
shape as **[unverified]** with no live instance ever probed. If the stored value
is not the literal `Completed` (ServiceNow often stores an integer behind a
display label), the API returns zero rows and **every** target renders "never
cloned" from day one. Step 1 resolves that before any code changes.

## Current state

**`crates/daku-core/src/last_clone.rs:16-17`** — one page, ten rows, all targets:

```rust
pub const LAST_CLONE_SIGNAL_ID: &str = "last_clone";
pub const CLONE_INSTANCE_PATH: &str = "/api/now/table/clone_instance?sysparm_query=state=Completed^ORDERBYDESCcompleted&sysparm_fields=state,completed,target&sysparm_limit=10";
```

**`crates/daku-core/src/last_clone.rs:24-52`** — the parser. `Some(vec![])`
already means "no clone has ever completed"; there is no third state for "I
could not see far enough back":

```rust
/// Newest Completed clone per target, in response order (already newest-first).
/// `None` = the source cannot answer (non-200 or unreadable body) — nothing is
/// then known about any target. `Some(vec![])` = no clone has ever completed.
pub fn parse_last_clones(status: u16, body: &str) -> Option<Vec<CloneRow>> {
    if status != 200 {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let rows = value.get("result")?.as_array()?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut newest = Vec::new();
    for row in rows {
        ...
        if seen.insert(target.to_ascii_lowercase()) {
            newest.push(CloneRow { target: target.to_owned(), completed: completed.to_owned() });
        }
    }
    Some(newest)
}
```

**`crates/daku-core/src/last_clone.rs`** `persist_clone_target` — the collision:

```rust
    let payload = match row {
        Some(row) => { ... }
        None => serde_json::json!({ "supported": true, "completed": null }),
    };
```

**The mechanism that already exists, in the sibling collector —
`crates/daku-core/src/drift.rs:229-236`:**

```rust
    let total = response
        .header("X-Total-Count")
        .and_then(|value| value.parse::<u64>().ok());
    let truncated = records.len() >= PLUGIN_PAGE_LIMIT
        || total.is_some_and(|count| count > records.len() as u64);
```

So `HttpResponse::header` is available on the same client. It was simply never
applied here.

**`src/dashboard_state.rs`** — `summarize_payload`'s `"last_clone"` arm and
`detail_from_payload` read `completed` / `age_days` / `supported` / `skipped`.
Read both in full before editing.

### Constraints you must honor

- **`CONTEXT.md`**: **Signal**, **Environment**, **Operator**. The clone
  **source** is the Environment with `clone_source: true`; the others are clone
  **targets**.
- `docs/research/servicenow-signals.md` marks the `clone_instance` field shape
  unverified. **Do not silently "fix" the query based on a guess** — Step 1 is
  an Operator-run verification, and if it cannot be run, this plan stops at
  Step 2 with the truncation fix only.
- ADR-0007: persist the **latest snapshot** per Signal × Environment, prune
  aggressively. A third payload state adds no rows.
- The Signal is exempt from the health rollup (`crates/daku-core/src/health.rs:32`),
  so nothing here can move an Environment's health.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Signal tests | `cargo test -p daku-core last_clone` | all pass |
| Client tests | `cargo test -p daku dashboard_state` | all pass |
| Operator probe | see Step 1 | Operator-run against a real Environment |

## Scope

**In scope**:
- `crates/daku-core/src/last_clone.rs`
- `src/dashboard_state.rs` (the last-clone summary/detail arms and their tests)
- `crates/daku-core/tests/fixtures/last_clone/` (new fixture)
- `docs/research/servicenow-signals.md` (Step 1's result only)

**Out of scope** (do NOT touch):
- The failure branches and `skip_targets` — plan 048 owns them.
- The availability gate — plan 049 owns it.
- `crates/daku-core/src/drift.rs` — plan 058 owns its truncation.
- `age_days` and `days_from_civil` — correct and pinned by tests.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Distinguish a truncated clone page from "never cloned" (#82).`

## Steps

### Step 1: Verify the `state=Completed` filter against a real Environment

**This step is Operator-run — you cannot do it, and you must not guess.**
Put this in your report and ask the Operator to run it against a clone-source
Environment, using their own credentials:

```
GET /api/now/table/clone_instance?sysparm_query=state=Completed&sysparm_limit=1
GET /api/now/table/clone_instance?sysparm_fields=state,completed,target&sysparm_limit=5
```

The second call shows what `state` values are actually stored. Record the answer
in `docs/research/servicenow-signals.md`, replacing the `[unverified]` marker
for `clone_instance`.

- If `state=Completed` returns rows → the filter is right; proceed to Step 2 and
  change nothing about the query's filter.
- If it returns nothing while the unfiltered call shows completed clones → the
  filter is wrong. **STOP and report**, with the observed values. Correcting the
  filter is a bigger change than this plan and needs the real values in hand.
- If the Operator cannot run it → proceed to Step 2 and 3 only, and say clearly
  in your report that the filter remains unverified.

### Step 2: Report how many rows there were

Change `parse_last_clones` to return the raw row count alongside the deduplicated
newest-per-target list — e.g. `Option<(Vec<CloneRow>, usize)>`, or a small
struct if that reads better. Keep the existing doc comment's `None` /
`Some(empty)` meanings intact and document the new field.

In `collect`, compute truncation the way `drift.rs` already does — `raw_rows >= CLONE_PAGE_LIMIT`
or `X-Total-Count` greater than the rows read — using a new
`pub const CLONE_PAGE_LIMIT: usize = 10;` that `CLONE_INSTANCE_PATH`'s
`sysparm_limit` is written from, so the two can never disagree.

**Verify**: `cargo test -p daku-core last_clone` → all pass.

### Step 3: Give an unmatched target a third state

In `persist_clone_target`, take a `truncated: bool` and split the `None` arm:

```rust
        // The page was full, so this target may simply be older than the ten
        // newest clones. Saying "never" would be a confident wrong answer.
        None if truncated => serde_json::json!({ "supported": true, "completed": null, "unknown": "older_than_page" }),
        None => serde_json::json!({ "supported": true, "completed": null }),
```

Then add an arm in `src/dashboard_state.rs` so the card says so — something in
the register of its neighbours, e.g. "not in the last 10 clones". Read the
existing `"last_clone"` arm of `summarize_payload` and match its style; do not
invent a new phrasing convention.

**Verify**: `cargo test -p daku-core last_clone` → all pass.
`cargo test -p daku dashboard_state` → all pass. `bun run check` → exit 0.

## Test plan

New fixture: `crates/daku-core/tests/fixtures/last_clone/full_page.json` — ten
completed clone rows all targeting `acme-test`, none targeting `acme-dev`. Model
its shape on the existing `two_targets.json`.

New tests in `crates/daku-core/src/last_clone.rs` `mod tests`, using the
existing `collect_last_clone` helper and `TempDb` (**not** a hand-rolled temp
path):

1. `last_clone_full_page_marks_an_unmatched_target_unknown` — with
   `full_page.json`, assert `test` gets a real `completed`, and `dev`'s payload
   has `unknown == "older_than_page"` and **not** a bare `completed: null`.
2. `last_clone_short_page_still_says_never_cloned` — with the existing
   `completed.json` (fewer than ten rows), assert `dev` keeps today's exact
   payload: `supported: true`, `completed: null`, **no** `unknown` key. This
   pins that the truthful answer is unchanged.
3. `last_clone_uses_total_count_when_present` — a response carrying
   `X-Total-Count` greater than the rows returned marks unmatched targets
   unknown even on a short page.
4. `parse_last_clones_reports_the_raw_row_count` — a direct unit test on the
   parser: duplicate targets deduplicate in the list but still count toward the
   raw total.

One test in `src/dashboard_state.rs` `mod tests` for the new card phrase.

**Verification**: `cargo test -p daku-core last_clone` → all pass, +4 tests.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "CLONE_PAGE_LIMIT" crates/daku-core/src/last_clone.rs` → ≥ 2
      matches, and `CLONE_INSTANCE_PATH`'s `sysparm_limit` is built from it
- [ ] `grep -n "X-Total-Count" crates/daku-core/src/last_clone.rs` → ≥ 1 match
- [ ] `grep -n "older_than_page" crates/daku-core/src/last_clone.rs src/dashboard_state.rs`
      → matches in both files
- [ ] `cargo test -p daku-core last_clone` → all pass, four more tests
- [ ] `grep -rn "temp_dir()" crates/daku-core/src/last_clone.rs` → no matches
- [ ] Your report states the outcome of Step 1, including "not run" if the
      Operator could not run it
- [ ] `plans/README.md` status row for 057 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- **Step 1 shows `state=Completed` returns nothing** while completed clones
  exist. Do not change the filter on a guess.
- Test 2 fails — that means the truthful "never cloned" answer changed, which is
  a regression, not a fix.
- You are tempted to switch to one request per target
  (`target=<name>^state=Completed&sysparm_limit=1`). That removes the truncation
  class entirely but multiplies request count by the number of Environments;
  it is a real option, but it is a **design change** that needs the Operator's
  call. Report it as a recommendation instead of implementing it.

## Maintenance notes

- Three payload states now exist for a target: a real `completed`, "never
  cloned", and "older than the page". Any future reader must handle all three;
  collapsing the last two back together is the regression to watch for.
- `CLONE_PAGE_LIMIT` and `CLONE_INSTANCE_PATH` must move together — that is why
  the path is built from the constant.
- The N-requests-per-target alternative stays on the table. If Environments grow
  past a handful, or if "older than the page" starts showing up routinely, that
  is the signal to take it.
- Step 1's answer belongs in `docs/research/servicenow-signals.md` whatever it
  is — a verified negative is worth as much as a verified positive here.
