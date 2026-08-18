#!/usr/bin/env bun
//
// Sign update archives and (re)generate the Sparkle appcast for a directory.
//
// Usage:
//   bun scripts/appcast.ts <updates-dir>
//
// <updates-dir> holds the packaged archives (e.g. Daku-0.1.0.zip) plus any
// older archives so Sparkle can build binary deltas. appcast.xml is written
// into that directory. The private EdDSA key is read from SPARKLE_PRIVATE_KEY
// when set, otherwise from the login keychain (see docs/packaging.md).
//
// Env overrides:
//   SPARKLE_BIN                dir containing the Sparkle tools
//   SPARKLE_PRIVATE_KEY        EdDSA private key (CI; otherwise the keychain)
//   DAKU_DOWNLOAD_URL_PREFIX   base URL for enclosure links
import { $ } from "bun";
import { existsSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";

const projectRoot = resolve(import.meta.dir, "..");

export const defaultDownloadUrlPrefix =
  "https://github.com/martinthommesen/daku/releases/latest/download/";

/** Locate Sparkle's `generate_appcast`: SPARKLE_BIN first, then the pinned
 *  distribution scripts/bundle.sh caches under .daku-cache, then PATH. */
export function findGenerateAppcast(): string | null {
  const fromEnv = process.env.SPARKLE_BIN;
  if (fromEnv) {
    const candidate = join(fromEnv, "generate_appcast");
    if (existsSync(candidate)) return candidate;
  }

  const cacheRoot = join(projectRoot, ".daku-cache", "sparkle");
  if (existsSync(cacheRoot)) {
    const versionOrder = new Intl.Collator("en", { numeric: true });
    const versions = readdirSync(cacheRoot)
      .filter((name) => !name.startsWith("."))
      .sort((a, b) => versionOrder.compare(b, a));
    for (const version of versions) {
      const candidate = join(cacheRoot, version, "bin", "generate_appcast");
      if (existsSync(candidate)) return candidate;
    }
  }

  return Bun.which("generate_appcast");
}

/** Sign the archives in `updatesDir` and (re)write appcast.xml. */
export async function generateAppcast(
  updatesDir: string,
  downloadUrlPrefix: string,
): Promise<void> {
  const generator = findGenerateAppcast();
  if (!generator) {
    throw new Error(
      "generate_appcast not found. Run scripts/bundle.sh once to populate " +
        ".daku-cache/sparkle, or set SPARKLE_BIN to a Sparkle tools bin/ dir.",
    );
  }
  console.log(`Using: ${generator}`);
  const privateKey = process.env.SPARKLE_PRIVATE_KEY?.trim();
  const command = [
    generator,
    "--download-url-prefix",
    downloadUrlPrefix,
    "--release-notes-url-prefix",
    downloadUrlPrefix,
    ...(privateKey ? ["--ed-key-file", "-"] : []),
    updatesDir,
  ];
  if (privateKey) {
    // Bun's shell has no `.stdin()` method; redirect from a Buffer instead.
    await $`${command} < ${Buffer.from(privateKey)}`;
  } else {
    await $`${command}`;
  }
  console.log(`Wrote ${join(updatesDir, "appcast.xml")}`);
}

if (import.meta.main) {
  const updatesDir = process.argv[2];
  if (!updatesDir) {
    console.error("usage: bun scripts/appcast.ts <updates-dir>");
    process.exit(1);
  }
  const prefix =
    process.env.DAKU_DOWNLOAD_URL_PREFIX ?? defaultDownloadUrlPrefix;
  await generateAppcast(updatesDir, prefix);
}
