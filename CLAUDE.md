## Agent skills

### Issue tracker

Issues live in GitHub Issues for `martinthommesen/daku` (via `gh`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### ServiceNow

Always use the `now-sdk` skill (`.claude/skills/now-sdk/SKILL.md`) for anything to do with ServiceNow or Fluent.

### Matt Pocock skills (mandatory)

The `mattpocock-skills:*` skills are the canonical, mandatory skills for this project. Always use the one that fits the situation — e.g. `tdd` for features/bugs, `diagnosing-bugs` for debugging, `code-review` for reviews, `research` for docs/API questions, `domain-modeling` for CONTEXT.md/ADRs, `codebase-design` for module design, `writing-for-agents` for CLAUDE.md/skills, `wizard` for human-only steps, `grilling` to stress-test plans, `prototype` for design questions, `resolving-merge-conflicts` for conflicts. Also `improve-codebase-architecture` (`.claude/skills/`) for architecture audits.
