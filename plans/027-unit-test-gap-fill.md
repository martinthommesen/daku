# Plan 027: Fill the unit-test gaps — DashboardState branches, ServiceNow client failure modes, `load_environments` negatives, and fix two vacuous tests

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- src/dashboard_state.rs crates/daku-core/src/servicenow.rs crates/daku-core/src/config.rs crates/daku-core/src/collector.rs crates/daku-client/src/persistence.rs src/updater.rs`
> Several of these files are legitimately changed by plans 012, 013, 017,
> 020, 021 (see "Ordering"). For each file that changed, re-read the excerpt
> below against the live code; if a symbol this plan tests no longer exists,
> apply the ordering rule for that section instead of treating it as a STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW (tests only, plus one small extraction in `crates/daku-client/src/persistence.rs`)
- **Depends on**: plans/011-green-baseline-check-gate.md; ordering constraints with 012/013/017/020/021 below
- **Category**: tests
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/54

## Why this matters

Four pockets of logic are either untested or "tested" by assertions that cannot fail:

1. `src/dashboard_state.rs` is the entire GPUI view-model. Its 6 tests all reuse one fixture and never exercise: selection fallback when the selected Environment disappears, `select()` on an unknown id, `muted` while disconnected, `None` sample gap-filling, the `<2 Environments` compare strip, the pairwise/plugin drift branches, or `card_summary` for any signal but `jobs`. Regressions here only show up in a running app (no UI tests exist).
2. `crates/daku-core/src/servicenow.rs` — the only outbound dependency — has no test for a transport error, an OAuth token endpoint that fails, token expiry with a moving clock, a missing Credential, or `urlencode` of a secret containing `&`/`+`/`%`.
3. `crates/daku-core/src/config.rs::load_environments` reads Operator-edited JSON; only the happy path (the committed example file) is tested.
4. Two green tests assert nothing: `src/updater.rs::routing_user_driver_satisfies_sparkle_protocols` returns early whenever the Debug.app isn't built (always under plain `cargo test`), and `crates/daku-client/src/persistence.rs::desktop_settings_paths_are_build_specific` compares a function with the expression that implements it — while the real logic in `load_or_create_app_settings` (legacy migration, token minting, rewrite conditions) is untestable because it takes no path.

## Current state

### `src/dashboard_state.rs` (verified at HEAD)

- `DashboardState { connected, environments, selected_id, snapshots: HashMap<String, HashMap<String, SignalSnapshotDto>>, samples: HashMap<(String,String), Vec<SamplePoint>> }` (`:37-43`); `SIGNAL_IDS` (`:10-18`), `WAITING = "Waiting"` (`:20`), `TREND_SIGNALS = ["jobs","syslog"]` (`:35`).
- `apply` (`:83-121`): `EnvironmentsUpdated` replaces `environments` and, if `selected_id` is `None` **or no longer present**, sets it to the first Environment's id (`:86-97`); `SignalSnapshotsUpdated` replaces that Environment's map; `SignalSamplesUpdated` replaces that (env, signal) vec; other messages ignored (`:120`).
- `select(&mut self, id)` (`:123-131`) — no-op for unknown ids. `selected_id()`, `selected()` (`:135-144`). `sidebar()` (`:146-157`) sets `muted: !self.connected`.
- `cards()` (`:159-188`): status = snapshot state or `WAITING`; sparkline maps `point.value_real.unwrap_or(0.0)` for `TREND_SIGNALS` only.
- `compare_strip()` (`:190-227`): `<2` Environments → `{visible:false, has_mismatch:false}`; with a clone source (`clone_source_id`, `:229-247`, = the Environment whose `drift` payload has `"role":"source"`) mismatch = any build ≠ source build; without one, pairwise `builds.windows(2).any(|p| p[0]!=p[1])`; plus `plugin_mismatch` via `drift_mismatch` (`:285-293`: `build_matches == false` or `mismatches > 0`) for non-source Environments.
- `card_summary(signal_id)` (`:249-262`) → `summarize_payload` (`:295-370`), returns `""` for unparseable JSON; per-signal formats: availability `"{ms} ms · {build}"`/`"{ms} ms"`/build/`""`; jobs `"{n} overdue · {m} error"`; syslog `"{n} errors / h"`; mid_ecc `"{up}/{total} up · queue {q}"`; outbound `"{n} HTTP fail"`; drift `"source of truth"` | `"{n} plugins differ"` | `""`; last_clone = `completed` string or `""`. **Plan 013 adds** an early `return String::new()` when the payload has a `"skipped"` key.
- Fixture: `fixture_events()` (`:377-459`) — two Environments `prod` (Degraded, Reachable, drift `role:source`) and `test` (Healthy, Asleep, drift `mismatches:3, build_matches:false`, last_clone `completed:"2026-08-05 09:00:00"`), jobs samples for prod `[1,2,3]`, empty syslog samples. Helpers `env(id,label,health,reachability)` (`:461-474`) and `snap(signal_id,state,payload_json)` (`:476-483`) are module-level (usable from tests).
- Tests (`:485-578`): `loaded()` fixture; `dashboard_state_environments_updated_preserves_ids_labels_order`, `_health_degraded_maps_dot`, `_asleep_reachability_does_not_change_health`, `_jobs_samples_fill_sparkline` (asserts `card_summary("jobs") == "2 overdue · 1 error"`), `_missing_snapshot_is_waiting`, `_compare_strip_build_mismatch`.

### `crates/daku-core/src/servicenow.rs` (verified at HEAD)

- `ServiceNowClient::request(&self, environment, credentials: &dyn CredentialStore, method, path, body) -> anyhow::Result<HttpResponse>` (`:90-121`): `authorize` → `send` → on 401 with OAuth, drop the cached token and retry once.
- `authorize` (`:123-146`): `credentials.get(id)?` → `None` → `anyhow!("no credential for environment {id}")`; Basic → `basic_authorization`; OAuth → `oauth_access`.
- `oauth_access` (`:147-191`): cache hit if `clock.now() < valid_until`; else POST `/oauth_token.do` with `urlencode(client_id/secret)`; non-200 → `Err("oauth_token.do returned HTTP {status} for {id}")`; body → `AccessGrant { access_token, expires_in: Option<u64> }` with `.context("oauth token JSON")`; `expires_in` default 1800; **plan 012 clamps `expires_in` and uses `checked_add`**.
- `send` (`:193-205`): `self.transport.execute(request)?` — a transport `Err` propagates unchanged; 429 retry ≤ `MAX_429_RETRIES = 2` with `retry_after_delay` (plan 012 caps it).
- `urlencode` (`:268-279`): unreserved `A-Za-z0-9-_.~` kept, everything else `%XX` uppercase.
- Test scaffolding (`:346-450`): `ScriptedTransport::new(Vec<HttpResponse>)` (+`requests()`), `SharedTransport(Arc<ScriptedTransport>)` (`:664-670`), `RecordingClock` (fixed `now()` = epoch+1_700_000_000 s, records sleeps), `basic_env()` (id `dev`, Basic), `oauth_env()` (id `prod`, OAuth), `ok_table()`, `token_ok(token)`; `MemoryCredentialStore::default()` + `.insert(id, blob)`. Existing tests: 429 ×3, basic auth header, `servicenow_http_oauth_cache_skips_second_token_fetch` (`:576-615`), `servicenow_http_oauth_refreshes_once_on_401` (`:618`), `parse_aggregate_count_reads_stats_count_string`.
- `Clock` trait (`:43-46`): `fn now(&self) -> SystemTime; fn sleep(&self, Duration);` — an advancing clock is a 10-line `struct AdvancingClock(Mutex<SystemTime>)` in the test module.

### `crates/daku-core/src/config.rs` (verified at HEAD, whole file 126 lines)

```rust
// :15-31
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod { OauthClientCredentials, Basic }
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EnvironmentConfig { pub id: String, pub label: String, pub instance_url: String, pub auth_method: AuthMethod, pub sort_order: i64, #[serde(default)] pub clone_source: bool }
// :40-46
pub fn load_environments(path: &Path) -> anyhow::Result<Vec<EnvironmentConfig>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut environments: Vec<EnvironmentConfig> = serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    environments.sort_by_key(|environment| environment.sort_order);
    Ok(environments)
}
```

Only test: `example_environments_json_parses` (`:111-125`) reads `environments.example.json` via `CARGO_MANIFEST_DIR`. `crates/daku-core/src/collector.rs:155-202` `start_default_loop(path, store, settings, shutdown) -> Option<Receiver<ServerMessage>>` returns `None` on missing/unparseable/empty file (prints to stderr). **Plan 017** adds `https://`/userinfo validation to `load_environments` — its tests cover the URL rules; do not duplicate.

### `crates/daku-client/src/persistence.rs` (verified at HEAD, whole file 242 lines)

- `read_app_settings_source(app_settings_path: &Path, legacy_settings_paths: &[PathBuf]) -> io::Result<Option<(Vec<u8>, bool)>>` (`:100-117`) — primary bytes with `true`, else first legacy with `false`, else `None`. Path-injected: **testable as-is**.
- `load_or_create_app_settings() -> io::Result<AppSettings>` (`:119-143`) uses `default_app_settings_path()`/`default_legacy_settings_paths()` (`:70-88`, build-specific: debug → `<repo>/temp/app.json`, release → `~/.daku/app.json`), then: `token_was_persisted` = the JSON has a non-empty `daemon_exposure.token`; parse `AppSettings` (default on `None`); `minted = settings.daemon_exposure.ensure_token()`; rewrite via `write_json_atomically` (0600, `:188-205`) when `!loaded_from_primary || !token_was_persisted || minted`.
- The vacuous test `desktop_settings_paths_are_build_specific` (`:215-241`).
- **Plan 020** reshapes `AppSettings` (drops `analytics_enabled`/`theme`/`language`, may delete `StateStore`/window state). The token-minting/migration logic in `load_or_create_app_settings` survives 020 (it is about `daemon_exposure`, which stays). Write the tests against `daemon_exposure.token` only.

### `src/updater.rs` (verified at HEAD)

`routing_user_driver_satisfies_sparkle_protocols` (`:811-830`): computes `target/debug/Daku daku Debug.app/Contents/Frameworks/Sparkle.framework/Sparkle`; `if !library.exists() { return; }` — so under `cargo test` it passes vacuously. **Plan 021** deletes the custom `UserDriver` and this test.

### Ordering

- **012** (servicenow caps): land first or in either order — the tests here do not overlap (no Retry-After/`expires_in`-cap tests here).
- **013** (asleep): changes `summarize_payload` (skipped → `""`); the `card_summary` cases below hold either way.
- **017** (https validation): its tests own the URL rules; this plan's config tests only cover parse errors, unknown `auth_method`, duplicate ids, and the `start_default_loop` `None` path.
- **020** (settings cleanup): if landed first, `AppSettings` has fewer fields — Step 4 still applies (`daemon_exposure` remains). If 020 has **not** landed, do not add tests for `analytics_enabled`/`theme`/`language`.
- **021** (updater): if landed first, `routing_user_driver_satisfies_sparkle_protocols` no longer exists — skip Step 5a.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Dashboard tests | `cargo test -p daku dashboard_state` | all pass |
| ServiceNow tests | `cargo test -p daku-core servicenow_http` | all pass |
| Config tests | `cargo test -p daku-core load_environments` and `cargo test -p daku-core start_default_loop` | all pass |
| Client persistence tests | `cargo test -p daku-client app_settings` | all pass |
| Updater tests | `cargo test -p daku updater` | pass (1 ignored if Step 5a applies) |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `src/dashboard_state.rs` (tests only)
- `crates/daku-core/src/servicenow.rs` (tests only)
- `crates/daku-core/src/config.rs` (tests only)
- `crates/daku-core/src/collector.rs` (tests only)
- `crates/daku-client/src/persistence.rs` (extract `load_or_create_app_settings_at(path, legacy_paths)`; keep `load_or_create_app_settings()` as a one-line wrapper; replace the vacuous test)
- `src/updater.rs` (one `#[ignore]` attribute, only if plan 021 has not landed)
- `plans/README.md` (status row)

**Out of scope**:
- Any behaviour change. If a test reveals a bug (e.g. duplicate ids), **pin the current behaviour** in the test and note it in the report — do not fix.
- Retry-After / `expires_in` cap tests (plan 012), URL validation tests (017), the loopback/process integration tests (025/026), the temp-DB helper (028 — if it has landed, use `TempDb` for any new DB-backed test; none is required here).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Fill unit-test gaps in dashboard state, ServiceNow client, config, and client persistence.`

## Steps

### Step 1: DashboardState branches (`src/dashboard_state.rs` `mod tests`)

Add, modelled on `dashboard_state_compare_strip_build_mismatch` (`:571-577`) and using `loaded()`, `env(..)`, `snap(..)`:

1. `dashboard_state_removed_selected_environment_falls_back_to_first`: `loaded()`; `state.select("test")`; apply `EnvironmentsUpdated { environments: vec![env("prod", "Production", Healthy, Reachable)] }` → `selected_id() == Some("prod")`. Then apply `EnvironmentsUpdated { environments: vec![] }` → `selected_id() == None`.
2. `dashboard_state_select_unknown_id_is_noop`: `loaded()`; `select("nope")` → `selected_id() == Some("prod")`.
3. `dashboard_state_disconnected_mutes_every_row`: `loaded()`; `set_connected(false)` → `sidebar().iter().all(|r| r.muted)`; `set_connected(true)` → none muted.
4. `dashboard_state_none_sample_becomes_zero`: `loaded()`; apply `SignalSamplesUpdated { environment_id: "prod", signal_id: "jobs", points: vec![SamplePoint{observed_at:1, value_real:None}, SamplePoint{observed_at:2, value_real:Some(4.0)}] }` → jobs card sparkline `== [0.0, 4.0]`; and a non-trend signal (`availability`) card has an empty sparkline even if samples are applied for it.
5. `dashboard_state_single_environment_hides_compare_strip`: `DashboardState::new()`; apply `EnvironmentsUpdated` with one env → `compare_strip() == CompareStrip { visible: false, has_mismatch: false }`.
6. `dashboard_state_pairwise_build_mismatch_without_clone_source`: two envs, no drift `role:source`; availability builds `"a"` and `"a"` → `has_mismatch == false`; then re-apply `test` with build `"b"` → `true`.
7. `dashboard_state_plugin_only_mismatch`: two envs, same build, `prod` drift `{"role":"source"}`, `test` drift `{"mismatches":1,"build_matches":true}` → `has_mismatch == true`; with `{"mismatches":0,"build_matches":true}` → `false`.
8. `dashboard_state_card_summary_per_signal`: `loaded()`; assert exact strings — `card_summary("availability") == "142 ms · glide-zurich-patch3"`, `"syslog" == "38 errors / h"`, `"mid_ecc" == "3/3 up · queue 12"`, `"outbound" == "4 HTTP fail"`, `"drift" == "source of truth"`; `select("test")` → `"drift" == "3 plugins differ"`, `"last_clone" == "2026-08-05 09:00:00"`; apply `SignalSnapshotsUpdated` for `test` with `snap("jobs","healthy","not json")` → `card_summary("jobs") == ""`.
9. `dashboard_state_ignores_non_dashboard_messages`: `loaded()`; apply `ServerMessage::ShuttingDown` → `sidebar().len() == 2` and selection unchanged.

**Verify**: `cargo test -p daku dashboard_state` → previous 6 + 9 new pass.

### Step 2: ServiceNow client failure modes (`crates/daku-core/src/servicenow.rs` `mod tests`)

Add an advancing clock next to `RecordingClock`:

```rust
    struct AdvancingClock(Mutex<SystemTime>);
    impl AdvancingClock {
        fn advance(&self, by: Duration) { let mut now = self.0.lock().expect("clock"); *now += by; }
    }
    impl Clock for AdvancingClock {
        fn now(&self) -> SystemTime { *self.0.lock().expect("clock") }
        fn sleep(&self, duration: Duration) { self.advance(duration); }
    }
```

Tests (model on `servicenow_http_oauth_cache_skips_second_token_fetch`, `:576`):

1. `servicenow_http_transport_error_propagates`: `ScriptedTransport::new(vec![])` (no responses → its `execute` returns `Err("no scripted response left for …")`) with `basic_env()` → `request(..)` is `Err` whose string contains `"no scripted response left"`.
2. `servicenow_http_missing_credential_is_an_error`: empty `MemoryCredentialStore` → `Err` containing `"no credential for environment dev"`.
3. `servicenow_http_oauth_token_endpoint_non_200_is_an_error`: `oauth_env()`, scripted `[HttpResponse{status:401, headers: vec![], body: "{}".into()}]` → `Err` containing `"oauth_token.do returned HTTP 401"`; and the transport saw exactly one request (the token POST).
4. `servicenow_http_oauth_token_body_not_json_is_an_error`: scripted `[HttpResponse{status:200, headers: vec![], body: "<html>".into()}]` → `Err` containing `"oauth token JSON"`.
5. `servicenow_http_oauth_refetches_after_expiry`: `Arc<AdvancingClock>` starting at `UNIX_EPOCH + 1_700_000_000 s`; scripted `[token_ok("tok-1"), ok_table(), token_ok("tok-2"), ok_table()]` — but `token_ok` must carry `expires_in`: read `token_ok` (`:444-451`); if its body has no `expires_in`, build the response inline as `{"access_token":"tok-1","expires_in":60}`. Request → 200; `clock.advance(Duration::from_secs(61))`; request → 200; assert **two** `oauth_token.do` requests and the second data request has header `Authorization: Bearer tok-2`.
6. `servicenow_http_oauth_secret_is_form_urlencoded`: credential blob `{"client_id":"id","client_secret":"a&b=c d+e%"}` with `[token_ok("t"), ok_table()]` → the token request body contains `client_secret=a%26b%3Dc%20d%2Be%25`.
7. `urlencode_keeps_unreserved_and_escapes_the_rest`: `urlencode("AZaz09-_.~") == "AZaz09-_.~"`, `urlencode(" /?#") == "%20%2F%3F%23"`.

**Verify**: `cargo test -p daku-core servicenow_http` → 16 passed;
`cargo test -p daku-core urlencode` → 2 passed.

### Step 3: `load_environments` negatives (`crates/daku-core/src/config.rs` and `collector.rs` `mod tests`)

Helper in `config.rs` tests: `fn write_temp(name: &str, body: &str) -> PathBuf` writing to `std::env::temp_dir().join(format!("daku-config-{name}-{}.json", uuid::Uuid::new_v4()))` (`uuid` is a dev-dep of daku-core); remove the file at the end of each test.

1. `load_environments_invalid_json_error_names_the_path`: body `{not json` → `Err` whose `format!("{e:#}")` contains `"parsing "` and the file name.
2. `load_environments_missing_file_error_names_the_path`: nonexistent path → `Err` containing `"reading "`; and `error.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound))` (this is what `collector.rs::is_not_found` relies on).
3. `load_environments_rejects_unknown_auth_method`: one entry with `"auth_method":"saml"` → `Err`.
4. `load_environments_sorts_by_sort_order_and_keeps_duplicate_ids`: two entries with `sort_order` 2 then 1 → returned order 1, 2; two entries sharing `"id":"prod"` → `Ok` with `len() == 2` (**pins current behaviour**; note it in the report as a candidate for validation).
5. In `collector.rs` tests: `start_default_loop_returns_none_for_missing_and_empty_config`: `start_default_loop(&nonexistent_path, StateStore::daemon(temp db path), &DaemonSettings::default(), Arc::new(AtomicBool::new(false)))` → `None`; with a file containing `[]` → `None`. (No thread is spawned in either case — verify by reading `collector.rs:155-178`.)

**Verify**: `cargo test -p daku-core load_environments` → 8 passed;
`cargo test -p daku-core start_default_loop` → 1 passed.

### Step 4: Real tests for client persistence (`crates/daku-client/src/persistence.rs`)

Refactor (behaviour-preserving):

```rust
pub fn load_or_create_app_settings() -> io::Result<AppSettings> {
    load_or_create_app_settings_at(&default_app_settings_path(), &default_legacy_settings_paths())
}

pub fn load_or_create_app_settings_at(path: &Path, legacy_settings_paths: &[PathBuf]) -> io::Result<AppSettings> {
    let source = read_app_settings_source(path, legacy_settings_paths)?;
    … // the existing body of load_or_create_app_settings from `let loaded_from_primary` onward, unchanged, using `path`
}
```

Delete `desktop_settings_paths_are_build_specific`. Add tests (temp dir per test: `std::env::temp_dir().join(format!("daku-app-{}", uuid::Uuid::new_v4()))`, `uuid` is a regular dep):

1. `app_settings_source_prefers_primary_then_legacy`: primary present → `Some((bytes, true))`; only legacy present → `Some((bytes, false))`; neither → `None`.
2. `app_settings_are_created_with_a_minted_token`: no files → `load_or_create_app_settings_at` returns settings with non-empty `daemon_exposure.token`; the primary file now exists; on unix its mode is `0o600`; a second call returns the **same** token (no re-mint) and does not rewrite the file (compare `mtime` or file bytes).
3. `app_settings_empty_token_is_minted_and_rewritten`: write primary `{"daemon_exposure":{"token":""}}` → returned token non-empty and the file's `daemon_exposure.token` is now non-empty.
4. `app_settings_legacy_file_is_migrated_to_primary`: only legacy `{"daemon_exposure":{"token":"keep-me"}}` → returned token `"keep-me"` and the primary file now exists with that token.

**Verify**: `cargo test -p daku-client app_settings` → 4 passed; `grep -n desktop_settings_paths_are_build_specific crates/daku-client/src/persistence.rs` → no matches.

### Step 5: Vacuous updater test

5a. Only if `routing_user_driver_satisfies_sparkle_protocols` still exists in `src/updater.rs` (plan 021 not landed): replace the early `return` with a proper skip — add `#[ignore = "requires target/debug/Daku daku Debug.app with Sparkle; run with --ignored after bun run dev"]` on the test and change `if !library.exists() { return; }` to `assert!(library.exists(), "build the Debug.app first: bun run dev");`.

**Verify**: `cargo test -p daku updater` → the test shows as `ignored`; `cargo test -p daku updater -- --ignored` fails with the "build the Debug.app first" message unless the app was built (that is expected).

### Step 6: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- Listed exhaustively in Steps 1–5 (9 + 7 + 5 + 4 tests, +1 attribute). Patterns: `dashboard_state.rs:571`, `servicenow.rs:576`, `config.rs:111`, `persistence.rs` (client) — new pattern established here.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `cargo test -p daku dashboard_state` → ≥15 passed
- [ ] `cargo test -p daku-core servicenow_http` → 16 passed
- [ ] `cargo test -p daku-core urlencode` → 2 passed
- [ ] `cargo test -p daku-core load_environments` → 8 passed
- [ ] `cargo test -p daku-core start_default_loop` → 1 passed
- [ ] `cargo test -p daku-client app_settings` → 1 passed — only `missing_app_settings_are_written_with_a_token` matches this filter. For all three Step 4 tests use `cargo test -p daku-client persistence` → 3 passed (`missing_app_settings_are_written_with_a_token`, `an_empty_token_is_minted_and_rewritten`, `a_persisted_token_survives_legacy_keys_without_a_rewrite`); `load_or_create_app_settings_at` exists and `load_or_create_app_settings` is a one-line wrapper
- [ ] `grep -n 'if !library.exists() { return; }' src/updater.rs` → no matches (or the test no longer exists)
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 027 updated

## STOP conditions

- A new test fails and the failure looks like a real bug (not a wrong expectation) — pin current behaviour if it is benign (duplicate ids), otherwise report with the failing assertion and stop that step.
- `summarize_payload` output strings differ from the "Current state" formats (someone changed the UI copy) — update the expected strings only if the change is cosmetic; otherwise report.
- `load_or_create_app_settings` body no longer matches (plan 020 restructured it) — write the four persistence tests against whatever path-injected function 020 left; if there is none, STOP and report.
- Any test needs the network, the Keychain, or `~/.daku` — never; rewrite it or report.

## Maintenance notes

- `AdvancingClock` is the seam for any future time-based client behaviour (token TTL clamp, breaker); keep it in `servicenow.rs` tests.
- Reviewers: check that no test reads the real home directory (`dirs::home_dir()` must not appear in new tests) and that Step 4 did not change `write_json_atomically` or the rewrite conditions.
- Deferred: `is_not_found` handling for `start_default_loop` when the parse error wraps an `io::Error` (edge case, currently prints "not started"); duplicate-id validation (design decision — see plan 017's maintenance notes).
