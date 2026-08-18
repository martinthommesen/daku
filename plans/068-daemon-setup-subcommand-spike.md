# Plan 068: Spike — decide whether `daku-daemon` should fix what `doctor` already diagnoses

> **Executor instructions**: This is a **spike plan**. Its deliverable is a
> written decision note plus, at most, a throwaway prototype — **not** a shipped
> feature. Do not implement the final design. If anything in the "STOP
> conditions" section occurs, stop and report. When done, update the status row
> for this plan in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-daemon/src/main.rs crates/daku-core/src/collector.rs crates/daku-core/src/config.rs crates/daku-core/src/persistence.rs README.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding.

## Status

- **Priority**: P3
- **Effort**: M (spike)
- **Risk**: MED (the feature handles secrets; the spike itself does not)
- **Depends on**: none
- **Category**: direction
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

The daemon has two subcommands and both are read-only. `probe-availability`
observes; `doctor` diagnoses and even prints the exact remedy — *"credential:
MISSING (Keychain service daku, account = id)"* — and then cannot act on it.

Everything else is delegated to the Operator by the README:

- copy `environments.example.json` to `~/.daku/environments.json` **and
  `chmod 600` it yourself** (the daemon only enforces `0700` on the directory
  and `0600` on files *it* writes);
- run `security add-generic-password -s daku -a <id> -w '<json blob>'` per
  Environment, with the JSON assembled by hand — and `-w` on a command line
  means the secret lands in shell history;
- relaunch after editing config.

`run_doctor` already reads `environments.json`, resolves Keychain Credentials
and probes reachability. It knows every failure and its fix. The asymmetry —
two diagnostic subcommands, zero remediating ones — is the whole finding, and
the Operator is 100% of the user base, so this friction is the entire setup
experience.

**This is a spike because the feature writes secrets.** Every existing code path
only *reads* Credentials. That is a meaningful change in what daku is trusted to
do, and it deserves a decision note before an implementation.

## Current state

**`crates/daku-daemon/src/main.rs`** — the argument parser, showing the two
subcommands and how a third would slot in:

```rust
        let mut probe_availability = false;
        let mut doctor = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "probe-availability" => {
                    probe_availability = true;
                }
                "doctor" => {
                    doctor = true;
                }
```

**`crates/daku-daemon/src/main.rs`** — `doctor` already has everything:

```rust
    let report = daku_core::run_doctor(
        &environments_path,
        &settings,
        Arc::new(daku_core::config::KeychainCredentialStore),
        daku_core::servicenow::ServiceNowClient::new(...),
        daku_core::persistence::StateStore::daemon(...),
    )
```

**`crates/daku-daemon/src/main.rs`** — and already knows the remedy verbatim:

```rust
fn format_doctor_row(row: &daku_core::DoctorRow) -> String {
    let credential = match (row.credential_present, &row.credential_error) {
        (true, _) => "credential: present".to_owned(),
        (false, None) => "credential: MISSING (Keychain service daku, account = id)".to_owned(),
        (false, Some(error)) => format!("credential: ERROR {error}"),
    };
```

There is a test asserting `doctor` never prints `client_secret` or `password` —
that property must survive whatever this becomes.

**`crates/daku-core/src/config.rs`** — the `CredentialStore` trait is
**read-only**:

```rust
/// Looks up the secret blob for an Environment id.
///
/// One Keychain item per Environment (`service=daku`, `account=<id>`).
/// Value is JSON: oauth → `{"client_id","client_secret"}`; basic → `{"username","password"}`.
pub trait CredentialStore: Send + Sync {
```

**`crates/daku-core/src/persistence.rs`** — `ensure_daku_dir` forces `0700` on
the directory and `0600` on files the daemon writes; `environments.json` is
Operator-created, which is exactly why the README has to say `chmod 600`.

**`README.md`** — the hand-run setup steps, including the
`security add-generic-password` example.

### Constraints you must honor

- **ADR-0004** is binding: secrets live in the macOS **Keychain** under a
  daku-owned service; non-secrets in `~/.daku/`; real Environments use OAuth
  client credentials; Basic auth is for PDI stand-ins only. A setup flow must
  implement that, not reinterpret it.
- **`docs/spec/v1.md` §6**: `https://` URLs only, directory `0700`,
  daemon-written files `0600`.
- **`plans/README.md` › Public hygiene**: never put hostnames, usernames or
  secrets in plans, commits or output. The note you write must contain **no
  real values** — placeholders only.
- **The `.claude/skills/wizard` skill exists for exactly this shape** — an
  interactive bash walkthrough for steps only a human can perform. `CLAUDE.md`
  names it. Evaluate it as a first-class option; it may beat a Rust subcommand.
- `format_doctor_row`'s never-print-secrets property is tested. Preserve it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 (leave the tree green) |
| Current diagnosis | `cargo run -p daku-daemon -- doctor` | Operator-run, on their own machine |

## Scope

**In scope**:
- `docs/research/operator-setup.md` (create — the deliverable)
- A **throwaway** prototype on a local branch, if needed. It must not land on
  `main`.

**Out of scope** (do NOT do these):
- Any change to `main`'s source.
- Writing, reading aloud, logging or echoing a real Credential **anywhere**,
  including in the note, a prototype, or your report.
- Changing `CredentialStore` or `run_doctor`.
- A GUI onboarding flow in the desktop app — a much larger surface; the note may
  mention it as an alternative but must not design it.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**. A prototype
  lives on a short-lived local branch you **delete** when finished. Only the
  note lands on `main`.
- Commit style: imperative, e.g. `Add the Operator-setup decision note (#93).`

## Steps

### Step 1: Write down the real setup path, end to end

From a clean machine to a working daku, exactly as the README describes it
today. For each step record: who must do it, what can go wrong, and whether
`doctor` already detects that failure. That table is the note's spine — it shows
precisely which friction is absorbable and which is irreducibly human.

Pay attention to the two sharp edges:
- `chmod 600` on an Operator-created `environments.json` — daku creates `~/.daku`
  as `0700` but does not own that file's mode.
- `security add-generic-password … -w '<secret>'` puts a secret in shell
  history. Note that `-w` **without** a value prompts instead; whether the
  README should say so is a finding in its own right.

### Step 2: Evaluate three shapes

**A. A `wizard` script.** Use the `wizard` skill to generate an interactive bash
walkthrough. Zero new Rust, zero new trust: the Operator still runs `security`
themselves, but guided, with the JSON assembled correctly and no secret on the
command line. Cost: a script to maintain; it cannot verify as it goes unless it
shells out to `doctor`.

**B. `daku-daemon setup`.** A Rust subcommand that creates `~/.daku`, writes
`environments.json` from a template with the right mode, prompts for Credentials
without echoing, and writes them to the Keychain. Cost: `CredentialStore` gains
a write path — the first time daku writes a secret rather than reading one — and
that needs its own threat thinking.

**C. `daku-daemon doctor --fix`.** Narrower: `doctor` already knows what is
wrong; let it repair only the mechanical failures (a missing directory, a wrong
file mode, a missing `environments.json`) and keep *printing* instructions for
the Credential steps. Cost: smallest; benefit: also smallest.

For each, record: files touched, whether it writes secrets, what could go wrong,
and what has to be tested.

### Step 3: Answer the questions that need looking, not guessing

- Can a Credential be written to the Keychain **without** it appearing in any
  process argument list, environment variable, shell history, or log? Confirm
  the mechanism you would use; do not assume.
- What does `security-framework` (already a `daku-core` dependency on macOS)
  offer for *writing* a generic password, and what does it prompt the user with?
- Does the Operator have to unlock the Keychain, and what does that look like
  the first time daku reads back what it wrote?

**Use placeholder values throughout.** Nothing you write down or run may
contain a real Credential.

### Step 4: Write the decision note

`docs/research/operator-setup.md`, following the shape of the existing research
notes: current state (Step 1's table), the three options with real costs, a
recommendation, explicit **open questions**, and **follow-up plan stubs**.

Cite symbols, never bare line numbers — `docs/research/hosted-daemon.md` rotted
that way and plan 060 is cleaning it up.

**Verify**: `bun run check` → exit 0. `git status` → only the new note.

## Test plan

No tests — this plan ships a document. Verification is that the note answers
each Step 3 question from evidence, and that **no real Credential value appears
anywhere** in the note, the prototype, or your report.

Additionally state in the note that whatever ships must preserve the tested
never-print-secrets property of `format_doctor_row`.

## Done criteria

ALL must hold:

- [ ] `docs/research/operator-setup.md` exists and covers: the end-to-end setup
      table (Step 1), all three shapes with costs (Step 2), answers to the three
      Step 3 questions, a recommendation, open questions, and follow-up plan
      stubs
- [ ] The note explicitly evaluates the `wizard` skill as option A
- [ ] The note contains **no** real hostname, username, or secret — placeholders
      only
- [ ] Every symbol the note names is found by `git grep` at `HEAD`
- [ ] The note cites no bare line numbers
- [ ] `git diff --name-only` lists only `docs/research/operator-setup.md` and
      `plans/README.md`
- [ ] `git branch` shows no leftover spike branch
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 068 updated to DONE, with the
      recommendation in one line

## STOP conditions

Stop and report back (do not improvise) if:

- You find yourself implementing a setup command. This plan ships a note.
- Step 3 shows a Credential cannot be written without exposing it somewhere
  (history, argv, a log). **That is the most valuable possible outcome** —
  write it up and recommend option A or C, which never handle the secret.
- A real Credential value would need to appear anywhere to make progress.
- The recommendation turns out to be "keep the README instructions, fix the
  `-w` advice". That is a valid, cheap outcome — say so plainly.

## Maintenance notes

- **The trust boundary this feature would move** is the thing to keep in view:
  daku currently only *reads* Credentials. Writing them is a different promise to
  the Operator, and the note should say so in those terms.
- `doctor`'s never-print-secrets test is the existing guarantee. Anything built
  from this note inherits it and should extend it, not weaken it.
- The `-w` shell-history point from Step 1 may be worth fixing in `README.md`
  regardless of which option wins — it is a one-line documentation change with
  real value. Call it out separately in the note so it does not get stuck behind
  the larger decision.
