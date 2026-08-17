# Environments overview = sidebar + detail

The main screen is a **waku-like sidebar** (Platforms → Environments) plus an **Environment detail** pane (health, Signal cards, compare-vs-others strip). Chosen over environment columns and a Signal×Environment matrix in the [#13](https://github.com/martinthommesen/daku/issues/13) prototype (`prototypes/environments-overview/`). The matrix may return later as a secondary drift view, not the home screen.

**Amendment (2026-08-17):** the sidebar lists Environments only — the Platforms group is dropped until a second Platform exists (v1 is ServiceNow-only). Selecting a Signal card opens a **drill-in** region under the cards (see `CONTEXT.md` › Screen). Shell components come from gpui-component (ADR-0008).
