# Plan 052: Neither distribution channel ships bytes nobody verified

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- scripts/release.ts scripts/appcast.ts scripts/bundle.sh homebrew/daku.rb resources/Info.plist docs/packaging.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

daku ships two ways (ADR-0006): a notarised DMG with Sparkle auto-updates, and
a Homebrew cask. Both currently have a hole where artifact verification should
be.

**The cask has none at all.** `sha256 :no_check` on a *version-pinned* URL means
`brew install` accepts whatever bytes the release asset serves. The documented
build command for that artifact is `--unsigned`, so there is no codesign
signature and no notarisation either, and Sparkle is compiled out for this
channel — so there is no second layer. Anyone who can substitute the release
asset gets code execution on an Operator's Mac.

**The Sparkle path can succeed without producing an appcast.** `release.ts`
wraps `generateAppcast()` in a `try` that only re-throws when `SPARKLE_PRIVATE_KEY`
or `SPARKLE_BIN` is set. Otherwise it warns, continues, and the script's final
line still says *"Upload the DMG, the ZIP (Sparkle enclosure) and appcast.xml to
a GitHub Release"*. Only `dist/updates` is cleaned, never `dist/appcast.xml`, so
the previous release's appcast is still sitting there ready to be uploaded
against a new DMG. Release is manual with no CI — **the printed instructions are
the checklist**, and they are wrong in exactly the case where the verification
material is missing.

**And the deferral has no home.** `plans/015-release-pipeline-sparkle-fixes.md`
(DONE) records: *"`SUPublicEDKey`/`sha256 :no_check` are tracked in
`docs/packaging.md` Appendix A."* Appendix A has six items; item 4 covers the
Ed25519 keypair, and **nothing in the file mentions the cask checksum**. So the
item was deferred to a list it was never added to.

## Current state

**`homebrew/daku.rb:1-12`**:

```ruby
# Draft Homebrew cask. Installs the channel-homebrew DMG so Sparkle is a
# compile-time no-op and `brew upgrade` owns updates.
# Build that artifact with: DAKU_CHANNEL=homebrew ./scripts/bundle.sh --unsigned
cask "daku" do
  version "0.1.0"
  sha256 :no_check

  url "https://github.com/martinthommesen/daku/releases/download/v#{version}/Daku-#{version}-homebrew.dmg"
```

The cask is still a **draft** — it is not published to any tap. That is what
makes this cheap to fix now rather than a live incident.

**`scripts/release.ts:172-197`** — the fail-open appcast step and the
unconditional upload instruction:

```ts
const updatesDirectory = join(projectRoot, "dist", "updates");
await rm(updatesDirectory, { force: true, recursive: true });
await mkdir(updatesDirectory, { recursive: true });
await $`ditto ${zipPath} ${join(updatesDirectory, zipName)}`;

const downloadUrlPrefix =
  process.env.DAKU_DOWNLOAD_URL_PREFIX ?? defaultDownloadUrlPrefix;
try {
  console.log("\n==> Generating the Sparkle appcast");
  await generateAppcast(updatesDirectory, downloadUrlPrefix);
  await $`ditto ${join(updatesDirectory, "appcast.xml")} ${join(projectRoot, "dist", "appcast.xml")}`;
} catch (error) {
  if (process.env.SPARKLE_PRIVATE_KEY || process.env.SPARKLE_BIN) {
    throw error;
  }
  console.warn(
    `Appcast skipped (Sparkle private key missing): ${error instanceof Error ? error.message : error}`,
  );
}

console.log(`\nApp ready: ${appBundle}`);
...
console.log(
  "Upload the DMG, the ZIP (Sparkle enclosure) and appcast.xml to a GitHub Release when notarised.",
);
```

**`scripts/release.ts:98-101`** — the channel flag and DMG name:

```ts
const homebrewChannel = process.env.DAKU_CHANNEL === "homebrew";
// Must match scripts/bundle.sh, which owns the DMG name.
const dmgName = `${appName}-${version}${homebrewChannel ? "-homebrew" : ""}.dmg`;
```

**`resources/Info.plist:27-30`** — the public key is still a comment, not a key:

```xml
    <!-- Sparkle feed on GitHub Releases. Add SUPublicEDKey after generating
    ...
    <key>SUFeedURL</key>
```

**`docs/packaging.md`** Appendix A — the six-item Operator checklist. Read it in
full; you are adding item 7.

### Constraints you must honor

- **ADR-0006** (`docs/adr/0006-macos-packaging.md`): DMG+Sparkle is primary,
  Homebrew cask is the alternate, and *"Sparkle is off or no-op in cask builds
  so the two channels do not fight."* Do not enable Sparkle for the cask.
- `plans/README.md` › Public hygiene: **never put instance hostnames, usernames
  or secrets in plans, scripts or commits.** `SPARKLE_PRIVATE_KEY` and
  `DAKU_CODESIGN_IDENTITY` are secrets — reference the variable name only,
  never a value, in code, docs or output.
- `docs/agents/git-workflow.md`: no GitHub Actions. Release stays manual and
  macOS-bound; `plans/README.md` records "Bun test harness for `scripts/*.ts`"
  as considered and rejected, so **do not add a test framework here** — the
  verification for this plan is running the script and reading its output.
- TypeScript style in `scripts/`: `#!/usr/bin/env bun`, `import { $ } from "bun"`,
  top-level `await`, `throw new Error(...)` for fatal conditions. Match it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Lint | `bun run lint` | exit 0 |
| Full gate | `bun run check` | exit 0 |
| Dry release (no creds) | `bun run release --unsigned` | see Step 2 |

## Scope

**In scope**:
- `scripts/release.ts`
- `homebrew/daku.rb`
- `docs/packaging.md`

**Out of scope** (do NOT touch):
- `scripts/appcast.ts` — its signing logic is correct; only its *caller* is
  fail-open.
- `scripts/bundle.sh` — the `--unsigned` mode itself is legitimate for local
  builds. What changes is that the cask stops documenting it.
- `src/updater.rs` — the Sparkle driver is plan 021's work and is correct.
- `resources/Info.plist` — do **not** commit a public key; the Operator adds it
  from their own keypair. This plan only makes its absence *fail the release*.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Fail the release when the appcast or Sparkle public key is missing; checksum the cask (#77).`

## Steps

### Step 1: Stop shipping a stale appcast

In `scripts/release.ts`, next to the `rm` of `dist/updates`, also remove
`dist/appcast.xml`:

```ts
await rm(join(projectRoot, "dist", "appcast.xml"), { force: true });
```

so a run that does not regenerate it leaves nothing behind to upload by mistake.

**Verify**: `bun run lint` → exit 0.

### Step 2: Make the appcast step fatal on the Sparkle channel

Replace the `catch` condition. The appcast is only meaningful for the Sparkle
channel, so:

- If `homebrewChannel` is true — skip the appcast step entirely and say so.
  There is no appcast for the cask; that is ADR-0006's design.
- Otherwise — **always** re-throw. A Sparkle release without an appcast is not a
  release. If the Operator wants a build without one, they already have
  `--unsigned` / `--adhoc`, and the message should say so.

Then make the closing instructions conditional: only mention `appcast.xml` when
one was actually generated this run, and only mention the ZIP for the Sparkle
channel.

**Verify**: `bun run release --unsigned` with no `SPARKLE_*` variables set →
the script **fails** at the appcast step with a message naming
`SPARKLE_PRIVATE_KEY` / `SPARKLE_BIN` (not a value), and the closing "upload
appcast.xml" line is **not** printed. Then
`DAKU_CHANNEL=homebrew bun run release --unsigned` → succeeds, skips the appcast
with an explanatory line, and does not mention `appcast.xml` in its closing
instructions.

### Step 3: Preflight the Sparkle public key

Before the closing instructions on the Sparkle channel, read the built bundle's
`Info.plist` and abort if the update-verification key is absent:

```ts
const plist = join(appBundle, "Contents", "Info.plist");
const publicKey = await $`plutil -extract SUPublicEDKey raw ${plist}`.quiet().nothrow();
if (publicKey.exitCode !== 0) {
  throw new Error(
    "The built app has no SUPublicEDKey, so Sparkle updates would be unverifiable. " +
      "Generate a keypair (docs/packaging.md Appendix A item 4) and add the public key to resources/Info.plist.",
  );
}
```

**Verify**: `bun run release --unsigned` on the Sparkle channel (with
`SPARKLE_BIN` set so Step 2 passes, or by temporarily reordering) → fails with
that message, because `resources/Info.plist` has no `SUPublicEDKey` today.
Record in your report that it failed for the expected reason.

### Step 4: Give the cask a real checksum and a signed artifact

In `scripts/release.ts`, on the homebrew channel only, compute the DMG's
checksum and print it as an explicit release step:

```ts
if (homebrewChannel) {
  const digest = (await $`shasum -a 256 ${outputPath}`.quiet().text()).split(/\s+/)[0];
  console.log(`\nCask sha256: ${digest}`);
  console.log(`Update homebrew/daku.rb: version "${version}", sha256 "${digest}"`);
}
```

In `homebrew/daku.rb`:
- Replace `sha256 :no_check` with a placeholder the release step fills in, and
  add a comment saying the value comes from `bun run release` on the homebrew
  channel.
- Change the build comment so it no longer documents `--unsigned` as the cask
  build path. The published cask artifact must be signed and notarised like the
  DMG; `--unsigned` stays a local-build convenience only.

**Verify**: `DAKU_CHANNEL=homebrew bun run release --unsigned` prints a
`Cask sha256:` line with a 64-character hex digest.
`grep -n "no_check" homebrew/daku.rb` → no matches.
`grep -n "unsigned" homebrew/daku.rb` → no matches.

### Step 5: Close the deferral in the checklist

In `docs/packaging.md` Appendix A, add item 7 covering the cask: sign and
notarise the homebrew DMG like the Sparkle one, take the sha256 from
`bun run release`, and update `homebrew/daku.rb`'s `version` and `sha256`
together before publishing the cask. Also update the Homebrew section
(`docs/packaging.md`, "Homebrew cask (alternate)") so it points at item 7.

**Verify**: `grep -n "sha256" docs/packaging.md` → at least one match in
Appendix A.

## Test plan

There is no automated test layer here by design (`plans/README.md` records the
Bun test harness for `scripts/*.ts` as considered and rejected — release is
manual and macOS-bound, so failures are immediate). Verification is the five
`bun run release` invocations above, each with its stated expected outcome.
Record each one's actual output summary in your report.

Do check that nothing you added prints a secret: after your changes, run
`bun run release --unsigned` and confirm no line contains a key or identity
**value** — only variable names.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "no_check" homebrew/daku.rb` → no matches
- [ ] `grep -n "unsigned" homebrew/daku.rb` → no matches
- [ ] `grep -n "appcast.xml" scripts/release.ts` shows the new `rm` and a
      **conditional** closing instruction
- [ ] `grep -n "SUPublicEDKey" scripts/release.ts` → at least one match
- [ ] `grep -n "sha256" docs/packaging.md` → at least one match
- [ ] `bun run release --unsigned` (no `SPARKLE_*` set) exits **non-zero** and
      never prints "upload … appcast.xml"
- [ ] `DAKU_CHANNEL=homebrew bun run release --unsigned` exits 0 and prints a
      64-hex-character `Cask sha256:` line
- [ ] No secret **value** appears in any changed file or in the script's output
- [ ] `git diff --name-only` lists only `scripts/release.ts`,
      `homebrew/daku.rb`, `docs/packaging.md` and `plans/README.md`
- [ ] `plans/README.md` status row for 052 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- `plutil -extract SUPublicEDKey raw` is not available or behaves differently on
  this machine — report it rather than substituting a parser.
- Making the appcast fatal breaks a release mode the help text advertises
  (`--adhoc`, `--skip-notarize`, `--skip-build`) in a way you cannot resolve
  inside `release.ts`.
- You are tempted to commit a public key, a signing identity or any other
  credential value to make a check pass. Do not. Report instead.

## Maintenance notes

- The cask's `version` and `sha256` must **always** move together. That pairing
  is now the point of Appendix A item 7; a version bump without a fresh digest
  is the failure to catch in review.
- `resources/Info.plist` deliberately still has no `SUPublicEDKey`. Step 3 turns
  that from a silent gap into a release-blocking error, which is the correct
  state until the Operator generates a keypair.
- This plan makes **no claim** about what Sparkle 2.9.4 does at runtime when
  `SUPublicEDKey` is absent — that is not verifiable from this repo. The
  preflight exists because an unverifiable update channel should not be
  shippable either way.
- If a cask tap is ever created, revisit: a published tap makes the checksum
  gap live rather than prospective, and that would be worth an ADR note.
