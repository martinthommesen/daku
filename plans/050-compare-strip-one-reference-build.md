# Plan 050: The Compare strip tints against the same build it calls a mismatch, and the rule becomes testable

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- src/dashboard_state.rs src/app.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug + tests
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

The **Compare strip** decides "is there a mismatch?" one way and "which rows do
I tint?" a completely different way.

`DashboardState::compare_strip()` computes `has_mismatch` against the **clone
source's** build. `src/app.rs`'s renderer tints each row against the
**selected Environment's** build. Two reference builds in one widget.

Concretely, with prod (the clone source) and test on build `a` and dev on build
`b`: select **dev**, and the strip tints **prod and test** in the warning
colour and leaves **dev** — the Environment that actually drifted — plain. It
highlights the correct Environments as the problem.

There is a second, structural half. The tint rule lives inside a GPUI render
function, which this crate has no way to test: `src/app.rs` is 725 lines with
exactly one test. Commit `2bdeaba` fixed a real display bug in this very
expression ("tint compare rows only when both builds are known") and landed
with no regression test, because there is nowhere to put one. Moving the
decision into `DashboardState` — which already has 32 tests — is what stops the
next fix here from being unverifiable too.

## Current state

**`src/dashboard_state.rs:101-108`** — the row model the renderer consumes:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRow {
    pub id: String,
    pub label: String,
    pub build: Option<String>,
    pub drift: String,
    pub last_clone: String,
}
```

**`src/dashboard_state.rs` `compare_rows()`** — no mismatch information at all:

```rust
    pub fn compare_rows(&self) -> Vec<CompareRow> {
        self.environments
            .iter()
            .map(|environment| CompareRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                build: environment_build(&self.snapshots, &environment.id),
                drift: self.signal_summary(&environment.id, "drift"),
                last_clone: self.signal_summary(&environment.id, "last_clone"),
            })
            .collect()
    }
```

**`src/dashboard_state.rs:393-424`** — `compare_strip()`, which uses the
**clone source** as the reference and falls back to pairwise:

```rust
    pub fn compare_strip(&self) -> CompareStrip {
        if self.environments.len() < 2 {
            return CompareStrip { visible: false, has_mismatch: false };
        }
        let source_id = self.clone_source_id();
        let source_build = source_id.and_then(|id| environment_build(&self.snapshots, id));
        let builds: Vec<_> = self
            .environments
            .iter()
            .filter_map(|environment| environment_build(&self.snapshots, &environment.id))
            .collect();
        let build_mismatch = if let Some(source) = source_build {
            builds.iter().any(|build| build != &source)
        } else {
            builds.windows(2).any(|pair| pair[0] != pair[1])
        };
        let plugin_mismatch = self.environments.iter().any(|environment| {
            source_id != Some(environment.id.as_str())
                && self
                    .snapshots
                    .get(&environment.id)
                    .and_then(|map| map.get("drift"))
                    .is_some_and(|snapshot| drift_mismatch(&snapshot.payload_json))
        });
        let has_mismatch = build_mismatch || plugin_mismatch;
        CompareStrip { visible: true, has_mismatch }
    }
```

**`src/app.rs:596-646`** — the renderer, which uses the **selected**
Environment as the reference:

```rust
fn compare_strip(
    has_mismatch: bool,
    selected_id: &str,
    rows: &[CompareRow],
    cx: &App,
) -> impl IntoElement {
    let selected_build = rows
        .iter()
        .find(|row| row.id == selected_id)
        .and_then(|row| row.build.clone());
    v_flex()
        ...
        .children(rows.iter().map(|row| {
            let mismatch = matches!(
                (&row.build, &selected_build),
                (Some(build), Some(selected)) if build != selected
            );
            compare_row_cells([...])
            .text_sm()
            .text_color(if mismatch {
                cx.theme().warning
            } else {
                cx.theme().muted_foreground
            })
        }))
        .when(has_mismatch, |element| {
            element.child(
                div()
                    ...
                    .child("build / drift mismatch"),
            )
        })
}
```

Note the `(Some(build), Some(selected))` guard — that is `2bdeaba`'s fix and
its **rule must be preserved**: an unknown build on either side never tints.

### Constraints you must honor

- **`CONTEXT.md`** › Screen: **Compare strip** is "the row under the Signal
  cards that lines up build, drift, and last-clone across the other
  Environments." Keep that name in identifiers and comments. *Avoid*: matrix,
  comparison table.
- `src/app.rs` is presentation only. The convention this plan establishes and
  that the codebase already follows elsewhere: **`dashboard_state.rs` decides,
  `app.rs` maps a decision to a theme token.** See `SidebarRow.muted`
  (`src/dashboard_state.rs:93-99`) → `src/app.rs:206-210`, which is exactly this
  shape. Match it.
- `CompareRow` derives `Clone, Debug, PartialEq, Eq` — adding a `bool` field
  keeps all four.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Client tests | `cargo test -p daku dashboard_state` | all pass |
| Visual check | `DAKU_UI_FIXTURE=1 bun run dev` | Operator-run; not a gate |

## Scope

**In scope**:
- `src/dashboard_state.rs`
- `src/app.rs`

**Out of scope** (do NOT touch):
- `crates/daku-core/src/drift.rs` — the daemon-side `build_matches` tri-state is
  plan 049. This plan is client-only and must work with today's payload.
- `CompareStrip.has_mismatch` and the "build / drift mismatch" footer — the
  footer's meaning (anything differs anywhere) is correct and stays.
- `clone_source_id()` and `environment_build()` — read them, do not change them.
- The `plugin_mismatch` half of `compare_strip()`.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Tint the Compare strip against the clone source's build, decided in dashboard_state (#75).`

## Steps

### Step 1: Choose the reference build in one place

In `src/dashboard_state.rs`, add a private method that both `compare_strip()`
and `compare_rows()` call, so there is exactly one definition of "the build
everything is compared against":

```rust
    /// The build the Compare strip measures every Environment against: the
    /// clone source's, or — when there is no clone source or its build is
    /// unknown — the first known build in Environment order.
    fn reference_build(&self) -> Option<String> {
        self.clone_source_id()
            .and_then(|id| environment_build(&self.snapshots, id))
            .or_else(|| {
                self.environments
                    .iter()
                    .find_map(|environment| environment_build(&self.snapshots, &environment.id))
            })
    }
```

Then rewrite `compare_strip()`'s `build_mismatch` to use it:

```rust
        let reference = self.reference_build();
        let build_mismatch = reference.as_ref().is_some_and(|reference| {
            self.environments.iter().any(|environment| {
                environment_build(&self.snapshots, &environment.id)
                    .is_some_and(|build| &build != reference)
            })
        });
```

This preserves today's behaviour for the clone-source case and makes the
no-clone-source fallback deterministic (first known build) instead of pairwise.

**Verify**: `cargo test -p daku dashboard_state` → all pass. If a `compare_strip`
test fails, read it: the pairwise fallback and the first-known-build fallback
agree on every existing fixture, so a failure means something else changed.

### Step 2: Put the tint decision on the row

Add a field to `CompareRow`:

```rust
    /// True when this Environment's build is known, the reference build is
    /// known, and they differ. Unknown on either side is never a mismatch —
    /// the Compare strip must not tint what it could not read.
    pub mismatch: bool,
```

Populate it in `compare_rows()` from `self.reference_build()`, computed **once**
before the `map`:

```rust
    pub fn compare_rows(&self) -> Vec<CompareRow> {
        let reference = self.reference_build();
        self.environments
            .iter()
            .map(|environment| {
                let build = environment_build(&self.snapshots, &environment.id);
                CompareRow {
                    id: environment.id.clone(),
                    label: environment.label.clone(),
                    mismatch: matches!(
                        (&build, &reference),
                        (Some(build), Some(reference)) if build != reference
                    ),
                    build,
                    drift: self.signal_summary(&environment.id, "drift"),
                    last_clone: self.signal_summary(&environment.id, "last_clone"),
                }
            })
            .collect()
    }
```

**Verify**: `cargo build -p daku` → fails only where `CompareRow` is
constructed in tests; fix those in Step 4.

### Step 3: Make the renderer read the field

In `src/app.rs`:

1. Delete the `selected_build` binding at the top of `fn compare_strip`.
2. Replace `let mismatch = matches!(...)` with `let mismatch = row.mismatch;`
   (or use `row.mismatch` inline).
3. Remove the now-unused `selected_id: &str` parameter and update the single
   call site in `render_detail`.

**Verify**: `cargo build -p daku` → exit 0.
`grep -n "selected_build" src/app.rs` → no matches.

### Step 4: Update test constructors

Every `CompareRow { .. }` literal in `src/dashboard_state.rs` `mod tests` needs
the new field. Set it to the value the test's fixture implies, not blindly
`false`.

**Verify**: `cargo test -p daku` → all pass. `bun run check` → exit 0.

## Test plan

New tests in `src/dashboard_state.rs` `mod tests`, modelled on the existing
`compare_*` tests (they build state via `state.apply(&...)` with the local
`summary(...)` / `snap(...)` helpers around `src/dashboard_state.rs:697-830`).

1. `compare_rows_tint_the_drifted_environment_not_the_selected_one` — the
   regression this plan fixes. Three Environments: `prod` (clone source, build
   `a`), `test` (build `a`), `dev` (build `b`). Assert
   `rows.iter().filter(|row| row.mismatch).map(|row| &row.id).collect::<Vec<_>>() == ["dev"]`.
   Then `state.select("dev")` and assert the **same** result — the tint must not
   depend on selection.
2. `compare_rows_do_not_tint_an_unknown_build` — `dev` has no availability
   snapshot; assert `!rows[2].mismatch` and that `rows[2].build.is_none()`.
   This pins `2bdeaba`'s rule.
3. `compare_rows_do_not_tint_when_the_reference_build_is_unknown` — no
   Environment has a readable build; assert no row has `mismatch`.
4. `compare_strip_and_rows_agree_on_mismatch` — for the fixture in test 1,
   assert `state.compare_strip().has_mismatch` **and** that at least one row has
   `mismatch == true`. This is the invariant that was broken: the footer and the
   tint must never disagree about whether anything differs.

**Verification**: `cargo test -p daku dashboard_state` → all pass, +4 tests.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "selected_build" src/app.rs` → no matches
- [ ] `grep -n "mismatch" src/app.rs` shows only `row.mismatch` and
      `has_mismatch`, no `matches!` expression comparing builds
- [ ] `grep -n "pub mismatch" src/dashboard_state.rs` → one match
- [ ] `grep -c "fn reference_build" src/dashboard_state.rs` → `1`
- [ ] `cargo test -p daku dashboard_state` → all pass, four more tests than before
- [ ] `git diff --name-only` lists only `src/dashboard_state.rs`, `src/app.rs`
      and `plans/README.md`
- [ ] `plans/README.md` status row for 050 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- Step 1 breaks an existing `compare_strip` test — report which fixture and how,
  rather than adjusting the assertion to match new behaviour.
- Removing `selected_id` from `fn compare_strip` reveals a second caller you did
  not expect.
- You conclude the reference build should be the selected Environment after all
  — that is a product decision, not an executor call. Report it.

## Maintenance notes

- The invariant to protect in review: **`compare_strip().has_mismatch` and
  `compare_rows()[..].mismatch` must be computed from the same reference
  build.** Test 4 pins it; keep that test if the fields are ever refactored.
- Plan 049 makes the daemon's `build_matches` tri-state. That is the *drift
  Signal's* comparison and is independent of this one, which compares the
  `build` string the Compare strip already reads from the availability
  snapshot. Do not try to merge them.
- This plan establishes the seam for the rest of `src/app.rs`'s untested rules
  (`status_color`, `health_tag`, `reachability_tag`, `paint_sparkline`
  normalisation, `src/app.rs:478-524` and `:676-698`). Moving those is
  deliberately **not** in this plan — do them one at a time when each is next
  touched, following this same shape.
