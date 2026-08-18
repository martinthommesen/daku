# Operator setup — decision note

**Question**: `daku-daemon` has two subcommands and both are read-only.
`doctor` diagnoses the whole setup and prints the exact remedy, then cannot act
on it. Should the daemon gain a subcommand that *fixes* what `doctor` finds —
and in particular, should daku start **writing** Credentials instead of only
reading them?

Spike for plan 068 / issue #93. Verified against the tree at `2f59138`; claims
cite symbols, not line numbers (`docs/research/hosted-daemon.md` rotted the
other way; plan 060 is cleaning that up). Every daku symbol named here is found
by `git grep` at that commit; identifiers from `security-framework`, `libc`,
`man security`, and the *proposed* shapes in §2 are external or hypothetical and
are labelled where they appear. **All values below are placeholders.**

## 1. The setup path today, end to end

From a clean machine to a working daku, exactly as `README.md` describes it.

| # | Step | Who | What goes wrong | Does `doctor` detect it? |
|---|------|-----|-----------------|--------------------------|
| 1 | Build the workspace (`cargo check --workspace`) | Operator | Missing Metal toolchain, no Xcode CLT | No — outside daku |
| 2 | Create `~/.daku/` | daku (`ensure_daku_dir`) or the Operator | Nothing: `ensure_daku_dir` creates the parent and re-asserts `0700` on **every** call when the directory is named `.daku` | n/a |
| 3 | Copy `environments.example.json` → `~/.daku/environments.json` | Operator | File absent, or in the wrong place | Yes — `run_doctor` → `load_environments` fails, error names the path |
| 4 | `chmod 600` that file | **Operator, unaided** | Left `0644`. `ensure_daku_dir` sets `0600` only on the file it creates itself (the SQLite DB); it never inspects `environments.json`'s mode | **No** — nothing reads the mode |
| 5 | Edit ids / labels / `instance_url` / `auth_method` | Operator | `http://`, userinfo, query/fragment → `validate_instance_url` rejects; unknown `auth_method` → serde error; duplicate ids are silently accepted (pinned by `load_environments_sorts_by_sort_order_and_keeps_duplicate_ids`) | Yes for the URL/serde failures; **no** for duplicate ids |
| 6 | Store one Keychain item per Environment (`service=daku`, `account=<id>`, JSON blob) | **Operator, unaided** | Item missing, or under the wrong service/account | Yes — `credential: MISSING (Keychain service daku, account = id)` via `format_doctor_row` |
| 6a | …using the README's `security add-generic-password … -w '<json>'` | Operator | The secret lands in `argv` and in shell history | No — invisible to daku |
| 6b | …re-running it after rotating a secret | Operator | **Fails.** `man security`: `-U   Update item if it already exists (if omitted, the item cannot already exist)`; the README example has no `-U` | No |
| 6c | …with a blob whose shape does not match `auth_method` (e.g. `{"username","password"}` on an `oauth_client_credentials` Environment) | Operator | `ServiceNowClient`'s `authorize` fails at `serde_json::from_str` with `oauth credential JSON` | **Misdiagnosed.** `run_doctor` only checks *presence* (`credentials.get` → `Ok(Some(_))`), so the row reads `credential: present` and the failure shows up as `unreachable` plus an error string |
| 7 | Optional `~/.daku/settings.json` (`poll_interval_secs`) | Operator | Typo → `DaemonSettingsStore::open` error; stale value → daemon reads it only at start | Partly — `doctor` prints the effective `poll interval` |
| 8 | Relaunch after editing config | Operator | Edits appear not to take | No |
| 9 | First read-back of a Keychain item written by `/usr/bin/security` | macOS | ACL prompt (see §3) | n/a |

Two of the nine steps are irreducibly human today (**4** and **6**), and they
are exactly the two with no detection: a world-readable config file and a
secret in shell history are both invisible to `doctor`.

## 2. Three shapes

### A. A `wizard` script

An interactive bash walkthrough (the `wizard` skill — note the plan's path
`.claude/skills/wizard` is stale; that directory holds only `improve`,
`improve-codebase-architecture`, `now-sdk`, and the skill ships as the plugin
skill `CLAUDE.md` names) that copies the example file, `chmod 600`s it, prompts
for each field, assembles the JSON, runs `security add-generic-password -a <id>
-s daku -U -w` **with `-w` last so the shell prompts**, and finishes by shelling
out to `cargo run -p daku-daemon -- doctor`.

- Files touched: one new script under `scripts/`. No Rust.
- Writes secrets? **No** — the Operator's own `security` invocation does, and
  the value never reaches a daku process, `argv`, or history.
- What could go wrong: script rot against `environments.example.json`; bash
  quoting of a JSON blob; nothing verifies as it goes except the final `doctor`.
- What must be tested: nothing automatable beyond a shellcheck-style lint; its
  correctness is the `doctor` run at the end.

### B. `daku-daemon setup`

A Rust subcommand: create `~/.daku/`, write `environments.json` from the
template with mode `0600`, prompt per Environment without echoing, and write
the blob to the Keychain.

- Files touched: `crates/daku-daemon/src/main.rs` (`Arguments::parse`, a
  `run_setup_command`), `crates/daku-core/src/config.rs`
  (`CredentialStore` gains `set`, `KeychainCredentialStore` implements it,
  `MemoryCredentialStore` too), `README.md`.
- Writes secrets? **Yes.** This is the first time daku writes rather than reads
  one — a different promise to the Operator than the current code makes.
- Cost not visible from the outside: **no no-echo prompt exists in this tree.**
  There is no `rpassword`/`termios` usage anywhere, so this needs either a new
  dependency or ~15 lines of `unsafe` `libc::tcgetattr`/`tcsetattr` around
  `ECHO` (`libc` is already a `cfg(unix)` dependency of `daku-core`).
- What could go wrong: a panic or `?` between reading the secret and zeroing it
  leaves it in a `String` on the heap; an `anyhow` context line that
  interpolates the blob; a partially-written setup (config file written,
  Keychain item not).
- What must be tested: the never-print-secrets property extended to every new
  output path; `CredentialStore::set` round-trips through
  `MemoryCredentialStore`; the config file is created `0600`.
- What it buys that A does not: `security_framework::passwords::set_generic_password`
  creates *or updates* (`set_password_internal` retries with `SecItemUpdate` on
  `errSecDuplicateItem`), so rotation just works — the `-U` trap of step 6b
  disappears.

### C. `daku-daemon doctor --fix`

Repair only the mechanical failures, keep *printing* instructions for the
Credential steps.

- Files touched: `crates/daku-daemon/src/main.rs` and a small helper beside
  `ensure_daku_dir`.
- Writes secrets? **No.** The trust boundary does not move.
- Scope of the repair: create `~/.daku/` (already free via `ensure_daku_dir`),
  copy `environments.example.json` when `environments.json` is absent, and
  `chmod 600` it when the mode is wider — i.e. steps 2, 3 and 4, of which 4 is
  today undetected.
- What could go wrong: overwriting an edited config (must be create-only);
  `--fix` on a shared/symlinked path.
- What must be tested: the file mode after the fix; that an existing
  `environments.json` is never overwritten; `doctor`'s exit code
  (`doctor_exit_code`) still means what it meant.

## 3. The three questions that needed looking

**Q1 — Can a Credential be written to the Keychain without appearing in `argv`,
an environment variable, shell history, or a log?**

Yes, two ways, both documented:

- `man security`, on `add-generic-password`: `-w password   Specify password to
  be added. Put at end of command to be prompted (recommended)`. With `-w` last
  the value is read from the terminal, never appears in `argv`, and never
  reaches shell history. **The README's example passes the blob inline to `-w`,
  which is precisely the discouraged form.**
- Programmatically, `security_framework::passwords::set_generic_password(service,
  account, password: &[u8])` takes the bytes in process memory; nothing touches
  `argv` or the environment.

So the STOP condition "a Credential cannot be written without exposing it" does
**not** fire — but note that the exposure the README currently creates is real.

**Q2 — What does `security-framework` offer for writing, and what does it prompt
with?**

`security-framework` 3.7 (a `cfg(target_os = "macos")` dependency of
`daku-core`, already used by `keychain_get`) exposes
`set_generic_password`, `set_generic_password_options`,
`delete_generic_password`, and the `PasswordOptions` builder. `set_generic_password`
issues `SecItemAdd` and falls back to `SecItemUpdate` on `errSecDuplicateItem`,
so create and rotate are the same call. daku does not enable the crate's
`OSX_10_15` feature, so `kSecUseDataProtectionKeychain` is never set and items
land in the same file-based login keychain that `/usr/bin/security` writes to by
default. That last step is a feature-flag inference; the observable version is
stronger and does not depend on it — `keychain_get` already reads back what the
README's `security` command wrote, which is why the current setup flow works at
all, so a write through the same crate lands where the reader looks. **Prompting: the API itself displays nothing.** Any
dialog comes from the keychain's lock state and the item's ACL (Q3).

**Q3 — Does the Operator have to unlock the Keychain, and what does the first
read-back look like?**

Split this into what is documented and what is inferred, because **the spike
deliberately wrote nothing to the Keychain**, so the UX half is unverified.

*Documented* (`man security`, `add-generic-password`): "By default, the
application which creates an item is trusted to access its data without
warning. You can remove this default access by explicitly specifying an empty
app pathname: `-T ""`." `-A` allows any application (flagged insecure);
repeated `-T appPath` adds trusted applications.

*Inferred from that*: an item created by `/usr/bin/security` trusts
`/usr/bin/security`, **not** `daku-daemon` — so the Operator's *current* setup
already produces an authorization decision the first time daku reads the item.
An item created by a `daku-daemon setup` would instead trust the daemon binary;
an unsigned local rebuild changes that binary, so the trust would have to be
re-established. Neither the exact dialog nor the rebuild behaviour was observed
here. The login keychain is unlocked at login on a normal Mac; a locked keychain
would add an unlock prompt to any `SecItem*` call, read or write.

## 4. Recommendation

**Option C plus the two one-line `README.md` fixes; not B, and A only if the
Operator base ever exceeds one.**

Reasoning, kept to what is cited above:

- The two undetected failures (steps 4 and 6a) are fixed by *different* things.
  Step 4 is mechanical and C repairs it for a few lines beside
  `ensure_daku_dir`. Step 6a is a documentation bug: `-w` at the **end** of the
  command prompts, and the man page calls that form "recommended".
- Option B's whole advantage over A+C is convenience, and its central unknown —
  what the Operator actually sees on first read-back, and whether an unsigned
  rebuild re-prompts — is exactly the thing this spike could not verify. A
  recommendation for B would rest on its one unverified leg.
- B also moves the trust boundary: daku would hold a plaintext Credential in
  process memory and own its lifetime. That is worth doing only for a benefit
  larger than "skips two `security` invocations per Environment".
- A is strictly better than the status quo and strictly cheaper than B, but for
  a single Operator a corrected README plus `doctor --fix` covers the same
  ground with no script to rot.

**The two README fixes, quotable and independent of the larger decision** (this
plan's scope forbids editing `README.md`; a follow-up should apply them):

- Change the setup example to put `-w` last so the shell prompts, and add `-U`
  so a rotation does not fail on the existing item:
  `security add-generic-password -U -s daku -a prod -w` (the JSON blob is then
  typed at the prompt, and the shell records none of it).
- State next to the `chmod 600` sentence that daku does not enforce that mode
  on the Operator-created `environments.json`, so re-check it after editing.

**Whatever ships inherits the never-print-secrets property of
`format_doctor_row`** — pinned by
`format_doctor_row_never_prints_secrets_and_flags_missing_credential` — and must
extend that test to any new output path, not merely leave it passing.

## 5. Open questions

1. The Q3 UX half: what dialog does a first read-back produce, and does an
   unsigned rebuild of `daku-daemon` re-prompt? Needs a placeholder item under a
   throwaway service name on a real machine.
2. Should `run_doctor` *parse* the blob against `auth_method` (step 6c) rather
   than only checking presence? It would turn a misdiagnosis into a diagnosis,
   but it means `run_doctor` handling a decrypted secret in a new place.
3. Should `load_environments` reject duplicate ids (step 5)? Currently pinned as
   accepted.
4. Should a wrong mode on `environments.json` be a `doctor` *finding* even
   without `--fix`?
5. A GUI onboarding flow in the desktop app is a much larger surface and is out
   of scope here; it would subsume A/B/C rather than compose with them.

## 6. Follow-up plan stubs

- **Verify the Keychain ACL and prompt behaviour** with a placeholder item —
  answers open question 1 and is a precondition for ever reconsidering option B.
- **Fix the two README setup lines** (`-w` last, `-U`) — one-line documentation
  change, no code, no dependency on the rest of this note.
- **`daku-daemon doctor --fix`** — create-only `environments.json` from the
  example, tighten its mode to `0600`, leave the Credential steps as printed
  instructions. Tests: mode after fix, existing file never overwritten,
  `doctor_exit_code` unchanged.
- **Diagnose blob/`auth_method` mismatch in `doctor`** (open question 2), only
  if the extra secret handling is judged acceptable.
