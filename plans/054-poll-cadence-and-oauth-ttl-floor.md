# Plan 054: The poll interval means the poll interval, and an OAuth grant is never born expired

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/collector.rs crates/daku-core/src/servicenow.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: perf + bug
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Two one-line arithmetic bugs, both about a bound that is only enforced on one
side.

1. **The sleep does not account for the tick.** `CollectorLoop::tick` measures
   `elapsed` and uses it *only* to print an overrun warning; `run` then sleeps
   the full `interval` regardless. The effective period is `interval + tick
   duration`, not `interval`. Negligible when ticks are fast — and it compounds
   exactly when it should not: the warning tells the Operator the tick overran,
   and then the loop adds a full interval on top instead of catching up.
   `tick` also joins every per-Environment group before the shared collectors
   run, so a single reachable-but-slow Environment (jobs 2 calls + syslog 1 +
   MID/ECC 3 + outbound 1, each against `ureq`'s 30 s global timeout) can push a
   tick to minutes — after which *every* Environment waits another full 120 s.
2. **`expires_in` is clamped above but not below.** `MAX_TOKEN_TTL_SECS` caps
   the value (plan 012); nothing floors it, and nothing subtracts a skew margin.
   A server (or a proxy) reporting a near-zero `expires_in` writes a cache entry
   that is **already dead when it is written**, because `cached_access_token`
   filters on `now < valid_until`. Every subsequent request then re-does the
   OAuth POST *and* — since `authorize` is cache-first (plan 024) — re-reads the
   Keychain. Each of those extra token calls carries its own 429 budget
   (`MAX_429_RETRIES` × `MAX_RETRY_AFTER` = 2 × 30 s), so a rate-limited
   instance can add a minute of sleep per Signal per tick.

Plan 012 landed its stated cap correctly and completely. This is the *other end*
of the same server-controlled value, which 012 never scoped.

## Current state

**`crates/daku-core/src/collector.rs:238-269`** — `elapsed` is measured and only
printed:

```rust
    pub fn tick(&self) -> anyhow::Result<()> {
        let started = Instant::now();
        let mut errors: Vec<anyhow::Error> = std::thread::scope(|scope| {
            ...
        });
        if let Err(error) = run_sequential(&self.shared) {
            errors.push(error);
        }
        let elapsed = started.elapsed();
        if elapsed > self.interval {
            eprintln!(
                "daku collector tick took {:.0}s (poll interval {:.0}s)",
                elapsed.as_secs_f64(),
                self.interval.as_secs_f64()
            );
        }
        match errors.into_iter().next() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
```

**`crates/daku-core/src/collector.rs:271-285`** — `run` sleeps the full interval:

```rust
    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock, after: &dyn Fn()) {
        // Publish last-known state from SQLite so a fresh subscriber is not blank
        // until the first tick completes.
        after();
        while !shutdown.load(Ordering::Acquire) {
            if let Err(error) = self.tick() {
                eprintln!("daku collector tick failed: {error}");
            }
            after();
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            clock.sleep(self.interval);
        }
    }
```

**`crates/daku-core/src/servicenow.rs:13-20`** — the existing bounds:

```rust
const MAX_429_RETRIES: u8 = 2;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);
/// Upper bound on a single 429 back-off. Anything longer would stall the
/// shared collector thread for every Environment; the collector will retry
/// naturally on its next tick.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// Longest we will trust an OAuth grant regardless of what the server says.
const MAX_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;
```

**`crates/daku-core/src/servicenow.rs:193-200`** — the one-sided clamp:

```rust
        let expires_in = grant.expires_in.unwrap_or(1800).min(MAX_TOKEN_TTL_SECS);
        let valid_until = self
            .clock
            .now()
            .checked_add(Duration::from_secs(expires_in))
            .unwrap_or_else(|| self.clock.now());
```

**`crates/daku-core/src/servicenow.rs:157-164`** — the filter that makes a
zero-TTL entry useless:

```rust
    fn cached_access_token(&self, environment_id: &str) -> Option<String> {
        let cache = self.tokens.lock().expect("token cache");
        cache
            .get(environment_id)
            .filter(|cached| self.clock.now() < cached.valid_until)
            .map(|cached| cached.access_token.clone())
    }
```

**`crates/daku-core/src/collector.rs:29-37`** — the existing floor idiom to
match:

```rust
/// Fastest cadence the daemon will poll at, however low the setting is.
pub const MIN_POLL_INTERVAL_SECS: u64 = 30;

pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    match settings.poll_interval_secs {
        0 => DEFAULT_POLL_INTERVAL_SECS,
        secs => secs.max(MIN_POLL_INTERVAL_SECS),
    }
}
```

### Constraints you must honor

- The `Clock` trait (`crates/daku-core/src/servicenow.rs`) is how tests control
  time; `collector.rs` tests use a `StopOnSleep`-style clock. **Use the trait,
  never `SystemTime::now()` directly, in anything you add.**
- `plans/README.md` › Ownership locks: the **poll loop** belongs to
  `build_default_loop`; later plans register collectors. Do not restructure it.
- `plans/README.md` records "re-reading `poll_interval_secs` every tick" as
  considered and rejected — do not add that.
- Bounds get a doc comment saying *why*, like `MAX_RETRY_AFTER` above. Match it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Collector tests | `cargo test -p daku-core collector` | all pass |
| HTTP tests | `cargo test -p daku-core servicenow_http` | all pass |

## Scope

**In scope**:
- `crates/daku-core/src/collector.rs`
- `crates/daku-core/src/servicenow.rs`

**Out of scope** (do NOT touch):
- `MIN_POLL_INTERVAL_SECS` / `DEFAULT_POLL_INTERVAL_SECS` and their README
  documentation — plan 024 set them and `README.md` documents them.
- `MAX_RETRY_AFTER`, `MAX_429_RETRIES`, `MAX_TOKEN_TTL_SECS` — plan 012's caps
  are correct; you are adding a floor, not changing a cap.
- The 401-triggered one-shot refresh (`servicenow.rs:112-124`) — it works and
  covers a token that expires earlier than advertised.
- Per-Environment publishing / partial ticks — plan 022 deferred that
  deliberately.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative. Two independent fixes — two commits preferred, e.g.
  `Sleep the remainder of the poll interval, not a full one on top of the tick (#79).`

## Steps

### Step 1: Sleep the remainder of the interval

Change `tick` to return the elapsed duration alongside its result, or record it
on `self`; the simplest shape that keeps the error contract is:

```rust
    pub fn tick(&self) -> anyhow::Result<()> {
        ...
    }

    /// Runs one tick and reports how long it took, so the caller can sleep the
    /// remainder of the interval instead of a full interval on top of it.
    fn tick_timed(&self) -> (anyhow::Result<()>, Duration) { ... }
```

Keep `pub fn tick` as-is for the existing tests and callers; have it delegate.
Then in `run`:

```rust
            clock.sleep(self.interval.saturating_sub(elapsed));
```

`saturating_sub` means an overrunning tick sleeps zero and ticks again
immediately, which is the intended catch-up.

**Verify**: `cargo test -p daku-core collector` → all pass.

### Step 2: Floor and de-skew the OAuth grant

In `crates/daku-core/src/servicenow.rs`, add next to `MAX_TOKEN_TTL_SECS`:

```rust
/// Shortest OAuth grant we will act on. A server reporting a near-zero
/// `expires_in` would otherwise write a cache entry that is already expired,
/// turning every request into a fresh token POST plus a Keychain read.
const MIN_TOKEN_TTL_SECS: u64 = 60;
/// Subtracted from the advertised lifetime so a token that is seconds from
/// expiry is refreshed rather than sent and rejected.
const TOKEN_TTL_SKEW_SECS: u64 = 30;
```

and change the computation to clamp into the band, then subtract the skew
without underflowing below the floor:

```rust
        let expires_in = grant
            .expires_in
            .unwrap_or(1800)
            .clamp(MIN_TOKEN_TTL_SECS, MAX_TOKEN_TTL_SECS)
            .saturating_sub(TOKEN_TTL_SKEW_SECS)
            .max(MIN_TOKEN_TTL_SECS / 2);
```

Read the surrounding code and choose the exact expression that keeps
`valid_until` strictly in the future for every input; the property that matters
is stated in the test plan, not the specific arithmetic.

**Verify**: `cargo test -p daku-core servicenow_http` → all pass.

## Test plan

New tests in `crates/daku-core/src/collector.rs` `mod tests`, modelled on the
existing clock-driven loop tests (they use a test `Clock` that records sleeps
and stops the loop):

1. `run_sleeps_the_remainder_of_the_interval` — a clock whose `sleep` records
   its argument, and collectors that advance the clock by a known amount;
   assert the recorded sleep is `interval - tick_elapsed`, not `interval`.
2. `run_does_not_sleep_after_an_overrunning_tick` — tick longer than the
   interval; assert the recorded sleep is `Duration::ZERO`.

New tests in `crates/daku-core/src/servicenow.rs` `mod tests`, alongside the
existing `servicenow_http_oauth_huge_expires_in_does_not_panic`:

3. `servicenow_http_oauth_tiny_expires_in_is_floored` — script a token response
   with `"expires_in": 0`, make one request, then a second; assert the token
   endpoint was hit **once**, i.e. the cache was usable. Count transport calls
   the way the existing 429 tests do.
4. `servicenow_http_oauth_expiry_keeps_a_skew_margin` — a token with
   `expires_in` just above the floor is still considered valid immediately after
   being written (`cached_access_token` returns `Some`).
5. `servicenow_http_oauth_normal_expires_in_is_unchanged_in_spirit` — a normal
   1800 s grant is still cached and reused across two requests (guards against
   the clamp breaking the common path).

**Verification**: `cargo test -p daku-core collector` and
`cargo test -p daku-core servicenow_http` — run them **separately**; cargo takes
one TESTNAME and a second positional argument is an error.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "clock.sleep" crates/daku-core/src/collector.rs` shows a
      `saturating_sub`
- [ ] `grep -n "MIN_TOKEN_TTL_SECS" crates/daku-core/src/servicenow.rs` → ≥ 2
      matches, and the constant carries a doc comment explaining why
- [ ] `grep -n "MAX_TOKEN_TTL_SECS\|MAX_RETRY_AFTER\|MAX_429_RETRIES" crates/daku-core/src/servicenow.rs`
      shows all three values unchanged from before your edit
- [ ] `cargo test -p daku-core collector` → all pass, two more tests
- [ ] `cargo test -p daku-core servicenow_http` → all pass, three more tests
- [ ] `git diff --name-only` lists only the two in-scope files and
      `plans/README.md`
- [ ] `plans/README.md` status row for 054 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- Keeping `pub fn tick`'s signature forces an awkward shape — report the
  alternative you would prefer rather than changing the public API silently
  (`tick` is called by tests and by `run`).
- Any existing `servicenow_http` test changes behaviour after Step 2 — the
  common 1800 s path must be untouched, so a failure there means the clamp is
  wrong.
- You cannot write test 1 without reaching for a real sleep. Everything here
  must go through the `Clock` trait; a test with a real sleep is a flaky test in
  `bun run check`.

## Maintenance notes

- After Step 1, the overrun warning and the sleep read the same `elapsed`. If
  the warning is ever moved or reworded, keep them reading one value.
- `MIN_TOKEN_TTL_SECS` and `TOKEN_TTL_SKEW_SECS` interact: the skew must never
  push a floored TTL to zero or negative. Test 4 pins that; keep it.
- Deliberately **not** done: publishing per Environment as each finishes rather
  than after the whole tick joins. Plan 022 deferred that on purpose and it is a
  much larger change to the scoped-thread structure.
