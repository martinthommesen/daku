# daku v1 implementation plans

Handoff plans for executor agents implementing [docs/spec/v1.md](../docs/spec/v1.md).  
Map: [#17](https://github.com/martinthommesen/daku/issues/17) (closed). Index locked in [#19](https://github.com/martinthommesen/daku/issues/19).

**Template:** `.agents/skills/improve/references/plan-template.md`  
**Status values:** `TODO` | `IN PROGRESS` | `DONE` | `BLOCKED` | `REJECTED`  
**Public hygiene:** never put instance hostnames, usernames, or secrets in plans or commits — Operator-local `~/.daku/` / keychain only.

## Planned-at map

| Plans | `Planned at` in file | First landed on `main` |
|-------|----------------------|-------------------------|
| 001–003 (original) | was `b670982` | `da67ae9` |
| 004–007 (original) | was `da67ae9` | `315f38d` |
| 008–009 (original) | was `315f38d` | `d912bbb` |
| 010 (original) | was `d912bbb` | `567179a` |
| **001–010 (this revision)** | **`567179a`** (drift base) | **`ce612f5`** |

Drift checks use `567179a..HEAD` against each plan’s **Scope** paths (not only the plan markdown).

## Execution order

| Plan | Title | Priority | Status | Depends on |
|------|-------|----------|--------|------------|
| [001](001-import-waku-strip-agent.md) | Import pinned waku trees and strip agent domain until `cargo` workspace builds | P1 | DONE | inventory inlined in plan |
| [002](002-daemon-sqlite-skeleton.md) | Daemon skeleton + SQLite under `~/.daku` | P1 | DONE | 001 |
| [003](003-availability-signal.md) | Availability Signal + shared HTTP/OAuth/429 + CollectorLoop | P1 | DONE | 002 |
| [004](004-jobs-syslog-trends.md) | Scheduled jobs + syslog Signals with ~24h trends | P1 | DONE | 003 |
| [005](005-mid-ecc-signal.md) | MID/ECC Signal | P2 | DONE | 003 (soft: 004) |
| [006](006-outbound-signal.md) | Outbound/integration failures Signal | P2 | DONE | 003 (soft: 004) |
| [007](007-drift-last-clone.md) | Version/plugin drift + last-clone Signals | P2 | DONE | 003 |
| [008](008-health-rollup-protocol.md) | Environment health rollup + protocol events (+ samples) | P1 | DONE | **003** (stubs OK) |
| [009](009-gpui-shell-variant-c.md) | GPUI shell — sidebar + detail + job/syslog sparklines | P1 | DONE | 008 |
| [010](010-dmg-sparkle-homebrew.md) | Notarised DMG + Sparkle; Homebrew cask alternate | P3 | DONE | 009 |

## Dependency graph

```text
001 → 002 → 003 ┬→ 004 ─┐
                ├→ 005 ←┤ soft: aggregate_count
                ├→ 006 ←┘
                ├→ 007
                └→ 008 → 009 → 010
```

**Ownership locks (spec alignment):**

- **Poll loop** (~120s): plan **003** only; later plans register collectors.
- **HTTP + OAuth client-credentials + 429/`Retry-After`**: plan **003**.
- **Config SoT**: `~/.daku/environments.json` (no Environments SQLite table).
- **Environment health**: `healthy` \| `degraded` \| `down` only; **reachability** (`reachable` \| `unreachable` \| `asleep`) is separate — asleep must not become health=`degraded`.
- **~24h trends UI**: 004 stores samples; 008 emits `SignalSamplesUpdated`; 009 renders sparklines.

## Conventions for executors

- Vocabulary: `CONTEXT.md` (Platform, Environment, Signal, Credential, Operator, Environment health).
- Licence: GPL-3.0-only (ADR-0002); keep `LICENSE` from waku copy.
- **Git**: trunk-based on `main` — no PRs, no GitHub Actions (`docs/agents/git-workflow.md`). Commit on `main` (or a disposable local branch you delete after merging locally); push `main` only when the Operator wants remote updated.
- Commit style: imperative summary like existing `main`.
- Live ServiceNow calls are **Operator-local smoke** only; verification is local (`cargo test` / plan Done criteria), not Actions.
- Daemon Hello env: **`DAKU_DAEMON_TOKEN`** (locked in 001).

---

## Advisor audit — 2026-08-17 (`/improve deep`, planned at `f7fdbe7`)

Full-repo audit (correctness, security, perf, tests, tech debt, deps, DX/docs, direction) after plans 001–010 landed. Every vetted finding now has a plan (011–043); the maintainer asked for all of them. Numbering continues monotonically from 010. Drift checks for 011+ use `f7fdbe7..HEAD` against each plan's Scope paths.

**Tracking issue:** [#67](https://github.com/martinthommesen/daku/issues/67). **Gate for 011+:** `bun run check` (introduced by 011; 032 adds clippy `-D warnings`) must exit 0 as a done criterion.
**Protocol bumps:** 020, 029 and 039 each bump `PROTOCOL_VERSION` — always increment the live value, never set a fixed number.

### Execution order & status

Recommended order = table order (tiers: **A** baseline/bugs, **B** tests+debt foundations, **C** perf/deps, **D** direction). Within a tier, plans are independent unless "Depends on" says otherwise.

| Plan | Title | Priority | Effort | Depends on | Status |
|------|-------|----------|--------|------------|--------|
| [011](011-green-baseline-check-gate.md) | Make the local verification gate one command and green (fix `extra` flatten test, fmt, lint; `bun run check`) ([#34](https://github.com/martinthommesen/daku/issues/34)) | P1 | S | — | DONE |
| [012](012-bound-server-controlled-durations-and-token.md) | Cap `Retry-After` / OAuth `expires_in`; refuse an empty daemon token ([#35](https://github.com/martinthommesen/daku/issues/35)) | P1 | S | 011 | DONE |
| [014](014-replay-dashboard-on-subscribe.md) | Replay latest dashboard state to new subscribers; publish before first tick ([#36](https://github.com/martinthommesen/daku/issues/36)) | P1 | S | 011 | DONE |
| [013](013-asleep-never-degrades.md) | Asleep Environment never rolls up degraded; secondary Signals skip probing it ([#37](https://github.com/martinthommesen/daku/issues/37)) | P1 | M | 011 (land after 014) | DONE |
| [015](015-release-pipeline-sparkle-fixes.md) | Fix Sparkle release path: `CFBundleVersion`, checksum abort, DMG path, ZIP in checklist ([#38](https://github.com/martinthommesen/daku/issues/38)) | P1 | S | 011 | DONE |
| [016](016-pin-gpui-and-trim-root-deps.md) | Pin GPUI `rev`, drop `test-support` from release, trim unused root deps ([#39](https://github.com/martinthommesen/daku/issues/39)) | P2 | S | 011 | DONE |
| [017](017-https-only-instance-urls-and-0600-at-create.md) | Reject non-HTTPS Environment URLs at load; create daemon files 0600 from the start ([#40](https://github.com/martinthommesen/daku/issues/40)) | P2 | S | 011 | DONE |
| [019](019-daemon-log-file-and-empty-state.md) | Daemon stderr → `~/.daku/daemon.log` (0600, append); empty Environment list explains itself ([#41](https://github.com/martinthommesen/daku/issues/41)) | P2 | S | 011 | DONE |
| [018](018-supervisor-backoff-and-remote-reconnect.md) | Supervisor restarts with bounded backoff; remote daemons reconnect ([#42](https://github.com/martinthommesen/daku/issues/42)) | P2 | M | 011, 014 | DONE |
| [020](020-settings-cleanup-typed-poll-interval.md) | Typed `DaemonSettings { poll_interval_secs }`; delete desktop settings mirror + dead window/preference plumbing; protocol bump ([#43](https://github.com/martinthommesen/daku/issues/43)) | P2 | M | 011 (supersedes 011's `extra` tests) | DONE |
| [021](021-updater-standard-driver.md) | Sparkle via `SPUStandardUpdaterController`; delete custom `UserDriver`/events/status/preview ([#44](https://github.com/martinthommesen/daku/issues/44)) | P2 | M | 011, 015 | DONE |
| [028](028-temp-db-test-helper-and-collector-isolation.md) | `test_support::{TempDb, prod()}`; migrate ~19 temp-DB sites; collector isolation test ([#45](https://github.com/martinthommesen/daku/issues/45)) | P3 | S | 011 (before 031; ideally before 013/022/023) | DONE |
| [025](025-loopback-websocket-integration-test.md) | Loopback WebSocket integration test (auth, Ping/Ack, Origin, Shutdown, dashboard, disconnect) ([#46](https://github.com/martinthommesen/daku/issues/46)) | P2 | M | 011 (adapt if 029 lands first) | DONE |
| [026](026-daemon-process-blackbox-test.md) | Black-box daemon process test (ready line, token refusal, spawn→Ping→drop reaps, parent watchdog) ([#47](https://github.com/martinthommesen/daku/issues/47)) | P2 | M | 011, 012 | DONE |
| [029](029-delete-waku-replay-machinery.md) | Delete inherited waku session/runtime replay machinery from protocol, hub, client; protocol bump ([#48](https://github.com/martinthommesen/daku/issues/48)) | P2 | M | 011, 014 | DONE |
| [030](030-delete-unused-assets-fonts-i18n.md) | Delete unreferenced icons/fonts/CoreText FFI; shrink i18n to six menu strings, embed once ([#49](https://github.com/martinthommesen/daku/issues/49)) | P2 | S–M | 011 (soft 020) | DONE |
| [031](031-collector-consolidation-typed-signal-state.md) | One per-Environment collector loop (`Signal` trait) + typed `SignalState`; last-clone unreachable → down; absorbs 013's gate ([#51](https://github.com/martinthommesen/daku/issues/51)) | P2 | M–L | 011, 013, 028 (see 022 note) | DONE |
| [022](022-per-environment-collector-concurrency.md) | Poll Environments concurrently (per-Environment collector groups on scoped threads; SQLite busy_timeout; tick-overrun warning) ([#50](https://github.com/martinthommesen/daku/issues/50)) | P2 | M | 011, 013, 014 | DONE |
| [023](023-drift-inventory-throttle-and-mid-aggregate.md) | Cache drift plugin/store-app inventories for 30 min instead of refetching every tick ([#52](https://github.com/martinthommesen/daku/issues/52)) | P2 | S | 011 | DONE |
| [024](024-poll-interval-floor-and-credential-memo.md) | Floor `poll_interval_secs` at 30 s; check the OAuth token cache before reading the Keychain ([#53](https://github.com/martinthommesen/daku/issues/53)) | P3 | S | 011 (soft 020) | DONE |
| [027](027-unit-test-gap-fill.md) | Unit-test gap fill: DashboardState branches, ServiceNow failure modes, `load_environments` negatives, client persistence, vacuous tests ([#54](https://github.com/martinthommesen/daku/issues/54)) | P3 | M | 011; ordering vs 012/013/017/020/021 stated per section | DONE |
| [032](032-delete-dead-platform-theme-code-clippy-gate.md) | Delete rustc-flagged dead platform/theme code, fix sidebar tint width, add clippy `-D warnings` to the gate ([#55](https://github.com/martinthommesen/daku/issues/55)) | P2 | S–M | 011, 016, 020, 021, 029, 030 (last debt plan) | DONE |
| [033](033-protocol-crate-hygiene-and-hollow-scaffolding.md) | `daku-protocol` free of dirs/OS/i18n deps; explicit client re-exports; `HollowBackend`→`SettingsBackend`; drop `export_types` stub ([#56](https://github.com/martinthommesen/daku/issues/56)) | P3 | M | 011, 020, 029, 030 | DONE |
| [034](034-ureq-3-native-roots.md) | ServiceNow HTTP transport on `ureq` 3 with platform root certificates (+ optional rusqlite bump) ([#57](https://github.com/martinthommesen/daku/issues/57)) | P2 | S–M | 011 | DONE |
| [035](035-cargo-config-profile-and-block-patch.md) | Clean `.cargo/config.toml`; keep release symbols (`.dSYM`); try dropping the personal-fork `block` patch ([#58](https://github.com/martinthommesen/daku/issues/58)) | P3 | S | 011, 015, 016 | DONE |
| [036](036-db-tooling-and-migration-identity.md) | Trim Bun DB tooling; key applied migrations on numeric prefix ([#59](https://github.com/martinthommesen/daku/issues/59)) | P3 | S | 011 | DONE |
| [037](037-onboarding-docs-and-env-var-table.md) | Document fresh-clone prerequisites and every environment variable daku reads ([#60](https://github.com/martinthommesen/daku/issues/60)) | P3 | S | 011, 016; soft 019, 035 | DONE |
| [038](038-signal-detail-render-error-and-drill-in.md) | Show why a Signal is red (render `error`/`detail`), then decide drill-in (decision note) ([#61](https://github.com/martinthommesen/daku/issues/61)) | P2 | S | 011, 013 | DONE |
| [039](039-header-freshness-url-and-compare-strip.md) | Header freshness ("polled Ns ago") + `instance_url` on `EnvironmentSummary` (protocol bump) + richer compare strip ([#62](https://github.com/martinthommesen/daku/issues/62)) | P2 | S–M | 011, 014 | DONE |
| [041](041-daemon-doctor-command.md) | `daku-daemon doctor` — per-Environment config / Credential presence / reachability / build ([#63](https://github.com/martinthommesen/daku/issues/63)) | P2 | S | 011; soft 019, 020 | DONE |
| [043](043-drift-mismatch-list-payload.md) | Drift persists a bounded `mismatch_list` and renders it under the drift card ([#64](https://github.com/martinthommesen/daku/issues/64)) | P2 | M | 011; soft 038, 031 | DONE |
| [040](040-last-clone-per-target-with-age.md) | Last-clone per clone target with `age_days` (spike on `clone_instance.target`, then build) ([#65](https://github.com/martinthommesen/daku/issues/65)) | P2 | M | 011; soft 013, 031 | DONE |
| [042](042-hosted-daemon-seam-spike.md) | Hosted-daemon seam spike — README attach path + `docs/research/hosted-daemon.md` decision note ([#66](https://github.com/martinthommesen/daku/issues/66)) | P3 | S | 011, 014; soft 020 | DONE |

### Dependency notes

- **011 first, always**: every later plan's done criteria call `bun run check`, and it turns the one red test green.
- 014 before 013 and 018 and 022 (all touch `crates/daku-core/src/collector.rs` `run`/`tick` or rely on replay); 014 before 029 (029 must keep 014's dashboard cache).
- 020 supersedes 011's `poll_interval_secs` README line and `extra` tests; 024's floor is written for both the `extra` and the typed shape.
- 028 (temp-DB helper) should precede 031 and ideally 013/022/023 so new tests use it.
- 031 vs 022: 022 lands first (collector.rs-only, adds `register_group`); 031 must preserve the per-Environment group structure — see 031's Depends-on note.
- 032 lands last among debt plans (016, 020, 021, 029, 030 delete dead code first) and then turns on clippy in the gate.
- 033 after 020, 029, 030 (they decide what leaves `daku-protocol`).
- 035 after 015 (both edit `scripts/release.ts`/`bundle.sh`); 037 after 016/019/035 (documents what they change).
- 038 before 043 (drift list renders under the card detail); 039's `instance_url` unblocks 038's deep-link option and 040's target matching.
- Protocol bumps in 020, 029, 039: each increments the live `PROTOCOL_VERSION`; desktop and daemon ship together.

### Findings considered and rejected

- "No CI / no PRs" — decided in `docs/agents/git-workflow.md`; the gate is local (`bun run check`).
- Loopback-only daemon, token via env, unsigned builds, Keychain service `daku`, Basic auth for PDIs, `DAKU_DB_PATH` override — by design (ADR-0004/0006, README).
- Thread-per-request in `server.rs`, unbounded outgoing channel — authenticated + loopback; revisit only if `--allow-non-loopback` becomes supported (see 042).
- SQLite per-row autocommit, `store.open()` per collector, missing index on `prune` — tables are ≤ ~1.5 k rows; not worth doing.
- PERF-06 (MID agents via Aggregate API) — one list call already yields both counts and keeps per-MID fields DIR-01/038 wants; two aggregate calls would be more requests (recorded in 023).
- Sparkline down-sampling — ≤ 2 880 points once 024's 30 s floor exists.
- Removing drizzle entirely — ADR-0007 keeps the drizzle→SQL pipeline.
- Bun test harness for `scripts/*.ts` (TEST-10) — release is manual and macOS-bound; failures are immediate.
- Testing the dead journal/replay machinery (TEST-08) — deleted by 029 instead.
- Re-reading `poll_interval_secs` every tick — restart is documented (011/020); not worth the plumbing.
- `resolver = "2"` on edition 2024 — no `rust-version` until 037; resolver 3 buys nothing here.
- Splitting `drift.rs`/`servicenow.rs`/`process.rs` — cohesive single-topic modules.
- "Upgrade happened" marker Signal — build drift already flags it; would need an ADR-0007 exception.
- `scripts/delete-debug-app.ts` `sh.waku.dev`/`codes.waku.dev` — intentional legacy bundle IDs for cleaning pre-rename debug data.

---

## gpui-component adoption — 2026-08-17 (planned at `826a636`)

Decided in a grill session; ADR-0008 + ADR-0005 amendment. **Tracking issue:** [#71](https://github.com/martinthommesen/daku/issues/71). Spike branch `spike/gpui-component` (`10e6585`) is the working reference and is deleted once 044 lands. Drift checks for 044+ use `826a636..HEAD` (045/046: the previous plan's landing commit) against each plan's Scope paths. Land in order; the Operator visually checks each step (`DAKU_UI_FIXTURE=1 bun run dev` + PDI) before the next.

| Plan | Title | Priority | Effort | Depends on | Status |
|------|-------|----------|--------|------------|--------|
| [044](044-gpui-component-shell-and-pin.md) | Shell on gpui-component: Root, TitleBar, Sidebar, tokens; zed sha pinned in Cargo.lock only ([#68](https://github.com/martinthommesen/daku/issues/68)) | P1 | M | — | DONE |
| [045](045-environment-detail-restyle.md) | Environment detail restyle: header, pills, Signal cards, compare strip ([#69](https://github.com/martinthommesen/daku/issues/69)) | P1 | M | 044 | DONE |
| [046](046-drill-in-and-deep-links.md) | Card selection → Drill-in region + Open-in-ServiceNow links ([#70](https://github.com/martinthommesen/daku/issues/70)) | P2 | M | 045 | DONE |

---

## Advisor audit — 2026-08-18 (`/improve deep`, planned at `2bdeaba`)

Second full-repo audit, run one day after the 011–043 batch and immediately
after the gpui-component adoption (044–046) landed. Six parallel read-only
agents (daemon runtime, Signal collectors, GPUI shell + client, security/deps/
packaging, test coverage, docs + direction), each given the "considered and
rejected" list above as a suppression list. Every finding below was re-read in
the source by the advisor before a plan was written. The maintainer asked for
all of them plus three direction plans. Numbering continues monotonically from
046. Drift checks for 047+ use `2bdeaba..HEAD` against each plan's Scope paths.

**Baseline at `2bdeaba`:** `bun run check` exits 0 — fmt, `clippy -D warnings`,
201 tests, oxlint. No verification-baseline plan was needed this time.

**Not audited:** GPUI render output (untestable in this setup — verified only by
the Operator running `DAKU_UI_FIXTURE=1 bun run dev`); anything needing a live
ServiceNow instance (plans 057, 058 and 068 each name an Operator-run check);
`prototypes/`; vendored git dependencies. `cargo-audit` is not installed, so
dependency posture was reasoned from `Cargo.lock` rather than scanned.

**Tracking issue:** [#94](https://github.com/martinthommesen/daku/issues/94).
**Gate for 047+:** `bun run check` must exit 0 as a done criterion.
**No protocol bumps** in this batch — 047 and 065 both explicitly avoid one.

### Execution order & status

Recommended order = table order (tiers: **A** correctness that makes the console
lie, **B** narrower correctness + security, **C** perf/debt, **D** tests/docs/DX,
**E** direction). Within a tier, plans are independent unless "Depends on" says
otherwise.

| Plan | Title | Priority | Effort | Depends on | Status |
|------|-------|----------|--------|------------|--------|
| [047](047-unobserved-environment-is-not-healthy.md) | An Environment daku has never probed must not render as healthy and reachable ([#72](https://github.com/martinthommesen/daku/issues/72)) | P1 | S | — | TODO |
| [048](048-last-clone-persists-every-target-on-failure.md) | Last-clone records why it has no answer when the clone-source probe fails ([#73](https://github.com/martinthommesen/daku/issues/73)) | P1 | S | — | TODO |
| [050](050-compare-strip-one-reference-build.md) | The Compare strip tints against the same build it calls a mismatch, and the rule becomes testable ([#75](https://github.com/martinthommesen/daku/issues/75)) | P1 | S | — | TODO |
| [051](051-local-daemon-reconnect-and-supervisor-test.md) | The supervisor recovers a local daemon whose socket dropped, and the restart loop finally has a test ([#76](https://github.com/martinthommesen/daku/issues/76)) | P1 | S–M | — | TODO |
| [049](049-drift-build-tri-state-skip-reason-and-asleep-gate.md) | Drift stops guessing — unknown builds are unknown, the skip reason is true, and asleep Environments are not probed ([#74](https://github.com/martinthommesen/daku/issues/74)) | P2 | S–M | 048 | TODO |
| [052](052-release-integrity-cask-checksum-and-appcast.md) | Neither distribution channel ships bytes nobody verified ([#77](https://github.com/martinthommesen/daku/issues/77)) | P2 | S | — | TODO |
| [053](053-client-state-hygiene.md) | Prune removed Environments, close the subscribe gap, mute the detail when disconnected ([#78](https://github.com/martinthommesen/daku/issues/78)) | P2 | S | 047 | TODO |
| [054](054-poll-cadence-and-oauth-ttl-floor.md) | The poll interval means the poll interval, and an OAuth grant is never born expired ([#79](https://github.com/martinthommesen/daku/issues/79)) | P2 | S | — | TODO |
| [055](055-panic-isolation-for-shared-collectors.md) | A panic in a shared collector cannot silently end polling ([#80](https://github.com/martinthommesen/daku/issues/80)) | P2 | S | — | TODO |
| [056](056-parse-signal-payloads-once.md) | Parse each Signal payload once when it arrives, not once per element per frame ([#81](https://github.com/martinthommesen/daku/issues/81)) | P2 | M | 050 | TODO |
| [057](057-last-clone-truncation-vs-never-cloned.md) | "No clone in the page I read" stops looking like "never cloned" ([#82](https://github.com/martinthommesen/daku/issues/82)) | P2 | S–M | 048 | TODO |
| [058](058-drift-truncation-count-order-and-surface.md) | Drift knows when it only saw part of the inventory, and says so ([#83](https://github.com/martinthommesen/daku/issues/83)) | P3 | M | 049 | TODO |
| [059](059-test-hygiene-sandbox-home-and-429-mapping.md) | Tests clean up after themselves, stop racing on the environment, and pin what a rate-limited Environment looks like ([#84](https://github.com/martinthommesen/daku/issues/84)) | P3 | S–M | soft 051 | TODO |
| [060](060-docs-reconciliation.md) | The docs stop asserting things that are no longer true ([#85](https://github.com/martinthommesen/daku/issues/85)) | P3 | S | — | TODO |
| [061](061-gpui-component-bump-recipe.md) | The documented recipe for bumping the UI toolkit actually works ([#86](https://github.com/martinthommesen/daku/issues/86)) | P3 | S | — | TODO |
| [062](062-delete-fps-counter-and-i18n.md) | Delete the FPS counter that renders the word "FPS", and the i18n framework serving four English strings ([#87](https://github.com/martinthommesen/daku/issues/87)) | P3 | S | — | TODO |
| [063](063-typescript-typecheck-in-the-gate.md) | The TypeScript in this repo is type-checked, and the lint plugin is linted ([#88](https://github.com/martinthommesen/daku/issues/88)) | P3 | S | 052 | TODO |
| [064](064-client-side-instance-url-check.md) | The desktop validates the URL it hands to macOS, instead of trusting the daemon ([#89](https://github.com/martinthommesen/daku/issues/89)) | P3 | S | — | TODO |
| [065](065-payload-key-contract-test.md) | The payload keys the daemon writes and the desktop reads are pinned to each other ([#90](https://github.com/martinthommesen/daku/issues/90)) | P3 | M | 048, 049, 057 | TODO |
| [066](066-mid-ecc-drill-in-rows.md) | The MID/ECC Drill-in shows which MID is down, from data daku already fetches ([#91](https://github.com/martinthommesen/daku/issues/91)) | P2 | S | — | TODO |
| [067](067-reload-command-spike.md) | **Spike** — how the Operator reloads config and forces a poll without relaunching ([#92](https://github.com/martinthommesen/daku/issues/92)) | P3 | M | 051 | TODO |
| [068](068-daemon-setup-subcommand-spike.md) | **Spike** — should `daku-daemon` fix what `doctor` already diagnoses ([#93](https://github.com/martinthommesen/daku/issues/93)) | P3 | M | — | TODO |

### Dependency notes

- **048 before 049 and 057** — 048 extracts the `skip_targets` helper both reuse.
- **049 before 058** — 049 makes `build_matches` tri-state; 058 then works on the
  final payload shape.
- **047 before 053** — both edit `sidebar()`'s `muted`; 053 is additive on top.
- **050 before 056** — 050 changes `compare_rows()`; 056 then makes it cheap.
- **052 before 063** — both edit `scripts/release.ts`; type-check the final shape.
- **051 before 059 and 067** — 051 adds tests to `crates/daku-daemon/tests/process.rs`
  (059 then fixes that file's hygiene) and changes when the supervisor respawns
  (one of 067's candidate mechanisms).
- **048, 049, 057 before 065** — each adds or changes a payload key; 065 pins the
  final shapes.
- **049 vs 055**: both touch how drift and last-clone are scheduled. 055 moves
  them inside `tick`'s panic-capturing scope and **must preserve their ordering
  after the per-Environment groups join** — 049's availability gate depends on
  this tick's availability snapshot already being committed. Land one at a time
  and re-read 055's STOP conditions.
- **060 vs everything**: 060 corrects verification commands inside DONE plans
  003/007/026/027/031/046. If a corrected command *fails* rather than returning a
  different number, that is an incompletely-landed plan — 060 says to report it,
  not to edit the expectation.

### Findings considered and rejected

- `encode_query` escaping only four characters (`src/dashboard_state.rs`) — every
  string it receives is one of seven hardcoded literals; the only variable part
  is `instance_url`, which `config.rs` validates. No ServiceNow field value ever
  reaches a URL. (Plan 064 covers the `instance_url` half.)
- `StateStore::open`'s `OPEN_LOCK` being process-local while `probe-availability`
  and `doctor` open the same DB from other processes — the `journal_mode` PRAGMA
  only contends on a database not yet in WAL, so this is a first-run-only race.
  The migration half is already correct across processes.
- SQLite `-wal`/`-shm` sidecars created at the process umask rather than `0600` —
  `~/.daku` is forced `0700`, so nothing outside the Operator's account reaches
  them; the code comments already acknowledge it.
- `_ => {}` in `DashboardState::apply` — the only unhandled variants are
  transport-owned (`Hello`, `Rejected`, `Response`); `ShuttingDown` never reaches
  `apply`.
- The dead `|| list.len() > DRILL_IN_ROW_LIMIT` clause in `drill_in` — correct
  belt-and-braces if either constant moves.
- `freshness` sub-minute precision drifting against the 30 s re-render tick —
  cosmetic; shortening the tick costs frames.
- A `rust-toolchain.toml` — marginal for a single-Operator repo where
  `rust-version = "1.96"` already errors on an old toolchain.
- `chrono`/`time` and `base64` version duplication, and deprecated
  `@esbuild-kit/*` under `drizzle-kit` — all transitive, none actionable.
- Multiple `clone_source: true` silently resolving to the first by `sort_order` —
  real but the weakest item found; fold into 049 if that file is open anyway.
- `SYS_STORE_APP_PATH` requesting `latest_version` and never reading it — a few
  bytes.
- Environment health never returning `down` for a reachable Environment whose
  Signal is `down` — matches `docs/spec/v1.md` §5 ("unreachable → down"); by
  design, not drift.
- `docs/packaging.md`'s `DAKU_CHANNEL` row omitting `scripts/release.ts` as a
  reader — misleads nobody into a broken action.
- Adding a watchdog that auto-restarts a dead collector thread — 055 makes the
  death observable; self-healing is a separate design decision.
- Real pagination for drift's plugin inventory, and one-request-per-target for
  last-clone — both genuine options, both request-count trade-offs that need the
  Operator's call. Recorded as recommendations inside 057 and 058 instead.
