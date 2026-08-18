## Agent skills

### Issue tracker

Issues live in GitHub Issues for `martinthommesen/daku` (via `gh`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Git workflow

Trunk-based on `main` only — **no pull requests**, **no GitHub Actions**. See `docs/agents/git-workflow.md`.

### ServiceNow

Always use the `now-sdk` skill (`.claude/skills/now-sdk/SKILL.md`) for anything to do with ServiceNow or Fluent.

### Matt Pocock skills (mandatory)

The `mattpocock-skills:*` skills are the canonical, mandatory skills for this project. Always use the one that fits the situation — e.g. `tdd` for features/bugs, `diagnosing-bugs` for debugging, `code-review` for reviews, `research` for docs/API questions, `domain-modeling` for CONTEXT.md/ADRs, `codebase-design` for module design, `writing-for-agents` for CLAUDE.md/skills, `wizard` for human-only steps, `grilling` to stress-test plans, `prototype` for design questions, `resolving-merge-conflicts` for conflicts. Also `improve` for architecture audits and for generating plans under `plans/` (its `references/plan-template.md` is the template every plan uses). `improve-codebase-architecture` (`.claude/skills/`) is human-invoked only (`/improve-codebase-architecture`) — you cannot call it.

### Verification gate

There is no CI. Before committing to `main`, run `bun run check` (fmt check + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` + oxlint) and require exit 0. The gate includes `cargo clippy -- -D warnings`; do not add `#[allow]` to pass it without a comment saying why. Plans under `plans/` use it as a done criterion.
