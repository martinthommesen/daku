#!/usr/bin/env bun

import { $ } from "bun";
import { access, mkdir, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { defaultDownloadUrlPrefix, generateAppcast } from "./appcast";

const appName = "Daku";
const packageName = "daku";
const defaultNotaryProfile = "NOTARY";
const projectRoot = resolve(import.meta.dir, "..");

const help = `Build a production Daku.app / DMG and (optionally) notarize it.

Usage:
  bun run release [options]

Does not upload. Attach dist/Daku-<version>.dmg, dist/Daku-<version>.zip
(the Sparkle enclosure) and dist/appcast.xml to a GitHub Release. Notarisation needs a Developer ID + notary profile — see
docs/packaging.md Appendix A. Missing credentials: use --unsigned or --adhoc.

Options:
  --unsigned                    skip codesign (writes dist/Daku.app)
  --adhoc                       ad-hoc sign, no notarization
  --skip-notarize               signed DMG without notary
  --signing-identity <name>     Developer ID Application identity
                                (or DAKU_CODESIGN_IDENTITY)
  --notary-profile <name>       notarytool keychain profile name only
                                (default: NOTARY; or DAKU_NOTARY_PROFILE)
  --skip-build                  reuse an already-built release binary
  --help                        show this help
`;

const { values } = parseArgs({
  args: Bun.argv.slice(2),
  options: {
    adhoc: { type: "boolean" },
    help: { type: "boolean", short: "h" },
    "notary-profile": { type: "string" },
    "signing-identity": { type: "string" },
    "skip-build": { type: "boolean" },
    "skip-notarize": { type: "boolean" },
    unsigned: { type: "boolean" },
  },
  strict: true,
});

if (values.help) {
  console.log(help);
  process.exit(0);
}

if (process.platform !== "darwin") {
  throw new Error("DMG packaging must run on macOS.");
}

const unsigned = values.unsigned ?? false;
const adhoc = values.adhoc ?? false;
const skipNotarize = Boolean(values["skip-notarize"] || unsigned);
const configuredSigningIdentity =
  values["signing-identity"] ?? process.env.DAKU_CODESIGN_IDENTITY;
const notaryProfile =
  values["notary-profile"] ??
  process.env.DAKU_NOTARY_PROFILE ??
  defaultNotaryProfile;

if (unsigned && values["signing-identity"]) {
  throw new Error("Use either --unsigned or --signing-identity, not both.");
}
if (adhoc && values["signing-identity"]) {
  throw new Error("Use either --adhoc or --signing-identity, not both.");
}
if (!unsigned && !adhoc && !configuredSigningIdentity) {
  console.warn(
    "No Developer ID. Building unsigned. See docs/packaging.md.",
  );
}

process.chdir(projectRoot);

type CargoMetadata = {
  packages: Array<{ name: string; version: string }>;
};

// SAFETY: `cargo metadata --format-version 1` is a stable schema; only `packages[].name/version` are read.
const metadata = JSON.parse(
  await $`cargo metadata --no-deps --format-version 1`.quiet().text(),
) as CargoMetadata;
const cargoPackage = metadata.packages.find(
  (candidate) => candidate.name === packageName,
);
if (!cargoPackage) {
  throw new Error(`Cargo package "${packageName}" was not found.`);
}

const version = cargoPackage.version;
const homebrewChannel = process.env.DAKU_CHANNEL === "homebrew";
// Must match scripts/bundle.sh, which owns the DMG name.
const dmgName = `${appName}-${version}${homebrewChannel ? "-homebrew" : ""}.dmg`;
const zipName = `${appName}-${version}.zip`;
const outputPath = resolve(projectRoot, "dist", dmgName);

const bundleScript = join(projectRoot, "scripts/bundle.sh");
const skipCodesign = unsigned || (!configuredSigningIdentity && !adhoc);
const skipBuild = values["skip-build"] ? "1" : "0";
const identityEnv = configuredSigningIdentity && !unsigned
  ? configuredSigningIdentity
  : "";

console.log(`\n==> Bundling ${appName} ${version}`);
if (skipCodesign) {
  await $`env DAKU_SKIP_CARGO_BUILD=${skipBuild} ${bundleScript} release --unsigned`;
} else {
  await $`env DAKU_SKIP_CARGO_BUILD=${skipBuild} DAKU_CODESIGN_IDENTITY=${identityEnv} ${bundleScript} release`;
}

const appBundle = join(projectRoot, "dist", `${appName}.app`);
await access(appBundle);
await access(outputPath);

if (!unsigned && !adhoc && !skipNotarize && configuredSigningIdentity) {
  console.log("\n==> Submitting the DMG for Apple notarization");
  const resultText =
    await $`xcrun notarytool submit ${outputPath} --keychain-profile ${notaryProfile} --wait --output-format json`
      .quiet()
      .text();
  // SAFETY: notarytool `--output-format json` documents `id`/`message`/`status`; all are read as optional.
  const result = JSON.parse(resultText) as {
    id?: string;
    message?: string;
    status?: string;
  };
  if (result.status !== "Accepted") {
    throw new Error(
      `Notarization ${result.status ?? "failed"}${result.id ? ` (${result.id})` : ""}: ` +
        (result.message ?? "inspect the submission with notarytool log"),
    );
  }
  console.log(`Notarization accepted: ${result.id ?? "unknown submission"}`);
  await $`xcrun stapler staple -v ${outputPath}`;
  await $`xcrun stapler staple -v ${appBundle}`;
} else {
  console.warn(
    "\nSkipped notarisation (unsigned/adhoc/no profile). Do not publish this DMG.",
  );
}

const zipPath = resolve(projectRoot, "dist", zipName);
await mkdir(dirname(zipPath), { recursive: true });
await $`ditto -c -k --keepParent ${appBundle} ${zipPath}`;

// Symbols for crash symbolication; the shipped binaries stay stripped.
const symbolsDirectory = resolve(projectRoot, "dist", `${appName}-${version}-dSYM`);
await rm(symbolsDirectory, { force: true, recursive: true });
await mkdir(symbolsDirectory, { recursive: true });
for (const binary of [packageName, `${packageName}-daemon`]) {
  const symbols = join(projectRoot, "target", "release", `${binary}.dSYM`);
  try {
    await access(symbols);
  } catch {
    throw new Error(
      `Missing ${symbols} — build the release profile before packaging.`,
    );
  }
  await $`ditto ${symbols} ${join(symbolsDirectory, `${binary}.dSYM`)}`;
}
const symbolsZip = `${symbolsDirectory}.zip`;
await $`ditto -c -k --keepParent ${symbolsDirectory} ${symbolsZip}`;
await rm(symbolsDirectory, { force: true, recursive: true });

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
console.log(`DMG ready: ${outputPath}`);
console.log(`ZIP ready: ${zipPath}`);
console.log(`Symbols ready: ${symbolsZip}`);
console.log(
  "Upload the DMG, the ZIP (Sparkle enclosure) and appcast.xml to a GitHub Release when notarised.",
);
