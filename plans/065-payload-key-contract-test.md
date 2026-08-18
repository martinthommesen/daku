# Plan 065: The payload keys the daemon writes and the desktop reads are pinned to each other

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src src/dashboard_state.rs Cargo.toml`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW
- **Depends on**: `plans/048`, `plans/049`, `plans/057` (each adds or changes a
  payload key; land them first so the pinned fixtures are the final shape)
- **Category**: tests
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

`SignalSnapshotDto.payload_json` is an **untyped string** on the wire. Seven
collectors write JSON into it by key; `src/dashboard_state.rs` reads it back out
by key. Nothing connects the two.

Both sides are tested, and that is the problem: the daemon's tests assert what
it writes, the desktop's 32 tests assert what it renders from a **hand-written
fixture that duplicates the same keys**. Rename or retype a key and the daemon
writes it, the desktop renders `Waiting` or an em-dash, and every test on both
sides still passes.

**To be clear: every key matches today.** All seven Signals were checked. This
plan is not fixing a live break — it is closing the gap that makes one
invisible, on a seam that four other plans in this batch are actively editing
(`skip_targets` reasons, `build_matches` tri-state, `older_than_page`,
`truncated`).

One concrete gap the duplicate fixture already misses: the clone source's own
403 payload, `{"role":"source","supported":false}`, appears in no dashboard
test. `summarize_payload` renders it as a bare "clone source" and
`detail_from_payload` returns empty — so the source card looks healthy and
silent at the exact moment its targets are reporting that the source cannot list
clones.

## Current state

**The wire type — `crates/daku-protocol/src/protocol.rs:138-145`**:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalSnapshotDto {
    pub signal_id: String,
    pub state: String,
    pub observed_at: i64,
    pub payload_json: String,
}
```

The protocol round-trip tests never look inside `payload_json`.

**The writers** — one `serde_json::json!` per Signal, e.g.
`crates/daku-core/src/drift.rs`:

```rust
    let payload = serde_json::json!({
        "mismatches": mismatches,
        "build_matches": build_matches,
        "truncated": source.truncated || other.truncated,
        "mismatch_list": &mismatch_list[..mismatch_list.len().min(MISMATCH_LIST_LIMIT)],
        "mismatch_list_truncated": mismatch_list.len() > MISMATCH_LIST_LIMIT,
    });
```

plus `crates/daku-core/src/persistence.rs`'s two shared shapes:

```rust
    let payload = serde_json::json!({ "skipped": reason });
```
```rust
    let payload = serde_json::json!({
        "reachability": "unreachable",
        "detail": message,
    });
```

**The readers** — `src/dashboard_state.rs`: `summarize_payload`,
`detail_from_payload`, `drift_mismatch`, `environment_build`,
`drift_mismatch_lines`, `drill_in`. All key lookups on
`serde_json::Value`.

**The duplicate fixture** — `src/dashboard_state.rs` `fixture_events()` (around
`:697-814`), which also backs `DAKU_UI_FIXTURE=1`:

```rust
                snap(
                    "availability",
                    "healthy",
                    r#"{"reachability":"reachable","rtt_ms":142,"build":"glide-zurich-patch3"}"#,
                ),
```

**The precedent for crossing the crate boundary in tests** —
`crates/daku-client/Cargo.toml`:

```toml
[dev-dependencies]
daku-core = { path = "../daku-core" }
```

So a `[dev-dependencies]` edge from the root crate to `daku-core` is an
established pattern here, not a new one.

### Constraints you must honor

- **Do not type `payload_json`.** ADR-0007 and the protocol keep it an opaque
  string; typing it would be a protocol change and a `PROTOCOL_VERSION` bump.
  Plan 056 also explicitly rejects a typed payload struct on the client. Pin the
  contract with tests, not with types.
- **`daku-core` must be a `[dev-dependencies]` edge only** on the root crate. A
  regular dependency would pull the daemon runtime (rusqlite, ureq,
  security-framework) into the desktop binary.
- `DAKU_UI_FIXTURE=1` loads `fixture_events()`. If you change it, the fixture UI
  must still render the same states — that is the Operator's smoke path.
- `plans/028`'s rule: new `daku-core` tests use `TempDb` and
  `test_support::prod()`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Client tests | `cargo test -p daku dashboard_state` | all pass |
| Core tests | `cargo test -p daku-core` | all pass |
| Fixture UI | `DAKU_UI_FIXTURE=1 bun run dev` | Operator-run; renders as before |

## Scope

**In scope**:
- `Cargo.toml` (root — a `[dev-dependencies]` entry only)
- `src/dashboard_state.rs` (tests and `fixture_events`)
- `crates/daku-core/src/**` (a test-only payload-emitting helper, if needed)
- `crates/daku-core/tests/fixtures/payloads/` (new, if you take option B)

**Out of scope** (do NOT touch):
- `crates/daku-protocol/src/protocol.rs` — no wire change, no version bump.
- Any collector's production payload shape. If a test reveals a mismatch,
  **report it**; do not change a payload to make a test pass.
- The client's accessor logic.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Build the dashboard fixture from payloads the collectors actually produce (#90).`

## Steps

### Step 1: Choose how the two sides meet

**Option A — dev-dependency.** Add `daku-core` to the root crate's
`[dev-dependencies]` and, in `src/dashboard_state.rs`'s test module, build each
Signal's payload by calling the collector's own persistence path (via `TempDb`)
and reading the snapshot back. Highest fidelity: the test breaks the moment a
collector changes a key.

**Option B — committed canonical payloads.** Add a `daku-core` test that writes
each Signal's canonical payload to
`crates/daku-core/tests/fixtures/payloads/<signal>.json`, and have
`src/dashboard_state.rs` `include_str!` those. Simpler, no dependency edge, but
the files can go stale unless the `daku-core` test asserts they match what it
generates — so that assertion is mandatory under this option.

Pick one, record why in your report. Prefer A unless the dependency edge causes
a problem.

**Verify**: `cargo tree -p daku --depth 1` — `daku-core` appears only as a dev
dependency (or not at all, under B). `cargo build -p daku --release` still
builds without it.

### Step 2: Cover all seven Signals plus the shared shapes

The contract to pin, one case each:

| Signal | Cases |
|--------|-------|
| availability | reachable + build; asleep; unreachable |
| jobs | counts; skipped |
| syslog | count; skipped |
| mid_ecc | counts; skipped |
| outbound | count; skipped |
| drift | source role; compare with mismatches; skipped |
| last_clone | source role (`supported: true` **and** `supported: false`); target with `completed` + `age_days`; target never cloned; every skip reason |

Plus the two shared shapes from `persistence.rs`: `persist_signal_skipped`'s
`{"skipped": …}` and `persist_signal_down`'s `{"reachability","detail"}`.

For each, assert the client renders a **non-degenerate** result — i.e.
`card_summary` or `card_detail` returns something other than the empty string,
except where empty is the intended, documented outcome (a `skipped` payload
deliberately returns an empty summary). Assert the intended outcome explicitly
rather than accepting whatever comes back.

**Verify**: `cargo test -p daku dashboard_state` → all pass.

### Step 3: Cover the clone-source 403 case

Add the missing case: a source payload of `{"role":"source","supported":false}`.
Decide what the card *should* say — the source cannot list clones, which is
information the Operator needs — and assert it. If today's output is a bare
"clone source" with no detail, **that is a finding**: report it, and either fix
the phrase in `src/dashboard_state.rs` (a one-line change, in scope) or record
it as a follow-up.

**Verify**: `cargo test -p daku dashboard_state` → all pass.

### Step 4: Rebuild `fixture_events` from the pinned payloads

Once the payloads come from a single source, use it for `fixture_events()` too,
so `DAKU_UI_FIXTURE=1` shows real collector output rather than a hand-written
imitation.

**Verify**: `DAKU_UI_FIXTURE=1 bun run dev` — Operator confirms the fixture
Environment detail renders as before. Record their answer.

## Test plan

Steps 2–3 are the test plan. Add one meta-test that makes the contract
self-enforcing:

`every_signal_id_has_a_pinned_payload` — iterate `SIGNAL_IDS` (the constant
`src/dashboard_state.rs` already uses to validate card selection) and assert
each has at least one pinned payload case. A new Signal then fails this test
until someone pins it, which is the whole point.

**Verification**: `cargo test -p daku dashboard_state` → all pass, with cases
for all seven Signals; `cargo test -p daku-core` → all pass.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] Every id in `SIGNAL_IDS` has at least one pinned payload case, enforced by
      `every_signal_id_has_a_pinned_payload`
- [ ] A payload case exists for `{"role":"source","supported":false}`
- [ ] `grep -n "daku-core" Cargo.toml` → appears only under `[dev-dependencies]`
      (option A) or not at all (option B)
- [ ] `cargo build -p daku --release` exits 0
- [ ] `cargo test -p daku dashboard_state` → all pass
- [ ] Your report records which option you chose, and the Operator's Step 4
      confirmation
- [ ] No `json!` was added to `crates/daku-core/src` **outside** a
      `#[cfg(test)]` module — read `git diff crates/daku-core/src` and confirm;
      option A's test-only payload helper is allowed, a production payload
      change is not
- [ ] `plans/README.md` status row for 065 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- **A pinned payload does not render.** That is a live contract break — report
  the key and both sides, and do not change the collector to match the client or
  vice versa without saying so.
- Option A pulls `daku-core` into the release binary
  (`cargo tree -p daku --depth 1` shows it outside dev-dependencies).
- The Operator reports the fixture UI changed after Step 4.
- Pinning a payload requires editing a collector's production `json!`.

## Maintenance notes

- **The rule this establishes**: a new key on either side needs a pinned case.
  `every_signal_id_has_a_pinned_payload` enforces it for whole Signals; keys
  within a Signal still rely on review.
- Four other plans in this batch add payload keys (`clone_source_unreachable`,
  a tri-state `build_matches`, `older_than_page`, a rendered `truncated`). If
  any land after this one, they each add a case here.
- Deliberately **not** done: typing `payload_json`. That is a protocol change,
  and plan 056 rejects a typed struct on the client for the same reason —
  tolerating unknown and missing keys is a feature of the current shape.
