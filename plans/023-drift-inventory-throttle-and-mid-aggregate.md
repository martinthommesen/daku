# Plan 023: Refresh drift plugin/store-app inventories every 30 minutes, not every tick

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/drift.rs`
> If the file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate)
- **Category**: perf
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/52

## Why this matters

Every ~120 s tick the drift Signal downloads two full Table-API pages (`sys_plugins` and `sys_store_app`, up to 1000 rows each, ~100–200 KB JSON apiece on a real instance) **for every Environment** — the source and each clone — to diff inventories that change on the order of days (plugin activations, store-app installs, clones). On a three-Environment setup that is 6 heavy requests per tick, ≈4 300 per day, for data that is identical almost every time. It is the largest single contributor to tick latency after unreachable hosts, and a needless load on the instances.

Fix: keep the last successfully fetched inventory per Environment in memory inside `DriftCollector` and reuse it for `INVENTORY_REFRESH_SECS` (30 min). Builds are still compared every tick (they come free from the availability snapshot via `reuse_availability_build`), so build drift after an upgrade is still caught within one tick; plugin drift is caught within 30 minutes. A failed fetch is not cached, so an Environment that was down retries next tick.

### PERF-06 (MID agents via Aggregate API) — considered, **not** done here

The audit suggested replacing the `ecc_agent` list (`sysparm_limit=10000`, `crates/daku-core/src/mid_ecc.rs:18`) with two aggregate counts. Verdict after reading `mid_ecc.rs`: not worth it now. The `10000` is a ceiling, not a payload — real fleets are tens to low hundreds of MID rows × 4 short fields (a few KB), and the single list request already yields both `agents_total` and `agents_unhealthy`; two aggregate calls would be **more** requests for a smaller body, and would throw away `host_name`/`status`, which the direction backlog (DIR-01: show *which* MID is down) wants next. Revisit only if an Operator has >1 000 MIDs (then paginate or query unhealthy-only + total count).

## Current state

### `crates/daku-core/src/drift.rs`

```rust
// :3-6
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// :17-21
pub const DRIFT_SIGNAL_ID: &str = "drift";
pub const PLUGIN_PAGE_LIMIT: usize = 1000;
pub const SYS_PLUGINS_PATH: &str = "/api/now/table/sys_plugins?sysparm_fields=id,version,active&sysparm_limit=1000";
pub const SYS_STORE_APP_PATH: &str = "/api/now/table/sys_store_app?sysparm_fields=scope,id,version,latest_version,active&sysparm_limit=1000";

// :31-59
struct EnvInventory {
    build: Option<String>,
    plugins: Vec<PluginRecord>,
    truncated: bool,
}

pub struct DriftCollector {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
    poll_interval: Duration,
}

impl DriftCollector {
    pub fn new(environments, credentials, client: impl Into<Arc<ServiceNowClient>>, store, poll_interval: Duration) -> Self { … }
}
```

`collect` (`:63-125`): finds the `clone_source` Environment, computes `max_age_secs = 2 × poll_interval`, calls `fetch_env_inventory(&self.client, source, self.credentials.as_ref(), &connection, observed_at, max_age_secs)`, then for every other Environment `collect_other(&connection, &self.client, environment, self.credentials.as_ref(), observed_at, max_age_secs, source_inventory.as_ref())`, which calls `fetch_env_inventory` again for that Environment.

```rust
// :164-186
fn fetch_env_inventory(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    connection: &Connection,
    observed_at: i64,
    max_age_secs: i64,
) -> anyhow::Result<EnvInventory> {
    let (plugins, plugins_truncated) =
        fetch_plugin_page(client, environment, credentials, SYS_PLUGINS_PATH)?;
    let (store_apps, store_truncated) =
        fetch_plugin_page(client, environment, credentials, SYS_STORE_APP_PATH)?;
    let mut combined = plugins;
    combined.extend(store_apps);
    Ok(EnvInventory {
        build: fetch_build(client, environment, credentials, connection, observed_at, max_age_secs)?,
        plugins: combined,
        truncated: plugins_truncated || store_truncated,
    })
}
```

`fetch_build` (`:209-234`) reuses the availability snapshot's `build` when fresh, else GETs `glide.war` — unchanged by this plan. `PluginRecord` derives `Clone` (`:383-388`). `fetch_env_inventory` is a free function taking `client`/`credentials` — it needs the collector's cache, so it becomes a method (or takes the cache as a parameter).

Tests (`:420-793`): `DriftTransport { source_plugins, other_plugins, store_apps, build }` answers by URL substring (`acme-prod` → source), asserting `sysparm_limit=1000` on both plugin URLs; `env(id, host, clone_source)`; `collect_pair(source_plugins, other_plugins) -> (PathBuf, StateStore)` runs **one** `collect()`. `drift_signal_reuses_fresh_availability_build` (`:737+`) shows how to pre-seed availability snapshots. Fixtures: `crates/daku-core/tests/fixtures/drift/{plugins_a,plugins_a_v2,store_apps_empty}.json`.

Conventions: `Mutex` from `std::sync` in this crate's daemon-side code (`servicenow.rs` uses `std::sync::Mutex`); no new dependencies.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Drift tests | `cargo test -p daku-core drift_signal` | all pass (incl. new) |
| Diff tests | `cargo test -p daku-core diff_plugin_inventory` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/drift.rs`
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-core/src/mid_ecc.rs` (see verdict above).
- `fetch_build` / availability reuse, `PLUGIN_PAGE_LIMIT`, pagination of >1000 rows.
- Persisting *which* plugins differ (direction backlog DIR-06 / plan 043).
- `collector.rs` (plan 022 makes drift a "shared" collector; nothing here conflicts).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Cache drift plugin inventories for 30 minutes instead of refetching every tick.`

## Steps

### Step 1: In-memory inventory cache on `DriftCollector`

Add near the constants:

```rust
/// Plugin/store-app inventories change on the order of days; refetch this
/// often. Builds are still compared every tick via the availability snapshot.
pub const INVENTORY_REFRESH_SECS: i64 = 30 * 60;
```

Add a cached-page type and a field:

```rust
#[derive(Clone)]
struct CachedInventory {
    fetched_at: i64,
    plugins: Vec<PluginRecord>,
    truncated: bool,
}

pub struct DriftCollector {
    … existing fields …
    /// Last successful plugin/store-app fetch per Environment id.
    inventories: std::sync::Mutex<HashMap<String, CachedInventory>>,
}
```

Initialise `inventories: std::sync::Mutex::new(HashMap::new())` in `new`.

### Step 2: Route inventory fetches through the cache

Turn `fetch_env_inventory` into a method that consults the cache first, and only hits the network on a miss/stale entry:

```rust
impl DriftCollector {
    fn env_inventory(
        &self,
        environment: &EnvironmentConfig,
        connection: &Connection,
        observed_at: i64,
        max_age_secs: i64,
    ) -> anyhow::Result<EnvInventory> {
        let cached = self
            .inventories
            .lock()
            .expect("drift inventory cache")
            .get(&environment.id)
            .filter(|entry| observed_at.saturating_sub(entry.fetched_at) <= INVENTORY_REFRESH_SECS)
            .cloned();
        let (plugins, truncated) = match cached {
            Some(entry) => (entry.plugins, entry.truncated),
            None => {
                let (plugins, plugins_truncated) =
                    fetch_plugin_page(&self.client, environment, self.credentials.as_ref(), SYS_PLUGINS_PATH)?;
                let (store_apps, store_truncated) =
                    fetch_plugin_page(&self.client, environment, self.credentials.as_ref(), SYS_STORE_APP_PATH)?;
                let mut combined = plugins;
                combined.extend(store_apps);
                let truncated = plugins_truncated || store_truncated;
                self.inventories.lock().expect("drift inventory cache").insert(
                    environment.id.clone(),
                    CachedInventory { fetched_at: observed_at, plugins: combined.clone(), truncated },
                );
                (combined, truncated)
            }
        };
        Ok(EnvInventory {
            build: fetch_build(&self.client, environment, self.credentials.as_ref(), connection, observed_at, max_age_secs)?,
            plugins,
            truncated,
        })
    }
}
```

Delete the free `fetch_env_inventory`. Update the two call sites: in `collect` (source) call `self.env_inventory(source, &connection, observed_at, max_age_secs)`; `collect_other` currently takes `client` + `credentials` and calls `fetch_env_inventory` — change it to take `&self`-derived data by making it a method (`fn collect_other(&self, connection, environment, observed_at, max_age_secs, source: Option<&EnvInventory>)`) that calls `self.env_inventory(...)`. Keep the persisted payload identical (`persist_drift_compare` unchanged), so a stale cached inventory still reports `truncated` as before.

**Verify**: `cargo test -p daku-core drift_signal` → all existing tests pass (each runs a single `collect`, so cache is cold: same requests as before).

### Step 3: Tests

Add a counting variant of the transport in `mod tests` (wrap `DriftTransport` or add an `Arc<AtomicUsize>` field counting plugin-page requests — the URL contains `/api/now/table/sys_plugins` or `sys_store_app`), then:

- `drift_signal_reuses_inventory_within_refresh_window`: build a `DriftCollector` (prod source + test) with the counting transport; call `collect()` twice; assert plugin-page requests == 4 (2 pages × 2 Environments, first tick only) — the second tick reuses the cache; both drift snapshots still exist with `mismatches == 0`.
- `drift_signal_refetches_inventory_after_refresh_window`: same, but between the two ticks the cache must be considered stale. `observed_at` comes from `SystemTime::now()` inside `collect`, so make the window observable: after the first `collect()`, mutate the cache directly from the test (`collector.inventories.lock().unwrap().values_mut().for_each(|e| e.fetched_at -= INVENTORY_REFRESH_SECS + 1)`) — the test module lives in the file and can touch the private field; then `collect()` again and assert plugin-page requests == 8.
- `drift_signal_failed_inventory_is_not_cached`: transport whose plugin page returns HTTP 500 on the first tick and 200 on the second (a `Mutex<u32>` call counter); after two ticks the "test" Environment's snapshot is `healthy`/`degraded` (not `down`) and plugin-page requests == 4 + 2 (the retry).

Model on `collect_pair` (`:507-533`) and `DriftTransport` (`:451-495`).

**Verify**: `cargo test -p daku-core drift_signal` → all pass, including the 3 new tests.

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- New: the three tests in Step 3.
- Existing `drift_signal_*` and `diff_plugin_inventory_*` unchanged and green.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'INVENTORY_REFRESH_SECS\|struct CachedInventory\|inventories:' crates/daku-core/src/drift.rs` → all present
- [ ] `grep -n 'fn fetch_env_inventory' crates/daku-core/src/drift.rs` → no match (replaced by the method)
- [ ] `cargo test -p daku-core drift_signal` passes with 3 new tests
- [ ] `bun run check` exits 0
- [ ] `git status` shows only `crates/daku-core/src/drift.rs` and `plans/README.md` modified
- [ ] `plans/README.md` status row for 023 updated

## STOP conditions

- `DriftCollector`/`fetch_env_inventory`/`collect_other` no longer match the excerpts (e.g. plan 031 consolidated collectors) — the cache then belongs to whatever struct owns drift state; report.
- Any existing `drift_signal_*` test fails after Step 2 (cold-cache behaviour must be identical).
- The plan requires touching `mid_ecc.rs` or `collector.rs` — it must not.

## Maintenance notes

- If plan 043 (persist mismatch list) lands, it diffs the same cached inventories — no extra fetch.
- `INVENTORY_REFRESH_SECS` is a constant on purpose (hard-coded v1 defaults per spec §5); make it a setting only if an Operator asks.
- Reviewers: check the cache is keyed by Environment id and never persists across daemon restarts (in-memory only — a restart refetches, which is fine).
