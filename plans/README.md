# daku v1 implementation plans

Handoff plans for executor agents implementing [docs/spec/v1.md](../docs/spec/v1.md).  
Map: [#17](https://github.com/martinthommesen/daku/issues/17). Index locked in [#19](https://github.com/martinthommesen/daku/issues/19).

**Planned against:** `da67ae9` (2026-08-17) for 004–007; foundation plans stamped at `b670982` / `da67ae9`.  
**Template:** `.agents/skills/improve/references/plan-template.md`  
**Public hygiene:** never put instance hostnames, usernames, or secrets in plans or commits — Operator-local `~/.daku/` / keychain only.

## Execution order

| Plan | Title | Priority | Status | Depends on |
|------|-------|----------|--------|------------|
| [001](001-import-waku-strip-agent.md) | Import pinned waku trees and strip agent domain until `cargo` workspace builds | P1 | pending | [inventory](https://github.com/martinthommesen/daku/blob/research/waku-fork-inventory/docs/research/waku-fork-inventory.md) |
| [002](002-daemon-sqlite-skeleton.md) | Daemon skeleton + SQLite under `~/.daku` | P1 | pending | 001 |
| [003](003-availability-signal.md) | Availability Signal (build/latency probe); fixtures + local PDI smoke | P1 | pending | 002 |
| [004](004-jobs-syslog-trends.md) | Scheduled jobs + syslog Signals with ~24h trends | P1 | pending | 003 |
| [005](005-mid-ecc-signal.md) | MID/ECC Signal | P2 | pending | 003 |
| [006](006-outbound-signal.md) | Outbound/integration failures Signal | P2 | pending | 003 |
| [007](007-drift-last-clone.md) | Version/plugin drift + last-clone Signals | P2 | pending | 003 |
| 008 | Environment health rollup + protocol events for UI | P1 | not authored | **003** (stubs OK) |
| 009 | GPUI shell — sidebar + Environment detail (variant C) | P1 | not authored | 008 |
| 010 | Notarised DMG + Sparkle; Homebrew cask alternate | P3 | not authored | 009 |

## Dependency graph

```text
inventory(#18) → 001 → 002 → 003 ┬→ 004
                                 ├→ 005
                                 ├→ 006
                                 ├→ 007
                                 └→ 008 → 009 → 010
```

Plans 004–007 may run in parallel after 003. Plan 008 may start after 003 with stub Signals.

## Conventions for executors

- Vocabulary: `CONTEXT.md` (Platform, Environment, Signal, Credential, Operator, Environment health).
- Licence: GPL-3.0-only (ADR-0002); keep `LICENSE` from waku copy.
- Branch naming: `plan/NNN-short-slug` unless the operator says otherwise.
- Commit style: imperative summary like existing `main` (`Add daku v1 hand-off spec…`).
- Do **not** push/PR unless asked.
- Live ServiceNow calls are **Operator-local smoke** only; CI uses fixtures.
