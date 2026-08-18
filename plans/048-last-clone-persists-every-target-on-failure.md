# Plan 048: Last-clone records why it has no answer when the clone-source probe fails

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

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

`LastCloneCollector::collect` has three branches that end without an answer for
the clone **targets**. Commit `2bdeaba` fixed two of them and left the third.

Its commit message names the cost exactly: *"A Waiting card renders an animated
Skeleton, so a permanent Waiting kept the whole shell re-rendering every
frame; last-clone now records why it has no answer."* The third branch — the
clone source is unreachable, or answers with a status that is neither 200 nor
403 — still returns after persisting a snapshot for the source only.

Two distinct failures follow:

1. **Never succeeded** (source offline at startup, wrong Credential, VPN down):
   every target's last-clone card sits on Waiting forever and animates a
   Skeleton at frame rate — the exact regression `2bdeaba` set out to kill,
   still reachable through the branch it did not touch.
2. **Succeeded once, then lost the source**: `persist_signal_snapshot` upserts
   on `(environment_id, signal_id)` and nothing deletes, so each target card
   **freezes** on its last good value ("12 days ago") while the true age keeps
   growing. A stale answer presented as current is worse than no answer.

## Current state

`crates/daku-core/src/last_clone.rs` — the last-clone Signal. Read once from
the clone-source Environment (the `clone_instance` record lives there), then
one snapshot written per clone target.

**`crates/daku-core/src/last_clone.rs:139-155`** — branch A, *no clone source*.
This one is correct; use it as the pattern:

```rust
        else {
            // Without a clone source the card would sit on "Waiting" forever
            // (and its Skeleton would animate forever); say why instead.
            for environment in &self.environments {
                persistence::persist_signal_skipped(
                    &connection,
                    &environment.id,
                    LAST_CLONE_SIGNAL_ID,
                    observed_at,
                    "no_clone_source",
                )?;
            }
            return Ok(());
        };
```

**`crates/daku-core/src/last_clone.rs:156-182`** — branch B, **the bug**. Both
arms persist for `source.id` only and return:

```rust
        let response = match self.client.request(
            source,
            self.credentials.as_ref(),
            "GET",
            CLONE_INSTANCE_PATH,
            None,
        ) {
            Ok(response) if response.status == 200 || response.status == 403 => response,
            Ok(response) => {
                return persist_last_clone_unreachable(
                    &connection,
                    &source.id,
                    &format!("HTTP {}", response.status),
                    observed_at,
                )
                .map_err(anyhow::Error::from);
            }
            Err(error) => {
                return persist_last_clone_unreachable(
                    &connection,
                    &source.id,
                    &error.to_string(),
                    observed_at,
                )
                .map_err(anyhow::Error::from);
            }
        };
```

**`crates/daku-core/src/last_clone.rs:183-202`** — branch C, *403 — source
cannot list clones*. Also correct; note it filters the source out of the loop:

```rust
        let Some(rows) = rows else {
            for environment in self
                .environments
                .iter()
                .filter(|environment| environment.id != source.id)
            {
                persistence::persist_signal_skipped(
                    &connection,
                    &environment.id,
                    LAST_CLONE_SIGNAL_ID,
                    observed_at,
                    "clone_source_cannot_list_clones",
                )?;
            }
            return Ok(());
        };
```

**`crates/daku-core/src/persistence.rs:185-202`** — the helper both correct
branches use:

```rust
/// Records that `signal_id` deliberately skipped probing (`reason` is
/// `"asleep"` or `"unreachable"` — the Availability outcome it deferred to).
pub fn persist_signal_skipped(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    observed_at: i64,
    reason: &str,
) -> io::Result<()> {
    let payload = serde_json::json!({ "skipped": reason });
```

**`src/dashboard_state.rs:673-684`** — the client turns a skip reason into the
card's detail line. You must add an arm here for the new reason:

```rust
    if let Some(reason) = value.get("skipped").and_then(|item| item.as_str()) {
        // The card's main line already reads "skipped"; do not repeat the word.
```

Read that block in full before editing — it is a `match`/`if` chain over the
existing reason strings (`asleep`, `unreachable`, `need_two_environments`,
`no_clone_source`, `clone_source_cannot_list_clones`).

### Constraints you must honor

- **`CONTEXT.md`** vocabulary: **Signal**, **Environment**, **Signal card**.
  Use "clone source" and "clone target" as `last_clone.rs` already does.
- A `skipped` snapshot **never votes** in the Environment health rollup
  (`crates/daku-protocol/src/protocol.rs:71-73`), and last-clone is exempt from
  the rollup anyway (`crates/daku-core/src/health.rs:32`). So this change cannot
  move any Environment's health — it only replaces a permanent Waiting with an
  honest sentence.
- The reason string must be **distinct** from `clone_source_cannot_list_clones`,
  which means something different (the source answered, with 403). Use
  `clone_source_unreachable`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Signal tests | `cargo test -p daku-core last_clone` | all pass |
| Client tests | `cargo test -p daku dashboard_state` | all pass |

## Scope

**In scope**:
- `crates/daku-core/src/last_clone.rs`
- `src/dashboard_state.rs` (the skip-reason phrase arm and its test only)

**Out of scope** (do NOT touch):
- Branch A and branch C above — they are `2bdeaba`'s fix and are pinned by
  `last_clone_signal_403_writes_healthy_unsupported` and
  `last_clone_signal_without_clone_source_is_skipped_everywhere`.
- `persist_last_clone_unreachable` itself — the source's own `down` snapshot is
  correct and is pinned by `last_clone_signal_probe_failure_is_down_unreachable`.
- The `sysparm_limit=10` page size and the "not in page vs never cloned"
  ambiguity — that is plan 057. Do not change `CLONE_INSTANCE_PATH` here.
- `crates/daku-core/src/drift.rs` — plan 049 owns it.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**
  (`docs/agents/git-workflow.md`).
- Commit message style: imperative, e.g.
  `Persist a skipped last-clone snapshot for every target when the source is unreachable (#73).`

## Steps

### Step 1: Extract the target-skip loop

Branches A and C each inline the same loop. Add one private helper next to
`persist_clone_source` in `crates/daku-core/src/last_clone.rs`:

```rust
/// Records on every clone target that last-clone has no answer this tick and
/// why, so the card says something instead of animating "Waiting" forever.
fn skip_targets(
    connection: &Connection,
    environments: &[EnvironmentConfig],
    source_id: Option<&str>,
    observed_at: i64,
    reason: &str,
) -> io::Result<()> {
    for environment in environments
        .iter()
        .filter(|environment| Some(environment.id.as_str()) != source_id)
    {
        persistence::persist_signal_skipped(
            connection,
            &environment.id,
            LAST_CLONE_SIGNAL_ID,
            observed_at,
            reason,
        )?;
    }
    Ok(())
}
```

Rewrite branch A to call `skip_targets(&connection, &self.environments, None, observed_at, "no_clone_source")`
(with `source_id: None` every Environment is written, which is branch A's
current behaviour) and branch C to call it with `Some(source.id.as_str())` and
`"clone_source_cannot_list_clones"`.

**Verify**: `cargo test -p daku-core last_clone` → all pass, same count as
before. Both existing branch tests must still pass **unchanged**.

### Step 2: Use it in the failing branch

In branch B, before each `return`, call
`skip_targets(&connection, &self.environments, Some(source.id.as_str()), observed_at, "clone_source_unreachable")?;`
so the source still gets its `down` snapshot from
`persist_last_clone_unreachable` and every target gets a `skipped` snapshot
naming the reason.

**Verify**: `cargo test -p daku-core last_clone` → all pass.

### Step 3: Give the reason a sentence in the client

In `src/dashboard_state.rs`, add an arm to the skip-reason chain at
`src/dashboard_state.rs:673-684` mapping `"clone_source_unreachable"` to a
phrase in the same register as its neighbours — e.g.
`"clone source unreachable"`. Match the existing arms' capitalisation and
wording style exactly; read them before writing.

**Verify**: `cargo test -p daku dashboard_state` → all pass.

## Test plan

New tests in `crates/daku-core/src/last_clone.rs` `mod tests`. Model them on
`last_clone_signal_403_writes_healthy_unsupported` (which already asserts the
source snapshot *and* a target's skipped snapshot) — reuse the existing
`collect_last_clone(status, body)` helper, which builds prod/test/dev with
`prod` as the clone source.

1. `last_clone_signal_probe_failure_skips_every_target` — call
   `collect_last_clone(500, r#"{"error":{"message":"boom"}}"#)`; assert
   `prod`'s snapshot is `down` (as today) **and** that both `test` and `dev`
   have a snapshot with `state == "skipped"` whose `payload_json` contains
   `clone_source_unreachable`.
2. `last_clone_signal_transport_error_skips_every_target` — same assertion for
   the `Err(error)` arm. The existing `LastCloneTransport` always returns
   `Ok`; add a small transport struct that returns
   `Err(anyhow!("connection refused"))` from `execute`, following the
   `NoProbeTransport` pattern used in `crates/daku-core/src/jobs.rs` `mod tests`.

One test in `src/dashboard_state.rs` `mod tests`: extend
`card_detail_phrases_skipped` (or add a sibling) asserting
`detail_from_payload(r#"{"skipped":"clone_source_unreachable"}"#)` returns the
new phrase.

**Verification**: `cargo test -p daku-core last_clone` → all pass, +2 tests.
`cargo test -p daku dashboard_state` → all pass.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -c "persist_signal_skipped" crates/daku-core/src/last_clone.rs` → `1`
      (only inside `skip_targets`)
- [ ] `grep -n "clone_source_unreachable" crates/daku-core/src/last_clone.rs src/dashboard_state.rs`
      → matches in both files
- [ ] `cargo test -p daku-core last_clone` → all pass, two more tests than before
- [ ] `grep -n "sysparm_limit=10" crates/daku-core/src/last_clone.rs` → still
      present and unchanged (plan 057 owns that)
- [ ] `git diff --name-only` lists only `crates/daku-core/src/last_clone.rs`,
      `src/dashboard_state.rs` and `plans/README.md`
- [ ] `plans/README.md` status row for 048 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The three branches in "Current state" do not match the live code.
- Extracting `skip_targets` changes the behaviour of either existing branch
  test — that means branch A and branch C differ in a way this plan missed.
- You find a fourth branch in `collect` that returns without writing target
  snapshots.

## Maintenance notes

- Every future early return in `LastCloneCollector::collect` must call
  `skip_targets` with a distinct reason, and every new reason needs an arm in
  `src/dashboard_state.rs`. A reason with no client arm renders as a bare
  `skipped` with no explanation — that is the failure mode to watch for in
  review.
- Reviewers: check that the source's own snapshot is still written *before* the
  target loop, so a `?` on the target loop cannot swallow it.
- Plan 049 adds an availability gate to this same `collect` function. If both
  are in flight, land 048 first; 049's gate is a new early return that must
  also call `skip_targets`.
