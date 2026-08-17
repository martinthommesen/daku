# Plan 010: Notarised DMG + Sparkle; Homebrew cask alternate

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- scripts resources src/updater.rs docs/packaging.md README.md`
> Confirm 009 DONE (`cargo check -p daku`). On mismatch, STOP.

## Status

- **Priority**: P3
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: plans/009-gpui-shell-variant-c.md
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/23

## Why this matters

ADR-0006: notarised `.app` / DMG + Sparkle primary; Homebrew cask alternate with Sparkle disabled. Adapts waku packaging scripts — no new updater stack.

## Current state

- After 001, expect some of: `scripts/bundle.sh`, `scripts/release.ts`, `scripts/appcast.ts`, `src/updater.rs`, `resources/*`. Inventory listed these as packaging (not day-1).
- macOS-only; GPL-3.0-only; secrets never in git.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Inventory scripts | `test -f scripts/bundle.sh` | exit 0 (if missing, restore from pin — see Step 1) |
| Unsigned bundle | `./scripts/bundle.sh --unsigned` **or** the flag printed by `./scripts/bundle.sh --help` that skips codesign — record the exact invocation in `docs/packaging.md` in the same commit | `test -d dist/Daku.app \|\| test -d "dist/daku.app"` (pick one name in Step 2 and match here) |
| Check updater | `cargo check -p daku` | exit 0 |
| Cask style | `brew style homebrew/daku.rb` if file + brew exist; else `ruby -c homebrew/daku.rb` | exit 0 |
| No private keys | `rg -n 'BEGIN .*PRIVATE' scripts resources docs` | no matches |

If `bundle.sh` has no `--unsigned`, use: `SKIP_CODESIGN=1 ./scripts/bundle.sh` and document that; if neither works, STOP and add a single `--unsigned` flag rather than inventing a second script.

## Scope

**In scope**

1. Rebrand plist/icns/scripts → `Daku.app`, bundle id `app.daku`.
2. `scripts/bundle.sh` produces app + DMG; embed `daku-daemon` as upstream did.
3. Sparkle via existing `src/updater.rs` + appcast scripts; public appcast URL on GitHub Releases.
4. Build switch `DAKU_CHANNEL=homebrew` (or Cargo feature `channel-homebrew`) makes updater a no-op — unit/stub test required.
5. `homebrew/daku.rb` cask draft + `docs/packaging.md`.
6. Appendix A human checklist for Developer ID / notary / Sparkle private key (no values in repo).

**Out of scope**

- Linux/Windows; Mac App Store; committing key material; replacing Sparkle without ADR.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Ensure packaging scripts exist

```sh
test -f scripts/bundle.sh || echo MISSING_BUNDLE
test -f src/updater.rs || echo MISSING_UPDATER
```

If `MISSING_*`, restore those files only from waku pin `4c483bc282faf4ce9296390887f09b44abb34f27`, then rebrand.

**Verify**: `test -f scripts/bundle.sh && test -f src/updater.rs`

### Step 2: Rebrand + unsigned bundle

Lock app dir name to **`Daku.app`**. Document exact unsigned command in `docs/packaging.md`.

**Verify**: run that documented command → `test -d dist/Daku.app`; `rg -n 'Waku' resources/Info.plist scripts/bundle.sh` → no matches (comments exempt if unavoidable — prefer zero).

### Step 3: Sparkle + homebrew no-op

**Verify**: `cargo test -p daku updater_channel` → homebrew/cask build does not schedule checks; `cargo check -p daku` → exit 0.

### Step 4: Cask draft

**Verify**: `test -f homebrew/daku.rb`; `ruby -c homebrew/daku.rb` → exit 0 (or `brew style` if available).

### Step 5: Docs + secret scan

**Verify**: `test -f docs/packaging.md`; `rg -n 'BEGIN .*PRIVATE' scripts resources docs` → no matches; `rg -n 'dev[0-9]+\\.service-now' docs` → no matches.

## Test plan

| Case | Expected |
|------|----------|
| unsigned bundle command | `dist/Daku.app` exists |
| updater_channel homebrew | no-op |
| notarised upload | only if Appendix A creds exist — else leave Status note BLOCKED for notarisation but scripts DONE |

## Done criteria

- [ ] `test -d dist/Daku.app` after documented unsigned command (CI may skip dist artifact — then require the command to be documented and dry-runnable with `--help` exit 0 **and** a `scripts/bundle.sh` that contains `Daku.app`)
- [ ] `cargo test -p daku updater_channel` exit 0
- [ ] `test -f homebrew/daku.rb && test -f docs/packaging.md`
- [ ] `rg -n 'BEGIN .*PRIVATE' scripts resources docs` → no matches
- [ ] `plans/README.md` row 010 Status = `DONE` (or `BLOCKED` with reason `notarisation credentials missing` **only if** unsigned+cask+docs already meet the bullets above)

## STOP conditions

- Asked to publish notarised DMG without Developer ID / notary profile — finish unsigned+docs; run Appendix A; do not fake notary.
- Sparkle private key missing before publishing appcast — STOP upload; unsigned local OK.
- Replace Sparkle without ADR — STOP.

## Maintenance notes

- CI secrets for notary later — repo settings only.
- Reviewers: cask and Sparkle must not both auto-update one install.

---

## Appendix A — Operator wizard checklist (human-only)

No secret values in issues/chat/repo.

1. Apple Developer Program membership.
2. Developer ID Application certificate in login keychain.
3. Notary credentials via `notarytool store-credentials` — store profile **name** only in private notes.
4. Sparkle Ed25519 keypair; private key in release secrets; public key only in repo if scripts already expect it.
5. Appcast URL + GitHub Release asset name `Daku-x.y.z.dmg`.
6. Test notarised open on a second Mac; publish cask with Sparkle disabled.
