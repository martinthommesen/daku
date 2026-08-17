# Plan 036: Trim the Bun DB tooling and key applied migrations on their numeric prefix

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- package.json bun.lock drizzle.config.ts db/ crates/daku-core/build.rs crates/daku-core/src/persistence.rs crates/daku-core/README.md`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate)
- **Category**: migration / dx
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/59

## Why this matters

1. **Bun devDependencies nobody uses.** `@libsql/client` (12 native `libsql` binaries for 8 platforms in `bun.lock`) exists only so `bun run db:push` can push the drizzle schema into `./temp/app.db` — a file the Rust runner never reads (Rust applies the SQL itself; `build.rs` embeds `db/migrations/*.sql`). `typescript ^7.0.2` is installed but nothing runs `tsc` (no `tsconfig.json`; Bun executes TS natively; oxlint has its own parser and declares no `typescript` peer). Slower `bun install`, more supply chain, and a `db:push` script that suggests the DB is managed from TypeScript. `@types/bun: "latest"` is the only unpinned dep.
2. **Migration identity is drizzle's random tag.** The `migrations` table is keyed on `tag` = the filename stem (`0000_naive_bulldozer`). The routine early-project move "delete migration 0000, edit `db/schema.ts`, `bun run db:generate`" yields `0000_<new-random-name>`; on any Operator DB that already has the tables, the daemon runs `CREATE TABLE signal_samples` again and `StateStore::open()` fails at startup — the whole daemon refuses to start. Keying applied migrations on the numeric prefix (which `build.rs` already enforces as the apply order) removes that trap in one SQL predicate; a written rule ("never regenerate a shipped migration, only append") covers the content-change case.

ADR-0007 keeps the drizzle → SQL → rusqlite pipeline; this plan does not remove drizzle.

## Current state

`package.json` (whole file at HEAD):

```json
{
  "name": "daku",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "bun ./scripts/dev.ts",
    "release": "bun ./scripts/release.ts",
    "db:generate": "drizzle-kit generate",
    "db:push": "drizzle-kit push",
    "lint": "oxlint -c oxlint.config.ts ."
  },
  "devDependencies": {
    "@libsql/client": "^0.17.4",
    "@oxlint/plugins": "1.78.0",
    "@types/bun": "latest",
    "drizzle-kit": "^0.31.10",
    "drizzle-orm": "^0.45.2",
    "oxlint": "1.78.0",
    "typescript": "^7.0.2"
  }
}
```

(Plan 011 adds a `"check"` script — keep it.) `bun.lock:145` resolves `@types/bun` to `1.3.14`. `drizzle-orm` lists `@libsql/client` and `bun-types` only as **optional** peers; `drizzle-kit`'s deps are `@drizzle-team/brocli`, `@esbuild-kit/esm-loader`, `esbuild`, `tsx` — `generate` never opens a database.

`drizzle.config.ts` (whole file):

```ts
import { defineConfig } from "drizzle-kit";

export default defineConfig({
  dialect: "sqlite",
  schema: "./db/schema.ts",
  out: "./db/migrations",
  // The Rust runner applies these files in order; it never reads drizzle's
  // journal, so keep names stable and prefix-ordered.
  migrations: { prefix: "index" },
  dbCredentials: {
    url: './temp/app.db'
  }
});
```

`db/schema.ts:1-11` is a doc comment explaining that drizzle is build-time only; two tables (`signal_snapshots`, `signal_samples`). `db/migrations/` holds `0000_naive_bulldozer.sql` (17 lines: two `CREATE TABLE` + one `CREATE INDEX`, separated by `--> statement-breakpoint`) and `meta/_journal.json` + `meta/0000_snapshot.json`.

`crates/daku-core/build.rs:21-60`: collects `db/migrations/*.sql`, requires a numeric prefix (`tag.split('_').next().parse::<u32>()`, panics otherwise), sorts by it, asserts the prefixes are contiguous from 0, and emits `pub static MIGRATIONS: &[(&str, &str)] = &[("0000_naive_bulldozer", include_str!(…)), …]` — tag = filename stem.

`crates/daku-core/src/persistence.rs`:

```rust
// :17-20
const MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS migrations (
         tag        TEXT PRIMARY KEY,
         applied_at INTEGER NOT NULL
     )";

// :35-66
pub fn apply_migrations(connection: &Connection) -> io::Result<usize> {
    connection.execute_batch(MIGRATIONS_TABLE).map_err(to_io_error)?;
    let mut applied = 0;
    for (tag, sql) in MIGRATIONS {
        let already_applied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM migrations WHERE tag = ?1)",
                params![tag],
                |row| row.get(0),
            )
            .map_err(to_io_error)?;
        if already_applied {
            continue;
        }
        let transaction = connection.unchecked_transaction().map_err(to_io_error)?;
        transaction
            .execute_batch(sql)
            .map_err(|error| io::Error::other(format!("migration {tag} failed: {error}")))?;
        transaction
            .execute("INSERT INTO migrations(tag, applied_at) VALUES(?1, ?2)", params![tag, unix_time() as i64])
            .map_err(to_io_error)?;
        transaction.commit().map_err(to_io_error)?;
        applied += 1;
    }
    Ok(applied)
}
```

Existing test `apply_migrations_creates_signal_tables` (`:304-316`) uses helpers `temp_db_path(label)` (`:289`) and `table_exists(&connection, name)` (`:293`) in the same `mod tests`; asserts a second `apply_migrations` returns `0`.

`crates/daku-core/README.md` (12 lines) describes the crate; no migration rule yet.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Reinstall | `bun install` | exit 0; `bun.lock` shrinks |
| Generate still works | `bun run db:generate` | exit 0; prints "No schema changes, nothing to migrate" (or similar) and creates no new file |
| Lint | `bun run lint` | exit 0 |
| Migration tests | `cargo test -p daku-core apply_migrations` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `package.json`, `bun.lock` (regenerated by `bun install`)
- `drizzle.config.ts`
- `crates/daku-core/src/persistence.rs` (`apply_migrations` predicate + one test)
- `db/schema.ts` (doc comment), `crates/daku-core/README.md` (one paragraph)
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-core/build.rs` — the numeric-prefix contract it enforces is exactly what we key on; no change.
- `db/migrations/*` — do not rename or regenerate the shipped migration.
- Removing drizzle (ADR-0007), `oxlint.config.ts`, `tools/oxlint/*`.
- `node_modules/` is gitignored; `bun install` may modify it freely.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Trim Bun DB tooling; key applied migrations on their numeric prefix.`

## Steps

### Step 1: Trim `package.json` / `drizzle.config.ts`

- Remove the `"db:push"` script.
- Remove `"@libsql/client"` and `"typescript"` from `devDependencies`.
- Pin `"@types/bun"` to the version in `bun.lock` (`"1.3.14"` at HEAD — read `bun.lock` for the current value).
- In `drizzle.config.ts` delete the `dbCredentials: { url: './temp/app.db' }` block (keep `migrations: { prefix: "index" }` and its comment).
- Run `bun install`.

**Verify**: `grep -c 'libsql\|"typescript"' package.json bun.lock` → `0` for both files; `bun run db:generate` → exit 0 and `git status --short db/` is empty (no new migration file); `bun run lint` → exit 0.

### Step 2: Key applied migrations on the numeric prefix

In `crates/daku-core/src/persistence.rs` `apply_migrations`, replace the `already_applied` query with a prefix match. Minimal change: derive the prefix once per entry and compare on it —

```rust
    for (tag, sql) in MIGRATIONS {
        // Identity is the numeric prefix build.rs enforces (`0000_…`), not
        // drizzle's random suffix, so regenerating a migration's name never
        // re-applies it on an existing database.
        let prefix = tag.split('_').next().unwrap_or(tag);
        let already_applied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM migrations WHERE substr(tag, 1, ?1) = ?2)",
                params![prefix.len() as i64, prefix],
                |row| row.get(0),
            )
            .map_err(to_io_error)?;
```

Keep inserting the full `tag` (existing DBs already hold `0000_naive_bulldozer`; the prefix predicate matches old and new rows alike). No schema change to the `migrations` table.

Add a test after `apply_migrations_creates_signal_tables` (same helpers):

```rust
    #[test]
    fn apply_migrations_matches_by_numeric_prefix() {
        let path = temp_db_path("prefix");
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        assert!(apply_migrations(&connection).unwrap() >= 1);
        // Simulate a regenerated migration name for the same index.
        connection
            .execute("UPDATE migrations SET tag = '0000_renamed_by_regeneration'", [])
            .unwrap();
        assert_eq!(apply_migrations(&connection).unwrap(), 0);
        assert!(table_exists(&connection, "signal_snapshots"));
        let _ = fs::remove_file(path);
    }
```

**Verify**: `cargo test -p daku-core apply_migrations` → 2 passed.

### Step 3: Write the rule down

- `db/schema.ts` doc comment: append the sentence `Never edit or regenerate a migration that has shipped — only append a new \`NNNN_*.sql\`; the Rust runner identifies applied migrations by the numeric prefix.`
- `crates/daku-core/README.md`: add a short "Migrations" paragraph: `SQL under \`db/migrations\` is embedded by \`build.rs\` and applied by \`persistence::apply_migrations\`, keyed on the numeric prefix. Append new files (\`bun run db:generate\`); never regenerate a shipped one.`

**Verify**: `grep -n 'never regenerate\|Never edit or regenerate' db/schema.ts crates/daku-core/README.md` → 1 match each (case-insensitive is fine).

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- New: `apply_migrations_matches_by_numeric_prefix` (Step 2), modelled on `apply_migrations_creates_signal_tables` (`persistence.rs:304`).
- Existing `apply_migrations_creates_signal_tables` still asserts idempotence.
- `bun run db:generate` no-op run is the tooling check.

## Done criteria

- [ ] `grep -n 'db:push\|libsql\|"typescript"' package.json drizzle.config.ts` → no matches; `grep -n '"@types/bun": "latest"' package.json` → no match
- [ ] `bun run db:generate` exits 0 with no new file under `db/migrations/`
- [ ] `grep -n 'substr(tag, 1' crates/daku-core/src/persistence.rs` → 1 match
- [ ] `cargo test -p daku-core apply_migrations` → 2 passed
- [ ] The rule sentence exists in `db/schema.ts` and `crates/daku-core/README.md`
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 036 updated

## STOP conditions

- `bun run db:generate` fails after removing `@libsql/client` (drizzle-kit version now requires it) — restore the dependency and report.
- `bun run lint` fails after removing `typescript` (a plugin needs it) — restore and report.
- `apply_migrations` no longer matches the excerpt (e.g. a content-hash scheme landed).
- More than one migration file exists and their prefixes are not contiguous (build.rs would already panic — report).

## Maintenance notes

- If two shipped migrations ever need the same index (a mistake), build.rs panics on the contiguity assertion — good; fix the numbering, never the predicate.
- If content-level protection is wanted later, store `sha256(sql)` next to `tag` and refuse to start when a recorded prefix's hash differs — deliberately not done now (no external users, and the rule in Step 3 covers it).
- Reviewers: check that the `INSERT` still records the full tag and that no `db/migrations` file was renamed.
