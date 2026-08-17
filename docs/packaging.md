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

After a notarised build:

```sh
bun run release --signing-identity "Developer ID Application: …"
bun scripts/appcast.ts dist/updates
```

Attach `dist/Daku-x.y.z.dmg` and `dist/appcast.xml` to the GitHub Release.
Appcast signing needs the Sparkle private key in the login keychain or
`SPARKLE_PRIVATE_KEY` (CI / release secrets only). Put the matching public
key in `resources/Info.plist` as `SUPublicEDKey` — public half only.

Homebrew / cask builds must not also auto-update via Sparkle. Compile that
artifact with `DAKU_CHANNEL=homebrew` (passes `--features channel-homebrew`,
skips embedding Sparkle, writes `dist/Daku-x.y.z-homebrew.dmg`). Runtime
`DAKU_CHANNEL=homebrew` also no-ops a Sparkle-enabled binary.

## Homebrew cask (alternate)

Draft: [`homebrew/daku.rb`](../homebrew/daku.rb). `auto_updates false`.
Publish only the `Daku-x.y.z-homebrew.dmg` artifact — never the Sparkle DMG.

## Appendix A — Operator checklist (human-only)

No secret values in issues, chat, or the repo.

1. Apple Developer Program membership.
2. Developer ID Application certificate in the login keychain.
3. Notary credentials via `notarytool store-credentials` — store the
   profile **name** only in private notes (`DAKU_NOTARY_PROFILE`).
4. Sparkle Ed25519 keypair (`generate_keys` from the Sparkle `bin/`
   cache). Private key in release secrets / keychain; public key only in
   `Info.plist`.
5. Appcast URL + GitHub Release asset name `Daku-x.y.z.dmg`.
6. Test a notarised open on a second Mac; publish the cask with Sparkle
   disabled.

Do not run notarisation or publish an appcast without the credentials
above. Unsigned local bundles are fine without them.
