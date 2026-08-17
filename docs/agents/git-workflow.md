# Git workflow

This repo is **trunk-based on `main`**. Pull requests and GitHub Actions are **disabled** and must not be used.

## Rules

1. **Commit and push to `main`.** There is no PR review gate and no Actions CI. Local verification is the quality gate: `bun run check` (see `package.json`; described in `CLAUDE.md`) must exit 0, plus each `plans/*` Done criteria.
2. **Do not open pull requests.** Do not create `.github/workflows/`. Do not ask for PR URLs or Actions runs.
3. **Do not leave long-lived topic branches.** Prefer committing on `main`. Short-lived local branches are fine for experiments; delete them when finished. Do **not** push topic/research/prototype branches to `origin` as a standing archive — land useful artifacts on `main` (e.g. under `docs/research/`, `prototypes/`) then delete the branch.
4. **`main` is protected** (no deletion, linear history, signed commits). Other refs are disposable.
5. **Issues** remain the tracker (`docs/agents/issue-tracker.md`). Specs and decisions stay in issues + `docs/` + `CONTEXT.md` / ADRs.

## Pruning (agents / maintainers)

After landing work that lived on a side branch:

```sh
# delete remote topic branches (never main)
git push origin --delete <branch> …

# delete local branches
git branch -D <branch> …

git fetch --prune
git branch -a   # expect only main (+ origin/main)
```

## Why

Operator-local product; public GPL source of truth is `main`. Branch sprawl and PR/Actions ceremony do not fit this repo’s operating mode.
