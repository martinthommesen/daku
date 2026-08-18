# Plan 067: Spike — decide how the Operator reloads config and forces a poll without relaunching daku

> **Executor instructions**: This is a **spike plan**. Its deliverable is a
> written decision note plus, at most, a throwaway prototype — **not** a shipped
> feature. Do not implement the final design; the point is to answer the open
> questions so a build plan can be written with confidence. If anything in the
> "STOP conditions" section occurs, stop and report. When done, update the
> status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/collector.rs crates/daku-protocol/src/protocol.rs crates/daku-client/src/process.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding.

## Status

- **Priority**: P3
- **Effort**: M (spike)
- **Risk**: MED (the shapes it evaluates are risky; the spike itself is not)
- **Depends on**: `plans/051-local-daemon-reconnect-and-supervisor-test.md`
  (051 changes when the supervisor respawns, which is one of the candidate
  mechanisms here)
- **Category**: direction
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

The protocol is one-directional exactly where the Operator needs it not to be.
Three event kinds go out (`EnvironmentsUpdated`, `SignalSnapshotsUpdated`,
`SignalSamplesUpdated`); the entire inbound surface is
`Ping | GetSettings | UpdateSettings`.

Consequences the README states in its own words, three times: *"The daemon reads
this file at start — relaunch daku after creating or editing it."* Adding an
Environment, fixing a typo in a URL, or correcting a Keychain entry all cost a
full app relaunch. And after the Operator fixes something **in ServiceNow**,
there is no way to say "check now" — they wait up to a poll interval with no
feedback.

This is narrower than something `plans/README.md` already rejected. That
rejection was *"re-reading `poll_interval_secs` every tick"* — an implicit,
per-tick re-read. This is an explicit, Operator-initiated command. Worth saying
plainly so the distinction is not lost.

The reason this is a spike and not a build plan: the obvious implementation
touches the scoped-thread collector structure that plans 022 and 031 own, and
there is a much cruder alternative that might be entirely good enough. That
choice should be made with evidence, not in a build plan's Step 1.

## Current state

**`crates/daku-protocol/src/protocol.rs:42-53`** — the whole inbound surface:

```rust
pub enum Command {
    Ping,
    GetSettings,
    UpdateSettings { settings: DaemonSettings },
}
```

**`crates/daku-core/src/collector.rs`** `start_default_loop` — config is read
once, and an absent file means the daemon never polls at all, even if the file
appears later:

```rust
pub fn start_default_loop(
    environments_path: &Path,
    store: StateStore,
    settings: &DaemonSettings,
    shutdown: Arc<AtomicBool>,
) -> Option<Receiver<ServerMessage>> {
    let environments = match load_environments(environments_path) {
        Ok(environments) => environments,
        Err(error) => {
            if is_not_found(&error) {
                eprintln!("daku collector idle: missing {}", environments_path.display());
                return None;
            }
            eprintln!("daku collector not started: {error}");
            return None;
        }
    };
```

`build_default_loop` then bakes that snapshot into every collector: each
per-Environment group and both shared collectors own a `Vec<EnvironmentConfig>`
by value.

**`crates/daku-core/src/collector.rs`** — a one-shot collect already exists as a
callable unit, which is a useful precedent for "poll now":

```rust
pub fn probe_availability_once(...)
```

**`crates/daku-client/src/process.rs`** — `replace_local_daemon` already
performs a full daemon swap with a client fan-out, under `inner.restart`. That
is the "crude alternative" this spike must weigh.

**`plans/README.md`** records three prior protocol bumps (plans 020, 029, 039),
each incrementing the live `PROTOCOL_VERSION` — the convention exists.

### Constraints you must honor

- **`plans/README.md` › Ownership locks**: the poll loop belongs to plan 003's
  `build_default_loop`; **Config SoT is `~/.daku/environments.json`** with no
  Environments SQLite table. Any design must keep the JSON file authoritative.
- **`docs/spec/v1.md` §10**: alerting, multi-user access control and a
  second-Platform seam are out of scope. A reload command is none of those, but
  do not let the design grow into a settings UI.
- **A protocol bump means desktop and daemon ship together** (`plans/README.md`).
  Always increment the live `PROTOCOL_VERSION`; never set a fixed number.
- Plans 022/031 own the scoped-thread group structure. A design that mutates it
  at runtime must say exactly how, or be rejected.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 (the tree must be left green) |
| Prototype run | `DAKU_UI_FIXTURE=1 bun run dev` | Operator-run, if a prototype is built |

## Scope

**In scope**:
- `docs/research/reload-command.md` (create — the deliverable)
- A **throwaway** prototype on a local branch, if one is needed to answer a
  question. It must not land on `main`.

**Out of scope** (do NOT do these):
- Any change to `main`'s source. This spike ships a document.
- Bumping `PROTOCOL_VERSION`.
- Building the feature.
- Designing a settings UI, an alerting surface, or anything else in
  `docs/spec/v1.md` §10.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**. A prototype
  lives on a short-lived local branch you **delete** when finished
  (`docs/agents/git-workflow.md` rule 3). Only the note lands on `main`.
- Commit style: imperative, e.g.
  `Add the reload-command decision note (#92).`

## Steps

### Step 1: Establish what the Operator actually wants

Two distinct wants hide behind "reload". Separate them in the note, because they
may have different answers:

- **Reload config** — `~/.daku/environments.json` changed: an Environment added,
  removed, relabelled, or a URL corrected.
- **Poll now** — config is fine; the Operator fixed something in ServiceNow and
  wants the next tick immediately.

For each, write down: what has to change in the daemon, what the desktop needs
to send, and what the Operator sees while it happens.

### Step 2: Evaluate three shapes, with evidence

For each, read the code and record concrete costs — files touched, what breaks,
what has to be tested — not impressions.

**A. Daemon self-restart.** `Command::Reload` sets the shutdown flag; the
supervisor's existing respawn path brings a fresh daemon up with fresh config.
Reuses `replace_local_daemon`, which is already correct about locking and client
fan-out (and which plan 051 is hardening). Cost: the daemon's in-memory caches
(OAuth tokens, the 30-minute drift inventory) are lost, so the first tick after
a reload is more expensive. Question to answer: how much more?

**B. Rebuild the collector graph in place.** `Command::Reload` re-reads
`environments.json` and builds a new `CollectorLoop`, swapped in behind the
shutdown flag. Keeps the process and its SQLite connection. Cost: touches the
scoped-thread structure plans 022/031 own, and needs a clear answer for what
happens to a tick already in flight. Question to answer: is there a swap point
that does not require restructuring `tick`?

**C. Poll-now only.** No reload at all — `Command::PollNow` just interrupts the
sleep so the next tick starts immediately. Much smaller. Config changes still
need a relaunch. Question to answer: how much of the felt friction does this
actually remove?

Note that **C composes with A**: poll-now for the common case, self-restart for
config changes, and no collector-graph surgery at all.

### Step 3: Answer the one question that needs code

Whichever shape leads, one thing cannot be settled by reading: **how long a
daemon restart actually takes end to end**, from `Command` to the first fresh
`EnvironmentsUpdated` reaching the desktop. That number decides whether option A
is acceptable on its own.

Measure it on a local branch — time a `replace_local_daemon` cycle against the
fixture path — and put the number in the note. Delete the branch afterwards.

### Step 4: Write the decision note

`docs/research/reload-command.md`, following the shape of the existing research
notes (`docs/research/hosted-daemon.md`, `docs/research/signal-drill-in.md`):
current state, the options with their real costs, a recommendation, and explicit
**open questions** and **follow-up plan stubs**.

Two things the existing notes teach by counter-example (plan 060 is cleaning
both up):

- **Cite symbols, not line numbers.** Line references in `docs/research/**` have
  already rotted once.
- **Never write "folded into plan NNN" unless you have verified it landed.**
  `hosted-daemon.md` did exactly that and was wrong for months.

Stamp the note with the commit it was written against.

**Verify**: `bun run check` → exit 0. `git status` → only the new note.

## Test plan

No tests — this plan ships a document. The verification is that the note answers
each Step 2 question with evidence from the code, carries Step 3's measured
number, and states its open questions honestly rather than resolving them by
assertion.

## Done criteria

ALL must hold:

- [ ] `docs/research/reload-command.md` exists and covers: the two distinct
      wants (Step 1), all three shapes with concrete costs (Step 2), the
      measured restart time (Step 3), a recommendation, open questions, and
      follow-up plan stubs
- [ ] Every symbol the note names is found by `git grep` at `HEAD`
- [ ] The note cites **no** bare line numbers
- [ ] The note explicitly distinguishes this from the rejected
      "re-read `poll_interval_secs` every tick"
- [ ] `git diff --name-only` lists only `docs/research/reload-command.md` and
      `plans/README.md`
- [ ] `git branch` shows no leftover spike branch
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 067 updated to DONE, with the
      recommendation in one line

## STOP conditions

Stop and report back (do not improvise) if:

- You find yourself implementing the feature. This plan ships a note.
- Step 3 cannot be measured without changing `main`.
- The evidence says none of the three shapes is worth it. **That is a valid
  outcome** — write it up, recommend no change, and mark the plan DONE with that
  conclusion. A spike that says "not worth doing" has done its job.
- A design seems to require an Environments table in SQLite. That contradicts
  `plans/README.md`'s Config SoT lock — report rather than propose it.

## Maintenance notes

- Whoever writes the build plan from this note inherits the protocol-bump
  convention: increment the live `PROTOCOL_VERSION`, and desktop and daemon ship
  together.
- If option C (poll-now only) wins, say so loudly in the note — it is by far the
  cheapest and it may remove most of the friction. The lazy answer being the
  right one is a result, not a failure.
- `docs/research/hosted-daemon.md` is the cautionary example for how these notes
  rot. Symbols over line numbers, and verified claims over "folded into plan N".
