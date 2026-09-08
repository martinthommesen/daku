# Platform registry: ServiceNow, HTTP, GitHub

v1 (`docs/spec/v1.md` §10) was ServiceNow-only with the Platforms sidebar
group explicitly deferred (ADR-0005 amendment). This ADR admits the
second-and-third platform behind a compiled-in registry — no dynamic
plugins, no WASM, so the signing story and `clippy -D warnings` hold.

* **Config**: `EnvironmentConfig.platform` (`servicenow` | `http` |
  `github`, defaulting to ServiceNow so v1 files keep loading; unknown
  values rejected). GitHub URLs must look like
  `https://github.com/<owner>/<repo>`, enforced at load.
* **Collection**: `build_default_loop` dispatches per Environment —
  ServiceNow keeps all fifteen Signals, HTTP registers only its probe,
  GitHub only Actions. Drift and last-clone compare the ServiceNow subset
  only and stay unregistered with none. Probes never cross platforms: the
  HTTP probe hits `instance_url` verbatim, Actions hits `api.github.com`
  for the parsed repo, and neither reads ServiceNow reachability snapshots
  (`gated_by_availability() == false`).
* **Credentials**: the same store, keyed by Environment id. HTTP sends a
  Basic header only when a basic blob is stored (public status pages keep
  none); GitHub reuses the basic shape with the token as the password
  (username ignored) on a `Bearer` header.
* **Health**: the rollup is platform-agnostic (votes are votes);
  informational rules are per-Signal, not per-platform.
* **Desktop**: cards, headlines, explainer, and copy render
  `signal_ids_for(platform)`; unknown platform ids fall back to the
  ServiceNow set so a newer daemon never blanks an older desktop. The
  sidebar groups by platform only when more than one is configured —
  single-platform installs render exactly as before. The Environment sheet
  has no platform picker (v1): edits keep the stored platform, new
  Environments start as ServiceNow, other platforms are hand-declared
  (`docs/platforms.md`).

Explicitly still out: a fourth platform (copy the HTTP probe — it is the
thinnest template), OAuth for GitHub (a PAT in the stored blob covers one
Operator), CI/CD preview/commit calls (reads only; daku never mutates a
repo), paged run history beyond the newest 30.
