# Packaging

Primary channel: notarised `Daku.app` / DMG + Sparkle. Alternate: Homebrew
cask (`homebrew/daku.rb`) — `brew` upgrades that install; Sparkle is a no-op.

Secrets (Developer ID, notary password, Sparkle private key) never go in git.

## Unsigned local bundle

```sh
./scripts/bundle.sh --unsigned
```

Writes `dist/Daku.app` (and `dist/Daku-<version>.dmg`) without codesign.
`--help` lists flags. `SKIP_CODESIGN=1 ./scripts/bundle.sh release` is the
same skip.

Debug.app for `bun run dev` is unchanged: `./scripts/bundle.sh debug`.

## Sparkle (primary)

`scripts/bundle.sh release` embeds Sparkle.framework from a pinned
2.9.4 archive cached in `.daku-cache/sparkle/`. `src/updater.rs` loads it
at runtime.

Feed URL (in `resources/Info.plist`):

`https://github.com/martinthommesen/daku/releases/latest/download/appcast.xml`

`CFBundleVersion` is set to the Cargo version at bundle time; bump `version`
in `Cargo.toml` for every release or Sparkle will not offer the update.

After a notarised build:

```sh
bun run release --signing-identity "Developer ID Application: …"
bun scripts/appcast.ts dist/updates
```

Attach `dist/Daku-x.y.z.dmg`, `dist/Daku-x.y.z.zip` (the Sparkle enclosure
referenced by the appcast) and `dist/appcast.xml` to the GitHub Release.
Appcast signing needs the Sparkle private key in the login keychain or
`SPARKLE_PRIVATE_KEY` (CI / release secrets only). Put the matching public
key in `resources/Info.plist` as `SUPublicEDKey` — public half only.

Keep `dist/Daku-x.y.z-dSYM.zip` with the release (crash symbolication); it is
not a Sparkle asset.

Homebrew / cask builds must not also auto-update via Sparkle. Compile that
artifact with `DAKU_CHANNEL=homebrew` (passes `--features channel-homebrew`,
skips embedding Sparkle, writes `dist/Daku-x.y.z-homebrew.dmg`). Runtime
`DAKU_CHANNEL=homebrew` also no-ops a Sparkle-enabled binary.

## Release-time environment variables

| Variable | Read by | Effect |
|----------|---------|--------|
| `DAKU_SKIP_CARGO_BUILD` | `scripts/bundle.sh`, `scripts/release.ts` | `=1` reuses the existing `target/release` binaries. |
| `SKIP_CODESIGN` | `scripts/bundle.sh` | `=1` is the same as `--unsigned`. |
| `DAKU_CODESIGN_IDENTITY` | `scripts/bundle.sh`, `scripts/release.ts` | Developer ID Application identity — **local keychain / release secrets only, never commit**. |
| `DAKU_NOTARY_PROFILE` | `scripts/release.ts` | notarytool keychain profile **name** (default `NOTARY`). |
| `DAKU_DOWNLOAD_URL_PREFIX` | `scripts/appcast.ts`, `scripts/release.ts` | Base URL for appcast enclosure links. |
| `SPARKLE_BIN` | `scripts/appcast.ts`, `scripts/release.ts` | Directory holding the Sparkle tools (`generate_appcast`). |
| `SPARKLE_PRIVATE_KEY` | `scripts/appcast.ts` | EdDSA appcast signing key — **release secrets / keychain only, never commit**. |
| `DAKU_CHANNEL` | `scripts/bundle.sh`, `homebrew/daku.rb` | `homebrew` builds the cask artifact with Sparkle left out. |

## Homebrew cask (alternate)

Draft: [`homebrew/daku.rb`](../homebrew/daku.rb). `auto_updates false`.
Publish only the `Daku-x.y.z-homebrew.dmg` artifact — never the Sparkle DMG.

```sh
DAKU_CHANNEL=homebrew bun run release --signing-identity "Developer ID Application: …"
```

prints `Cask sha256: <digest>`; put that digest and the matching `version`
into the cask before publishing (Appendix A item 7). `--unsigned` is for
local builds only, never for the published cask.

## Appendix A — Operator checklist (human-only)

No secret values in issues, chat, or the repo.

1. Apple Developer Program membership.
2. Developer ID Application certificate in the login keychain.
3. Notary credentials via `notarytool store-credentials` — store the
   profile **name** only in private notes (`DAKU_NOTARY_PROFILE`).
4. Sparkle Ed25519 keypair (`generate_keys` from the Sparkle `bin/`
   cache). Private key in release secrets / keychain; public key only in
   `Info.plist`.
5. Appcast URL + GitHub Release asset names `Daku-x.y.z.dmg` and
   `Daku-x.y.z.zip`.
6. Test a notarised open on a second Mac; publish the cask with Sparkle
   disabled.
7. Cask integrity: sign and notarise `Daku-x.y.z-homebrew.dmg` exactly like
   the Sparkle DMG (never publish an `--unsigned` build), take the digest
   from the `Cask sha256:` line printed by
   `DAKU_CHANNEL=homebrew bun run release`, and update `homebrew/daku.rb`'s
   `version` and `sha256` **together** in the same commit. A version bump
   without a fresh digest is the failure to catch in review.

Do not run notarisation or publish an appcast without the credentials
above. Unsigned local bundles are fine without them.
