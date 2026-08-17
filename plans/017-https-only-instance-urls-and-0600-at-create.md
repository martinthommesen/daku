# Plan 017: Reject non-HTTPS Environment URLs at load time and create daemon files 0600 from the start

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/config.rs crates/daku-core/src/persistence.rs crates/daku-core/src/settings.rs README.md docs/adr/0004-servicenow-auth-and-credentials.md docs/spec/v1.md`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate)
- **Category**: security
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/40

## Why this matters

1. `EnvironmentConfig.instance_url` is deserialised with no validation and concatenated verbatim into every request (`join_url`); Basic or Bearer credentials are then attached to whatever URL results. A typo (`http://`) or a `user:pass@host` in `~/.daku/environments.json` sends OAuth client credentials / basic creds in cleartext every ~120 s, forever, with no warning; a userinfo would also end up in ureq error strings persisted to `signal_snapshots.payload_json`. ADR-0004 mandates OAuth for real Environments; it does not record a decision to allow plaintext transport. Validate once at load: `https://` only, no userinfo, no query/fragment.
2. `ensure_daku_dir` does `File::create` (umask default, typically 0644) then `set_permissions(0o600)`; `settings::write_atomic` writes its temp file with `fs::write` (default mode). The 0700 parent mode is only enforced when the parent is literally named `.daku`, so `DAKU_DB_PATH` overrides get default modes. `crates/daku-client/src/persistence.rs::write_json_atomically` already does it right (`OpenOptions::mode(0o600)`). ADR-0004 and spec §6 claim "non-secret Environment config … mode 0600" — nothing enforces it for `environments.json` (the Operator `cp`s it). Align docs to what the code guarantees.

## Current state

### `crates/daku-core/src/config.rs`

```rust
// :22-31
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EnvironmentConfig {
    pub id: String,
    pub label: String,
    pub instance_url: String,
    pub auth_method: AuthMethod,
    pub sort_order: i64,
    #[serde(default)]
    pub clone_source: bool,
}
// :40-46
pub fn load_environments(path: &Path) -> anyhow::Result<Vec<EnvironmentConfig>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut environments: Vec<EnvironmentConfig> =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    environments.sort_by_key(|environment| environment.sort_order);
    Ok(environments)
}
```

Imports at `:8`: `use anyhow::{anyhow, Context};`. Only test (`:111-125`): `example_environments_json_parses` loads `environments.example.json` (three `https://…example.service-now.com` entries) via `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../environments.example.json")`.

`crates/daku-core/src/servicenow.rs:233-235`:

```rust
fn join_url(instance_url: &str, path: &str) -> String {
    format!("{}{}", instance_url.trim_end_matches('/'), path)
}
```

Callers of `load_environments`: `crates/daku-core/src/collector.rs` (`start_default_loop`, `probe_availability_once`). Tests elsewhere build `EnvironmentConfig` structs directly (never through `load_environments`), so validation in `load_environments` breaks no fixture.

### `crates/daku-core/src/persistence.rs`

```rust
// :68-86
/// Ensures the db file exists as `0o600`. When the parent is `.daku`, also set it `0o700`.
pub fn ensure_daku_dir(db_path: &Path) -> io::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        if parent
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new(".daku"))
        {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    if !db_path.exists() {
        fs::File::create(db_path)?;
    }
    #[cfg(unix)]
    fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
```

`std::os::unix::fs::PermissionsExt` is imported at `:8-9` under `#[cfg(unix)]`. `StateStore::open` (`:115-126`) calls `ensure_daku_dir` then re-asserts 0600 after WAL setup. Test `daku_dir_permissions_are_0700_and_0600` (`:317-335`) creates `<tmp>/daku-home-<uuid>/.daku/app.db` and asserts dir 0700 / db 0600.

### `crates/daku-core/src/settings.rs`

```rust
// :89-97
fn write_atomic(path: &Path, settings: &DaemonSettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(settings).map_err(to_io_error)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, data)?;
    fs::rename(temporary, path)
}
```

### Exemplar to copy — `crates/daku-client/src/persistence.rs:188-204`

```rust
fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(value).map_err(to_io_error)?;
    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&data)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
```

(needs `use std::fs::OpenOptions; use std::io::Write as _;` and `#[cfg(unix)] use std::os::unix::fs::OpenOptionsExt;`.)

### Docs

- `docs/adr/0004-servicenow-auth-and-credentials.md:3`: "… non-secret Environment config lives in **`~/.daku/`** (mode 0600)."
- `docs/spec/v1.md:85`: "| Non-secrets | `~/.daku/` (e.g. `environments.json`), mode 0600 |"
- `README.md:28`: "Copy [`environments.example.json`](environments.example.json) to `~/.daku/environments.json` and edit Environment URLs/labels."

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Config tests | `cargo test -p daku-core config::` | all pass |
| Permission tests | `cargo test -p daku-core permissions` | all pass |
| Settings tests | `cargo test -p daku-core settings::` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/config.rs`
- `crates/daku-core/src/persistence.rs`
- `crates/daku-core/src/settings.rs`
- `README.md` (step 1 of Operator smoke), `docs/adr/0004-servicenow-auth-and-credentials.md`, `docs/spec/v1.md` (one wording fix each)
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-core/src/servicenow.rs` — `join_url` stays; validation happens once at load.
- Adding the `url` crate to `daku-core` — a string check is enough here (no new dependency).
- `crates/daku-client/src/persistence.rs` — already correct.
- Any per-Environment `allow_insecure` flag — YAGNI; PDIs are HTTPS too. If an Operator ever needs it, that is a new decision.
- `DAKU_DB_PATH` semantics.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Require https Environment URLs and create daemon files 0600.`

## Steps

### Step 1: Validate `instance_url` in `load_environments`

In `crates/daku-core/src/config.rs`, add below `load_environments`:

```rust
/// Environment URLs carry Credentials on every request: https only, no
/// userinfo, no query/fragment. Trailing `/` is tolerated (`join_url` trims it).
fn validate_instance_url(id: &str, url: &str) -> anyhow::Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(anyhow!("environment {id}: instance_url must start with https://"));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(anyhow!("environment {id}: instance_url has no host"));
    }
    if host.contains('@') {
        return Err(anyhow!("environment {id}: instance_url must not contain userinfo"));
    }
    if rest.contains('?') || rest.contains('#') {
        return Err(anyhow!("environment {id}: instance_url must not contain a query or fragment"));
    }
    Ok(())
}
```

and in `load_environments`, before the sort:

```rust
    for environment in &environments {
        validate_instance_url(&environment.id, &environment.instance_url)?;
    }
```

Add tests to `mod tests` (write a temp JSON file per case; helper `fn write_temp(json: &str) -> PathBuf` using `std::env::temp_dir().join(format!("daku-env-{}.json", uuid::Uuid::new_v4()))` — `uuid` is a dev-dep of daku-core):

- `load_environments_rejects_http_url` — `[{"id":"dev","label":"Dev","instance_url":"http://acme-dev.example.service-now.com","auth_method":"basic","sort_order":0}]` → `Err` whose `to_string()` contains `must start with https://`.
- `load_environments_rejects_userinfo` — `https://user:pw@acme.example.service-now.com` → error contains `userinfo`.
- `load_environments_rejects_query_and_fragment` — `https://acme.example.service-now.com/?x=1` and `…#frag` → error.
- `load_environments_accepts_trailing_slash` — `https://acme.example.service-now.com/` → `Ok`, one Environment.

**Verify**: `cargo test -p daku-core config::` → 5 passed (1 existing + 4 new).

### Step 2: Create the DB file 0600, always 0700 the parent daku creates

Replace `ensure_daku_dir` with:

```rust
/// Ensures the db file exists as `0o600` and its parent as `0o700`.
pub fn ensure_daku_dir(db_path: &Path) -> io::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if !db_path.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        options.open(db_path)?;
    }
    #[cfg(unix)]
    fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
```

Add `#[cfg(unix)] use std::os::unix::fs::OpenOptionsExt;` next to the existing `PermissionsExt` import. Note: `create_new` + the `exists()` guard: if the file appears between the check and the open (`AlreadyExists`), map that error to `Ok(())` — `.or_else(|e| if e.kind() == io::ErrorKind::AlreadyExists { Ok(()) } else { Err(e) })` on the `open` result (drop the file handle).

**Caveat on the 0700 parent**: `DAKU_DB_PATH=/tmp/x.db` would now chmod `/tmp` — guard it: only chmod the parent when `create_dir_all` actually created it. Simplest: check `parent.exists()` **before** `create_dir_all`; chmod only if it did not exist, OR if its file name is `.daku` (keep the existing rule). Implement exactly that (two-condition `if`).

Extend the test `daku_dir_permissions_are_0700_and_0600`: add a second case where the parent is `<tmp>/daku-home-<uuid>/custom/` (not named `.daku`) — assert dir 0700 (freshly created by daku) and file 0600. Add a third case: pre-create `<tmp>/daku-home-<uuid>/pre/` with 0755, then call `ensure_daku_dir(pre/app.db)` — assert the dir mode is unchanged (0755) and the file is 0600.

**Verify**: `cargo test -p daku-core permissions` → pass. `cargo test -p daku-core` → all pass (every collector test opens a temp DB through `StateStore::open`).

### Step 3: `settings::write_atomic` writes 0600

Rewrite `write_atomic` in `crates/daku-core/src/settings.rs` following the client exemplar exactly (OpenOptions with `mode(0o600)`, `write_all`, `sync_all`, `rename`, final `set_permissions(0o600)`); add the imports `use std::fs::OpenOptions; use std::io::Write as _; #[cfg(unix)] use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};`.

Add a test `daemon_settings_file_is_0600` in `settings.rs` `mod tests` (unix only): open a store on a temp path, `replace(DaemonSettings::default())`, assert `fs::metadata(&path).permissions().mode() & 0o777 == 0o600`.

**Verify**: `cargo test -p daku-core settings::` → all pass.

### Step 4: Align docs with what is enforced

- `README.md:28`: change the copy instruction to: "Copy [`environments.example.json`](environments.example.json) to `~/.daku/environments.json` (`chmod 600` it — the daemon only enforces `0700` on the directory and `0600` on files it writes) and edit Environment URLs/labels. URLs must be `https://` with no user:password part."
- `docs/adr/0004-servicenow-auth-and-credentials.md:3`: replace "(mode 0600)" with "(directory `0700`; files daku writes are `0600`, `environments.json` is Operator-created — `chmod 600`)".
- `docs/spec/v1.md:85`: replace "mode 0600" with "dir `0700`; daemon-written files `0600`; `https://` URLs only".

**Verify**: `grep -n 'chmod 600' README.md docs/adr/0004-servicenow-auth-and-credentials.md` → 2 matches; `grep -n 'https://. URLs only\|https:// URLs only' docs/spec/v1.md` → 1 match.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `config.rs`: 4 negative/positive `load_environments_*` tests (Step 1), modelled on `example_environments_json_parses`.
- `persistence.rs`: extended `daku_dir_permissions_are_0700_and_0600` (three parents: `.daku`, fresh custom, pre-existing 0755).
- `settings.rs`: `daemon_settings_file_is_0600`.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'fn validate_instance_url' crates/daku-core/src/config.rs` → 1 match, called from `load_environments`
- [ ] `grep -n 'File::create\|fs::write(' crates/daku-core/src/persistence.rs crates/daku-core/src/settings.rs` → no matches
- [ ] `grep -c 'mode(0o600)' crates/daku-core/src/persistence.rs crates/daku-core/src/settings.rs` → ≥1 each
- [ ] `cargo test -p daku-core config::` → 5 passed; `permissions` and `settings::` tests pass
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 017 updated

## STOP conditions

- `load_environments` / `ensure_daku_dir` / `write_atomic` differ from the excerpts (e.g. plan 020 already rewrote `settings.rs` — then only do its Step 3 if `write_atomic` still uses `fs::write`).
- `example_environments_json_parses` fails after Step 1 (the example file has a non-https URL — fix the example, not the validator, and note it).
- Any collector test fails after Step 2 (permission handling on the test tmpdir) — report the OS error.

## Maintenance notes

- If a non-https stand-in is ever genuinely needed, add an explicit per-Environment opt-in field and an ADR note — do not loosen `validate_instance_url` silently.
- The `create_new` + `AlreadyExists` mapping matters if two daemons race on first start (debug + release share `~/.daku/app.db` — see backlog).
- Reviewers: check the 0700 chmod cannot touch a directory daku did not create (the `pre-existing 0755` test).
