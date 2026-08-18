# Plan 047: An Environment daku has never probed must not render as healthy and reachable

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/health.rs src/dashboard_state.rs src/app.rs`
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

daku is a monitoring console. Right now, between daemon start and the first
completed poll, every Environment renders with a green health dot, a
"reachable" badge, and **no freshness caveat at all** — because the header only
shows "polled … ago" when there is something to date. The same thing happens
forever-until-first-tick for an Environment newly added to
`~/.daku/environments.json`.

An operator console that says "healthy · reachable" about an Environment it has
never contacted is the single failure mode this product exists to prevent. Plan
014 correctly fixed "blank until the first tick"; it did not distinguish
*"observed and fine"* from *"no data yet"*.

After this plan, an unobserved Environment is visibly unobserved.

## Current state

Three files, one chain.

- `crates/daku-core/src/health.rs` — builds `EnvironmentSummary` for every
  Environment and sends `EnvironmentsUpdated`.
- `src/dashboard_state.rs` — the client-side fold; owns the `freshness` helper.
- `src/app.rs` — renders the Environment detail header.

**`crates/daku-core/src/health.rs:19-39`** — an empty vote list rolls up healthy:

```rust
pub fn health_rollup(
    reachability: Reachability,
    signals: &[(&str, SignalState)],
) -> EnvironmentHealth {
    match reachability {
        // Reachability is reported separately; a sleeping Environment cannot
        // be observed, so its Signals must not vote.
        Reachability::Unreachable => return EnvironmentHealth::Down,
        Reachability::Asleep => return EnvironmentHealth::Healthy,
        Reachability::Reachable => {}
    }
    let mut health = EnvironmentHealth::Healthy;
    for &(signal_id, state) in signals {
        if signal_id == LAST_CLONE_SIGNAL_ID || state == SignalState::Skipped {
            continue;
        }
        if matches!(state, SignalState::Down | SignalState::Degraded) {
            health = EnvironmentHealth::Degraded;
        }
    }
    health
}
```

**`crates/daku-core/src/health.rs:66-92`** — with no Availability snapshot,
reachability defaults to `Reachable` and `last_observed_at` is `None`:

```rust
        let reachability = env_snaps
            .iter()
            .find(|snapshot| snapshot.signal_id == AVAILABILITY_SIGNAL_ID)
            .map(|snapshot| wire_reachability(&snapshot.payload_json))
            .unwrap_or(Reachability::Reachable);
        ...
        summaries.push(EnvironmentSummary {
            ...
            health: health_rollup(reachability, &votes),
            reachability,
            last_observed_at: env_snaps.iter().map(|snapshot| snapshot.observed_at).max(),
        });
```

**`crates/daku-core/src/collector.rs:271-275`** — `run` publishes once *before*
the first tick (deliberate, plan 014 — keep this):

```rust
    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock, after: &dyn Fn()) {
        // Publish last-known state from SQLite so a fresh subscriber is not blank
        // until the first tick completes.
        after();
```

**`src/dashboard_state.rs:62-79`** — `freshness` returns `None` for an
unobserved Environment, so the caller renders nothing:

```rust
/// "polled 42 s ago" / "polled 3 min ago" / "polled 2 h ago" for the selected
/// Environment, or `None` before the first observation.
pub fn freshness(last_observed_at: Option<i64>, now: i64) -> Option<Freshness> {
    let age = now.saturating_sub(last_observed_at?).max(0);
```

**`src/app.rs:276-291`** — `when_some` means `None` renders no label at all:

```rust
                                    .when_some(
                                        freshness(environment.last_observed_at, unix_now()),
                                        |element, fresh| {
                                            element.child("\u{b7}").child(
                                                div()
                                                    .text_color(if fresh.stale {
                                                        cx.theme().warning
                                                    } else {
                                                        cx.theme().muted_foreground
                                                    })
                                                    .child(fresh.label),
                                            )
                                        },
                                    ),
```

### Constraints you must honor

- **`CONTEXT.md`** locks the vocabulary: **Environment health** is
  "**healthy**, **degraded**, or **down**" and nothing else; **reachability**
  (`reachable` | `unreachable` | `asleep`) is a separate field. `plans/README.md`
  › "Ownership locks" repeats this: "Environment health: `healthy` | `degraded`
  | `down` only". **Do not add a fourth `EnvironmentHealth` variant and do not
  add a fourth `Reachability` variant.** The fix lives in the freshness/render
  path, not in the health enum.
- `daku_protocol::EnvironmentSummary.last_observed_at` is already
  `Option<i64>` — the "never observed" fact is already on the wire. Use it.
- Error style: `crates/daku-core` returns `anyhow::Result`; the client fold in
  `src/dashboard_state.rs` is pure functions over `&ServerMessage`. Match both.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Core tests | `cargo test -p daku-core health` | all pass |
| Client tests | `cargo test -p daku freshness` | all pass |
| Visual check | `DAKU_UI_FIXTURE=1 bun run dev` | app launches (Operator-run; not a gate) |

## Scope

**In scope**:
- `src/dashboard_state.rs`
- `src/app.rs`

**Out of scope** (do NOT touch):
- `crates/daku-protocol/src/protocol.rs` — no protocol change is needed;
  `last_observed_at: Option<i64>` already carries the fact. Do not bump
  `PROTOCOL_VERSION`.
- `crates/daku-core/src/health.rs` `health_rollup` — the asleep and unreachable
  arms are load-bearing (plan 013) and pinned by 11 tests. Leave them alone.
- `crates/daku-core/src/collector.rs:271-274` — the pre-tick `after()` call is
  plan 014's fix. Do not remove it.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**
  (`docs/agents/git-workflow.md`). Commit on `main` or a disposable local
  branch you delete after merging locally.
- Commit message style: imperative summary with the issue in parentheses, e.g.
  `Say "never polled" instead of rendering an unobserved Environment as healthy (#72).`

## Steps

### Step 1: Make `freshness` speak for the unobserved case

In `src/dashboard_state.rs`, change `freshness` so it always returns a
`Freshness`, with the `None` input producing the unobserved label:

```rust
/// "polled 42 s ago" / "polled 3 min ago" / "polled 2 h ago" for the selected
/// Environment. An Environment with no observation yet reads "never polled"
/// and is stale by definition — daku has not contacted it.
pub fn freshness(last_observed_at: Option<i64>, now: i64) -> Freshness {
    let Some(last_observed_at) = last_observed_at else {
        return Freshness {
            label: "never polled".to_owned(),
            stale: true,
        };
    };
    let age = now.saturating_sub(last_observed_at).max(0);
    ...
}
```

Keep the existing `age < 60` / `age < 3600` / else branches and
`stale: age > STALE_AFTER_SECS` unchanged for the `Some` path.

**Verify**: `cargo test -p daku freshness` → all pass (existing tests that
compare against `Some(Freshness { .. })` will need their expectations updated
to the bare struct in Step 3; a compile error here is expected and fine).

### Step 2: Render the label unconditionally

In `src/app.rs`, replace the `.when_some(freshness(...), |element, fresh| …)`
block at `src/app.rs:276-291` with an unconditional `.child(...)` that renders
the same two children (the `·` separator and the coloured label). The
`fresh.stale` → `cx.theme().warning` choice is unchanged, so an unobserved
Environment renders "never polled" in the warning colour.

**Verify**: `cargo build -p daku` → exit 0.

### Step 3: Mark the health dot and health tag as unknown when nothing was observed

An Environment with `last_observed_at: None` must not show a saturated green
dot. `SidebarRow` already has a `muted: bool` used for the disconnected case
(`src/dashboard_state.rs:350-360`, `src/app.rs:206-210`) — reuse that exact
mechanism rather than inventing a second one:

1. In `src/dashboard_state.rs`, change the `sidebar()` construction so
   `muted` is `!self.connected || environment.last_observed_at.is_none()`.
2. In `src/app.rs`, the Environment detail header renders
   `health_tag(environment.health)` and `reachability_tag(environment.reachability)`
   (`src/app.rs:264-266`). Skip **both** tags when
   `environment.last_observed_at.is_none()` — the header then shows the label,
   the instance URL and "never polled", with no health or reachability claim.

**Verify**: `cargo test -p daku` → all pass. `bun run check` → exit 0.

### Step 4: Update the existing tests for the new `freshness` signature

`src/dashboard_state.rs` `mod tests` currently asserts `freshness(...)` against
`Option<Freshness>`. Update every call site to the bare `Freshness`.

**Verify**: `cargo test -p daku` → all pass.

## Test plan

New tests in `src/dashboard_state.rs` `mod tests` (model them on the existing
`dashboard_state_*` tests, which build `EnvironmentSummary` values through the
local `summary(...)` helper at `src/dashboard_state.rs:817-830`):

1. `freshness_without_an_observation_says_never_polled` —
   `freshness(None, 1_700_000_000)` → `label == "never polled"` and
   `stale == true`.
2. `freshness_keeps_its_existing_labels` — `freshness(Some(now - 42), now)` →
   `"polled 42 s ago"`, `stale == false`; `freshness(Some(now - 400), now)` →
   `stale == true`. (Guards against Step 1 changing the `Some` path.)
3. `sidebar_mutes_an_environment_with_no_observation` — build a connected
   `DashboardState` with one Environment whose `last_observed_at` is `None`;
   assert `state.sidebar()[0].muted`. Then set `last_observed_at: Some(...)`
   and assert `!muted`.

**Verification**: `cargo test -p daku dashboard_state` → all pass, including
the three new tests.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "when_some(" src/app.rs` returns no line whose argument is `freshness(` 
- [ ] `grep -n "fn freshness" src/dashboard_state.rs` shows a return type of
      `Freshness`, not `Option<Freshness>`
- [ ] `grep -n "never polled" src/dashboard_state.rs` → at least one match
- [ ] `cargo test -p daku dashboard_state` → all pass; the three new tests exist
- [ ] `grep -c "Healthy\|Degraded\|Down" crates/daku-protocol/src/protocol.rs` is
      unchanged from before your edit — no fourth `EnvironmentHealth` variant
- [ ] `git diff --name-only` lists only `src/dashboard_state.rs`, `src/app.rs`
      and `plans/README.md`
- [ ] `plans/README.md` status row for 047 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- You find yourself needing to add a variant to `EnvironmentHealth` or
  `Reachability` — that contradicts `CONTEXT.md` and `plans/README.md`'s
  ownership locks and means this plan's approach is wrong for the codebase.
- Removing the tags in Step 3 breaks the header layout in a way you cannot fix
  inside `src/app.rs` (report it; the Operator does the visual check).
- `cargo test -p daku-core health` fails — this plan does not touch
  `health.rs`, so a failure there means something else drifted.

## Maintenance notes

- The `muted` flag now carries two meanings (disconnected, never observed). If a
  third arrives, replace the bool with a small enum rather than adding another
  `||`.
- Plan 053 also touches `sidebar()`/`muted` and the detail header. If both are
  in flight, land 047 first — 053's change is additive on top of it.
- Deliberately **not** done here: omitting unobserved Environments from the
  pre-tick publish. Keeping them in the list is what lets the sidebar show the
  Operator that the Environment exists and has not been reached yet, which is
  more useful than an empty sidebar.
