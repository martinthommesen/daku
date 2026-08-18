# Plan 066: The MID/ECC Drill-in shows which MID is down, from data daku already fetches

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/mid_ecc.rs src/dashboard_state.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: direction
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

A dead MID server is one of the named pains in `docs/spec/v1.md` §5. Today the
MID/ECC **Signal card** says "2 unhealthy" and the **Drill-in** — the region
plan 046 built for exactly this — shows one line of text, because `drill_in`
has no arm for `mid_ecc` and falls through to `drill_in_text`. So the Operator
learns *how many*, then opens ServiceNow to learn *which host*.

The data is already in hand. `ECC_AGENTS_PATH` fetches
`status,validated,version,host_name` for every agent; `classify_mid_agents`
counts them and throws the rows away, every tick, on every Environment.

`docs/research/signal-drill-in.md` recommends precisely this — *"do (a)+(b) for
MID/ECC only"*. Part (b), the drill-in region, landed as plan 046; part (a),
persisting the unhealthy agents, never did. This is that unlanded half.

**Zero additional HTTP requests.** Drift already does the same thing with
`mismatch_list`, so both the persistence pattern and the render path exist.

## Current state

**`crates/daku-core/src/mid_ecc.rs:12`** — the fields are already requested:

```rust
pub const ECC_AGENTS_PATH: &str = "/api/now/table/ecc_agent?sysparm_fields=status,validated,version,host_name&sysparm_limit=10000";
```

**`crates/daku-core/src/mid_ecc.rs:17-42`** — and immediately reduced to two
numbers:

```rust
pub fn classify_mid_agents(body: &[u8]) -> anyhow::Result<(u64, u64)> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let agents = value
        .get("result")
        .and_then(|result| result.as_array())
        .ok_or_else(|| anyhow!("ecc_agent response missing result array"))?;
    let total = agents.len() as u64;
    let unhealthy = agents
        .iter()
        .filter(|agent| !mid_agent_healthy(agent))
        .count() as u64;
    Ok((total, unhealthy))
}

fn mid_agent_healthy(agent: &serde_json::Value) -> bool {
    agent.get("status").and_then(|status| status.as_str()) == Some("Up")
        && is_validated_true(agent.get("validated"))
}
```

**`crates/daku-core/src/mid_ecc.rs` `probe`** — the payload the card reads:

```rust
        Ok(Observation {
            state: mid_ecc_state(agents_unhealthy, ecc_error, ecc_output_ready),
            payload: serde_json::json!({
                "agents_total": agents_total,
                "agents_unhealthy": agents_unhealthy,
                "ecc_output_ready": ecc_output_ready,
                "ecc_error": ecc_error,
            }),
            sample: None,
        })
```

**`src/dashboard_state.rs`** — the fallthrough, and the pattern to copy from the
drift arm directly above it:

```rust
            "drift" => {
                let Some(list) = value.get("mismatch_list").and_then(|item| item.as_array()) else {
                    return self.drill_in_text(signal_id);
                };
                DrillIn::Rows {
                    headers: vec!["Plugin", "Source", "Here"],
                    rows: list
                        .iter()
                        .take(DRILL_IN_ROW_LIMIT)
                        .map(|entry| {
                            vec![
                                text(entry, "id"),
                                text(entry, "source_version"),
                                text(entry, "other_version"),
                            ]
                        })
                        .collect(),
                    truncated: value.get("mismatch_list_truncated")
                        == Some(&serde_json::Value::Bool(true))
                        || list.len() > DRILL_IN_ROW_LIMIT,
                }
            }
            ...
            _ => self.drill_in_text(signal_id),
```

**`src/dashboard_state.rs`** — the render shape, already handled by `src/app.rs`:

```rust
pub enum DrillIn {
    Rows {
        headers: Vec<&'static str>,
        rows: Vec<Vec<String>>,
        truncated: bool,
    },
    Trend(Vec<f64>),
    Text(String),
    Empty,
}
```

**The bound to copy** — `crates/daku-core/src/drift.rs`:
`pub const MISMATCH_LIST_LIMIT: usize = 50;`

### Constraints you must honor

- **ADR-0007**: persist the latest snapshot per Signal × Environment and *prune
  aggressively*. A bounded list inside the existing snapshot payload adds no
  rows and no history — that is why this is cheap. **Bound it** (10 is plenty
  for "which MID is down"; drift uses 50 for a different shape).
- **`CONTEXT.md`** › Screen: the **Drill-in** "shows that Signal's rows
  (mismatched plugins, clone rows, a larger trend) with a link into the
  Environment itself." Unhealthy MID agents are exactly that shape.
- **No new HTTP requests.** `plans/README.md` records the Aggregate-API
  alternative for MID as considered and rejected precisely because the single
  list call "keeps per-MID fields DIR-01/038 wants" — this plan is the payoff
  for that decision. Do not add a call.
- **`plans/README.md` › Public hygiene**: MID `host_name` values are
  Operator-infrastructure hostnames. They may render in the app and persist in
  the local SQLite; they must **never** appear in a test fixture, a plan, a
  commit message, or the daemon log. Use obviously-fake names like `mid-a` in
  tests.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Signal tests | `cargo test -p daku-core mid_ecc` | all pass |
| Client tests | `cargo test -p daku drill_in` | all pass |
| Visual check | `DAKU_UI_FIXTURE=1 bun run dev` | Operator-run |

## Scope

**In scope**:
- `crates/daku-core/src/mid_ecc.rs`
- `src/dashboard_state.rs`
- `crates/daku-core/tests/fixtures/mid_ecc/` (extend an existing fixture)

**Out of scope** (do NOT touch):
- `ECC_AGENTS_PATH`, `ECC_OUTPUT_READY_PATH`, `ECC_ERROR_PATH` — the fields are
  already right and the request count must not change.
- `mid_ecc_state` and `ECC_READY_DEGRADED_AT` — the health mapping is unchanged.
- `src/app.rs` — `DrillIn::Rows` already renders, including the `truncated`
  caveat.
- `ecc_agent_issue` (the 30-day issue table `docs/research/servicenow-signals.md`
  mentions). That is a second data source and a separate decision.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Persist the unhealthy MID agents so the Drill-in can name them (#91).`

## Steps

### Step 1: Return the unhealthy agents, not just the count

Change `classify_mid_agents` to return the unhealthy rows alongside the totals —
e.g. `(u64, Vec<MidAgent>)` with a small struct carrying `host_name`, `status`
and `version`, or `(u64, u64, Vec<serde_json::Value>)` if that reads better.
Keep `mid_agent_healthy` and `is_validated_true` exactly as they are; the
health rule is not changing.

A missing or empty `host_name` should render as an em-dash rather than being
dropped — an unnamed unhealthy agent still counts, and silently omitting it
would make the list disagree with `agents_unhealthy`.

**Verify**: `cargo test -p daku-core mid_ecc` → all pass (existing tests assert
the counts; those must not change).

### Step 2: Persist a bounded list

Add to the payload in `probe`:

```rust
pub const UNHEALTHY_LIST_LIMIT: usize = 10;
```

```rust
                "agents_unhealthy_list": &unhealthy_list[..unhealthy_list.len().min(UNHEALTHY_LIST_LIMIT)],
                "agents_unhealthy_list_truncated": unhealthy_list.len() > UNHEALTHY_LIST_LIMIT,
```

Each entry carries `host_name`, `status`, `version`. Mirror drift's key naming
(`<thing>_list` / `<thing>_list_truncated`) so the two Signals read alike.

**Verify**: `cargo test -p daku-core mid_ecc` → all pass.

### Step 3: Give the Drill-in an arm

In `src/dashboard_state.rs`'s `drill_in`, add a `"mid_ecc"` arm directly
modelled on the `"drift"` arm above it:

```rust
            "mid_ecc" => {
                let Some(list) = value
                    .get("agents_unhealthy_list")
                    .and_then(|item| item.as_array())
                else {
                    return self.drill_in_text(signal_id);
                };
                DrillIn::Rows {
                    headers: vec!["MID", "Status", "Version"],
                    rows: list
                        .iter()
                        .take(DRILL_IN_ROW_LIMIT)
                        .map(|entry| {
                            vec![
                                text(entry, "host_name"),
                                text(entry, "status"),
                                text(entry, "version"),
                            ]
                        })
                        .collect(),
                    truncated: value.get("agents_unhealthy_list_truncated")
                        == Some(&serde_json::Value::Bool(true))
                        || list.len() > DRILL_IN_ROW_LIMIT,
                }
            }
```

An **empty** list (every agent healthy) must fall through to
`drill_in_text`, not render an empty table — check what the drift arm does for
the equivalent case and match it.

**Verify**: `cargo test -p daku drill_in` → all pass. `bun run check` → exit 0.

### Step 4: Show it in the fixture

Add an unhealthy MID to `fixture_events()` in `src/dashboard_state.rs` so
`DAKU_UI_FIXTURE=1` exercises the new region. **Fake hostnames only** —
`mid-a`, `mid-b`.

**Verify**: Operator runs `DAKU_UI_FIXTURE=1 bun run dev`, selects the MID/ECC
card, and confirms rows appear. Record their answer.

## Test plan

Extend `crates/daku-core/tests/fixtures/mid_ecc/agents_down.json` (or add a
sibling) with a `host_name` on each row — **fake names only**.

New tests in `crates/daku-core/src/mid_ecc.rs` `mod tests`, using `TempDb`:

1. `mid_ecc_payload_lists_unhealthy_agents` — assert the persisted payload's
   `agents_unhealthy_list` names the down agent and **omits** healthy ones.
2. `mid_ecc_payload_bounds_the_unhealthy_list` — more than
   `UNHEALTHY_LIST_LIMIT` unhealthy agents; assert the list is capped and
   `agents_unhealthy_list_truncated` is `true`.
3. `mid_ecc_unhealthy_list_matches_the_count` — `agents_unhealthy_list.len()`
   equals `agents_unhealthy` when under the bound, including for an agent with
   no `host_name`. This is the invariant that keeps the card and the drill-in
   from disagreeing.

New tests in `src/dashboard_state.rs` `mod tests`, modelled on the existing
`drill_in_*` drift tests:

4. `drill_in_lists_unhealthy_mid_agents` — a `mid_ecc` payload with two entries
   → `DrillIn::Rows` with the three headers and two rows.
5. `drill_in_falls_back_to_text_when_every_mid_is_healthy` — empty list →
   `DrillIn::Text`, not an empty table.

**Verification**: `cargo test -p daku-core mid_ecc` → all pass, +3 tests;
`cargo test -p daku drill_in` → all pass, +2 tests.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "agents_unhealthy_list" crates/daku-core/src/mid_ecc.rs src/dashboard_state.rs`
      → matches in both files
- [ ] `grep -n "UNHEALTHY_LIST_LIMIT" crates/daku-core/src/mid_ecc.rs` → ≥ 2
      matches
- [ ] `git diff crates/daku-core/src/mid_ecc.rs | grep '^[-+].*ECC_.*_PATH'` →
      no output (no request changed)
- [ ] `grep -rn "service-now.com\|\.corp\|\.internal" crates/daku-core/tests/fixtures/mid_ecc/`
      → no real-looking hostnames
- [ ] `cargo test -p daku-core mid_ecc` → all pass, three more tests
- [ ] `cargo test -p daku drill_in` → all pass, two more tests
- [ ] Your report records the Operator's Step 4 confirmation
- [ ] `plans/README.md` status row for 066 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- You need a second HTTP request. The whole case for this plan is that the data
  is already fetched; if it is not, the plan is wrong.
- `agents_unhealthy_list.len()` cannot be made to agree with `agents_unhealthy`
  for some input — report which.
- Any fixture or test would contain a real hostname.

## Maintenance notes

- **The invariant to protect**: the list and `agents_unhealthy` describe the same
  set. Test 3 pins it; a future filter applied to only one of them is the way
  that breaks.
- `host_name` is Operator infrastructure. It renders in the app and persists in
  local SQLite — both fine, both local. It must never reach a commit, a plan or
  the daemon log.
- Key naming now matches drift (`<thing>_list` / `<thing>_list_truncated`). A
  third Signal wanting rows should follow the same shape.
- Deliberately **not** done: `ecc_agent_issue` (30-day retention) as a richer
  source, and a per-agent deep link. Both are follow-ups; this plan spends zero
  extra requests, and that is what makes it worth doing now.
