# Plan 056: Parse each Signal payload once when it arrives, not once per element per frame

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

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: `plans/050-compare-strip-one-reference-build.md` (050 changes
  `compare_rows()`, which this plan then makes cheap; land 050 first)
- **Category**: perf
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Every accessor `src/app.rs` calls does its own `serde_json::from_str` on the
snapshot payload, and `render` calls them all, every frame.

That is fine while renders are event-driven. They are not, whenever any Signal
card is Waiting: `src/app.rs` renders a gpui-component `Skeleton` for a Waiting
card, and `Skeleton` is `.with_animation(Animation::new(2s).repeat())` — a
repeating animation requests a frame continuously, re-running the root render.
The maintainer already diagnosed this in commit `2bdeaba`: *"A Waiting card
renders an animated Skeleton, so a permanent Waiting kept the whole shell
re-rendering every frame."*

`2bdeaba` removed one *source* of permanent Waiting (last-clone). It did not
change the cost. And Waiting is still the **normal state for the first poll
interval of every launch** — up to 120 s of full-rate repaint on a laptop, every
time daku opens.

With five Environments and one Waiting card that is roughly 15 + 5N ≈ 40 JSON
parses per frame, plus every summary `String` and `format!` rebuilt. `render_detail`
also calls `compare_strip()` and `compare_rows()` **unconditionally**, before
checking `strip.visible`, so the multi-Environment accessors run even when the
strip is hidden.

## Current state

**`src/app.rs:334-344`** — per-card accessor calls, each parsing:

```rust
                signal_card(
                    card,
                    self.state.card_summary(card.signal_id),
                    self.state.card_detail(card.signal_id),
                    ...
                )
```

**`src/dashboard_state.rs:443-476`** — each accessor re-parses. `card_summary`:

```rust
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
        else {
            return String::new();
        };
        summarize_payload(signal_id, &snapshot.payload_json)
```

…and `summarize_payload` opens with:

```rust
fn summarize_payload(signal_id: &str, payload_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return String::new();
    };
```

The same shape repeats in `card_detail` → `detail_from_payload`,
`drift_mismatch_lines`, `drift_mismatch`, `environment_build`, and `drill_in`.

**`src/app.rs:229-232`** — the unconditional multi-Environment work:

```rust
        let strip = self.state.compare_strip();
        let rows = self.state.compare_rows();
```

(read the surrounding lines; `strip.visible` is checked *after*.)

**`src/dashboard_state.rs:81-90`** — where parsed values would live:

```rust
    snapshots: HashMap<String, HashMap<String, SignalSnapshotDto>>,
    samples: HashMap<(String, String), Vec<SamplePoint>>,
```

**`src/app.rs:392-398`** — the animation source:

```rust
            .child(if waiting {
                Skeleton::new()
                    .w(px(96.0))
                    .h(px(22.0))
                    .rounded(cx.theme().radius)
                    .into_any_element()
```

Vendored, for reference —
`~/.cargo/git/checkouts/gpui-component-*/972a3eb/crates/ui/src/skeleton.rs`:

```rust
            .with_animation(
                "skeleton",
                Animation::new(Duration::from_secs(2))
                    .repeat()
                    .with_easing(bounce(ease_in_out)),
```

### Constraints you must honor

- **Do not replace `serde_json::Value` with a typed payload struct.** The
  payload is written by seven different collectors and read by key; a typed
  schema would have to tolerate unknown and missing keys exactly as `Value`
  does, and getting that subtly wrong turns a cheap perf fix into a silent
  rendering break. Plan 065 covers pinning the key contract with tests — that is
  the right place for typing, if ever.
- **Do not delete the `Skeleton`.** ADR-0008 accepts gpui-component's widgets;
  a Waiting card *should* look like it is loading. The fix is to make the frames
  cheap, not to remove the animation.
- `src/dashboard_state.rs` is the tested layer (32 tests). Every accessor's
  **output must be byte-identical** after this change — the existing tests are
  your regression suite and none of their expectations may be edited.
- `plans/README.md` records sparkline down-sampling as considered and rejected
  (bounded at ≤ 2880 points). Do not revisit it here.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Client tests | `cargo test -p daku dashboard_state` | all pass, **no expectation edited** |
| Visual check | `DAKU_UI_FIXTURE=1 bun run dev` | Operator-run; not a gate |

## Scope

**In scope**:
- `src/dashboard_state.rs`
- `src/app.rs`

**Out of scope** (do NOT touch):
- `crates/daku-protocol/src/protocol.rs` — `payload_json: String` stays on the
  wire. This is a client-side caching change only.
- `crates/daku-core/**` — the daemon is not involved.
- `paint_sparkline` and the sparkline data path.
- The `Skeleton` widget and the animation itself.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Parse Signal payloads once on apply instead of once per accessor per frame (#81).`

## Steps

### Step 1: Store the parsed payload beside the DTO

In `src/dashboard_state.rs`, change the snapshot map's value type so each entry
carries both the DTO and its parsed payload:

```rust
/// A snapshot plus its payload parsed once. Every accessor reads keys out of
/// `payload`; re-parsing per accessor per frame is what this exists to avoid
/// (a Waiting card animates a Skeleton, which repaints the shell continuously).
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    dto: SignalSnapshotDto,
    payload: serde_json::Value,
}
```

Build it in `apply`'s `SignalSnapshotsUpdated` arm:

```rust
                        .map(|snapshot| {
                            let payload = serde_json::from_str(&snapshot.payload_json)
                                .unwrap_or(serde_json::Value::Null);
                            (snapshot.signal_id.clone(), Snapshot { dto: snapshot.clone(), payload })
                        })
```

`Value::Null` for an unparseable payload preserves today's behaviour exactly:
every accessor currently returns its empty/default value when `from_str` fails,
and `Value::Null.get(...)` returns `None`, which lands in the same branches.
**Confirm that for each accessor as you convert it.**

**Verify**: `cargo build -p daku` → compile errors only at the accessor call
sites, which Step 2 fixes.

### Step 2: Convert every accessor to read the parsed value

Change these to take `&serde_json::Value` instead of `&str`, and drop their
`from_str` line:

- `summarize_payload(signal_id, …)`
- `detail_from_payload(…)`
- `drift_mismatch(…)`
- `environment_build(…)`
- the body of `drift_mismatch_lines`
- the body of `drill_in`

Keep the function names and their public/private visibility as they are — the
tests call several of them directly. Where a test passes a JSON **string**
today, add a thin `&str` wrapper that parses and delegates, so **no test
expectation changes**:

```rust
#[cfg(test)]
fn summarize_payload(signal_id: &str, payload_json: &str) -> String {
    summarize_value(signal_id, &serde_json::from_str(payload_json).unwrap_or(serde_json::Value::Null))
}
```

…or keep `summarize_payload` as the `&str` wrapper in all builds if that is
simpler. Either way: **tests keep their current call shape and their current
expected strings.**

**Verify**: `cargo test -p daku dashboard_state` → all pass, with **zero edits
to any `assert_eq!` expected value**. If you had to change an expectation, you
have changed behaviour — stop and report.

### Step 3: Stop computing the Compare strip when it is hidden

In `src/app.rs` `render_detail`, compute `strip` first and only call
`compare_rows()` inside the `strip.visible` branch.

**Verify**: `cargo test -p daku` → all pass. `bun run check` → exit 0.

### Step 4: Give text elements stable ids

`src/app.rs` builds element ids from text (e.g. `format!("line-{text}")`), so an
id changes whenever a value changes, resetting hover and tooltip state. Replace
text-derived ids with ids derived from the Signal id plus the row index.

**Verify**: `bun run check` → exit 0.
`grep -n 'format!("line-' src/app.rs` → no matches.

## Test plan

The 32 existing `dashboard_state` tests **are** the regression suite for this
change: every accessor's output must be unchanged. Do not edit them.

Add two tests in `src/dashboard_state.rs` `mod tests`:

1. `unparseable_payload_still_renders_empty` — apply a
   `SignalSnapshotsUpdated` whose `payload_json` is `"not json"` (the existing
   suite already has a fixture doing this — reuse it) and assert
   `card_summary`, `card_detail` and `drill_in` return the same empty/default
   values they do today. This pins the `Value::Null` substitution.
2. `payload_is_parsed_once_per_apply` — a behavioural proxy: apply a snapshot,
   then assert `card_summary` returns the same string on two consecutive calls
   and that the state is unchanged between them. **Say plainly in the test's
   doc comment that this pins idempotence, not the parse count** — the parse
   count is not observable without instrumentation, and a test that claims
   otherwise is a lie in the suite.

**Verification**: `cargo test -p daku dashboard_state` → all pass, +2 tests, no
expectation edited.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -c "from_str" src/dashboard_state.rs` is **lower** than before your
      change (record both numbers in your report)
- [ ] `grep -n "serde_json::from_str" src/dashboard_state.rs` shows no call
      inside `summarize_payload`, `detail_from_payload`, `drift_mismatch`,
      `environment_build`, `drift_mismatch_lines` or `drill_in` bodies
- [ ] `git diff src/dashboard_state.rs | grep '^-.*assert' | wc -l` → `0`
      (no assertion was removed or changed)
- [ ] `grep -n 'format!("line-' src/app.rs` → no matches
- [ ] In `src/app.rs`, `compare_rows()` is called only inside the
      `strip.visible` branch
- [ ] `cargo test -p daku dashboard_state` → all pass, two more tests
- [ ] `git diff --name-only` lists only `src/dashboard_state.rs`, `src/app.rs`
      and `plans/README.md`
- [ ] `plans/README.md` status row for 056 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- **You need to change any existing test's expected value.** That means the
  refactor altered behaviour; the whole point is that it must not.
- `Value::Null` does not reproduce today's behaviour for some accessor — report
  which one and what differs, rather than special-casing it.
- You find yourself introducing a typed payload struct. That is explicitly out
  of scope; report it as a follow-up instead.
- Step 3 changes what the Operator sees (the strip appearing or disappearing in
  a case where it did not before).

## Maintenance notes

- The invariant: **`Snapshot.payload` is derived from `Snapshot.dto.payload_json`
  and must be rebuilt whenever the DTO is replaced.** Only `apply` constructs
  `Snapshot`; keep it that way, or the two can drift.
- Any new accessor reads `&Value`, never a string. A new `from_str` in this file
  is the thing to reject in review.
- This does **not** stop the shell repainting while a card is Waiting — it makes
  each of those frames cheap. If the repaint itself ever needs to stop, the
  lever is the `Skeleton`, and that is an ADR-0008 conversation, not a perf fix.
- Deliberately **not** done: caching the rendered `String`s. Parsing was the
  expensive part; string formatting on an already-parsed `Value` is not worth
  another layer of cache invalidation.
