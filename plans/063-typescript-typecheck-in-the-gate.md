# Plan 063: The TypeScript in this repo is type-checked, and the lint plugin is linted

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- package.json oxlint.config.ts scripts tools db drizzle.config.ts`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: `plans/052-release-integrity-cask-checksum-and-appcast.md`
  (052 edits `scripts/release.ts`; land it first so this checks the final shape)
- **Category**: dx
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

`bun run check` runs `cargo fmt`, `cargo clippy -D warnings`, `cargo test` and
`oxlint`. Nothing type-checks the TypeScript. There is no `tsconfig.json` and no
`typescript` devDependency; `@types/bun` is installed and consumed by nothing.

That would be defensible if the TypeScript were incidental. It is not:
`scripts/release.ts` is the entire release automation, it is macOS-bound and
manual, and it parses two external JSON shapes with hand-written `as` casts —
cargo metadata and `notarytool --output-format json`. The `// SAFETY:` comments
are currently the **whole** guarantee that those shapes hold. Bun strips types
at runtime, so a wrong one surfaces as a crash mid-release, after the bundle is
built and signed.

There is a second, sharper irony. `oxlint.config.ts` enables fifteen rules that
are *all* type-hygiene rules — `no-unknown-returns`, `no-chained-type-assertions`,
`require-safety-comment-for-type-assertion` — enforced with no type checker
behind them. And `ignorePatterns` contains `tools/oxlint/anti-slop/**`, so the
plugin's own seventeen TypeScript files are linted by nothing and type-checked
by nothing.

## Current state

**`package.json:5-19`**:

```json
  "scripts": {
    "dev": "bun ./scripts/dev.ts",
    "release": "bun ./scripts/release.ts",
    "db:generate": "drizzle-kit generate",
    "lint": "oxlint -c oxlint.config.ts .",
    "check": "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast && oxlint -c oxlint.config.ts ."
  },
  "devDependencies": {
    "@oxlint/plugins": "1.78.0",
    "@types/bun": "1.3.14",
    "drizzle-kit": "^0.31.10",
    "drizzle-orm": "^0.45.2",
    "oxlint": "1.78.0"
  }
```

No `typescript`. No `tsconfig.json` anywhere in the repo.

**`oxlint.config.ts:4-20`** — the plugin excludes itself:

```ts
  ignorePatterns: [
    ".agent/**",
    ...
    "tools/oxlint/anti-slop/**",
    "node_modules/**",
    "target/**",
    "dist/**",
  ],
```

**`oxlint.config.ts:22-38`** — fifteen type-hygiene rules with no checker:

```ts
  rules: {
    "anti-slop/no-chained-type-assertions": "error",
    ...
    "anti-slop/require-safety-comment-for-type-assertion": "error",
  },
```

**`scripts/release.ts:83-85` and `:127-131`** — the two casts:

```ts
  // SAFETY: notarytool `--output-format json` documents `id`/`message`/`status`; all are read as optional.
  const result = JSON.parse(resultText) as {
    id?: string;
    message?: string;
    status?: string;
  };
```

**The TypeScript surface**: `scripts/*.ts` (4 files), `tools/oxlint/anti-slop/**`
(17 files), `db/schema.ts`, `drizzle.config.ts`, `oxlint.config.ts`.

### Constraints you must honor

- **`docs/agents/git-workflow.md`**: no CI. `bun run check` is the gate, so
  whatever you add must be fast and deterministic.
- `plans/README.md` records "Bun test harness for `scripts/*.ts`" as considered
  and rejected. This plan adds a **type checker**, not a test framework — do not
  drift into the latter.
- ADR-0007 keeps the drizzle → SQL → rusqlite pipeline, so `db/schema.ts` and
  `drizzle.config.ts` stay and must type-check.
- Bun is the runtime (`"type": "module"`). Use `types: ["bun"]` and
  `moduleResolution: "bundler"`; do not add a build step — nothing is compiled,
  `tsc` runs with `noEmit`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Install | `bun install` | exit 0 |
| Typecheck | `bun run typecheck` | exit 0, no errors |
| Lint | `bun run lint` | exit 0 |
| Full gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `package.json`
- `tsconfig.json` (create)
- `oxlint.config.ts`
- Type-error fixes in `scripts/**`, `tools/oxlint/anti-slop/**`, `db/schema.ts`,
  `drizzle.config.ts`

**Out of scope** (do NOT touch):
- Any `.rs` file.
- The behaviour of any script. If a type error reveals a genuine bug, **report
  it**; fix the type, not the logic, unless the two are the same edit.
- Adding a test framework or a build step.
- The fifteen anti-slop rules' configuration (they stay `error`).
- `.agents/**`, `.claude/**` and the other agent directories in
  `ignorePatterns` — those stay ignored.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Type-check the TypeScript and stop excluding the lint plugin from linting (#88).`

## Steps

### Step 1: Add TypeScript and a config

```sh
bun add -d typescript
```

Create `tsconfig.json` covering every `.ts` file in the repo:

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "types": ["bun"],
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "allowImportingTsExtensions": true,
    "verbatimModuleSyntax": true
  },
  "include": ["scripts/**/*.ts", "tools/**/*.ts", "db/**/*.ts", "*.config.ts"],
  "exclude": ["node_modules", "target", "dist", ".daku-cache"]
}
```

Add a `typecheck` script (`tsc --noEmit`) to `package.json`.

**Verify**: `bun run typecheck` → runs. **Expect errors on the first run** —
that is the point. Record how many and in which files before fixing anything.

### Step 2: Fix the type errors

Work file by file. The likely clusters:

- `tools/oxlint/anti-slop/**` — never type-checked before, so most errors will
  be here. The `@oxlint/plugins` types may need explicit imports.
- `scripts/release.ts` / `scripts/appcast.ts` — Bun `$` shell types, and the two
  `as` casts (which are already narrow and documented; they should survive).

**Fix types, not behaviour.** If a type error is telling you a real bug —
something that would genuinely crash — write it in your report and fix it, but
say so explicitly rather than folding it into a "type fix" commit.

If `tools/oxlint/anti-slop/**` turns out to need substantial rework to
type-check, **narrow the `include`** to `scripts/**`, `db/**` and `*.config.ts`
for now, report what the plugin would need, and say clearly that the plugin
remains unchecked. Shipping a checker over the release path is most of the value;
blocking on the plugin is not worth it.

**Verify**: `bun run typecheck` → exit 0.

### Step 3: Stop excluding the plugin from linting

Remove `"tools/oxlint/anti-slop/**"` from `oxlint.config.ts`'s `ignorePatterns`.

**Verify**: `bun run lint` → exit 0. If the plugin's own files violate the
anti-slop rules, fix them; if a rule genuinely cannot apply to plugin source
(e.g. a rule about runtime `typeof` in a file whose job is inspecting types),
add a **scoped** override with a comment saying why — never a blanket
re-exclusion.

### Step 4: Put it in the gate

Add `tsc --noEmit` to the `check` script, after `oxlint`:

```json
"check": "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast && oxlint -c oxlint.config.ts . && tsc --noEmit"
```

Then update `CLAUDE.md` and `AGENTS.md`'s "Verification gate" section to
describe what `bun run check` now runs. **They are byte-identical files — make
the same edit in both and verify with `diff`.**

**Verify**: `bun run check` → exit 0. `diff CLAUDE.md AGENTS.md` → no output.

## Test plan

No unit tests — a type checker is the test here. Verification is:

1. `bun run typecheck` exits 0 on a clean tree.
2. It **catches** a deliberate error: temporarily add `const x: number = "no";`
   to `scripts/release.ts`, confirm `bun run check` fails, then remove it.
   Record this in your report — a checker nobody proved catches anything is not
   worth its entry in the gate.
3. `bun run check` exits 0 after removal.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0 and its definition ends with `tsc --noEmit`
- [ ] `ls tsconfig.json` → exists
- [ ] `grep -n '"typescript"' package.json` → present in `devDependencies`
- [ ] `grep -n "anti-slop/\*\*" oxlint.config.ts` → no matches in
      `ignorePatterns` (or your report explains the scoped exception)
- [ ] `bun run typecheck` exits 0
- [ ] Your report records the deliberate-error check from the test plan, and the
      first-run error count from Step 1
- [ ] `diff CLAUDE.md AGENTS.md` → no output
- [ ] `git diff --name-only` contains no `.rs` file
- [ ] `plans/README.md` status row for 063 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- `bun add -d typescript` cannot run (no network).
- A type error reveals a **behavioural** bug in a script. Report it; do not
  quietly change release behaviour inside a typing commit.
- The first `tsc --noEmit` run produces more errors than you can fix without
  rewriting a file's design — narrow the `include` per Step 2 and report.
- `tsc --noEmit` adds more than a few seconds to `bun run check`. The gate is
  run before every commit; report the timing rather than accepting a slow gate.
- Removing the plugin's ignore entry produces violations you can only silence
  with a blanket disable.

## Maintenance notes

- The fifteen anti-slop rules now have a checker behind them. That is the point:
  `require-safety-comment-for-type-assertion` means something once `tsc` can see
  the assertion.
- New `.ts` files must land inside `tsconfig.json`'s `include`. A file outside it
  is silently unchecked — that is exactly how `tools/oxlint/anti-slop/**` ended
  up excluded from both tools.
- If Step 2 narrowed the `include`, the plugin's files are still unchecked.
  Record that in `plans/README.md`'s status note so it is not mistaken for done.
- Deliberately **not** added: a test framework for `scripts/*.ts`. Release is
  manual and macOS-bound; `plans/README.md` records that as settled.
