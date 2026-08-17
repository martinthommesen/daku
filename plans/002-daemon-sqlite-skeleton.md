# Plan 002: Daemon skeleton + SQLite under `~/.daku`

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- db crates/daku-core crates/daku-daemon environments.example.json docs/examples README.md`
> Also confirm plan 001 DONE: `test -f Cargo.toml && cargo check -p daku-daemon -q`. On mismatch with "Current state", STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/001-import-waku-strip-agent.md
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/20

## Why this matters

Spec §7 / ADR-0007 require local SQLite under `~/.daku/` (0700/0600) for Signal snapshots and short trends. The daemon must own the DB and expose a stable loopback bind + Hello handshake so plan 003 can poll without inventing persistence. This plan replaces waku’s agent-oriented schema with a minimal daku schema and proves migrations apply.

## Current state

- After plan 001: crates `daku-*`, hollow backend, compiling workspace; Hello auth env = **`DAKU_DAEMON_TOKEN`**.
- Persistence pattern to keep: `crates/daku-core/build.rs` embeds `db/migrations/*.sql`; `persistence.rs` has `apply_migrations` + path helpers; `db/schema.ts` is drizzle source.
- **Config source of truth (spec §6–7 / ADR-0004):** Operator file `~/.daku/environments.json` (mode 0600). **Do not** mirror Environments into a SQLite `environments` table in v1 — SQLite holds snapshots/samples only.
- ADR-0007: `~/.daku/` `0700`, db `0600`; latest snapshots + `signal_samples` for ~24h rings (empty until 004).
- CONTEXT.md: **Environment**, **Signal**, **Credential** (Keychain only — not SQLite).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Generate SQL | `bun run db:generate` | exit 0; `db/migrations/` updated |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |
| Migration tests | `cargo test -p daku-core apply_migrations` | all pass |
| Perms unit test | `cargo test -p daku-core daku_dir_permissions` | all pass |

## Scope

**In scope**

- Rewrite `db/schema.ts` — **only**:
  - `signal_snapshots` — environment_id, signal_id, observed_at, state, payload_json
  - `signal_samples` — environment_id, signal_id, observed_at, value_real (nullable), value_json (nullable)
- Regenerate migrations; embed via `build.rs`; strip agent SQL from `persistence.rs`.
- Default DB path: `~/.daku/app.db`. Override: `DAKU_DB_PATH`.
- On first open of the default path: create `~/.daku` as `0o700`, db file `0o600` — **unit-tested** on a temp home/dir (no manual-only verify).
- Daemon: bind loopback, refuse non-loopback without flag, read **`DAKU_DAEMON_TOKEN`**, apply migrations, print ready JSON (`address`, `protocol_version`), idle.
- `docs/examples/environments.example.json` (or repo-root `environments.example.json`): fake `https://*.example.service-now.com` URLs; fields `id`, `label`, `instance_url`, `auth_method` (`oauth_client_credentials` \| `basic`), `sort_order`. Optional later fields owned by plan 007 (`clone_source`) must **not** be added here.
- README: copy example → `~/.daku/environments.json`; secrets in Keychain.

**Out of scope**

- SQLite table for Environments (JSON file is SoT).
- Live ServiceNow HTTP / OAuth token fetch (003).
- GPUI (009); Keychain read implementation (003); alert history.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Rewrite drizzle schema

Replace agent tables with `signal_snapshots` + `signal_samples` only.

**Verify**: `bun run db:generate` → exit 0; `rg -n 'signal_snapshots' db/migrations/*.sql` → ≥1 hit; `rg -n 'create table.*environments' db/migrations/*.sql -i` → no matches.

### Step 2: Wire embed + apply_migrations

Embed new SQL; default open path `~/.daku/app.db`.

**Verify**: `cargo test -p daku-core apply_migrations` → pass on tempfile.

### Step 3: Daemon boot path

Bind loopback; require `DAKU_DAEMON_TOKEN` in the environment (any short local placeholder ≤8 chars for smoke); honor `DAKU_DB_PATH`.

**Verify**:

```sh
export DAKU_DB_PATH=/tmp/daku-plan002-test.db
export DAKU_DAEMON_TOKEN=dev
cargo run -p daku-daemon -- --bind 127.0.0.1:0
```

→ stdout JSON includes `address` and `protocol_version`; `/tmp/daku-plan002-test.db` exists; Ctrl-C → exit 0.

### Step 4: Permissions helper (required unit test)

Implement `ensure_daku_dir(path) -> Result<()>` setting dir `0o700` and db file `0o600`. Test with `tempfile` (or temp `HOME`).

**Verify**: `cargo test -p daku-core daku_dir_permissions` → pass; test asserts modes via `std::fs::metadata` permissions bits (on Unix).

### Step 5: Example config file

Add `environments.example.json` with fake example.service-now.com URLs only.

**Verify**: `test -f environments.example.json || test -f docs/examples/environments.example.json`; `rg -n 'service-now\\.com' environments.example.json docs/examples/environments.example.json 2>/dev/null` → only `example.service-now.com` hosts.

## Test plan

| Case | Expected |
|------|----------|
| apply_migrations empty DB | creates snapshot/sample tables |
| re-apply | idempotent |
| daku_dir_permissions | 0700 / 0600 on temp paths |
| refuse non-loopback | existing daemon test still passes |

## Done criteria

- [x] `bun run db:generate` and `cargo check -p daku-core -p daku-daemon` exit 0
- [x] `cargo test -p daku-core apply_migrations` and `daku_dir_permissions` pass
- [x] No `environments` table in migrations (`rg` check in Step 1)
- [x] Example JSON uses only fake hosts
- [x] `plans/README.md` row 002 Status = `DONE`

## STOP conditions

- Plan 001 not DONE / `cargo check` fails.
- Separating migration runner from agent SQL needs a second ORM — report; propose thin `migrations.rs` instead.
- Request to store Credentials in SQLite — refuse (ADR-0004).

## Maintenance notes

- Plan 003 reads `~/.daku/environments.json` (not SQLite) and owns the shared poll loop + HTTP client (429 / OAuth).
- Plan 004 writes `signal_samples`.
- Plan 007 may add `clone_source` to the **example JSON** in its own change set.
