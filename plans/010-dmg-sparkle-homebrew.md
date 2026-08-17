# Plan 010: Notarised DMG + Sparkle; Homebrew cask alternate

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plan 009 GPUI app launches via the repo’s documented dev script. Then `git diff --stat d912bbb..HEAD -- plans/010-dmg-sparkle-homebrew.md scripts/ src/updater.rs resources/`.

## Status

- **Priority**: P3
- **Effort**: L
- **Risk**: HIGH (signing/notary depends on Operator Apple account)
- **Depends on**: plans/009-gpui-shell-variant-c.md
- **Category**: direction
- **Planned at**: commit `d912bbb`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/23

## Why this matters

ADR-0006: v1 ships as a notarised macOS `.app` / DMG with Sparkle auto-updates; Homebrew cask is the alternate channel with Sparkle disabled so channels do not fight. Without packaging, the Operator cannot hand a colleague a build (intentional GPL distribution). This plan adapts waku’s `scripts/bundle.sh` / release / appcast path to **daku** branding — it does not invent a new updater stack.

## Current state

- ADR-0006 + spec §9.
- After plan 001: waku scripts may already be renamed (`scripts/bundle.sh`, optional `release.ts`, `appcast.ts`, `src/updater.rs`, Sparkle bits under `resources/`). Inventory: [waku-fork-inventory](https://github.com/martinthommesen/daku/blob/research/waku-fork-inventory/docs/research/waku-fork-inventory.md) lists these as packaging (not day-1).
- macOS-only (ADR-0001); Linux `bundle-linux.sh` stays out of scope / deleted if still present.
- Licence GPL-3.0-only; public GitHub is the source of truth for corresponding source.
- **Secrets never in git**: Developer ID certs, notary credentials, Sparkle Ed25519 private key, Apple ID app-specific passwords — Operator machine / CI secrets store only. Plans and issues name **variable names**, never values.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Unsigned local bundle (if script supports) | document exact `bun run` / `./scripts/bundle.sh` flags found in-repo | `.app` produced under `dist/` or documented path |
| Check updater compile | `cargo check -p daku` with Sparkle feature flags as in-repo | exit 0 |
| Appcast dry-run | script that builds appcast XML from a **local** DMG path | XML written; no upload required for done |

Exact script names after rename may be `scripts/bundle.sh` targeting `Daku.app` — discover from README and update this plan’s verify lines if they differ (do not invent parallel scripts).

## Suggested executor toolkit

- ADR-0006.
- If Apple Developer ID / notary profile is missing: generate a **wizard** checklist (`.claude/skills/wizard`) for the Operator — see Appendix A; do not block the *script* work on secrets being present, but STOP before claiming a notarised release is done.
- Public hygiene: no serial numbers, Team IDs that are personal, or key material in commits. Team ID placeholders like `YOUR_TEAM_ID` in docs are OK.

## Scope

**In scope**

1. **Rebrand bundle identity**
   - `Info.plist` / icns / bundle id → `com.…daku` (choose a stable id; document it in README). Replace remaining “Waku” user-visible strings in the packaged app.
   - Output `Daku.app` (or `daku.app` — pick one and stick to it).

2. **`scripts/bundle.sh` (or successor)**
   - Build release GPUI binary + embed/ship `daku-daemon` next to the app as waku did (discover layout from existing script).
   - Codesign with Developer ID Application when identity is available.
   - Produce DMG.

3. **Sparkle (primary channel)**
   - Keep/adapt `src/updater.rs` feed URL to a daku-owned HTTPS appcast location (GitHub Releases + static appcast is fine).
   - Adapt `scripts/appcast.ts` / `release.ts` for daku artifact names.
   - Document that the Sparkle **private** key lives only on the Operator release machine / CI secret store.

4. **Homebrew cask alternate**
   - Add a cask formula **draft** under `dist/homebrew/` or document a tap PR template (file in-repo is OK) that installs the notarised DMG/app from GitHub Releases.
   - **Sparkle off or no-op** in cask builds: compile flag / plist / updater early-return when `DAKU_CHANNEL=homebrew` or `#ifdef` / env detected at build time — document the exact switch you implement.
   - Upgrades for cask users: `brew upgrade --cask daku` (name TBD).

5. **README packaging section**
   - How to build unsigned for local smoke.
   - How to cut a signed+notarised release (points at Appendix A wizard for credentials).
   - How cask differs from Sparkle.

**Out of scope**

- Linux/Windows packages.
- Mac App Store.
- Committing any `.p12`, private keys, notary API JSON keys, or app-specific passwords.
- Changing GPL to a proprietary licence.

## Git workflow

- Branch: `plan/010-dmg-sparkle-homebrew`
- Commit example: `Add daku DMG/Sparkle packaging and Homebrew cask notes`

## Steps

### Step 1: Discover inherited packaging scripts

List what plan 001 actually left in `scripts/` and `resources/`. Update README with the real command names.

**Verify**: `ls scripts/bundle.sh scripts/release.ts scripts/appcast.ts src/updater.rs 2>/dev/null; ls resources 2>/dev/null | head` — note which exist; STOP only if **none** of bundle/updater remain and 001 deleted packaging without replacement — then restore from pinned waku SHA paths cited in inventory (copy those files only).

### Step 2: Rebrand bundle + unsigned build

Rename Waku → Daku in plist/scripts; produce an unsigned `.app` for local open (Gatekeeper may warn — OK for this step).

**Verify**: unsigned app launches; window title / About shows daku; `rg -n 'Waku' resources Info.plist scripts/bundle.sh` → no user-facing Waku left (comments OK).

### Step 3: Sparkle channel wiring

Point appcast URL at the public repo’s documented releases path; ensure updater compiles; add build flag to disable updater for cask.

**Verify**: `cargo check -p daku` exit 0; unit or stub test that “homebrew/cask build → updater no-op” holds.

### Step 4: Homebrew cask draft

Write cask template + README install instructions. Artifact URL uses GitHub Releases placeholders, not a private CDN.

**Verify**: `brew style` on the cask file if `brew` available; otherwise YAML/Ruby syntax review only.

### Step 5: Operator release doc + wizard

Add `docs/packaging.md` (or README section) describing the release checklist. Appendix A below can be turned into an interactive wizard script if useful — content must match.

**Verify**: docs contain **no** credential values; `gitleaks` clean on the commit; `rg -ni 'BEGIN (RSA |OPENSSH )?PRIVATE' docs scripts` → no matches.

## Test plan

| Case | Expected |
|------|----------|
| unsigned bundle | `.app` runs on Operator Mac |
| cask build flag | updater does not schedule checks |
| appcast script dry-run | writes XML for a local DMG |
| signed+notarised | **only** when Appendix A credentials exist — otherwise document BLOCKED and still complete script/doc work |

## Done criteria

- [ ] Unsigned `Daku` app bundle path documented and buildable
- [ ] Sparkle path adapted; cask builds disable Sparkle
- [ ] Homebrew cask draft + install docs
- [ ] No secrets in repo
- [ ] `plans/README.md` row 010 → `done` (note BLOCKED-notarisation if Operator lacks Apple ID — scripts/docs can still be done)

## STOP conditions

- No Apple Developer ID / notary profile on the machine **and** the task asks to publish a notarised DMG — stop; finish unsigned + docs; run Appendix A wizard for the human; do not fake notarisation.
- Sparkle private key missing — stop before uploading a signed appcast that clients cannot verify; unsigned local DMG is still OK.
- Pressure to commit key material “just for CI” — refuse.
- Replacing Sparkle with a different updater without an ADR — stop.

## Maintenance notes

- GitHub Actions notarisation later may use OIDC/API key secrets — configure in repo settings, never in plans.
- Reviewers: confirm cask and Sparkle cannot both auto-update the same install.
- Corresponding source for each release tag must be the public git tag (GPL).

---

## Appendix A — Operator wizard checklist (human-only)

Do **not** paste secret values into issues, chat, or the repo. Mark each step done locally.

1. Enrol / confirm **Apple Developer Program** membership for the signing identity you will use.
2. Create **Developer ID Application** certificate in Xcode or developer.apple.com; install in login keychain.
3. Create an **app-specific password** or **App Store Connect API key** for notarytool; store in Keychain or CI secrets — never in git.
4. Run `xcrun notarytool store-credentials` (or current Apple-recommended equivalent) to save a local notary profile name; record only the **profile name** in your private notes.
5. Generate Sparkle Ed25519 keypair with Sparkle’s `generate_keys` tool; store **private** key in release secrets; commit only the **public** key where the waku/daku scripts already expect it (if the fork commits a public key file).
6. Decide public **appcast URL** and GitHub Releases naming (`Daku-x.y.z.dmg`).
7. Cut a test notarised build once; verify Gatekeeper opens cleanly on a second Mac.
8. Publish Homebrew cask pointing at that Release; confirm Sparkle is disabled in that build.
