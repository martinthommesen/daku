# Plan 060: The docs stop asserting things that are no longer true

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- docs CLAUDE.md AGENTS.md plans/README.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

A stale doc is worse than a missing one: it is believed. Four separate places
in this repo now assert something the code contradicts, and every one of them is
a document an *agent* is pointed at.

1. **`docs/spec/v1.md`'s Traceability table says ADRs `0001–0007`** while §4 and
   §8 of the same file both cite ADR-0008, and `docs/adr/0008-gpui-component-shell.md`
   exists. The one table a reader uses to enumerate decisions omits the decision
   governing the entire UI stack. Commit `668b752` edited the *adjacent* row of
   that same table and left this one.
2. **`docs/research/hosted-daemon.md` has drifted badly.** It is the note
   `plans/README.md` cites as the authority for what a non-loopback bind costs,
   and its "Security-pass checklist" lists as **open** an item plan 012 closed
   (it claims the daemon accepts an empty token — `require_token` refuses one,
   and so does the supervisor). It cites `crates/daku-core/src/hollow_backend.rs`,
   which plan 033 renamed to `settings_backend.rs`. It references `is_remote()`,
   `reconfigure` and `local_hostname`, which `git grep` now finds **only inside
   that document**. And it says its recommended option (A) — deleting the
   desktop exposure plumbing — was *"folded into plan 020's settings cleanup"*,
   when `DaemonExposureSettings`, `AppSettings.daemon_exposure`,
   `parse_allowed_origins` and `spawn_configured` are all still live with a
   caller on the app-launch path. Half of (A) landed; the note claims all of it
   did.
3. **`CLAUDE.md` and `AGENTS.md` point agents at a skill they cannot invoke.**
   Both end with "Also `improve-codebase-architecture` (`.claude/skills/`) for
   architecture audits"; that skill's frontmatter is
   `disable-model-invocation: true`. Meanwhile the skill this repo actually runs
   on — `improve`, whose `references/plan-template.md` every plan in `plans/`
   uses — is never named.
4. **At least eight verification commands in DONE plans cannot produce their
   stated result.** `cargo test -p daku-core servicenow_http urlencode` returns
   `error: unexpected argument 'urlencode' found` — cargo takes one TESTNAME.
   `cargo test -p daku-client app_settings` is recorded as "4 passed"; it
   reports 1. Every plan tells future executors to re-run these as a drift
   check; they will hit a usage error and reasonably conclude the tests were
   deleted.

## Current state

**`docs/spec/v1.md:145`**:

```markdown
| ADRs | [`docs/adr/`](../adr/) 0001–0007 |
```

…while `docs/spec/v1.md:41` and `:100` both link
`[ADR-0008](../adr/0008-gpui-component-shell.md)`.

**`docs/research/hosted-daemon.md`** — read the whole file. The specific claims
to check are its "Current state", "Recommendation", "Follow-up plan stubs" and
"Security-pass checklist" sections.

Ground truth at `HEAD`, verified:

| The note says | The code says |
|---------------|---------------|
| daemon accepts an empty token | `crates/daku-daemon/src/main.rs` `require_token` refuses empty/whitespace; `crates/daku-client/src/process.rs` refuses it again |
| `crates/daku-core/src/hollow_backend.rs:32-35` | file does not exist; it is `crates/daku-core/src/settings_backend.rs` (36 lines), wired at `crates/daku-daemon/src/main.rs` |
| `is_remote()`, `reconfigure`, `local_hostname` | none exist — `git grep` finds them only in this document |
| option (A)'s deletion "folded into plan 020" | `DaemonExposureSettings` at `crates/daku-client/src/process.rs:40`, `parse_allowed_origins` at `:103`, `spawn_configured` at `:368` with a live caller at `src/daemon.rs:32`, `daemon_exposure` persisted at `crates/daku-client/src/persistence.rs:18` — all still there |

**`CLAUDE.md:25`** and **`AGENTS.md:25`** (the two files are byte-identical —
`diff` is empty):

```markdown
Also `improve-codebase-architecture` (`.claude/skills/`) for architecture audits.
```

and `.claude/skills/improve-codebase-architecture/SKILL.md:4`:

```yaml
disable-model-invocation: true
```

**`plans/027-unit-test-gap-fill.md:226-228`** — the broken commands:

```markdown
- [ ] `cargo test -p daku-core servicenow_http urlencode` → ≥14 passed
- [ ] `cargo test -p daku-core load_environments start_default_loop` → 5 passed
- [ ] `cargo test -p daku-client app_settings` → 4 passed; ...
```

The same two-filter shape appears in `plans/003`, `plans/007`, `plans/026`
(twice), `plans/031` and `plans/046`.

### Constraints you must honor

- **`docs/agents/writing-for-agents`** conventions apply: these files are read
  by agents. Be specific, prefer symbol names over line numbers (line numbers
  rot — that is half of why this plan exists).
- `plans/README.md` › Public hygiene: **never** put instance hostnames,
  usernames or secrets in docs or commits.
- **Do not change a plan's substance.** Plans 003–046 are DONE history. You are
  correcting *verification commands that cannot run*, not revising what those
  plans decided or claimed to do.
- `docs/agents/git-workflow.md` still governs: no PRs, no Actions.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Symbol check | `git grep -n "<symbol>"` | as stated per item |

## Scope

**In scope**:
- `docs/spec/v1.md`
- `docs/research/hosted-daemon.md`
- `CLAUDE.md`, `AGENTS.md`
- `plans/003-*.md`, `plans/007-*.md`, `plans/026-*.md`, `plans/027-*.md`,
  `plans/031-*.md`, `plans/046-*.md` — **verification commands only**

**Out of scope** (do NOT touch):
- Any `.rs`, `.ts` or `.sh` file. This plan changes no code.
- **Deleting the exposure plumbing.** Finishing option (A) is a real S–M change
  with a live caller on the app-launch path; it needs its own plan and a smoke
  run. Here you only correct what the note *claims*.
- `docs/adr/**` — no ADR is contradicted by the code (0004 and 0007 were
  spot-checked and hold).
- `README.md`'s environment-variable table — verified complete and correct.
- The gpui-component bump recipe — that is plan 061, and it needs a runnable
  check first.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Catch the hosted-daemon note and the v1 spec up to HEAD (#85).`

## Steps

### Step 1: Spec Traceability

Change `docs/spec/v1.md:145` to `0001–0008`. Then re-read §4, §8 and the
Traceability table together and confirm nothing else in that file enumerates
ADRs.

**Verify**: `grep -n "0001–0007" docs/spec/v1.md` → no matches.
`ls docs/adr/*.md | wc -l` → matches the range you wrote.

### Step 2: Reconcile `docs/research/hosted-daemon.md`

Working through the table in "Current state", and **re-verifying each claim
yourself with `git grep` before editing**:

1. Strike the empty-token item from the Security-pass checklist and note it as
   closed by plan 012, citing `require_token` by name.
2. Repoint `hollow_backend.rs` → `crates/daku-core/src/settings_backend.rs`.
3. Delete the `is_remote` / `reconfigure` / `local_hostname` bullets.
4. Amend the option (A) follow-up stub to state what actually happened:
   `local_hostname`, `is_remote` and `reconfigure` were deleted under plans
   018/020/032; `DaemonExposureSettings`, `AppSettings.daemon_exposure`,
   `parse_allowed_origins` and `spawn_configured` are still live. Name them.
5. Drop bare line-number citations in favour of symbol names throughout the
   sections you touch — the whole failure mode here is rotted line references.
6. Re-check the two `protocol.rs` citations (`DAEMON_TOKEN_ENV`,
   `MAX_WIRE_MESSAGE_BYTES`) and the "closes when plan 014 lands" item; 014 is
   DONE.

**Verify**: for every symbol the note names,
`git grep -n "<symbol>" -- ':!docs'` finds it in the code, or the note no longer
names it. Run that check for each and record the list in your report.

### Step 3: Fix the agent-facing skill pointer

In **both** `CLAUDE.md` and `AGENTS.md`, replace the trailing sentence so it
names `improve` for audits and plan generation, and marks
`improve-codebase-architecture` as human-invoked only
(`/improve-codebase-architecture`) — or drops it.

The two files are byte-identical today. Keep them that way: make the same edit
in both and verify with `diff`.

**Verify**: `diff CLAUDE.md AGENTS.md` → no output.
`grep -n "improve" CLAUDE.md` → names the invocable skill.

### Step 4: Make the plan verification commands runnable

For each of `plans/003`, `plans/007`, `plans/026`, `plans/027`, `plans/031`,
`plans/046`: find every `cargo test` line with two positional filters and split
it into one command per line. **Then run each corrected command** and write the
number it actually reports.

For `plans/027:228` specifically, the recorded "4 passed" for
`cargo test -p daku-client app_settings` does not match because only one of the
three tests that landed matches that filter. Either correct the expected number
to what the filter really returns, or note the three test names explicitly. Do
**not** rename the tests — that is a code change and out of scope.

**Verify**: every corrected command runs and returns the number now written next
to it. Paste the command/number pairs in your report. Note that this plan file
itself quotes the broken forms deliberately — leave `plans/060-*` alone.

## Test plan

There is no code change, so there are no new tests. Verification is the greps
and the corrected commands above, each with its recorded output.

One additional check: `bun run check` must still exit 0, confirming no doc edit
accidentally touched a file the gate reads (`oxlint` lints `*.ts` in the repo
root).

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "0001–0007" docs/spec/v1.md` → no matches
- [ ] `grep -rn "hollow_backend" docs/` → no matches
- [ ] `grep -rn "is_remote\|reconfigure\|local_hostname" docs/research/hosted-daemon.md`
      → no matches
- [ ] `grep -n "DaemonExposureSettings" docs/research/hosted-daemon.md` → at
      least one match, in a sentence saying it is **still present**
- [ ] `diff CLAUDE.md AGENTS.md` → no output
- [ ] `grep -c "improve-codebase-architecture" CLAUDE.md` → the pointer either
      names it as human-invoked only, or is gone
- [ ] `grep -rEn 'cargo test -p [a-z-]+ [a-z_]+ [a-z_]+' plans/ | grep -v '^plans/060-'`
      → no matches. **Exclude this plan from the check**: the broken forms are
      quoted in its "Current state" and "Done criteria" on purpose, as the
      record of what was wrong. Do not "fix" those quotations.
- [ ] Your report lists every corrected command with the number it actually
      returned
- [ ] `git diff --name-only` contains no `.rs`, `.ts` or `.sh` file
- [ ] `plans/README.md` status row for 060 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- Any claim in the "Current state" table does not reproduce when you `git grep`
  it — the code may have moved again since this plan was written.
- Correcting a plan's verification command would require changing what the plan
  *asserted was done*. Report the discrepancy instead; that is a finding, not a
  doc fix.
- A corrected command **fails** rather than returning a different number. That
  means a DONE plan did not fully land — stop, and report it as such rather than
  editing the expectation to match.
- You conclude the exposure plumbing should be deleted as part of this. It
  should not — it needs its own plan.

## Maintenance notes

- **Line-number citations in `docs/research/**` rot.** This plan removes the
  ones that already have; prefer symbol names in anything new.
- `CLAUDE.md` and `AGENTS.md` being byte-identical duplicates is a standing
  drift risk. Worth considering a symlink or making one a one-line pointer —
  deliberately not done here because it changes how agents discover them.
- The two-positional-argument `cargo test` mistake is easy to repeat. When
  writing a done criterion, **run the command first**; that is the whole lesson
  of item 4.
- `docs/research/hosted-daemon.md` is scoped to an older commit by design. That
  is fine for prose, but a path that resolves to nothing is not — keep paths
  live even when the analysis is historical.
