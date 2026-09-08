# Platforms

daku watches Environments grouped by platform. ServiceNow is the full
fifteen-Signal suite; HTTP and GitHub are single-Signal probes that reuse
the same health rollup, notifications, mutes, thresholds, and history.

Declare the platform per Environment in `~/.daku/environments.json`
(missing reads as `"servicenow"`), or pick it in the Environment sheet:
ServiceNow, HTTP probe, or GitHub Actions, each with its own URL hint and
validation (GitHub URLs must name one `owner/repo`). Changing the platform
on edit keeps thresholds and expected drift; the Credential shape is
auth-method-dependent, so nothing else migrates.

## HTTP — any status page or health endpoint

```json
{
  "id": "status",
  "label": "Status page",
  "instance_url": "https://status.example.com/health",
  "platform": "http",
  "auth_method": "basic",
  "sort_order": 3
}
```

The probe GETs the URL verbatim: `2xx`–`3xx` is healthy with the
round-trip time, anything else (including transport errors) is down. Query
strings and fragments never reach the snapshot. No Credential is needed
for public pages; a stored basic blob rides as a Basic header.
`http_probe_rtt_degraded_ms` (off by default, sheet: "Probe RTT ms (off)")
degrades slow-but-up targets. The card links the target itself.

## GitHub — Actions failures for one repo

```json
{
  "id": "app-ci",
  "label": "App CI",
  "instance_url": "https://github.com/acme/app",
  "platform": "github",
  "auth_method": "basic",
  "sort_order": 4
}
```

Store a personal access token with `security add-generic-password -U -s
daku -a app-ci -w` as `{"username":"token","password":"<pat>"}` (the
username is ignored; the password rides a `Bearer` header). The Signal
counts runs concluding `failure`/`timed_out` in the last 24 h (newest 30
runs); `actions_failed_degraded_at` (default 1) votes. Rows link each run
page; the card links the repo's Actions page. Reads only — daku never
triggers, cancels, or re-runs anything.

## Adding a fourth platform

Copy `crates/daku-core/src/http_probe.rs`: a `Signal` impl, a threshold if
it votes, registration in `build_default_loop`'s platform match, payload
cases in `payload_contract.rs` (re-bless with `DAKU_BLESS_PAYLOADS=1`),
fixture parity in `fixture_events_at` (`src/dashboard_state.rs`), card
rendering behind `signal_ids_for`, and a section here. No plugin machinery
to satisfy.
