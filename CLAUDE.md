## Agent skills

### Issue tracker

Issues live in GitHub Issues for `martinthommesen/daku` (via `gh`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
`docs/solutions/` holds documented solutions to past problems (bugs, best practices, workflow patterns), organized by category with YAML frontmatter (module, tags, problem_type).

### Knowledge capture

After a solved, verified problem, automatically invoke the `ce-compound` skill with `mode:non-interactive` at the completion checkpoint only when the work produced durable project reasoning that is not readily recoverable from the final code, tests, types, comments, or existing documentation, and losing it would plausibly cause recurrence, material risk, or substantial rediscovery. Apply this counterfactual: if the learning document disappeared, would a future engineer reading the final implementation still be likely to repeat the mistake or redo substantial investigation? If not, do not invoke it. Completion, effort, and diff size alone are not enough. Capture at the checkpoint so a qualifying learning can ship in the PR that produced it, and only where the repository treats captured learnings as tracked, committed knowledge.

### Reports

Write every report, summary, or handoff to the user through the `ce-noslop` skill. This applies when you are the top-level agent writing to the user, not when you are a subagent reporting to its caller. Do not apply it to code, config, verbatim quotes, or text the user asked to post as written.

### Git workflow

Trunk-based on `main` only — **no pull requests**, **no GitHub Actions**. See `docs/agents/git-workflow.md`.

### ServiceNow

Always use the `now-sdk` skill (`.claude/skills/now-sdk/SKILL.md`) for anything to do with ServiceNow or Fluent.

### Matt Pocock skills (mandatory)

The `mattpocock-skills:*` skills are the canonical, mandatory skills for this project. Always use the one that fits the situation — e.g. `tdd` for features/bugs, `diagnosing-bugs` for debugging, `code-review` for reviews, `research` for docs/API questions, `domain-modeling` for CONTEXT.md/ADRs, `codebase-design` for module design, `writing-for-agents` for CLAUDE.md/skills, `wizard` for human-only steps, `grilling` to stress-test plans, `prototype` for design questions, `resolving-merge-conflicts` for conflicts. Also `improve` for architecture audits and for generating plans under `plans/` (its `references/plan-template.md` is the template every plan uses). `improve-codebase-architecture` (`.claude/skills/`) is human-invoked only (`/improve-codebase-architecture`) — you cannot call it.

### Verification gate

There is no CI. Before committing to `main`, run `bun run check` (fmt check + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` + oxlint + `tsc --noEmit`) and require exit 0. The gate includes `cargo clippy -- -D warnings`; do not add `#[allow]` to pass it without a comment saying why. `tsc --noEmit` type-checks every `.ts` file listed in `tsconfig.json`'s `include`; a new `.ts` file outside it is silently unchecked. Plans under `plans/` use it as a done criterion.
