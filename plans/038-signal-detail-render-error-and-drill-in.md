# Plan 038: Show why a Signal is red — render persisted `error`/`detail` under each card, then decide on drill-in

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- src/dashboard_state.rs src/app.rs docs/research`
> If `src/dashboard_state.rs` or `src/app.rs` changed since this plan was
> written, compare the "Current state" excerpts against the live code before
> proceeding; on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S (Steps 1–3 build) + S (Step 4 spike note)
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/013-asleep-never-degrades.md (introduces `skipped` payloads this plan must render sanely)
- **Category**: direction
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/61

## Why this matters

The spec's driving pains — dead MID, stuck jobs, silent integration failure (`docs/spec/v1.md` §5) — end today at a number and a coloured dot. The daemon already persists *why*: `availability.rs` writes an `error` string ("no credential for environment prod", "HTTP 429", DNS/timeouts) and every other Signal writes `detail` on its down path. The GPUI client never renders either, so a Keychain miss and a VPN outage both look like `down · unreachable`, and a first-run mistake is indistinguishable from an outage. Issues #12/#13 (decision records for the UI) name "Signal cards (drill-in)" and "each Signal has its own state on drill-in"; the prototype (`prototypes/environments-overview/index.html:420-421`) makes cards selectable. Rendering the text the daemon already has is the cheapest first step; the drill-in pane is a product decision this plan scopes as a spike.

## Current state

### Payload keys written by the daemon (verified at HEAD)

| Signal | Key on failure | Where |
|---|---|---|
| availability | `"error": <string|null>` (always present) | `crates/daku-core/src/availability.rs:134-139` |
| jobs | `"detail"` (+ `"reachability":"unreachable"`) | `crates/daku-core/src/jobs.rs:151-153` |
| syslog | `"detail"` | `crates/daku-core/src/syslog.rs:129-131` |
| mid_ecc | `"detail"` | `crates/daku-core/src/mid_ecc.rs:194-196` |
| outbound | `"detail"` | `crates/daku-core/src/outbound.rs:118-120` |
| drift | `"detail"` (down) / `"skipped":"need_two_environments"` | `crates/daku-core/src/drift.rs:369-371`, `:359` |
| last_clone | `"detail"` (state stays `healthy`) | `crates/daku-core/src/last_clone.rs:143-145` |
| any (after plan 013) | `"skipped": "asleep"|"unreachable"` | `crates/daku-core/src/persistence.rs` `persist_signal_skipped` |

### `src/dashboard_state.rs`

```rust
// :56-60
pub struct SignalCard {
    pub signal_id: &'static str,
    pub status: String,
    pub sparkline: Vec<f64>,
}

// :244-256
    pub fn card_summary(&self, signal_id: &str) -> String {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return String::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
        else {
            return String::new();
        };
        summarize_payload(signal_id, &snapshot.payload_json)
    }
```

`summarize_payload(signal_id, payload_json)` (`:295-370`) parses the payload with `serde_json::from_str::<serde_json::Value>` and formats one line per Signal. Tests: `mod tests` at `:497+`, fixture via `loaded()` (`:500-505`), `card_summary` asserted at `:549` (`"2 overdue · 1 error"`). Fixture payloads live in `fixture_events()` (`:376-466`); no fixture snapshot currently carries `error`/`detail`.

### `src/app.rs`

```rust
// :246-286 (signal_card) — the middle child renders one line:
            .child(div().mt(px(6.0)).text_size(px(15.0)).child(if waiting {
                crate::dashboard_state::WAITING.to_owned()
            } else if summary.is_empty() {
                card.status.clone()
            } else {
                summary
            }))
            .when(card.sparkline.len() >= 2, |element| {
                element.child(sparkline(&card.sparkline, theme.accent))
            })
```

Theme tokens available: `theme.text_tertiary`, `theme.text_ghost`, `theme.danger`, `theme.warning` (see `status_dot` at `:357-365`). Cards have no `on_click`.

Conventions: `DashboardState` is a pure model with unit tests; `app.rs` only renders. Keep vocabulary: Environment, Signal, Operator.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Model tests | `cargo test -p daku dashboard_state` | all pass |
| Build client | `cargo check -p daku` | exit 0 |
| Fixture run (manual) | `DAKU_UI_FIXTURE=1 bun run dev` | app shows fixture; new detail line visible on the fixture card you add |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `src/dashboard_state.rs` (`card_detail`, fixture, tests)
- `src/app.rs` (`signal_card` one extra line)
- `docs/research/signal-drill-in.md` (new decision note, Step 4)
- `plans/README.md` (status row)

**Out of scope**:
- Any daemon/protocol change — `payload_json` is free-form; no new keys are added by this plan.
- Building the drill-in pane, per-MID lists, mismatch lists (043), or deep links — Step 4 only *decides*.
- Card selection state — part of the Step 4 decision, not built here.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested: `Render Signal error/detail under cards.` then `Add signal drill-in decision note.`

## Steps

### Step 1: `card_detail` in the model

In `src/dashboard_state.rs`, next to `card_summary`, add:

```rust
    /// One-line diagnostic for the selected Environment's Signal: the daemon's
    /// persisted `error` (availability) or `detail` (other Signals) string,
    /// or a human phrase for a skipped probe. Empty when there is nothing to say.
    pub fn card_detail(&self, signal_id: &str) -> String {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return String::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
        else {
            return String::new();
        };
        detail_from_payload(&snapshot.payload_json)
    }
```

and the pure helper (below `summarize_payload`):

```rust
fn detail_from_payload(payload_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return String::new();
    };
    if let Some(reason) = value.get("skipped").and_then(|item| item.as_str()) {
        return match reason {
            "asleep" => "skipped · Environment asleep".to_owned(),
            "unreachable" => "skipped · Environment unreachable".to_owned(),
            "need_two_environments" => "needs two Environments".to_owned(),
            other => format!("skipped · {other}"),
        };
    }
    ["error", "detail"]
        .iter()
        .find_map(|key| value.get(*key).and_then(|item| item.as_str()))
        .map(|text| text.chars().take(160).collect())
        .unwrap_or_default()
}
```

(160-char cap keeps a long ureq error from blowing up the card; the full string stays in the payload.)

Add to `fixture_events()` on the `test` Environment one snapshot with a detail so the fixture exercises it — e.g. change `snap("outbound", "healthy", …)` for `test` to `snap("outbound", "down", r#"{"reachability":"unreachable","detail":"HTTP 429"}"#)`. Check no existing test asserts the `test` outbound card (grep `outbound` in the tests module — at HEAD none does).

Tests (model on `dashboard_state_jobs_samples_fill_sparkline` at `:531`):
- `card_detail_reads_error_and_detail`: `detail_from_payload(r#"{"reachability":"unreachable","error":"no credential for environment prod"}"#) == "no credential for environment prod"`; same for `detail`; `{}` → `""`; malformed JSON → `""`.
- `card_detail_phrases_skipped`: `{"skipped":"asleep"}` → `"skipped · Environment asleep"`; `{"skipped":"need_two_environments"}` → `"needs two Environments"`.
- `card_detail_for_selected_environment`: `loaded()`, `select("test")`, `card_detail("outbound") == "HTTP 429"`; `card_detail("jobs") == ""`.

**Verify**: `cargo test -p daku dashboard_state` → all pass, incl. 3 new.

### Step 2: Render it

In `src/app.rs` `signal_card`, compute `let detail = self.state.card_detail(card.signal_id);` next to `summary`, and after the summary child add:

```rust
            .when(!detail.is_empty(), |element| {
                element.child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(detail),
                )
            })
```

**Verify**: `cargo check -p daku` → exit 0. `DAKU_UI_FIXTURE=1 bun run dev` → select "Test": the Outbound card shows `HTTP 429` under its summary; other cards unchanged.

### Step 3: Gate

**Verify**: `bun run check` → exit 0.

### Step 4: Decision note for drill-in (spike, no product code)

Write `docs/research/signal-drill-in.md` (≤ 1 page) answering, with file:line evidence:

1. **What detail exists but is not persisted?** `mid_ecc.rs:18` fetches `status,validated,version,host_name` per MID but `mid_ecc.rs:172-177` persists only counts; drift persists only `mismatches` (list is plan 043); jobs/syslog/outbound persist counts only (row-level detail would need new Table API queries — cite the `sysparm_query` constants at `jobs.rs:15-17`, `syslog.rs:19`, `outbound.rs:15`).
2. **Options**: (a) bounded lists in `payload_json` (first N rows) rendered in an expandable card — cost: payload growth every tick vs ADR-0007 "latest snapshot, prune aggressively" (lists are per-snapshot, not history, so it stays within the ADR if bounded); (b) card selection (`selectedSignal` like the prototype) + a detail region under the cards; (c) deep-link "Open in ServiceNow" with the same encoded query — needs `instance_url` on `EnvironmentSummary` (plan 039) and breaks the "glance without opening ServiceNow" promise for the detail step.
3. **Recommendation** with a one-paragraph rationale and a follow-up plan stub (title, in-scope files, S/M estimate). Recommended default unless the maintainer objects: (a)+(b) for MID/ECC and drift only (the two Signals whose detail is already fetched), (c) for jobs/syslog/outbound.
4. **Open questions** for the Operator (max 3).

**Verify**: file exists; `grep -c 'file:' docs/research/signal-drill-in.md` ≥ 3 is not required — instead `grep -n 'Recommendation' docs/research/signal-drill-in.md` → 1 match.

## Test plan

- `src/dashboard_state.rs`: `card_detail_reads_error_and_detail`, `card_detail_phrases_skipped`, `card_detail_for_selected_environment` (Step 1).
- Manual fixture run (Step 2).
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'pub fn card_detail\|fn detail_from_payload' src/dashboard_state.rs` → 2 matches
- [ ] `grep -n 'card_detail' src/app.rs` → 1 match
- [ ] `cargo test -p daku dashboard_state` passes with the 3 new tests
- [ ] `docs/research/signal-drill-in.md` exists and contains a `Recommendation` section
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 038 updated

## STOP conditions

- `card_summary`/`summarize_payload`/`signal_card` no longer match the excerpts (plan 013 adds an early return in `summarize_payload` — that is expected; anything else, report).
- A fixture test breaks because a test now asserts the `test` outbound card — pick another fixture snapshot rather than editing the assertion.
- GPUI `.when`/`div().child` API differs from `app.rs` usage (GPUI pin moved) — report.

## Maintenance notes

- Any new failure path in a Signal must write `detail` (or `error` for availability) — the card renders whichever exists; keep it a short human string, never a secret or full response body.
- Plan 043 (drift mismatch list) and the follow-up chosen in Step 4 build on `card_detail`; if a detail region is added, `card_detail` becomes its first line, not a replacement.
- Reviewers: check the 160-char cap and that `skipped` phrases don't reach `summarize_payload` (plan 013 returns `""` there).
