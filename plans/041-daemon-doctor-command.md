# Plan 041: `daku-daemon doctor` — one command that says, per Environment, "config found, Credential present, reachable, build X"

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-daemon/src/main.rs crates/daku-daemon/README.md crates/daku-core/src/collector.rs crates/daku-core/src/lib.rs README.md`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate); soft: plans/020-settings-cleanup-typed-poll-interval.md (if landed, read the interval from the typed field), plans/019-daemon-log-file-and-empty-state.md (covers the in-app empty state — not repeated here)
- **Category**: direction
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/63

## Why this matters

First run and "why is everything red?" are the two moments a single-Operator tool gets abandoned. Today every symptom lives on the daemon's stderr, which the GPUI app swallows: a missing `~/.daku/environments.json` becomes "No Environment selected.", a missing Keychain item becomes `down · unreachable` (`crates/daku-core/src/availability.rs:189-195` turns "no credential for environment prod" into an unreachable observation), and the README's smoke step (`README.md:39-56`) is `cargo run -p daku-daemon -- probe-availability`, which despite its name runs the **whole** default loop (`crates/daku-core/src/collector.rs:207-219`) and prints only `availability probe complete` (`crates/daku-daemon/src/main.rs:81`). A `doctor` subcommand that prints one row per Environment — config parsed, Credential present (presence only), reachability/state/build/error, poll interval — turns the README's hand-run smoke into a diagnosis and gives support a copy-pasteable table.

## Current state

### `crates/daku-daemon/src/main.rs`

```rust
// :10-14
fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse(std::env::args().skip(1))?;
    if arguments.probe_availability {
        return run_probe_availability();
    }
// :74-81
fn run_probe_availability() -> anyhow::Result<()> {
    let store = daku_core::persistence::StateStore::daemon(
        daku_core::persistence::StateStore::default_path(),
    );
    daku_core::probe_availability_once(&daku_core::default_environments_path(), store)?;
    println!("availability probe complete");
    Ok(())
}
// :92-98  struct Arguments { bind, parent_pid, allowed_origins, allow_non_loopback, probe_availability }
// :108-145 parse(): match on "probe-availability" | "--bind" | "--parent-pid" | "--allow-origin" | "--allow-non-loopback" | "--help"/"-h" | unknown → bail!
// :137-142 help text: "usage: {} [probe-availability] [--bind ADDRESS] [--allow-non-loopback] [--parent-pid PID] [--allow-origin ORIGIN]..."
// :168-209 tests: non_loopback_listener_requires_an_explicit_flag, parses_repeated_browser_origin_allowlist_entries, parses_explicit_non_loopback_opt_in, parses_probe_availability
```

`crates/daku-daemon/README.md:7-12` documents `probe-availability`.

### `crates/daku-core`

- `collector.rs:100-152` `build_default_loop(environments, credentials, store, interval, client)`; `:154-203` `start_default_loop`; `:207-219` `probe_availability_once(environments_path, store)` = load + build loop + `tick()`. `poll_interval_secs(&DaemonSettings)` at `:30-37` (typed field after plan 020).
- `availability.rs:150-196` `AvailabilityCollector::new(environments, credentials, client, store)` and `pub fn probe(&self, environment) -> AvailabilityObservation` — `probe` does not touch `store`; `AvailabilityObservation { reachability, state, build: Option<String>, rtt_ms, error: Option<String> }` (`:52-58`), `Reachability::as_str()`/`SignalState::as_str()` (`:24-49`).
- `config.rs:33-46` `default_environments_path()`, `load_environments(path)`; `:52-54` `trait CredentialStore { fn get(&self, id) -> anyhow::Result<Option<String>> }`; `:81-87` `KeychainCredentialStore` (`Ok(None)` when the item is missing, `Err` on other Keychain errors).
- `settings.rs` `DaemonSettingsStore::open(path)`; `DaemonSettings::default_path()` (`crates/daku-protocol/src/settings.rs:30-35`).
- `servicenow.rs` `ServiceNowClient::new(UreqTransport::default(), SystemClock)`.
- `lib.rs:19-30` re-exports (`probe_availability_once`, `start_default_loop`, `default_environments_path`, …).

Conventions: `anyhow` errors; tests in `#[cfg(test)] mod tests`; **never print a secret** — only presence.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Daemon tests | `cargo test -p daku-daemon` | all pass |
| Core tests | `cargo test -p daku-core doctor` | all pass |
| Operator-local smoke | `cargo run -p daku-daemon -- doctor` | table printed; exit 0 when all Environments have a Credential and are reachable/asleep, else exit 1 |
| No config | `HOME=$(mktemp -d) cargo run -p daku-daemon -- doctor; echo $?` | prints the missing path; exit 1 |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/collector.rs` (`DoctorRow`, `run_doctor`)
- `crates/daku-core/src/lib.rs` (re-export)
- `crates/daku-daemon/src/main.rs` (`doctor` argument, printing, exit code, tests)
- `crates/daku-daemon/README.md`, `README.md` (smoke step mentions `doctor`)
- `plans/README.md`

**Out of scope**:
- Removing `probe-availability` (keep it; it also writes snapshots, which `doctor` deliberately does not).
- Any UI change (plan 019 owns the empty state).
- A "add credential" subcommand — that would put secret handling in shell history; the README's Keychain step stays human.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested: `Add daku-daemon doctor.`

## Steps

### Step 1: `run_doctor` in daku-core

In `crates/daku-core/src/collector.rs` add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub id: String,
    pub label: String,
    pub credential_present: bool,
    pub credential_error: Option<String>,
    pub reachability: &'static str,
    pub state: &'static str,
    pub build: Option<String>,
    pub error: Option<String>,
    pub rtt_ms: u64,
}

pub struct DoctorReport {
    pub environments_path: PathBuf,
    pub poll_interval_secs: u64,
    pub rows: Vec<DoctorRow>,
}

/// Read-only diagnosis: config, Credential presence (never the value), and a
/// live Availability probe per Environment. Writes nothing to SQLite.
pub fn run_doctor(
    environments_path: &Path,
    settings: &DaemonSettings,
    credentials: Arc<dyn CredentialStore>,
    client: ServiceNowClient,
    store: StateStore,
) -> anyhow::Result<DoctorReport> {
    let environments = load_environments(environments_path)?;
    let probe = AvailabilityCollector::new(environments.clone(), credentials.clone(), client, store);
    let rows = environments
        .iter()
        .map(|environment| {
            let (credential_present, credential_error) = match credentials.get(&environment.id) {
                Ok(Some(_)) => (true, None),
                Ok(None) => (false, None),
                Err(error) => (false, Some(error.to_string())),
            };
            let observation = probe.probe(environment);
            DoctorRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                credential_present,
                credential_error,
                reachability: observation.reachability.as_str(),
                state: observation.state.as_str(),
                build: observation.build,
                error: observation.error,
                rtt_ms: observation.rtt_ms,
            }
        })
        .collect();
    Ok(DoctorReport {
        environments_path: environments_path.to_owned(),
        poll_interval_secs: poll_interval_secs(settings),
        rows,
    })
}
```

(`store` is only needed because `AvailabilityCollector::new` takes one; `probe` never opens it. Add `use std::path::PathBuf;` if missing.) Re-export `run_doctor`, `DoctorReport`, `DoctorRow` from `crates/daku-core/src/lib.rs` next to `probe_availability_once`.

Test in `collector.rs` `mod tests` (reuse `FixtureTransport` and the `prod` `EnvironmentConfig` from `collector_loop_tick_writes_availability_snapshot`): write a temp `environments.json` with one Basic-auth Environment, `MemoryCredentialStore` **without** a secret → row has `credential_present == false`, `reachability == "unreachable"` (the client errors "no credential…"), `error.is_some()`; then insert the secret → `credential_present == true`, `reachability == "reachable"`, `build == Some("glide-zurich-12-18-2025__patch0-hotfix1")`. Assert no `signal_snapshots` row was written (open the store, `SELECT COUNT(*)` → 0).

**Verify**: `cargo test -p daku-core doctor` → pass.

### Step 2: `doctor` subcommand

In `crates/daku-daemon/src/main.rs`:

- `Arguments`: add `doctor: bool`; parse `"doctor" => doctor = true`; help text gains `[doctor]`.
- In `main`, before the token check: `if arguments.doctor { return run_doctor_command(); }`.
- Add:

```rust
fn run_doctor_command() -> anyhow::Result<()> {
    let settings = daku_core::DaemonSettingsStore::open(daku_core::DaemonSettings::default_path())
        .context("could not load daemon settings")?
        .get();
    let environments_path = daku_core::default_environments_path();
    let report = daku_core::run_doctor(
        &environments_path,
        &settings,
        Arc::new(daku_core::config::KeychainCredentialStore),
        daku_core::servicenow::ServiceNowClient::new(
            daku_core::servicenow::UreqTransport::default(),
            daku_core::servicenow::SystemClock,
        ),
        daku_core::persistence::StateStore::daemon(daku_core::persistence::StateStore::default_path()),
    )
    .with_context(|| format!("doctor: {}", environments_path.display()))?;
    println!("config: {}", report.environments_path.display());
    println!("poll interval: {} s", report.poll_interval_secs);
    for row in &report.rows {
        println!("{}", format_doctor_row(row));
    }
    if report.rows.iter().any(|row| !row.credential_present || row.reachability == "unreachable") {
        std::process::exit(1);
    }
    Ok(())
}

fn format_doctor_row(row: &daku_core::DoctorRow) -> String {
    let credential = match (&row.credential_present, &row.credential_error) {
        (true, _) => "credential: present".to_owned(),
        (false, None) => "credential: MISSING (Keychain service daku, account = id)".to_owned(),
        (false, Some(error)) => format!("credential: ERROR {error}"),
    };
    format!(
        "{} ({}) · {} · {} {} · build {} · {} ms{}",
        row.id, row.label, credential, row.reachability, row.state,
        row.build.as_deref().unwrap_or("—"), row.rtt_ms,
        row.error.as_deref().map(|e| format!(" · {e}")).unwrap_or_default(),
    )
}
```

(The daemon binary already depends on `daku-core` and `anyhow`; `Arc` is imported at `:3`. If `UreqTransport`/`SystemClock`/`KeychainCredentialStore` are not `pub` at those paths, re-export them from `daku-core/src/lib.rs` — check `pub mod servicenow`/`pub mod config` at `lib.rs:5,13` — they are public modules, so the paths above resolve.)

Tests in `main.rs` `mod tests`: `parses_doctor` (`Arguments::parse(["doctor".into()])` → `doctor == true`); `format_doctor_row_never_prints_secrets_and_flags_missing_credential` — build a `DoctorRow` with `credential_present: false`, assert the line contains `MISSING` and does not contain the word `client_secret`/`password` (sanity), and a present row contains `credential: present`.

**Verify**: `cargo test -p daku-daemon` → all pass (existing 4 + 2 new). `HOME=$(mktemp -d) cargo run -p daku-daemon -- doctor; echo $?` → error mentions `environments.json` and exit code is non-zero.

### Step 3: Docs

- `crates/daku-daemon/README.md`: add `daku-daemon doctor` to the usage block and one sentence: "`doctor` prints one line per Environment (config, Credential presence — never the value —, reachability, build) and exits 1 if any Environment lacks a Credential or is unreachable. It writes nothing."
- `README.md` "Operator smoke": add step `4. \`cargo run -p daku-daemon -- doctor\` — one line per Environment; fix anything flagged before launching the app.`

**Verify**: `grep -n 'doctor' README.md crates/daku-daemon/README.md` → ≥2 matches.

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `crates/daku-core/src/collector.rs`: `doctor_reports_missing_and_present_credential_without_writing` (Step 1).
- `crates/daku-daemon/src/main.rs`: `parses_doctor`, `format_doctor_row_never_prints_secrets_and_flags_missing_credential` (Step 2).
- Operator-local: `cargo run -p daku-daemon -- doctor` on the real config.

## Done criteria

- [ ] `grep -n 'pub fn run_doctor\|pub struct DoctorRow' crates/daku-core/src/collector.rs` → 2 matches; `grep -n 'run_doctor' crates/daku-core/src/lib.rs` → 1
- [ ] `grep -n '"doctor"' crates/daku-daemon/src/main.rs` → parse arm + test
- [ ] `cargo test -p daku-daemon` and `cargo test -p daku-core doctor` pass
- [ ] `grep -rn 'client_secret\|password' crates/daku-daemon/src/main.rs` → only the test's negative assertion (no printing of values)
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 041 updated

## STOP conditions

- `AvailabilityCollector::probe` is no longer public or has changed signature (plan 031 may restructure collectors) — report.
- `Arguments::parse` no longer matches the excerpt.
- `DaemonSettingsStore::open` requires a legacy path list you cannot satisfy — use `open_with_legacy(path, std::iter::empty())`.

## Maintenance notes

- `doctor` probes only Availability by design (cheap, no writes). If per-Signal ACL problems become the common support question, extend `run_doctor` with one Aggregate call per Signal path — keep it read-only.
- Reviewers: confirm nothing in the doctor path calls `persist_*` and that the Credential value never reaches `println!`.
