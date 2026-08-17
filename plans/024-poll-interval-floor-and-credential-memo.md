# Plan 024: Floor `poll_interval_secs` at 30 s and stop reading the Keychain on every OAuth request

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/collector.rs crates/daku-core/src/servicenow.rs README.md`
> Plans 011/013/014/020/022 legitimately touch `collector.rs`; re-read the
> `poll_interval_secs` function (or its typed replacement from plan 020)
> before Step 1. If `servicenow.rs` `authorize`/`oauth_access` no longer
> match the excerpts, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate). Soft: plans/020-settings-cleanup-typed-poll-interval.md (changes where the interval is read — see Step 1 for both shapes).
- **Category**: perf
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/53

## Why this matters

1. **No floor on the poll interval.** Any value `> 0` is accepted (`collector.rs` `poll_interval_secs`). Sample volume and the full 24 h re-broadcast scale as 24 h ÷ interval: at the default 120 s that is ~720 points × 2 sampled Signals × N Environments per tick (tens of KB — fine); at `5` it is 17 280 points per series, re-serialised every 5 s and painted as 17k-segment sparkline paths, plus ~10 N ServiceNow requests every 5 s against instances the spec says to poll "politely" (§6). A typo should not DoS the Operator's own instance. Clamp to ≥ 30 s.
2. **Keychain read per HTTP request.** `ServiceNowClient::authorize` calls `credentials.get(&environment.id)` (a `securityd` IPC via `security-framework`) **before** checking the OAuth token cache — so ~10 Keychain reads per Environment per tick even when the bearer token is cached for 30 minutes. Each read is a Keychain ACL touch by the daemon binary; with unsigned/ad-hoc builds (a new code identity per rebuild) that nudges Operators toward permissive ACLs. Check the token cache first; the Keychain is then read once per token lifetime for OAuth Environments (Basic auth — PDI stand-ins only — still reads per request; not worth a cache).

Sparkline down-sampling was considered and dropped: with the 30 s floor a series is ≤ 2 880 points; GPUI paints that fine.

## Current state

### `crates/daku-core/src/collector.rs` (HEAD shape)

```rust
// :27-37
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;
pub const POLL_INTERVAL_SECS_KEY: &str = "poll_interval_secs";

pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    settings
        .extra
        .get(POLL_INTERVAL_SECS_KEY)
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
}
```

Used by `start_default_loop` (`:180-187`: `Duration::from_secs(poll_interval_secs(settings))`). Plan 011 adds tests `poll_interval_secs_reads_top_level_json_key` and `poll_interval_secs_falls_back_to_default_for_zero_or_non_number`. **If plan 020 has landed**, `DaemonSettings` has a typed `poll_interval_secs: u64` field and this function may have become a one-liner or moved — the clamp goes wherever the settings value is turned into the loop `Duration`.

`README.md` (after plan 011): `Optional poll cadence: put a top-level "poll_interval_secs" in ~/.daku/settings.json, e.g. {"poll_interval_secs": 60} (default **120**; …)`.

### `crates/daku-core/src/servicenow.rs`

```rust
// :123-146
    fn authorize(
        &self,
        environment: &EnvironmentConfig,
        credentials: &dyn CredentialStore,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let blob = credentials
            .get(&environment.id)?
            .ok_or_else(|| anyhow!("no credential for environment {}", environment.id))?;
        match environment.auth_method {
            AuthMethod::Basic => {
                let parsed: BasicCred =
                    serde_json::from_str(&blob).context("basic credential JSON")?;
                Ok(vec![(
                    "Authorization".into(),
                    basic_authorization(&parsed.username, &parsed.password),
                )])
            }
            AuthMethod::OauthClientCredentials => {
                let access = self.oauth_access(environment, &blob)?;
                Ok(vec![("Authorization".into(), format!("Bearer {access}"))])
            }
        }
    }

// :148-156 (start of oauth_access — the cache check)
    fn oauth_access(&self, environment: &EnvironmentConfig, blob: &str) -> anyhow::Result<String> {
        {
            let cache = self.tokens.lock().expect("token cache");
            if let Some(cached) = cache.get(&environment.id) {
                if self.clock.now() < cached.valid_until {
                    return Ok(cached.access_token.clone());
                }
            }
        }
        let parsed: OauthCred = serde_json::from_str(blob).context("oauth credential JSON")?;
        …
```

`tokens: Mutex<HashMap<String, CachedToken { access_token, valid_until }>>` (`:71-80`). The 401 path (`:109-117`) removes the cached token and retries — after this change that retry naturally re-reads the Keychain because the cache is empty.

Test scaffolding (`:346-420` + helpers `oauth_env()`, `token_ok("tok-1")`, `ok_table()`, `SharedTransport`, `ScriptedTransport::requests()`), and `servicenow_http_oauth_cache_skips_second_token_fetch` (`:576-615`) which issues two OAuth requests and asserts one token fetch — model the new test on it. `MemoryCredentialStore` (`config.rs:57-79`) is the in-memory `CredentialStore`.

Conventions: constants at file top; `anyhow`; tests in `mod tests` at the bottom.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Interval tests | `cargo test -p daku-core poll_interval` | all pass |
| Client tests | `cargo test -p daku-core servicenow_http` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/collector.rs` (or wherever plan 020 moved the interval read) — clamp + test
- `crates/daku-core/src/servicenow.rs` — cache-first `authorize` + test
- `README.md` — mention the floor (one clause)
- `plans/README.md` (status row)

**Out of scope**:
- `src/app.rs` sparkline (no down-sampling).
- Caching Basic-auth headers or a per-tick Keychain cache in `KeychainCredentialStore`.
- Changing `DEFAULT_POLL_INTERVAL_SECS`, `MAX_429_RETRIES`, or the OAuth TTL cap (plan 012).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Floor poll_interval_secs at 30 s; check the OAuth token cache before the Keychain.`

## Steps

### Step 1: Floor the interval

Add `pub const MIN_POLL_INTERVAL_SECS: u64 = 30;` next to `DEFAULT_POLL_INTERVAL_SECS`.

- **HEAD shape** (plan 020 not landed): change the function to

```rust
pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    settings
        .extra
        .get(POLL_INTERVAL_SECS_KEY)
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
        .max(MIN_POLL_INTERVAL_SECS)
}
```

- **Plan-020 shape** (typed field): apply `.max(MIN_POLL_INTERVAL_SECS)` at the single place the field becomes the loop `Duration` (grep `poll_interval_secs` in `collector.rs`), keeping `0`/absent → default semantics that 020 defines.

Add a test next to the plan-011 tests: `poll_interval_secs_is_floored_at_30` — settings with `poll_interval_secs = 5` → `30`; `= 30` → `30`; `= 31` → `31`.

README: extend the poll-cadence sentence with `; values below 30 are raised to 30`.

**Verify**: `cargo test -p daku-core poll_interval` → all pass (3 tests incl. the new one). `grep -n 'raised to 30' README.md` → 1 match.

### Step 2: Cache-first OAuth authorization

Refactor `authorize` so the Keychain is only consulted when needed:

```rust
    fn authorize(
        &self,
        environment: &EnvironmentConfig,
        credentials: &dyn CredentialStore,
    ) -> anyhow::Result<Vec<(String, String)>> {
        if environment.auth_method == AuthMethod::OauthClientCredentials {
            if let Some(access) = self.cached_access_token(&environment.id) {
                return Ok(vec![("Authorization".into(), format!("Bearer {access}"))]);
            }
        }
        let blob = credentials
            .get(&environment.id)?
            .ok_or_else(|| anyhow!("no credential for environment {}", environment.id))?;
        match environment.auth_method {
            AuthMethod::Basic => { /* unchanged */ }
            AuthMethod::OauthClientCredentials => {
                let access = self.oauth_access(environment, &blob)?;
                Ok(vec![("Authorization".into(), format!("Bearer {access}"))])
            }
        }
    }

    fn cached_access_token(&self, environment_id: &str) -> Option<String> {
        let cache = self.tokens.lock().expect("token cache");
        cache
            .get(environment_id)
            .filter(|cached| self.clock.now() < cached.valid_until)
            .map(|cached| cached.access_token.clone())
    }
```

Replace the inline cache check at the top of `oauth_access` with `if let Some(access) = self.cached_access_token(&environment.id) { return Ok(access); }` (one source of truth; `oauth_access` is only reached on a miss but stays correct if called directly).

Add a test `servicenow_http_oauth_reads_keychain_once_while_token_is_cached`: a `CountingCredentialStore` (wraps `MemoryCredentialStore`, `AtomicUsize` `gets`), scripted `[token_ok("tok-1"), ok_table(), ok_table()]`, two requests against `oauth_env()`; assert both return 200, one `oauth_token.do` request (as the existing cache test does), and `gets == 1`. Also assert the existing `servicenow_http_oauth_refreshes_once_on_401` still passes (its refresh path must read the Keychain again after the cache entry is removed — it will, because the cache is empty on retry).

**Verify**: `cargo test -p daku-core servicenow_http` → all pass, including the new test.

### Step 3: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `collector.rs`: `poll_interval_secs_is_floored_at_30`.
- `servicenow.rs`: `servicenow_http_oauth_reads_keychain_once_while_token_is_cached` (pattern: `servicenow_http_oauth_cache_skips_second_token_fetch`, `:576`).
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'MIN_POLL_INTERVAL_SECS' crates/daku-core/src/collector.rs` → const + use + test
- [ ] `grep -n 'fn cached_access_token' crates/daku-core/src/servicenow.rs` → 1 match; `authorize` checks it before `credentials.get`
- [ ] `cargo test -p daku-core poll_interval` and `cargo test -p daku-core servicenow_http` pass with the 2 new tests
- [ ] `grep -n 'raised to 30' README.md` → 1 match
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 024 updated

## STOP conditions

- Neither the HEAD-shape `poll_interval_secs` nor a plan-020 typed field can be found in `collector.rs` (interval derivation moved elsewhere) — report where.
- `authorize`/`oauth_access` no longer match the excerpts.
- `servicenow_http_oauth_refreshes_once_on_401` fails after Step 2 (the 401 refresh path must still re-read credentials).

## Maintenance notes

- If Basic auth ever becomes a supported production path (ADR-0004 says PDI-only), add a per-tick credential memo then, not now.
- The 30 s floor is a v1 hard-coded default like the health thresholds; if an Operator needs faster polling for a demo, that is a settings-schema decision, not a code tweak.
- Reviewers: check that a cache hit path never touches `credentials` and that the 401 refresh path still does.
