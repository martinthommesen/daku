# Plan 015: Make the Sparkle release path actually deliver updates (bundle version, checksum abort, DMG path, checklist)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- scripts/bundle.sh scripts/release.ts docs/packaging.md resources/Info.plist`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate only)
- **Category**: bug / security
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/38

## Why this matters

ADR-0006 makes Sparkle the **primary** update channel. Four independent defects in the release scripts mean it cannot work as-is, and nobody would notice until a second release ships:

1. **`CFBundleVersion` is the constant `1`.** `bundle.sh` rewrites only `CFBundleShortVersionString`; `generate_appcast` derives `sparkle:version` from `CFBundleVersion`, and Sparkle compares that. Every release advertises version `1`, so an installed 0.1.0 reports "up to date" against 0.2.0 forever.
2. **The Sparkle archive checksum is decorative.** `ensure_sparkle` is only invoked as `if ! ensure_sparkle; then`, and POSIX shells disable `set -e` inside a function called in an `if` condition. A failing `shasum -c` is followed by `tar`, `mv` into the cache, `sparkle_ok=1`, and the framework is embedded and code-signed into the release — and cached for every future build. The pin exists to protect the component that installs future updates; make it abort.
3. **`release.ts --output` is accepted but ignored**, and `DAKU_CHANNEL=homebrew` makes `bundle.sh` write `dist/Daku-<v>-homebrew.dmg` while `release.ts` notarises/staples/reports `dist/Daku-<v>.dmg`. Notarisation fails on a missing file or the script reports success for a DMG that does not exist at the printed path.
4. **The human checklist omits the ZIP enclosure.** `release.ts` builds `dist/Daku-<v>.zip`, feeds it to `generate_appcast`, and the appcast's enclosure URL points at `<prefix>/Daku-<v>.zip` — but the help text and `docs/packaging.md` say to attach only the DMG + `appcast.xml`. Sparkle would 404 on download.

All fixes are a few lines each; no Developer ID is needed to verify 1–3 locally with `--unsigned`.

## Current state

### `resources/Info.plist:21-24`

```xml
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
```

### `scripts/bundle.sh`

Line 3: `set -eu`. Version comes from Cargo.toml (`:133`): `version=$(awk -F\" '/^version = / { print $2; exit }' Cargo.toml)`.

```sh
# :144-163
ensure_sparkle() {
  if [ -d "$sparkle_framework_source" ]; then
    sparkle_ok=1
    return 0
  fi
  sparkle_staging="$sparkle_cache_root/.staging-$sparkle_version-$$"
  rm -rf "$sparkle_staging"
  mkdir -p "$sparkle_staging"
  sparkle_archive="$sparkle_staging/Sparkle-$sparkle_version.tar.xz"
  if ! curl -fsSL --retry 3 -o "$sparkle_archive" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$sparkle_version/Sparkle-$sparkle_version.tar.xz"; then
    rm -rf "$sparkle_staging"
    return 1
  fi
  echo "$sparkle_sha256  $sparkle_archive" | shasum -a 256 -c - >/dev/null
  tar -xJf "$sparkle_archive" -C "$sparkle_staging" ./Sparkle.framework ./bin
  rm "$sparkle_archive"
  mv "$sparkle_staging" "$sparkle_cache_entry"
  sparkle_ok=1
}

# :165-174
if [ "$profile" = "release" ] && [ "$homebrew_channel" = "0" ]; then
  if ! ensure_sparkle; then
    if [ "$unsigned" = "1" ]; then
      echo "warning: Sparkle download failed; unsigned bundle will omit the framework" >&2
    else
      echo "error: Sparkle $sparkle_version is required for a signed release" >&2
      exit 1
    fi
  fi
fi

# :186-190
plutil -replace CFBundleDisplayName -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleExecutable -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$bundle_identifier" "$contents/Info.plist"
plutil -replace CFBundleName -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"

# :252-260
  if [ "$homebrew_channel" = "1" ]; then
    dmg_path="dist/Daku-$version-homebrew.dmg"
  else
    dmg_path="dist/Daku-$version.dmg"
  fi
  rm -f "$dmg_path"
  hdiutil create -volname Daku -srcfolder "$dmg_staging" -ov -format UDZO "$dmg_path" >/dev/null
  rm -rf "$dmg_staging"
  echo "$dmg_path"
```

### `scripts/release.ts`

```ts
// :14-20 (help text)
const help = `Build a production Daku.app / DMG and (optionally) notarize it.
…
Does not upload. Attach dist/Daku-<version>.dmg and dist/appcast.xml to a
GitHub Release. …
// :26  --output <path>               DMG output path (default: dist/Daku-<version>.dmg)
// :42  output: { type: "string", short: "o" },     (parseArgs option)
// :98-104
const version = cargoPackage.version;
const dmgName = `${appName}-${version}.dmg`;
const zipName = `${appName}-${version}.zip`;
const outputPath = resolve(projectRoot, values.output ?? join("dist", dmgName));
if (extname(outputPath).toLowerCase() !== ".dmg") {
  throw new Error(`Output path must end in .dmg: ${outputPath}`);
}
// :113-117  bundle.sh is invoked WITHOUT any output argument
// :126  await $`xcrun notarytool submit ${outputPath} …`
// :141  await $`xcrun stapler staple -v ${outputPath}`;
// :174-177
console.log(`\nApp ready: ${appBundle}`);
console.log(`DMG ready: ${outputPath}`);
console.log(`ZIP ready: ${zipPath}`);
console.log("Upload the DMG + appcast.xml to a GitHub Release when notarised.");
```

`extname` is imported from `node:path` at `:5` (`dirname, extname, join, resolve`). `scripts/appcast.ts:64-76` runs `generate_appcast --download-url-prefix <prefix> … <updatesDir>` over the ZIP.

### `docs/packaging.md`

Line 37: `Attach \`dist/Daku-x.y.z.dmg\` and \`dist/appcast.xml\` to the GitHub Release.` Line 63 (Appendix A item 5): `Appcast URL + GitHub Release asset name \`Daku-x.y.z.dmg\`.`

Conventions: POSIX `sh` (not bash) in `bundle.sh` — no arrays, no `[[ ]]`; Bun scripts use `$` from `bun`; oxlint `anti-slop` rules apply to `.ts` (run `bun run lint`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Shell syntax | `sh -n scripts/bundle.sh` | exit 0 |
| Unsigned bundle (builds release binaries; slow first time) | `./scripts/bundle.sh --unsigned` | prints `dist/Daku-<v>.dmg`, `…/Daku.app`, `dist/Daku.app` |
| Skip rebuild on repeat runs | `DAKU_SKIP_CARGO_BUILD=1 ./scripts/bundle.sh --unsigned` | same |
| Read bundle version | `plutil -extract CFBundleVersion raw dist/Daku.app/Contents/Info.plist` | equals Cargo.toml version |
| Release script (unsigned) | `bun run release --unsigned` | exits 0; prints DMG/ZIP paths that exist |
| Lint | `bun run lint` | exit 0 |
| Gate | `bun run check` | exit 0 |

Note: `bundle.sh --unsigned` downloads Sparkle 2.9.4 into `.daku-cache/sparkle/` on first run (network). If offline, it warns and omits the framework — that is fine for verifying steps 1 and 3.

## Scope

**In scope**:
- `scripts/bundle.sh`
- `scripts/release.ts`
- `docs/packaging.md`
- `plans/README.md` (status row)

**Out of scope**:
- `resources/Info.plist` — leave the placeholder `1`; the build rewrites it. (Do not add `SUPublicEDKey`; that is a human step with a real key.)
- `homebrew/daku.rb` — explicit draft; its `version`/`sha256 :no_check` are filled at cask publish time.
- `scripts/appcast.ts`, `src/updater.rs` — no change.
- Signing/notarisation logic itself.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Fix release pipeline: bundle version, Sparkle checksum abort, DMG path, ZIP in checklist.`

## Steps

### Step 1: Write a real `CFBundleVersion`

In `scripts/bundle.sh`, after the `CFBundleShortVersionString` line (`:190`), add:

```sh
# Sparkle compares CFBundleVersion (sparkle:version). Keep it equal to the
# semver so each release is strictly newer than the last.
plutil -replace CFBundleVersion -string "$version" "$contents/Info.plist"
```

**Verify**: `sh -n scripts/bundle.sh` → exit 0. Then `./scripts/bundle.sh --unsigned` (or with `DAKU_SKIP_CARGO_BUILD=1` if the release binaries already exist) and `plutil -extract CFBundleVersion raw dist/Daku.app/Contents/Info.plist` → prints the Cargo.toml version (e.g. `0.1.0`), not `1`.

### Step 2: Abort on a bad Sparkle checksum

Replace the four lines after the `curl` block in `ensure_sparkle` (`shasum` … `sparkle_ok=1`) with:

```sh
  if ! echo "$sparkle_sha256  $sparkle_archive" | shasum -a 256 -c - >/dev/null; then
    echo "error: Sparkle $sparkle_version archive checksum mismatch" >&2
    rm -rf "$sparkle_staging"
    return 1
  fi
  if ! tar -xJf "$sparkle_archive" -C "$sparkle_staging" ./Sparkle.framework ./bin; then
    rm -rf "$sparkle_staging"
    return 1
  fi
  rm "$sparkle_archive"
  mv "$sparkle_staging" "$sparkle_cache_entry" || return 1
  sparkle_ok=1
```

**Verify**: `sh -n scripts/bundle.sh` → exit 0. Functional check without a network hit: temporarily (in your shell only, not the file) run
`rm -rf .daku-cache/sparkle && sed 's/^sparkle_sha256=.*/sparkle_sha256="0000000000000000000000000000000000000000000000000000000000000000"/' scripts/bundle.sh > /tmp/daku-bundle-badsha.sh && DAKU_SKIP_CARGO_BUILD=1 sh /tmp/daku-bundle-badsha.sh --unsigned; echo "exit=$?"; ls .daku-cache/sparkle`
→ stderr contains `checksum mismatch`, then the existing `warning: Sparkle download failed; unsigned bundle will omit the framework` (unsigned path continues), and `.daku-cache/sparkle` contains **no** `2.9.4` directory. Then run the real script once more so the cache is repopulated with the correct hash. (Delete `/tmp/daku-bundle-badsha.sh` afterwards.)

### Step 3: One source of truth for the DMG path

In `scripts/release.ts`:

- Delete the `--output` option: remove the help line `:26`, the parseArgs entry `:42` (`output: { type: "string", short: "o" },`), and the `extname` import if it becomes unused.
- Replace `:98-104` with:

```ts
const version = cargoPackage.version;
const homebrewChannel = process.env.DAKU_CHANNEL === "homebrew";
// Must match scripts/bundle.sh, which owns the DMG name.
const dmgName = `${appName}-${version}${homebrewChannel ? "-homebrew" : ""}.dmg`;
const zipName = `${appName}-${version}.zip`;
const outputPath = resolve(projectRoot, "dist", dmgName);
```

- Right after the `await access(appBundle);` line (`:120`), add `await access(outputPath);` so a missing DMG fails immediately with a clear ENOENT instead of at notarisation.

**Verify**: `bun run lint` → exit 0. `bun run release --unsigned` → exits 0 and prints `DMG ready: <path>` and `ZIP ready: <path>` for files that exist (`ls -l dist/Daku-*.dmg dist/Daku-*.zip`). `bun run release --output x.dmg` → fails with an "Unknown option" error from `parseArgs` (strict mode).

### Step 4: Checklist names the ZIP

- `scripts/release.ts` help text (`:19-20`): change to `Does not upload. Attach dist/Daku-<version>.dmg, dist/Daku-<version>.zip (the Sparkle enclosure) and dist/appcast.xml to a GitHub Release.` and the final log line to `console.log("Upload the DMG, the ZIP (Sparkle enclosure) and appcast.xml to a GitHub Release when notarised.");`
- `docs/packaging.md:37`: `Attach \`dist/Daku-x.y.z.dmg\`, \`dist/Daku-x.y.z.zip\` (the Sparkle enclosure referenced by the appcast) and \`dist/appcast.xml\` to the GitHub Release.`
- `docs/packaging.md:63`: `Appcast URL + GitHub Release asset names \`Daku-x.y.z.dmg\` and \`Daku-x.y.z.zip\`.`
- Add one sentence under the "Feed URL" paragraph in `docs/packaging.md`: `\`CFBundleVersion\` is set to the Cargo version at bundle time; bump \`version\` in \`Cargo.toml\` for every release or Sparkle will not offer the update.`

**Verify**: `grep -n 'Daku-x.y.z.zip' docs/packaging.md` → 2 matches; `grep -n 'CFBundleVersion' docs/packaging.md` → 1 match.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- No unit-test harness exists for the scripts (a Bun test file for release.ts was judged not worth it yet). Verification is the manual checks in Steps 1–3 above; record their outputs in the plan's status note.
- `sh -n scripts/bundle.sh` and `bun run lint` are the syntactic gates.

## Done criteria

- [ ] `grep -n 'CFBundleVersion' scripts/bundle.sh` → 1 `plutil -replace` line
- [ ] `grep -n 'checksum mismatch' scripts/bundle.sh` → 1 match; `grep -c 'return 1' scripts/bundle.sh` increased by ≥3 vs HEAD
- [ ] `grep -n -- '--output\|values.output' scripts/release.ts` → no matches
- [ ] `grep -n 'DAKU_CHANNEL' scripts/release.ts` → 1 match; `grep -n 'await access(outputPath)' scripts/release.ts` → 1 match
- [ ] `plutil -extract CFBundleVersion raw dist/Daku.app/Contents/Info.plist` after an unsigned bundle equals the Cargo.toml version
- [ ] `grep -n 'Daku-x.y.z.zip' docs/packaging.md` → 2 matches
- [ ] `bun run check` exits 0; `sh -n scripts/bundle.sh` exits 0
- [ ] `git status` shows only in-scope files modified (`dist/`, `.daku-cache/` are gitignored)
- [ ] `plans/README.md` status row for 015 updated

## STOP conditions

- `bundle.sh` no longer has the `ensure_sparkle` function or the `plutil -replace CFBundleShortVersionString` line as excerpted.
- `release.ts` no longer computes `outputPath` as excerpted, or already passes a path into `bundle.sh`.
- `./scripts/bundle.sh --unsigned` fails for a reason unrelated to this plan (e.g. `cargo build --release` fails, Metal toolchain missing) — report; do not try to fix the build here.
- `plutil` is unavailable (not macOS) — this plan can only be verified on macOS; report.

## Maintenance notes

- If a build number ever needs to be independent of the semver (e.g. multiple builds per version), switch `CFBundleVersion` to `git rev-list --count HEAD` — but then `SUStandardVersionComparator` must still see it increase; keep it numeric-dotted.
- The Sparkle version/hash pair (`sparkle_version`, `sparkle_sha256`) must always be bumped together; the new checksum abort makes a mismatched bump fail loudly instead of silently caching.
- Reviewers: run the bad-hash check in Step 2 once; it is the only proof the abort works in `sh`.
- Deferred: `homebrew/daku.rb` `version` derivation at release time; `SUPublicEDKey`/`sha256 :no_check` are tracked in `docs/packaging.md` Appendix A.
