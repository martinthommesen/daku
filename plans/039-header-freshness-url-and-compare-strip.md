# Plan 039: Environment header shows freshness and URL; compare strip shows drift and last-clone, as the prototype does

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol/src/protocol.rs crates/daku-core/src/health.rs crates/daku-core/src/server.rs src/dashboard_state.rs src/app.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/014-replay-dashboard-on-subscribe.md (touches `server.rs`; land first to avoid conflicts)
- **Category**: direction
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/62

## Why this matters

daku is "open and look" (`docs/spec/v1.md` §3). A passive dashboard is only trustworthy if it shows **how fresh** it is: today a "healthy" prod whose collector stalled 30 minutes ago looks identical to a live one. `EnvironmentSummary.last_observed_at` is already on the wire (`crates/daku-protocol/src/protocol.rs:117-125`) but never rendered (`git grep last_observed_at src/` → only the fixture builder). The decided header (issue #13: "selected Environment header (URL, health rollup, freshness)"; prototype `prototypes/environments-overview/index.html:451`: `${env.url} · ${healthLabel} · polled ~2 min ago`) also shows the URL — the thing the Operator copies to go fix something. And the compare strip (ADR-0005 / spec §8 "vs other Environments") is decided UI that today prints only builds (`src/app.rs:376-418`) while the prototype (`index.html:428-433`) shows drift and last-clone per other Environment; spec §5 calls drift and last-clone "first-class".

Three build steps, all additive: freshness (UI only), URL (one new wire field → `PROTOCOL_VERSION` bump), richer strip (model + UI).

## Current state

### `crates/daku-protocol/src/protocol.rs`

```rust
// :8
pub const PROTOCOL_VERSION: u32 = 1;
// :117-125
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub id: String,
    pub label: String,
    pub platform_id: String,
    pub health: EnvironmentHealth,
    pub reachability: Reachability,
    pub last_observed_at: Option<i64>,
}
```

Tests that pin the version/shape: `protocol.rs` tests `environments_updated_round_trips` (constructs `EnvironmentSummary` literally), `protocol_version_is_daku_domain` (`assert_eq!(PROTOCOL_VERSION, 1)`), and `crates/daku-core/src/server.rs:544` (`assert_eq!(PROTOCOL_VERSION, 1)`). Both client (`crates/daku-client/src/client.rs:86,96`, `process.rs:201`) and daemon (`server.rs:296-306`) compare against the same constant, so a bump needs no other change.

### `crates/daku-core/src/health.rs:46-80` (`publish_dashboard`)

Builds `EnvironmentSummary { id, label, platform_id: SERVICENOW_PLATFORM_ID.into(), health, reachability, last_observed_at: env_snaps.iter().map(|s| s.observed_at).max() }` from `environments: &[EnvironmentConfig]` — and `EnvironmentConfig` (`crates/daku-core/src/config.rs:22-31`) has `pub instance_url: String`.

### `src/dashboard_state.rs`

- Fixture builder `env(id, label, health, reachability)` at `:468-481` sets `last_observed_at: Some(1_700_000_000)`.
- `compare_rows()` at `:258-268` returns `Vec<(String, String, Option<String>)>` = `(id, label, build)` via `environment_build(&self.snapshots, &id)` (`:271-282`).
- `summarize_payload(signal_id, payload_json)` at `:295-370` already formats `"drift"` (`"source of truth"` / `"{count} plugins differ"` / `""`) and `"last_clone"` (raw `completed` string). `card_summary` (`:244-256`) reads the *selected* Environment only.
- Selected Environment: `selected() -> Option<&EnvironmentSummary>`.

### `src/app.rs`

```rust
// :170-190 (header inside render_detail)
                    .child(
                        div()
                            .px(px(22.0)).pt(px(18.0)).pb(px(10.0)).border_b_1().border_color(theme.border)
                            .child(div().text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD).child(environment.label.clone()))
                            .child(
                                div().mt(px(6.0)).flex().flex_row().items_center().gap(px(8.0))
                                    .child(health_badge(environment.health, theme))
                                    .child(reachability_badge(environment.reachability, theme)),
                            ),
                    )
// :419-460 compare_strip(has_mismatch, selected_id, rows: &[(String, String, Option<String>)], theme)
//   renders `format!("{label}: {}", build.as_deref().unwrap_or("—"))` per other Environment
```

`listen_dashboard` (`:48-98`) shows the GPUI async pattern: `cx.spawn(async move |this, cx| { … cx.background_executor().spawn(...).await … this.update(cx, |this, cx| { …; cx.notify(); }) })`. The pinned GPUI has `BackgroundExecutor::timer(&self, Duration) -> Task<()>` (zed `crates/gpui/src/executor.rs:183`).

Vocabulary: Environment, Signal, Operator; "clone source" for the drift role.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Health tests | `cargo test -p daku-core health_rollup_publish` | pass |
| Model tests | `cargo test -p daku dashboard_state` | all pass |
| Client build | `cargo check -p daku` | exit 0 |
| Fixture run | `DAKU_UI_FIXTURE=1 bun run dev` | header shows URL + "polled …"; strip shows drift/clone |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-protocol/src/protocol.rs` (`instance_url` field, `PROTOCOL_VERSION` +1, tests)
- `crates/daku-core/src/health.rs` (populate `instance_url`; test)
- `crates/daku-core/src/server.rs:544` (version assertion only)
- `src/dashboard_state.rs` (freshness helper, `compare_rows` shape, fixture, tests)
- `src/app.rs` (header line, strip rows, 30 s refresh timer)
- `plans/README.md` (status row)

**Out of scope**:
- Passing `poll_interval_secs` on the wire. Decision: use a fixed `STALE_AFTER_SECS = 300` in the client (2.5× the default cadence). Rationale: the interval is Operator-tunable but rarely changed; putting it on the wire adds a second protocol field for a threshold the UI would still have to multiply. Mark it `// ponytail: fixed threshold; put poll_interval_secs on EnvironmentsUpdated if Operators tune cadence.`
- Rendering `instance_url` anywhere except the header; deep links (plan 038 spike).
- Any Signal/collector change.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested: `Show polled-ago and instance URL in the Environment header (protocol v2).` then `Compare strip shows drift and last-clone per Environment.`

## Steps

### Step 1: Freshness in the model

In `src/dashboard_state.rs` add:

```rust
/// Older than this and the header tints "polled … ago" as stale.
// ponytail: fixed threshold (2.5× default cadence); put poll_interval_secs on
// EnvironmentsUpdated if Operators start tuning cadence.
pub const STALE_AFTER_SECS: i64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Freshness {
    pub label: String,
    pub stale: bool,
}

/// "polled 42 s ago" / "polled 3 min ago" / "polled 2 h ago" for the selected
/// Environment, or `None` before the first observation.
pub fn freshness(last_observed_at: Option<i64>, now: i64) -> Option<Freshness> {
    let age = now.saturating_sub(last_observed_at?).max(0);
    let label = if age < 60 {
        format!("polled {age} s ago")
    } else if age < 3600 {
        format!("polled {} min ago", age / 60)
    } else {
        format!("polled {} h ago", age / 3600)
    };
    Some(Freshness { label, stale: age > STALE_AFTER_SECS })
}
```

Tests: `freshness_formats_seconds_minutes_hours` (`Some(1000)`, now `1042` → `"polled 42 s ago"`, not stale; now `1000+180` → `"polled 3 min ago"`; now `1000+7200` → `"polled 2 h ago"`, stale), `freshness_none_before_first_observation` (`None` → `None`), `freshness_stale_after_threshold` (`age == STALE_AFTER_SECS` → not stale; `+1` → stale).

**Verify**: `cargo test -p daku dashboard_state` → pass incl. 3 new.

### Step 2: Render freshness + a 30 s refresh

In `src/app.rs` `render_detail`, inside the header, after the badges row add a third child:

```rust
                            .when_some(
                                crate::dashboard_state::freshness(environment.last_observed_at, unix_now()),
                                |element, freshness| {
                                    element.child(
                                        div().mt(px(4.0)).text_size(px(12.0))
                                            .text_color(if freshness.stale { theme.warning } else { theme.text_tertiary })
                                            .child(freshness.label),
                                    )
                                },
                            )
```

with a file-local helper `fn unix_now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }`.

Because renders only happen on `cx.notify()`, a stalled daemon would freeze the label. In `Daku::new` (where `listen_dashboard(&supervisor, cx)` is called — read `src/app.rs:20-46`), add a ticker modelled on `listen_dashboard`:

```rust
fn tick_freshness(cx: &mut Context<Daku>) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(std::time::Duration::from_secs(30)).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
    .detach();
}
```

**Verify**: `cargo check -p daku` → exit 0. `DAKU_UI_FIXTURE=1 bun run dev` → header shows `polled … h ago` in warning colour (fixture `last_observed_at` is 2023).

### Step 3: `instance_url` on the wire

- `protocol.rs`: add `pub instance_url: String,` to `EnvironmentSummary` (after `label`); increment `PROTOCOL_VERSION` by one from its live value (plans 020 and 029 may already have bumped it — never set a fixed number); update `environments_updated_round_trips` (add the field, assert `json["environments"][0]["instanceUrl"]`), `protocol_version_is_daku_domain` and `crates/daku-core/src/server.rs:544` to the new value.
- `server.rs:544`: `assert_eq!(PROTOCOL_VERSION, 2);` (or delete the assertion if plan 025 already replaced that test).
- `health.rs` `publish_dashboard`: `instance_url: environment.instance_url.clone(),`. Extend `health_rollup_publish_emits_dashboard_events_after_fixture` to assert the summary's `instance_url` equals the fixture Environment's.
- `src/dashboard_state.rs` fixture `env(...)`: add `instance_url: format!("https://{id}.example.service-now.com")`.
- `src/app.rs` header: render `environment.instance_url` (trim `https://` like the prototype) as a `text_tertiary` line under the label — the same style as the freshness line; put both on one row separated by ` · ` if you prefer, matching `index.html:451`.

**Verify**: `cargo test -p daku-protocol` and `cargo test -p daku-core health_rollup_publish` pass; `cargo check --workspace` → exit 0 (any other literal `EnvironmentSummary { … }` construction fails to compile until updated — fix those in-scope only; if one is out of scope, STOP).

### Step 4: Compare strip with drift and last-clone

In `src/dashboard_state.rs`, change `compare_rows` to return a small struct:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRow {
    pub id: String,
    pub label: String,
    pub build: Option<String>,
    pub drift: String,      // summarize_payload("drift", …) or ""
    pub last_clone: String, // summarize_payload("last_clone", …) or ""
}
```

filling `drift`/`last_clone` from `self.snapshots.get(&id).and_then(|m| m.get("drift"))` etc. Update `src/app.rs` `compare_strip` to take `&[CompareRow]` and render per other Environment: `"{label}: {build or —}"` plus, when non-empty, ` · drift {drift}` and ` · clone {last_clone}` (one `div` per row, `text_secondary`; you may split into two lines if wrapping looks bad).

Tests: `compare_rows_include_drift_and_last_clone` — `loaded()`: the `test` row has `drift == "3 plugins differ"` and `last_clone == "2026-08-05 09:00:00"`; the `prod` row has `drift == "source of truth"`.

**Verify**: `cargo test -p daku dashboard_state` → pass; `DAKU_UI_FIXTURE=1 bun run dev` → strip under prod shows `Test: glide-yokohama-patch1 · drift 3 plugins differ · clone 2026-08-05 09:00:00`.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `src/dashboard_state.rs`: 3 freshness tests (Step 1), `compare_rows_include_drift_and_last_clone` (Step 4).
- `crates/daku-protocol/src/protocol.rs`: updated round-trip + version tests (Step 3).
- `crates/daku-core/src/health.rs`: `instance_url` assertion added to the publish test (Step 3).
- Manual fixture run for the rendering (Steps 2, 4).

## Done criteria

- [ ] `grep -n 'pub instance_url' crates/daku-protocol/src/protocol.rs` → 1 match; `PROTOCOL_VERSION` is exactly one higher than before this plan (`git show HEAD~1:crates/daku-protocol/src/protocol.rs | grep PROTOCOL_VERSION`)
- [ ] `grep -n 'pub fn freshness\|STALE_AFTER_SECS\|pub struct CompareRow' src/dashboard_state.rs` → 3+ matches
- [ ] `grep -n 'freshness(\|tick_freshness\|instance_url' src/app.rs` → ≥3 matches
- [ ] `cargo test --workspace --no-fail-fast` → 0 failed
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 039 updated

## STOP conditions

- `EnvironmentSummary`, `publish_dashboard`, `compare_rows`, or the header block no longer match the excerpts.
- `BackgroundExecutor::timer` does not exist on the pinned GPUI (API moved) — report; do not hand-roll a thread.
- An `EnvironmentSummary` literal exists outside the in-scope files.
- Plan 025 or 029 already changed `PROTOCOL_VERSION` — bump from whatever it is, and say so.

## Maintenance notes

- Any future field on `EnvironmentSummary` bumps `PROTOCOL_VERSION` again; keep the fixture builder `env()` and the round-trip test in step.
- If Operators tune `poll_interval_secs` far from 120 s, replace `STALE_AFTER_SECS` with a value carried on `EnvironmentsUpdated` (one more field, one more bump).
- Reviewers: `instance_url` is non-secret but "sensitive by default" (issue #6); it is fine on the loopback wire — note this in the field's doc comment.
