# v1.1 attention surfaces, bounded history, and per-Environment thresholds

v1 (`docs/spec/v1.md`) shipped a dashboard-only console: open and look, no
alerts, hard-coded thresholds, a ~24 h sample ring, no alert-history store.
The usefulness batch keeps that posture — still no pager, no rules engine, no
on-call semantics, no acknowledgement, no second Platform — and admits exactly
what the Operator asked for:

* **One attention channel**: macOS notifications for Environment health
  transitions. One updating notification per Environment (flaps update, not
  stack). Cached replay on reconnect (including deliberate reload) marks seen
  without firing. Click takes the Operator to that Environment. Muted
  Environments stay silent. Global off switch.
* **Two ambient surfaces**: menu-bar dot + Dock badge/count derived from
  existing `worst_health()`, muted excluded. Passive only; the interactive
  menu-bar dropdown is a separate, abandonable step.
* **Client-side mutes** (`074`): desktop preference in `app.json`, 1 h / 4 h /
  24 h / unmute from the Environment header. Daemon keeps collecting; history
  stays complete; client stops shouting. Precondition for the surfaces above
  being tolerable through planned outages.
* **Per-Environment threshold overrides + expected drift** (`073`): defaulted
  `thresholds` object and `expected_drift` list on `EnvironmentConfig` in
  `~/.daku/environments.json`. Missing keys fall back to current consts;
  unknown keys reject at load. Availability gains an RTT threshold it never
  had. Drift says "N differ · M expected"; only unexpected votes degraded.
* **Two sanctioned history tables** (amending ADR-0007):
  1. `health_events` (`072`) — bounded log written by `publish_dashboard`
     on rollup change for two consecutive publishes + on build-string change,
     including a bootstrap event on first build observed. Published as its own
     `ServerMessage` with its own Hub cache key.
  2. `signal_rollups_hourly` (`078`) — idempotent hourly aggregates (avg for
     latency, max for backlog/error counts), 30 d retention, own message so the
     24 h raw series is untouched. Drill-in gains a 24 h / 7 d / 30 d switch.
* **Reload without relaunch** (`070`): `DaemonClient::shutdown` + supervisor
  respawn re-reads config and ticks immediately (~0.5 s measured). Gated to
  local daemons only. Menu item + `Cmd-R`. No protocol change.
* **Environment management GUI** (`080`): daemon writes `environments.json`
  atomically + Keychain item on explicit Operator command over the
  authenticated loopback socket (amending ADR-0004). Refused with
  `--allow-non-loopback`. Secret never in log/argv/DB/JSON. Ends by triggering
  the `070` reload.

Explicitly still out: connector/plugin seam for a second Platform, login/roles,
pager rules/channels/quiet-hours/ack, waku agent domain, non-ServiceNow
Platforms, hosted multi-user operation.

Consequences:

* Protocol bumps once per wave that needs one (`072`, `078`, `080`) — always
  increment live `PROTOCOL_VERSION`, never set a fixed number.
* Migrations once per wave that needs one (`072` → `0001`, `078` → `0002`) —
  gapless numeric prefixes (`crates/daku-core/build.rs` asserts).
* Payload additions pin to `payload_contract.rs` + `payloads.json`
  (`DAKU_BLESS_PAYLOADS=1`) and keep `fixture_events()` in parity.
* `doctor` prints effective thresholds; `payload_contract` pins new keys.
* Prior rejections reversed by this ADR: sparkline down-sampling (now
  justified at 86k raw points vs 720 hourly) and the "no history" half of the
  upgrade-marker rejection (build events now exist; a separate marker Signal
  is still rejected — drift already flags it).
