#!/usr/bin/env bun
// Guards the tsc footgun documented in CLAUDE.md: `tsc --noEmit` only checks
// the `include` roots in tsconfig.json, so a new `.ts` file anywhere else
// passes `bun run check` green while completely un-typechecked. This script
// fails when any tracked `.ts` file falls outside those roots.

import { $ } from "bun";

const text = await $`git ls-files '*.ts'`.text();
const tracked = text
  .split("\n")
  .map((line) => line.trim())
  .filter((line) => line.length > 0);

const covered = (file: string): boolean => {
  if (file.includes("node_modules/") || file.startsWith("target/") || file.startsWith("dist/")) {
    return true; // generated or vendored, never ours to check
  }
  return (
    file.startsWith("scripts/") ||
    file.startsWith("tools/") ||
    file.startsWith("db/") ||
    /^[^/]*\.config\.ts$/.test(file)
  );
};

const uncovered = tracked.filter((file) => !covered(file));
if (uncovered.length > 0) {
  console.error(
    `tsconfig.json covers scripts/**, tools/**, db/**, *.config.ts — move or include:\n${uncovered.join("\n")}`,
  );
  process.exit(1);
}
