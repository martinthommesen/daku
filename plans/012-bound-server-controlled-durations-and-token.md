# Plan 012: Cap Retry-After / token expiry from ServiceNow and refuse an empty daemon token

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/servicenow.rs crates/daku-daemon/src/main.rs`
> If either file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (for the `bun run check` gate; the code changes are independent)
- **Category**: security / bug
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/35

## Why this matters

All seven Signals for all Environments run on **one** collector thread (`crates/daku-core/src/collector.rs:60-84`). Three values that a remote server (or a captive portal / proxy in front of it) controls flow into that thread unbounded:

1. `Retry-After` on HTTP 429 is parsed as `u64` seconds (or an HTTP-date) and slept verbatim. A `Retry-After: 86400` parks every Signal for every Environment for a day; the UI just goes stale with no indication.
2. OAuth `expires_in` is added to `SystemTime::now()` with `+`, which **panics on overflow** (`SystemTime + Duration` is checked-and-panicking in std). A panic kills the collector thread; the daemon keeps running and reporting "connected".
3. The daemon accepts an **empty** `DAKU_DAEMON_TOKEN` (`DAKU_DAEMON_TOKEN=` exported-but-unset is a common shell/launchd mistake). Since the check is `expected.ct_eq(candidate)`, an empty expected token matches an empty client token — i.e. anyone. The desktop supervisor already refuses to spawn with an empty token (`crates/daku-client/src/process.rs:80-82`), but the documented manual launch (`crates/daku-daemon/README.md`) and `--allow-non-loopback` go straight through the server path.

All three fixes are a few lines each with clean unit tests.

## Current state

### `crates/daku-core/src/servicenow.rs`

```rust
// :13-14
const MAX_429_RETRIES: u8 = 2;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);
```

```rust
// :180-182 (inside ServiceNowClient::oauth_access)
        let expires_in = grant.expires_in.unwrap_or(1800);
        let valid_until = self.clock.now() + Duration::from_secs(expires_in);
```

```rust
// :193-205
    fn send(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let mut retries = 0;
        loop {
            let response = self.transport.execute(request)?;
            if response.status == 429 && retries < MAX_429_RETRIES {
                self.clock
                    .sleep(retry_after_delay(&response, self.clock.now()));
                retries += 1;
                continue;
            }
            return Ok(response);
        }
    }
```

```rust
// :281-294
fn retry_after_delay(response: &HttpResponse, now: SystemTime) -> Duration {
    let Some(value) = response.header("Retry-After") else {
        return DEFAULT_RETRY_AFTER;
    };
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Duration::from_secs(seconds);
    }
    http_date_delay(value, now).unwrap_or(DEFAULT_RETRY_AFTER)
}

fn http_date_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let parsed = httpdate::parse_http_date(value).ok()?;
    Some(parsed.duration_since(now).unwrap_or(Duration::ZERO))
}
```

`AccessGrant` (`:221-225`) has `expires_in: Option<u64>`.

Test scaffolding already in the file's `mod tests` (`:346-420`): `ScriptedTransport::new(vec![HttpResponse…])` returns scripted responses in order and records requests; `RecordingClock` has a fixed `now()` (`UNIX_EPOCH + 1_700_000_000 s`) and records `sleep()` durations in `clock.sleeps`; `basic_env()` builds a Basic-auth `EnvironmentConfig` with id `dev`; `ok_table()` returns a 200 JSON table response; `MemoryCredentialStore::default()` + `.insert("dev", r#"{"username":"reader","password":"secret"}"#)` seeds credentials. Existing tests to model after: `servicenow_http_retries_on_429_retry_after` (`:453-479`, asserts `clock.sleeps == [Duration::from_secs(1)]`) and `servicenow_http_retries_on_429_http_date` (`:482-513`, asserts `[Duration::from_secs(10)]`), `servicenow_http_oauth_cache_skips_second_token_fetch` (OAuth flow with a scripted token response — read it before writing the `expires_in` test).

### `crates/daku-daemon/src/main.rs`

```rust
// :10-19
fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse(std::env::args().skip(1))?;
    if arguments.probe_availability {
        return run_probe_availability();
    }
    let auth = std::env::var(DAEMON_TOKEN_ENV).context("DAKU_DAEMON_TOKEN is missing")?;
    // The bearer capability belongs only to this server process. Remove it
    // before any provider or workspace subprocess can inherit the daemon's
    // environment.
    unsafe { std::env::remove_var(DAEMON_TOKEN_ENV) };
```

`DAEMON_TOKEN_ENV` is `"DAKU_DAEMON_TOKEN"` (`crates/daku-protocol/src/protocol.rs:10`). Token comparison is `crates/daku-core/src/server.rs:476-478` (`subtle::ct_eq`) — do not touch it. Existing tests in `main.rs` (`:168-209`) are plain unit tests on `Arguments::parse` and `ensure_bind_allowed`; model the new one after `non_loopback_listener_requires_an_explicit_flag`.

Conventions: `anyhow::bail!`/`Context` for errors; constants `SCREAMING_SNAKE` at file top; imperative commit summaries.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Client tests | `cargo test -p daku-core servicenow_http` | all pass |
| Daemon tests | `cargo test -p daku-daemon` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/servicenow.rs` (retry cap, expiry cap, tests)
- `crates/daku-daemon/src/main.rs` (empty-token refusal + test)
- `crates/daku-daemon/README.md` (one sentence)
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-core/src/server.rs` `token_matches` / handshake — already constant-time; do not change.
- `crates/daku-client/src/process.rs` — the client-side empty-token guard already exists.
- Per-Environment circuit breaking, concurrency, or changing `MAX_429_RETRIES` — separate perf plan.
- Any change to what a 429 persists as (collectors already record the error string).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Cap Retry-After and OAuth expiry; refuse an empty daemon token.`

## Steps

### Step 1: Cap the 429 sleep

In `crates/daku-core/src/servicenow.rs` add next to the existing constants:

```rust
/// Upper bound on a single 429 back-off. Anything longer would stall the
/// shared collector thread for every Environment; the collector will retry
/// naturally on its next tick.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
```

Change `retry_after_delay` so every return path is clamped:

```rust
fn retry_after_delay(response: &HttpResponse, now: SystemTime) -> Duration {
    let Some(value) = response.header("Retry-After") else {
        return DEFAULT_RETRY_AFTER;
    };
    let delay = match value.trim().parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => http_date_delay(value, now).unwrap_or(DEFAULT_RETRY_AFTER),
    };
    delay.min(MAX_RETRY_AFTER)
}
```

Add tests in `mod tests` (model on `servicenow_http_retries_on_429_retry_after`):

- `servicenow_http_caps_huge_retry_after_seconds`: scripted `[429 with "Retry-After: 86400", ok_table()]` → status 200 and `clock.sleeps == [Duration::from_secs(30)]`.
- `servicenow_http_caps_far_future_retry_after_date`: `Retry-After: "Tue, 14 Nov 2033 22:13:30 GMT"` (≈10 years after `RecordingClock::now()`) → sleeps `[Duration::from_secs(30)]`.
- Leave the two existing 429 tests as they are — they must still pass (1 s and 10 s are under the cap).

**Verify**: `cargo test -p daku-core servicenow_http` → all pass, including the 2 new tests.

### Step 2: Make token expiry arithmetic non-panicking and bounded

Add a constant:

```rust
/// Longest we will trust an OAuth grant regardless of what the server says.
const MAX_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;
```

Replace `:180-182` with:

```rust
        let expires_in = grant.expires_in.unwrap_or(1800).min(MAX_TOKEN_TTL_SECS);
        let valid_until = self
            .clock
            .now()
            .checked_add(Duration::from_secs(expires_in))
            .unwrap_or_else(|| self.clock.now());
```

(`checked_add` cannot fail once `expires_in ≤ 24 h`, but keeps the code panic-free by construction; falling back to `now()` simply means "not cached".)

Add a test `servicenow_http_oauth_huge_expires_in_does_not_panic`: OAuth-mode environment (copy the setup from `servicenow_http_oauth_cache_skips_second_token_fetch`), token response body `{"access_token":"t","expires_in":18446744073709551615}` followed by `ok_table()`; assert the request returns 200 (i.e. no panic). Optionally also assert a second request with another `ok_table()` scripted does **not** re-fetch the token (cache used, TTL clamped to 24 h) — check how the existing cache test counts token requests (`transport.requests()` filtered on `oauth_token.do`).

**Verify**: `cargo test -p daku-core servicenow_http` → all pass, including the new test.

### Step 3: Refuse an empty daemon token

In `crates/daku-daemon/src/main.rs`, add a helper next to `ensure_bind_allowed`:

```rust
fn require_token(value: Result<String, std::env::VarError>) -> anyhow::Result<String> {
    let token = value.context("DAKU_DAEMON_TOKEN is missing")?;
    if token.trim().is_empty() {
        bail!("DAKU_DAEMON_TOKEN is empty; refusing to start an unauthenticated daemon");
    }
    Ok(token)
}
```

and change `main` line 15 to `let auth = require_token(std::env::var(DAEMON_TOKEN_ENV))?;`.

Add a test in the existing `mod tests`:

```rust
    #[test]
    fn empty_daemon_token_is_refused() {
        assert!(require_token(Ok(String::new())).is_err());
        assert!(require_token(Ok("   ".into())).is_err());
        assert!(require_token(Err(std::env::VarError::NotPresent)).is_err());
        assert_eq!(require_token(Ok("secret".into())).unwrap(), "secret");
    }
```

In `crates/daku-daemon/README.md`, after the sentence about `DAKU_DAEMON_TOKEN`, add: `An empty token is refused at startup.`

**Verify**: `cargo test -p daku-daemon` → all pass (5 tests). `cargo run -p daku-daemon 2>&1 | head -1` with `DAKU_DAEMON_TOKEN=` set to empty prints the "is empty" error and exits non-zero (run as `DAKU_DAEMON_TOKEN= cargo run -p daku-daemon; echo $?` → non-zero).

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `crates/daku-core/src/servicenow.rs` tests: `servicenow_http_caps_huge_retry_after_seconds`, `servicenow_http_caps_far_future_retry_after_date`, `servicenow_http_oauth_huge_expires_in_does_not_panic`. Pattern: `servicenow_http_retries_on_429_retry_after` (`:453`).
- `crates/daku-daemon/src/main.rs` tests: `empty_daemon_token_is_refused`. Pattern: `non_loopback_listener_requires_an_explicit_flag` (`:173`).
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'MAX_RETRY_AFTER\|MAX_TOKEN_TTL_SECS' crates/daku-core/src/servicenow.rs` → both constants defined and used
- [ ] `grep -n 'clock.now() +' crates/daku-core/src/servicenow.rs` → no matches
- [ ] `grep -n 'require_token' crates/daku-daemon/src/main.rs` → helper + call site + test
- [ ] `cargo test -p daku-core servicenow_http` and `cargo test -p daku-daemon` pass with the 4 new tests
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 012 updated

## STOP conditions

- The excerpts above do not match the live code (`send`, `retry_after_delay`, `oauth_access`, or `main` were restructured).
- `RecordingClock`/`ScriptedTransport` no longer exist in `servicenow.rs` tests (write no new scaffolding — report instead).
- Any existing `servicenow_http_*` test fails after Step 1 or 2 for a reason other than an intentional cap.
- `cargo test -p daku-daemon` cannot construct `std::env::VarError` (API change) — report.

## Maintenance notes

- If a per-Environment rate-limit breaker is added later (perf backlog), keep `MAX_RETRY_AFTER` as the hard ceiling and make the breaker skip the request entirely rather than raising the cap.
- Reviewers: confirm no change to `token_matches` and that the empty-token check happens **before** `remove_var`.
- Deferred: `catch_unwind`/restart around the collector thread (a panic still silently kills polling); this plan only removes the known panic path.
