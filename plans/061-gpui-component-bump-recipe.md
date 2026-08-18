# Plan 061: The documented recipe for bumping the UI toolkit actually works

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- Cargo.toml Cargo.lock README.md docs/adr/0008-gpui-component-shell.md`
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

Three places — `README.md`, the comment in `Cargo.toml`, and ADR-0008's "Pin
discipline" — give the same three-command recipe for bumping gpui-component:

```sh
cargo update -p gpui-component --precise <rev>
cargo update -p gpui-component-assets --precise <rev>
cargo update -p gpui --precise <zed sha from gpui-component's Cargo.lock at that rev>
```

The third command works. The first two probably cannot, because
`gpui-component` and `gpui-component-assets` carry `rev = "…"` **in
`Cargo.toml`**, which puts the revision inside the dependency's source id
(`git+…?rev=972a3ebf…`). `--precise` selects a version within a source; it
cannot move a revision the manifest pins. Changing that rev means editing
`Cargo.toml`.

ADR-0008 supplies the strongest evidence for this itself: it explains that
daku's `gpui`/`gpui_platform` lines deliberately carry **no** `rev` — the zed
commit is pinned in `Cargo.lock` only — and that is precisely what makes
`cargo update -p gpui --precise <sha>` work on them. The same reasoning says it
cannot work on a rev-pinned dependency.

This matters because gpui-component is the riskiest dependency in the repo: it
tracks `main`, moves daily, and ADR-0008's whole point is *bump both together*.
A maintainer following the documented recipe runs three commands, two of which
do nothing or error, and `gpui` moves without gpui-component — the exact
breakage the pin discipline exists to prevent.

**This finding is MED confidence.** It is reasoned from Cargo's git source-id
semantics plus ADR-0008's own explanation; the `--dry-run` check during the
audit failed on a network error rather than on revision resolution, so it is
**not** empirically confirmed. Step 1 confirms it before anything is rewritten.

## Current state

**`Cargo.toml:29-39`**:

```toml
# Prefer upstream Zed GPUI after browser/WKWebView strip (plan 001).
# The zed sha is pinned in Cargo.lock only — gpui-component depends on
# `gpui = { git = zed }` with no rev, and Cargo treats `git+zed?rev=X` and
# `git+zed` as different sources (ADR-0008). Bump with
# `cargo update -p gpui-component --precise <rev>` (also
# `-p gpui-component-assets`) then `cargo update -p gpui --precise <zed sha
# from gpui-component's Cargo.lock at that rev>`, and re-run `bun run check`.
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = [
    "font-kit",
] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "972a3ebfd01afca7da6d8b6f31c9a51288ea5565" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component", rev = "972a3ebfd01afca7da6d8b6f31c9a51288ea5565" }
```

**`Cargo.lock`** — the two source-id shapes, which is the whole argument:

```
source = "git+https://github.com/longbridge/gpui-component?rev=972a3ebf…#972a3ebf…"
source = "git+https://github.com/zed-industries/zed#e0931d5a…"
```

The first carries `?rev=` (from the manifest); the second does not.

**`README.md:41-51`** — the same three commands, in the Build section.

**`docs/adr/0008-gpui-component-shell.md`** — "Pin discipline", which both
states the reasoning correctly *and* repeats the unusable commands.

**`plans/044-gpui-component-shell-and-pin.md`** records the spike's verified
command list; only `cargo update -p gpui --precise …` was ever actually run. The
gpui-component form was written from symmetry.

### Constraints you must honor

- **ADR-0008's decision is not in question.** The `Cargo.lock`-only zed pin and
  the rev-pinned gpui-component are deliberate and correct. This plan changes
  *how the bump is described*, not the pinning strategy.
- `plans/README.md` records the `Cargo.lock`-only zed pin as settled — do not
  propose adding a `rev` to `gpui`/`gpui_platform`.
- **Do not run `cargo update` against the real `Cargo.lock`.** A stray
  re-resolution of every zed crate is exactly what the README warns against
  ("Do not run `cargo update` casually"). Step 1 uses a scratch clone.
- ADRs record decisions; correcting a mechanical recipe inside one is fine, but
  do not rewrite the decision or its "Considered options".

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Scratch clone | see Step 1 | — |

## Scope

**In scope**:
- `README.md`
- `Cargo.toml` (the comment only)
- `docs/adr/0008-gpui-component-shell.md` (the "Pin discipline" paragraph only)

**Out of scope** (do NOT touch):
- The actual dependency lines in `Cargo.toml`. No rev changes, no `rev` added
  to `gpui`/`gpui_platform`.
- `Cargo.lock`. This plan must not modify it.
- ADR-0008's decision, accepted costs, or considered options.
- Actually bumping gpui-component. That is a separate, Operator-timed
  operation.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Correct the gpui-component bump recipe: the rev lives in Cargo.toml (#86).`

## Steps

### Step 1: Confirm it empirically, on a scratch clone

**Do not run this in the working tree.** Make a throwaway copy and try the
documented command there:

```sh
scratch=$(mktemp -d)
git clone --no-hardlinks . "$scratch/daku"
cd "$scratch/daku"
cargo update -p gpui-component --precise <some other gpui-component sha>
```

Record the exact output. Three outcomes:

- **It errors** (package id spec did not match, or `--precise` rejected for a
  rev-pinned source) → the finding is confirmed. Proceed to Step 2 and quote the
  error in the corrected docs so the next reader does not re-try it.
- **It succeeds and changes `Cargo.lock`** → **the finding is wrong.** STOP.
  Report that the documented recipe works, mark this plan REJECTED in
  `plans/README.md` with that one-line reason, and change nothing.
- **It fails on a network error** (the audit's outcome) → retry once. If it
  still cannot reach the network, STOP and report — do not rewrite three
  documents on an unverified premise.

Clean up the scratch directory when done.

**Verify**: the recorded output, pasted into your report.

### Step 2: Write the recipe that works

The correct sequence, given a target gpui-component rev:

1. Edit both `rev = "…"` values in `Cargo.toml:36-37` to the new rev.
2. Read the zed sha from gpui-component's own `Cargo.lock` at that rev.
3. `cargo update -p gpui --precise <that zed sha>` — this one command cascades
   every zed crate, which is why `gpui` carries no `rev`.
4. `bun run check`, then launch the fixture (`DAKU_UI_FIXTURE=1 bun run dev`)
   for the Operator's visual check.

Write that into `README.md`, replacing the current three-command block, and say
explicitly **why** step 1 is a manual edit: the rev is part of the dependency's
source id, so `--precise` cannot move it. That sentence is what stops the
recipe rotting back.

**Verify**: `grep -n "cargo update -p gpui-component" README.md` → no matches.

### Step 3: Same correction in the other two places

Update the comment in `Cargo.toml:29-35` and ADR-0008's "Pin discipline"
paragraph to match `README.md`. All three must describe one procedure; the
reason ADR-0008 gives for the asymmetric pinning stays exactly as it is — it is
correct and it is the justification for the manual edit.

**Verify**: `grep -rn "cargo update -p gpui-component" README.md Cargo.toml docs/`
→ no matches. `bun run check` → exit 0 (confirms the `Cargo.toml` comment edit
did not disturb the manifest).

## Test plan

No code change, so no tests. Verification is Step 1's recorded output plus the
greps, and `git diff Cargo.lock` being empty.

## Done criteria

ALL must hold:

- [ ] Your report contains the verbatim output of Step 1
- [ ] `bun run check` exits 0
- [ ] `git diff --stat Cargo.lock` → empty
- [ ] `git diff Cargo.toml | grep '^[-+].*rev = '` → no output (no dependency
      line changed, only the comment)
- [ ] `grep -rn "cargo update -p gpui-component" README.md Cargo.toml docs/` →
      no matches
- [ ] `grep -n "cargo update -p gpui --precise" README.md` → still present
      (that command is correct and must stay)
- [ ] `git diff --name-only` lists only `README.md`, `Cargo.toml`,
      `docs/adr/0008-gpui-component-shell.md` and `plans/README.md`
- [ ] `plans/README.md` status row for 061 updated to DONE **or** REJECTED with
      the Step 1 outcome as its reason

## STOP conditions

Stop and report back (do not improvise) if:

- Step 1 shows the documented command **works** — mark REJECTED and change
  nothing.
- Step 1 cannot be run (no network after a retry).
- `Cargo.lock` changes at any point. Restore it (`git checkout -- Cargo.lock`)
  and report.
- You are tempted to add `rev` to `gpui`/`gpui_platform` to make the symmetry
  real. ADR-0008 rejected that explicitly; it would break the shared-source
  requirement with gpui-component.

## Maintenance notes

- The rule worth keeping visible: **a `rev` in `Cargo.toml` is edited by hand; a
  `Cargo.lock`-only pin is moved with `--precise`.** daku has one of each, on
  purpose, and that is the thing every future reader trips on.
- Whoever performs the next real bump should re-read the corrected recipe first
  and report any further gap — this plan makes the recipe plausible, but only an
  actual bump proves it end to end.
- `plans/044` records the spike's verified command list. If the recipe changes
  again, that file is the place to look for what was actually executed versus
  written from symmetry.
