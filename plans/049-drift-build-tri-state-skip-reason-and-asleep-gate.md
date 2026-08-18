# Plan 049: Drift stops guessing — unknown builds are unknown, the skip reason is true, and asleep Environments are not probed

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/drift.rs crates/daku-core/src/last_clone.rs src/dashboard_state.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: `plans/048-last-clone-persists-every-target-on-failure.md`
  (048 extracts the `skip_targets` helper this plan reuses; land 048 first)
- **Category**: bug
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Three defects in the same two collectors, all about the Signal claiming to know
something it does not.

1. **Unknown build compares as a string.** `build_matches` is
   `source.build == other.build` on two `Option<String>`. `fetch_build` returns
   `Ok(None)` — no error — whenever the `glide.war` read does not yield a
   build. So `Some("glide-zurich…") == None` → `build_matches: false` → the
   Environment goes amber for a build nobody could read; and worse,
   `None == None` → `build_matches: true` → **two unreadable builds report as
   agreement**, masking exactly the post-upgrade drift this Signal exists to
   catch.
2. **The skip reason is wrong half the time.** Both the "no clone source"
   branch and the "fewer than two Environments" branch call
   `persist_all_skipped`, which hard-codes `"need_two_environments"`. An
   Operator with three Environments and no `clone_source: true` is told to add
   an Environment; the actual one-line config fix is never surfaced.
   `last_clone.rs` already gets this right with `"no_clone_source"`.
3. **Asleep Environments are still probed.** Drift and last-clone implement
   `SignalCollector` directly and are registered as *shared* collectors, so
   they bypass the availability gate in `PerEnvironmentCollector::collect_environment`
   that jobs, syslog, mid_ecc and outbound get for free. A hibernating PDI — the
   normal state of a dev stand-in — holds a **red Drift card indefinitely** and
   re-issues two plugin pages plus a `glide.war` fallback **every tick, forever,
   with no backoff** (plan 023 deliberately does not cache failed fetches).

Defect 3 was **deferred on purpose** by plan 013, which lists both files under
"Out of scope … different loop shapes" and records in its Maintenance notes:
*"Add the same gate to `drift.rs::collect_other` when touching drift next."*
Drift has since been touched twice (plans 023 and 043, both DONE) without the
gate. This plan is that deferred follow-up, not a discovery of an oversight.

Defect 3 is also the dominant real-world trigger for defect 1: an asleep
Environment is the common case where `build` is `None` while the Environment is
*not* already down. Fixing the gate removes most of the false amber; fixing the
tri-state removes the rest and, more importantly, the masking.

## Current state

Two files.

- `crates/daku-core/src/drift.rs` (1110 lines) — version + plugin/app drift
  across Environments, compared against the clone source.
- `crates/daku-core/src/last_clone.rs` (537+ lines) — last-clone per target.

**`crates/daku-core/src/drift.rs:28-34`** — the two-state verdict:

```rust
pub fn drift_state(build_matches: bool, mismatches: u64) -> SignalState {
    if build_matches && mismatches == 0 {
        SignalState::Healthy
    } else {
        SignalState::Degraded
    }
}
```

**`crates/daku-core/src/drift.rs:337-360`** — where the comparison happens:

```rust
fn persist_drift_compare(
    connection: &Connection,
    environment_id: &str,
    source: &EnvInventory,
    other: &EnvInventory,
    observed_at: i64,
) -> io::Result<()> {
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
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        drift_state(build_matches, mismatches),
        &payload.to_string(),
    )
}
```

**`crates/daku-core/src/availability.rs:26-59`** — proof that a failed build
read is *not* an error, it is `build: None`:

```rust
pub fn classify_availability_response(
    status: u16,
    content_type: &str,
    body: &str,
    rtt_ms: u64,
) -> AvailabilityObservation {
    if is_hibernating(content_type, body) {
        return observation(Reachability::Asleep, SignalState::Healthy, None, rtt_ms, None);
    }
    if status == 200 && looks_like_table_api(body) {
        return observation(
            Reachability::Reachable,
            SignalState::Healthy,
            parse_glide_war(body),
            rtt_ms,
            None,
        );
    }
    let error = match status { 429 => Some("HTTP 429".to_owned()), _ => None };
    observation(Reachability::Unreachable, SignalState::Down, None, rtt_ms, error)
}
```

…and `crates/daku-core/src/drift.rs:239-260` (`fetch_build`) returns
`Ok(classify_availability_response(...).build)` — so a hibernating or
unreadable Environment yields `Ok(None)`, silently.

**`crates/daku-core/src/drift.rs:166-180`** — both branches, one reason:

```rust
        let Some(source) = self
            .environments
            .iter()
            .find(|environment| environment.clone_source)
        else {
            return persist_all_skipped(&connection, &self.environments, observed_at);
        };
        if self.environments.len() < 2 {
            return persist_all_skipped(&connection, &self.environments, observed_at);
        }
```

**`crates/daku-core/src/drift.rs:361-401`**:

```rust
fn persist_all_skipped(
    connection: &Connection,
    environments: &[EnvironmentConfig],
    observed_at: i64,
) -> anyhow::Result<()> {
    let mut first_error = None;
    for environment in environments {
        if let Err(error) = persist_drift_skipped(connection, &environment.id, observed_at) {
            first_error.get_or_insert_with(|| anyhow::Error::from(error));
        }
    }
    ...
}

fn persist_drift_skipped(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
) -> io::Result<()> {
    persistence::persist_signal_skipped(
        connection,
        environment_id,
        DRIFT_SIGNAL_ID,
        observed_at,
        "need_two_environments",
    )
}
```

**`crates/daku-core/src/drift.rs:136-164`** — `collect_other`, the per-target
path that needs the gate:

```rust
    fn collect_other(
        &self,
        connection: &Connection,
        environment: &EnvironmentConfig,
        observed_at: i64,
        max_age_secs: i64,
        source: Option<&EnvInventory>,
    ) -> anyhow::Result<()> {
        let Some(source) = source else {
            return persist_drift_down(
                connection,
                &environment.id,
                "clone source unreachable",
                observed_at,
            )
            .map_err(anyhow::Error::from);
        };
        match self.env_inventory(environment, connection, observed_at, max_age_secs) {
            ...
        }
    }
```

**The gate to copy — `crates/daku-core/src/collector.rs:125-148`:**

```rust
        if self.signal.gated_by_availability()
            && let Some(reachability @ (Reachability::Asleep | Reachability::Unreachable)) =
                recent_reachability(
                    connection,
                    &environment.id,
                    observed_at,
                    REACHABILITY_REUSE_SECS,
                )
        {
            return persistence::persist_signal_skipped(
                connection,
                &environment.id,
                self.signal.id(),
                observed_at,
                reachability.as_str(),
            )
            .map_err(anyhow::Error::from);
        }
```

`recent_reachability` is `pub` in `crates/daku-core/src/availability.rs:101-121`
and is already imported by `collector.rs`. `REACHABILITY_REUSE_SECS` is `300`.

**Ordering guarantee you can rely on**: `CollectorLoop::tick`
(`crates/daku-core/src/collector.rs:238-254`) joins **every** per-Environment
group before running `run_sequential(&self.shared)`, and Availability is the
first collector in each group. So when drift and last-clone run, this tick's
availability snapshot is already committed to SQLite.

**Client side — `src/dashboard_state.rs:571-573`:**

```rust
    value.get("build_matches") == Some(&serde_json::Value::Bool(false))
```

This already treats a missing or `null` `build_matches` as "not a mismatch", so
the tri-state payload needs **no client change** for correctness. Read it and
confirm before assuming.

### Constraints you must honor

- **`CONTEXT.md`**: **Environment health** is healthy/degraded/down;
  **reachability** (`reachable` | `unreachable` | `asleep`) is separate. A
  `skipped` Signal never votes (`crates/daku-protocol/src/protocol.rs:71-73`).
- Plan 013's rule, still in force: **asleep must never become degraded**.
- Plan 023's rule, still in force: a **failed** inventory fetch is never cached
  (`crates/daku-core/src/drift.rs:84-121`, pinned by
  `drift_signal_failed_inventory_is_not_cached`). Do not start caching failures.
- `drift.rs` stays one cohesive module — `plans/README.md` records "splitting
  `drift.rs`" as considered and rejected.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Drift tests | `cargo test -p daku-core drift` | all pass |
| Last-clone tests | `cargo test -p daku-core last_clone` | all pass |
| Client tests | `cargo test -p daku dashboard_state` | all pass |

## Scope

**In scope**:
- `crates/daku-core/src/drift.rs`
- `crates/daku-core/src/last_clone.rs`
- `src/dashboard_state.rs` — only if Step 4 shows a phrase is missing

**Out of scope** (do NOT touch):
- `crates/daku-core/src/collector.rs` — read the gate, do not move or
  generalise it. Turning drift/last-clone into `PerEnvironmentCollector`s is a
  much larger change than this plan and would undo plan 031's shape.
- `crates/daku-core/src/health.rs` — the rollup already neutralises asleep.
- `crates/daku-core/src/availability.rs` — read only.
- The `truncated` flag's computation and rendering — that is plan 058.
- `CLONE_INSTANCE_PATH` / `sysparm_limit=10` — that is plan 057.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Skip drift and last-clone on asleep Environments; make an unknown build unknown (#74).`
- Three logically separate changes — three commits is fine and preferred.

## Steps

### Step 1: Make the build comparison tri-state

In `crates/daku-core/src/drift.rs`:

1. Change the signature to `pub fn drift_state(build_matches: Option<bool>, mismatches: u64) -> SignalState`
   and treat `None` as *not a mismatch*:

```rust
/// `build_matches: None` means at least one side's build could not be read —
/// unknown is not a mismatch, and two unknowns are not agreement.
pub fn drift_state(build_matches: Option<bool>, mismatches: u64) -> SignalState {
    if build_matches != Some(false) && mismatches == 0 {
        SignalState::Healthy
    } else {
        SignalState::Degraded
    }
}
```

2. In `persist_drift_compare`, compute:

```rust
    let build_matches = match (&source.build, &other.build) {
        (Some(source_build), Some(other_build)) => Some(source_build == other_build),
        _ => None,
    };
```

   and put it in the payload unchanged (`serde_json` serialises `Option<bool>`
   as `true` / `false` / `null`).

**Verify**: `cargo test -p daku-core drift` → all pass. Existing tests that pass
a bare `bool` to `drift_state` will need `Some(...)`; that compile error is
expected.

### Step 2: Pass the real reason through `persist_all_skipped`

Add a `reason: &str` parameter to `persist_all_skipped` and `persist_drift_skipped`,
then call them with:

- `"no_clone_source"` from the missing-clone-source branch
  (matching `last_clone.rs:152`), and
- `"need_two_environments"` from the `environments.len() < 2` branch.

**Verify**: `cargo test -p daku-core drift` → the existing
`drift_signal_without_clone_source_skips` test now fails, because it pins the
old string for a two-Environment config. **That failure is the point** — update
its expectation to `"no_clone_source"` and keep a separate test for the
one-Environment case asserting `"need_two_environments"`.

### Step 3: Gate drift on availability

In `crates/daku-core/src/drift.rs`, import
`crate::availability::{REACHABILITY_REUSE_SECS, recent_reachability}` and add
two gates, both mirroring `collector.rs:125-148`:

1. **Source gate**, in `collect`, right after `source` is resolved and before
   `env_inventory(source, …)`: if `recent_reachability` for the source is
   `Asleep | Unreachable`, `persist_signal_skipped` for **every** Environment
   with the reachability string as the reason, and return `Ok(())`. Nothing can
   be compared without the source.
2. **Target gate**, at the top of `collect_other`: if `recent_reachability` for
   `environment` is `Asleep | Unreachable`, `persist_signal_skipped` for that
   Environment with the reachability string and return `Ok(())` — **before**
   the `source.is_none()` branch, so an asleep target does not get
   `"clone source unreachable"` written over it.

**Verify**: `cargo test -p daku-core drift` → all pass.

### Step 4: Gate last-clone on availability

In `crates/daku-core/src/last_clone.rs`, add one gate at the top of `collect`
after `connection` and `observed_at` are bound but before the clone-source
lookup: if the resolved clone source's `recent_reachability` is
`Asleep | Unreachable`, write a `skipped` snapshot for the source and call
plan 048's `skip_targets` helper with the reachability string, then return
`Ok(())`.

Then check `src/dashboard_state.rs:673-684`: the reasons `"asleep"` and
`"unreachable"` already have phrases (they are what the per-Environment
collectors write). Confirm with
`grep -n '"asleep"\|"unreachable"' src/dashboard_state.rs`. If both are present,
**no client change is needed** — say so in your report rather than editing.

**Verify**: `cargo test -p daku-core last_clone` → all pass. `bun run check` → exit 0.

## Test plan

Model every new test on the existing `drift_signal_*` / `last_clone_signal_*`
tests, which use `TempDb::new("<label>")` from `crates/daku-core/src/test_support.rs`
and a scripted `HttpTransport`. **Use `TempDb`; do not hand-roll a temp path**
(`plans/028` established this and reviewers reject fresh `temp_dir()` calls).

To set up an asleep Environment, write an availability snapshot directly before
calling `collect` — `crates/daku-core/src/availability.rs` `mod tests` already
does this in `recent_reachability_reads_fresh_asleep_snapshot`; copy that setup.

1. `drift_unknown_build_on_one_side_is_not_a_mismatch` — source has a build,
   target's `glide.war` read yields none; assert the snapshot state is
   `healthy` (given zero plugin mismatches) and `payload["build_matches"]` is
   `null`.
2. `drift_two_unknown_builds_do_not_report_agreement` — neither side has a
   build; assert `payload["build_matches"]` is `null`, **not** `true`. This is
   the masking regression.
3. `drift_known_builds_still_compare` — differing known builds still give
   `build_matches: false` and `degraded`.
4. `drift_signal_without_clone_source_skips` (update) — two Environments, no
   `clone_source`; assert `payload["skipped"] == "no_clone_source"`.
5. `drift_signal_single_environment_skips_need_two` — one Environment; assert
   `payload["skipped"] == "need_two_environments"`.
6. `drift_signal_skips_an_asleep_target` — fresh `asleep` availability snapshot
   for the target; assert its drift snapshot is `state == "skipped"` with
   `payload["skipped"] == "asleep"`, **and** that the transport recorded no
   plugin request for that Environment (follow the `NoProbeTransport` pattern in
   `crates/daku-core/src/jobs.rs` `mod tests`, which asserts exactly this).
7. `drift_signal_skips_everything_when_the_source_is_asleep`.
8. `last_clone_signal_skips_an_asleep_source` — same shape in `last_clone.rs`.

**Verification**: run the two filters as **separate commands** —
`cargo test -p daku-core drift`, then `cargo test -p daku-core last_clone` —
both all-pass with the new tests present. Cargo takes one TESTNAME; passing two
filters to a single `cargo test` is a usage error, not a wider run.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "fn drift_state" crates/daku-core/src/drift.rs` shows
      `build_matches: Option<bool>`
- [ ] `grep -c "recent_reachability" crates/daku-core/src/drift.rs` → ≥ 2
- [ ] `grep -c "recent_reachability" crates/daku-core/src/last_clone.rs` → ≥ 1
- [ ] `grep -n "need_two_environments" crates/daku-core/src/drift.rs` shows it
      only on the `environments.len() < 2` path, not as a hard-coded default
      inside `persist_drift_skipped`
- [ ] `cargo test -p daku-core drift` → all pass, ≥ 5 more tests than before
- [ ] `cargo test -p daku-core last_clone` → all pass
- [ ] `grep -rn "temp_dir()" crates/daku-core/src/drift.rs crates/daku-core/src/last_clone.rs`
      → no matches (new tests use `TempDb`)
- [ ] `git diff --name-only` lists only the in-scope files and `plans/README.md`
- [ ] `plans/README.md` status row for 049 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- `src/dashboard_state.rs:571-573` does **not** already treat a missing
  `build_matches` as not-a-mismatch — then the client needs a change this plan
  did not scope.
- The availability snapshot is not present when drift runs in your new tests —
  that would mean the tick ordering described above has changed, which
  invalidates the whole gate approach.
- Adding the gate makes an existing drift test fail in a way that is not the
  intended reason-string change in Step 2.

## Maintenance notes

- `build_matches` is now `true` / `false` / `null` on the wire. Any future
  reader must treat `null` as "unknown", never as agreement. The client already
  does; a new consumer might not.
- The gate is now duplicated in three places (`collector.rs` for the five
  per-Environment Signals, `drift.rs`, `last_clone.rs`). If a fourth appears,
  that is the signal to lift it into a shared helper — but not before.
- Reviewers should check that the target gate in `collect_other` runs **before**
  the `source.is_none()` branch, and that the source gate writes snapshots for
  every Environment (not just the source) so no card is left Waiting.
- Deliberately deferred: making drift's amber distinguishable from
  "build unknown" in the card text. The payload now carries the fact; rendering
  it is a UI decision, not part of this fix.
