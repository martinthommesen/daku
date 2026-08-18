# Plan 064: The desktop validates the URL it hands to macOS, instead of trusting the daemon

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- src/dashboard_state.rs crates/daku-core/src/config.rs crates/daku-protocol/src`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Clicking "Open ↗" on a Signal card hands a string to `cx.open_url`, which on
macOS reaches `NSWorkspace` — a privileged OS API that will launch whatever
scheme is registered for it. That string is built from
`EnvironmentSummary.instance_url`, which arrives **from the daemon**.

The only https-and-no-userinfo guard lives in `crates/daku-core/src/config.rs`,
inside the daemon's config loader. On the default path that is fine: the desktop
spawns the daemon, and the daemon validated the URL when it read
`~/.daku/environments.json`.

But `src/daemon.rs` documents an attach path where `DAKU_DAEMON_ADDRESS` +
`DAKU_DAEMON_TOKEN` point the desktop at a daemon it does not own, and
`daemon_url` accepts a bare address defaulting to cleartext `ws://`. On that
path nothing local validates `instance_url` at all.

**Be clear about the size of this**: it is defence in depth, not a live hole.
The attach path is explicitly outside the v1 support envelope
(`docs/research/hosted-daemon.md`), and a hostile daemon already controls
everything the app displays. What makes it worth fixing anyway is the specific
sink — a click reaching `NSWorkspace` with an attacker-chosen scheme is a
qualitatively different outcome from "shows wrong numbers", and the guard is
about ten lines.

**Checked and clean, so nobody re-investigates it**: the deep-link *paths* are
compile-time constants selected by `signal_id` — no ServiceNow field value ever
reaches a URL. `encode_query` only escapes four characters, which is correct
because every string it receives is one of those seven literals. The variable
part is `instance_url` and nothing else.

## Current state

**`src/dashboard_state.rs:238-258`** — every path is a `&'static str`; only the
base is variable:

```rust
    /// Deep link into the ServiceNow list the Signal is measured from, mirroring
    /// the collectors' encoded queries. `None` without a selected Environment.
    pub fn signal_url(&self, signal_id: &str) -> Option<String> {
        let path = match signal_id {
            "availability" => "/sys_properties_list.do?sysparm_query=name=glide.war",
            ...
            "last_clone" => "/clone_instance_list.do",
            _ => return None,
        };
        let base = self.selected()?.instance_url.trim_end_matches('/');
        Some(format!("{base}{}", encode_query(path)))
    }
```

**`src/app.rs:385-391`** — the sink:

```rust
                    .when_some(url, |element, url| {
                        element.child(
                            Link::new(SharedString::from(format!("open-{signal_id}")))
                                .href(url)
                                .child("Open \u{2197}"),
                        )
                    }),
```

gpui-component's `Link` calls `cx.open_url(&href)` on click
(`~/.cargo/git/checkouts/gpui-component-*/972a3eb/crates/ui/src/link.rs`).

**`crates/daku-core/src/config.rs:51-74`** — the validation that exists, and the
rules to reuse:

```rust
/// Environment URLs carry Credentials on every request: https only, no
/// userinfo, no query/fragment. Trailing `/` is tolerated (`join_url` trims it).
fn validate_instance_url(id: &str, url: &str) -> anyhow::Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(anyhow!("environment {id}: instance_url must start with https://"));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(anyhow!("environment {id}: instance_url has no host"));
    }
    if host.contains('@') {
        return Err(anyhow!("environment {id}: instance_url must not contain userinfo"));
    }
    if rest.contains('?') || rest.contains('#') {
        return Err(anyhow!("environment {id}: instance_url must not contain a query or fragment"));
    }
    Ok(())
}
```

Note it is **private** to `config.rs`.

**`crates/daku-protocol/src/protocol.rs:127-131`** — the field's own doc comment
already flags its sensitivity:

```rust
    /// Instance base URL — non-secret, but "sensitive by default": it travels
    /// only over the loopback wire and is shown to the Operator, never logged.
    pub instance_url: String,
```

### Constraints you must honor

- **ADR-0004** fixes the rule: `https://` only, no userinfo. Do not invent a
  different policy on the client — implement the *same* one.
- `daku-protocol` must stay free of filesystem and OS dependencies (plan 033).
  A pure string predicate is fine there; anything else is not.
- `daku-core` must **not** become a dependency of the root `daku` crate at build
  time. If sharing the predicate means that, do not share it — see Step 1.
- The failure mode is silent and correct: an Environment whose URL fails the
  check simply has no "Open ↗" link. Do not surface an error dialog.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Client tests | `cargo test -p daku signal_url` | all pass |
| Core tests | `cargo test -p daku-core config` | all pass |

## Scope

**In scope**:
- `crates/daku-protocol/src/` (a new shared predicate) **or** `src/dashboard_state.rs`
  (a local one) — Step 1 decides which
- `crates/daku-core/src/config.rs` (only to delegate to the shared predicate, if
  Step 1 chooses that route)
- `src/dashboard_state.rs`

**Out of scope** (do NOT touch):
- `encode_query` and the seven path constants — verified correct for their
  actual inputs.
- `src/app.rs`'s `Link` rendering — it already handles `None` via `when_some`.
- `crates/daku-client/src/client.rs` `daemon_url` and the `ws://` default —
  the attach path's transport security is `docs/research/hosted-daemon.md`'s
  territory, not this plan's.
- Adding validation to the daemon. It already validates.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Validate instance_url in the client before handing it to the OS opener (#89).`

## Steps

### Step 1: Decide where the predicate lives

Two options — pick the one that does not create a new dependency:

**A. Shared in `daku-protocol`** (preferred if it is clean): add
`pub fn is_supported_instance_url(url: &str) -> bool` to `daku-protocol`,
implementing exactly `config.rs`'s rules as a boolean. Both crates already
depend on `daku-protocol` (`crates/daku-core/Cargo.toml`, root `Cargo.toml`), so
this adds no dependency. Then have `config.rs`'s `validate_instance_url` call it
and keep its own per-rule error messages — those messages are what the Operator
sees in `daku-daemon doctor`, so **do not degrade them to one generic error**.

**B. Local to `src/dashboard_state.rs`** if A turns out to require pulling
anything else into `daku-protocol`. Duplicating ten lines of string checks is
cheaper than a dependency edge, and the duplication is bounded and testable.

Record which you chose and why in your report.

**Verify**: `cargo tree -p daku --depth 1` shows no new dependency compared to
before your change.

### Step 2: Use it in `signal_url`

```rust
        let instance_url = &self.selected()?.instance_url;
        // The daemon validates this when it loads environments.json, but the
        // desktop can be attached to a daemon it does not own
        // (DAKU_DAEMON_ADDRESS), and this string reaches the OS URL opener.
        if !is_supported_instance_url(instance_url) {
            return None;
        }
        let base = instance_url.trim_end_matches('/');
        Some(format!("{base}{}", encode_query(path)))
```

**Verify**: `cargo test -p daku` → all pass. `bun run check` → exit 0.

## Test plan

New tests in `src/dashboard_state.rs` `mod tests`, alongside the existing
`signal_url_*` tests (which build state via the local `summary(...)` helper —
note it currently produces `https://{id}.example.service-now.com`, so you will
need a variant that sets an arbitrary `instance_url`):

1. `signal_url_rejects_a_non_https_instance_url` — `http://…` and `file:///…`
   both yield `None`.
2. `signal_url_rejects_userinfo` — `https://user@host/` yields `None`.
3. `signal_url_rejects_a_query_or_fragment` — `https://host/?x=1` and
   `https://host/#f` yield `None`.
4. `signal_url_still_builds_for_a_valid_environment` — the existing happy path
   is unchanged, including the trailing-slash trim. **Do not edit the existing
   `signal_url` tests' expectations.**

If Step 1 chose option A, add one test in `daku-protocol` for the predicate
itself, and confirm `cargo test -p daku-core config` still passes with its
per-rule error messages intact.

**Verification**: `cargo test -p daku signal_url` → all pass, +4 tests.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "is_supported_instance_url" src/dashboard_state.rs` → at least
      one match, inside `signal_url`
- [ ] `cargo test -p daku signal_url` → all pass, four more tests
- [ ] `cargo test -p daku-core config` → all pass, with the per-rule error
      messages unchanged (`git diff crates/daku-core/src/config.rs | grep '^-.*anyhow!'`
      → no output, or every removed message reappears verbatim)
- [ ] `cargo tree -p daku --depth 1` shows no new dependency
- [ ] `git diff src/dashboard_state.rs | grep '^-.*assert' | wc -l` → `0`
- [ ] Your report states whether you chose option A or B, and why
- [ ] `plans/README.md` status row for 064 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- Option A would require adding a dependency to `daku-protocol` — take option B
  instead and say so.
- Sharing the predicate forces `config.rs` to lose its per-rule error messages.
  Those messages are user-facing in `daku-daemon doctor`; keep them and take
  option B.
- You find a ServiceNow field value reaching a URL anywhere. That would be a
  materially different and more serious finding — report it immediately rather
  than folding it into this plan.

## Maintenance notes

- **Two implementations of one rule is the risk this plan creates** (under
  option B) or removes (under option A). If B was chosen, a comment in each
  place pointing at the other is the minimum; ADR-0004 is the source of truth
  for the rule itself.
- Every future value that reaches `cx.open_url` needs the same treatment. Today
  `signal_url` is the only producer — that is the thing to check when a second
  one appears.
- Deliberately **not** done: hardening the `ws://` default in `daemon_url`, or
  anything else about the attach path. That belongs with the hosted-daemon work,
  and `docs/research/hosted-daemon.md` (corrected by plan 060) is where it is
  tracked.
