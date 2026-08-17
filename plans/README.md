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
| [004](004-jobs-syslog-trends.md) | Scheduled jobs + syslog Signals with ~24h trends | P1 | TODO | 003 |
| [005](005-mid-ecc-signal.md) | MID/ECC Signal | P2 | TODO | 003 (soft: 004) |
| [006](006-outbound-signal.md) | Outbound/integration failures Signal | P2 | TODO | 003 (soft: 004) |
| [007](007-drift-last-clone.md) | Version/plugin drift + last-clone Signals | P2 | TODO | 003 |
| [008](008-health-rollup-protocol.md) | Environment health rollup + protocol events (+ samples) | P1 | TODO | **003** (stubs OK) |
| [009](009-gpui-shell-variant-c.md) | GPUI shell — sidebar + detail + job/syslog sparklines | P1 | TODO | 008 |
| [010](010-dmg-sparkle-homebrew.md) | Notarised DMG + Sparkle; Homebrew cask alternate | P3 | TODO | 009 |

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
