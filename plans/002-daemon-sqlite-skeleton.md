# Plan 002: Daemon skeleton + SQLite under `~/.daku`

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat b670982..HEAD -- plans/002-daemon-sqlite-skeleton.md`
> Also confirm plan 001 done: `test -f Cargo.toml && cargo check -p daku-daemon -q`. On mismatch with "Current state", STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/001-import-waku-strip-agent.md
- **Category**: direction
- **Planned at**: commit `b670982`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/20

## Why this matters

Spec §7 / ADR-0007 require local SQLite under `~/.daku/` (0700/0600) for Signal snapshots and short trends. The daemon must own the DB and expose a stable loopback bind + Hello handshake so plan 003 can poll without inventing persistence. This plan replaces waku’s agent-oriented schema with a minimal daku schema and proves migrations apply.

## Current state

- After plan 001: renamed crates `daku-*`, hollow backend, compiling workspace.
- Upstream persistence pattern (after copy) — keep the **migration runner**, discard agent tables:

```text
crates/daku-core/build.rs          — embeds db/migrations/*.sql
crates/daku-core/src/persistence.rs — apply_migrations + path helpers (carve from waku)
db/schema.ts                       — drizzle source (rewrite)
db/migrations/                     — generated SQL
```

- ADR-0007: directory `~/.daku/` mode `0700`, db file `0600`; latest snapshots + later ~24h rings (rings can be empty tables now).
- ADR-0004: non-secret Environment config also under `~/.daku/` (e.g. `environments.json`) — create schema/docs; do not invent hostnames in fixtures.
- CONTEXT.md terms: **Environment**, **Signal**, **Credential** (secrets stay in Keychain — not SQLite).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Generate SQL | `bun run db:generate` | new/updated files under `db/migrations/` |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |
| Migration unit test | `cargo test -p daku-core apply_migrations` (or the renamed test module) | pass |
| Dir perms (manual) | after first daemon run: `stat -f '%Lp' ~/.daku` and db file | `700` / `600` |

## Suggested executor toolkit

- Inventory §4.1 / §7 on mixed `persistence.rs`.
- Spec §§6–7; ADR-0004, ADR-0007.

## Scope

**In scope**

- Rewrite `db/schema.ts` for daku tables (minimal):
  - `environments` — id, label, instance_url, auth_method, sort_order, created_at (no secrets columns).
  - `signal_snapshots` — environment_id, signal_id, observed_at, state, payload_json.
  - `signal_samples` — environment_id, signal_id, observed_at, value_real (nullable), value_json (nullable) — for future 24h trends; may stay unused until plan 004.
- Regenerate `db/migrations/` via Bun; ensure `build.rs` embeds them.
- Carve `apply_migrations` + open-db helpers; default DB path under `~/.daku/app.db` (or `Library/Application Support/daku/` **only** if dirs-crate forces it — then also symlink/document `~/.daku` per ADR-0007; prefer `~/.daku/app.db` as the canonical path).
- On first open: `create_dir_all` with `0o700`, open DB with mode `0o600`.
- Daemon starts, binds loopback, prints `DaemonReady` JSON, applies migrations, stays up until SIGINT.
- Fixture `environments.example.json` (fake `https://prod.example.service-now.com` style URLs only) documenting shape — **not** real hostnames.
- Unit tests: migrations apply on empty tempfile; second apply is no-op / idempotent.

**Out of scope**

- Live ServiceNow HTTP (plan 003).
- GPUI UI beyond what 001 already compiles (plan 009).
- Keychain credential read (003 may add a thin client; 002 only documents that secrets are not in SQLite).
- Alert history tables.

## Git workflow

- Branch: `plan/002-daemon-sqlite-skeleton`
- Commit message example: `Add daku SQLite schema and ~/.daku daemon paths`

## Steps

### Step 1: Rewrite drizzle schema

Replace agent tables in `db/schema.ts` with the three tables above. Keep drizzle + `drizzle.config.ts` pipeline.

**Verify**: `bun run db:generate` → exit 0; `ls db/migrations/*.sql` → ≥1 file referencing `signal_snapshots`.

### Step 2: Wire embed + apply_migrations

Ensure `crates/daku-core/build.rs` embeds new SQL. Strip agent SQL usage from `persistence.rs`; keep WAL pragmas + `apply_migrations`. Point `StateStore`/DB open default path at `~/.daku/app.db` (expand home safely).

**Verify**: `cargo test -p daku-core` — migration tests pass on tempfile.

### Step 3: Daemon boot path

`daku-daemon` main: bind loopback (keep refuse-non-loopback test), apply migrations to default path (or `DAKU_DB_PATH` override for tests), print ready JSON, idle.

**Verify**:

```sh
export DAKU_DB_PATH=/tmp/daku-plan002-test.db
# Set the daemon auth env (name from plan 001 rename) to any short local placeholder, then:
cargo run -p daku-daemon -- --bind 127.0.0.1:0
```

→ stdout contains JSON with `address` + `protocol_version`; process stays up; Ctrl-C exits 0. File `/tmp/daku-plan002-test.db` exists.

### Step 4: Permissions helper

When creating `~/.daku` (not tempfile), set `0o700` on the directory and `0o600` on the db file. Unit-test with a temp dir if possible; otherwise document Operator smoke.

**Verify**: unit test or documented manual `stat` checklist in plan maintenance notes.

### Step 5: Example config shape

Add `environments.example.json` at repo root or under `docs/examples/` with **example.com** URLs and `auth_method: "oauth_client_credentials" | "basic"`. README points Operators to copy → `~/.daku/environments.json`.

**Verify**: file exists; `rg -n 'service-now\\.com' environments.example.json docs/examples` → only `example.service-now.com` or similar fakes.

## Test plan

- `apply_migrations` on empty DB → N statements; re-run → 0.
- Opening DB creates parent dir with mode 700 when using a nested tempfile path.
- Daemon bind allowlist test still passes.

## Done criteria

- [ ] `bun run db:generate` + `cargo check -p daku-core -p daku-daemon` exit 0
- [ ] Migrations embed and apply in tests
- [ ] Default/override DB path documented; secrets not stored in SQLite schema
- [ ] Example environments file uses fake URLs only
- [ ] `plans/README.md` row 002 → `done`

## STOP conditions

- Plan 001 artifacts missing / `cargo check` fails before you start.
- Cannot separate migration runner from agent SQL without rewriting all of `persistence.rs` twice — report and propose a thin new `migrations.rs` module instead of improvising a second ORM.
- Pressure to store OAuth client secrets in SQLite — refuse; use Keychain (ADR-0004).

## Maintenance notes

- Plan 003 writes rows into `signal_snapshots`.
- Plan 004 uses `signal_samples` for syslog/jobs trends.
- Reviewers: confirm no secret columns; confirm `~/.daku` permissions story matches ADR-0007.
