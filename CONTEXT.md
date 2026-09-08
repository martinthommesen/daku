# daku

Infrastructure monitoring for platforms the owner/team runs — ServiceNow first.

## Language

**Platform**:
A product or system family daku can monitor: ServiceNow (the fifteen-Signal suite), generic HTTP probes, and GitHub Actions. Declared per Environment; the sidebar groups by platform once a second one is configured.
_Avoid_: integration, tool, vendor

**Environment**:
One concrete deployable instance of a Platform (e.g. ServiceNow prod, test, or dev; a status-page URL; a GitHub `owner/repo`). The Operator's config defines the list — defaults to prod/test/dev; not a fixed ceiling. A Personal Developer Instance (PDI) is a temporary stand-in for building daku, not a monitored Environment the team relies on.
_Avoid_: instance (ambiguous with "ServiceNow instance" in casual speech — prefer Environment when talking about daku's model), stage, tier

**Signal**:
A named observation daku collects from an Environment (availability, job backlog, error rate, …). v1 shipped seven for ServiceNow (availability/build, scheduled jobs, MID/ECC, syslog error rate, version/plugin drift, last-clone, outbound/integration failures); flow errors, email failures, upgrade history, sessions, table growth, slow transactions, update sets, and Instance Scan joined after.
_Avoid_: metric, check, probe, KPI

**Credential**:
Secrets that let daku read an Environment. Live in the macOS Keychain (daku-owned service); Environment URLs/labels live in `~/.daku/`. Never in git. Real Environments use OAuth client credentials; basic auth is only for PDI stand-ins.
_Avoid_: API key (too specific — auth method varies), token

**Operator**:
The person running daku. In v1 this is the platform owner on their own machine; not a multi-user role model.
_Avoid_: user, admin, viewer (those imply daku-side accounts we are not building in v1)

**Environment health**:
A rolled-up status for an Environment derived from its Signals: **healthy**, **degraded**, or **down**. Defaults are hard-coded; v1.1 allows per-Environment **threshold overrides**. Unreachable rolls `down`, asleep stays `healthy`, `last_clone` and `skipped` never vote.
_Avoid_: severity, priority, alert state

**Threshold override**:
Per-Environment configuration in `environments.json` that replaces a default Signal threshold for that Environment only (e.g. dev tolerates more overdue jobs than prod). Missing keys fall back to defaults; unknown keys are rejected at load.
_Avoid_: alert rule (implies a rules engine daku does not have), per-signal config

**Expected drift**:
Plugin/app ids or scopes declared in `environments.json` as planned differences from the clone source. Drift partitions mismatches into expected vs unexpected; only unexpected votes toward degraded. Rendered as "N differ · M expected".
_Avoid_: allowlist (implies security semantics), baseline

**Mute**:
A desktop preference (not daemon state) that silences attention surfaces for one Environment until a chosen time (1 h / 4 h / 24 h) or until unmuted. Stored in `app.json`. The daemon keeps collecting and history stays complete; the client stops notifying, badging, and dotting. Expired mutes clear automatically.
_Avoid_: silence (verb), snooze, acknowledge (implies on-call semantics daku does not have)

**Health event**:
A bounded, persisted record that an Environment's rolled-up health changed (for two consecutive publishes, so a single flap is not an event) or that its build string changed, including a bootstrap event on the first build observed. Written by `publish_dashboard`, published as its own `ServerMessage`, replayed to late subscribers. Rendered in the Recent timeline. Carries one optional Operator annotation.
_Avoid_: alert, incident, audit log

**Signal event**:
A bounded, persisted record that one Signal changed state, confirmed over two consecutive publishes like a health event. Skipped ticks leave streaks untouched. Merged into the Recent timeline beside health events.
_Avoid_: alert, incident, audit log

**Roll-up**:
An hourly aggregate over raw Signal samples (avg for latency, max for backlog/error counts) kept for 90 days. Raw 24 h samples answer "spiking now?"; roll-ups answer "normal for a Monday?" with a flat ~2160 points per Environment per Signal instead of ~86k raw points.
_Avoid_: downsampling (the rejected per-frame optimisation), archive

### Screen

**Environment detail**:
The main pane showing one selected Environment: its health, its Signal cards, and the compare strip. The sidebar lists Environments; selecting one shows its detail.
_Avoid_: dashboard (that is the whole window), page, view

**Signal card**:
The tile that shows one Signal for the selected Environment — its state, a one-line summary, and any diagnostic detail the daemon persisted.
_Avoid_: tile, widget, KPI, stat

**Compare strip**:
The row under the Signal cards that lines up build, drift, and last-clone across the other Environments.
_Avoid_: matrix (deferred secondary view — ADR-0005), comparison table

**Drill-in**:
The region that opens when a Signal card is selected and shows that Signal's rows (mismatched plugins, clone rows, a larger trend) with a link into the Environment itself.
_Avoid_: inspector, detail pane, panel, popup
