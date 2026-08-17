# Plan 034: Move the ServiceNow HTTP transport to `ureq` 3 with platform root certificates

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/Cargo.toml crates/daku-core/src/servicenow.rs`
> If either file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: MED (TLS behaviour change; verify against a real Environment)
- **Depends on**: plans/011-green-baseline-check-gate.md (gate)
- **Category**: migration
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/57

## Why this matters

`daku-core` polls ServiceNow with `ureq = "2"` (resolved 2.12.1, the last 2.x release, Dec 2024). ureq 2 trusts **only** the bundled Mozilla store (`webpki-roots`); it does not consult the macOS Keychain. The Operators this tool targets commonly sit behind corporate TLS interception or use private CAs — exactly the case where every ServiceNow request fails with a certificate error while the daemon's own WebSocket client (tungstenite with `rustls-tls-native-roots`) trusts the Keychain fine. Two root stores, inconsistent behaviour, one of them on an end-of-life major.

The transport is already isolated behind `HttpTransport` (one struct, ~50 lines); every test uses fake transports, so the migration touches one file plus `Cargo.toml`. After this plan: `ureq` 3 with the platform verifier, 30 s timeout preserved, `Authorization` not forwarded across redirects, both `webpki-roots` copies gone from `daku-core`'s tree.

## Current state

`crates/daku-core/Cargo.toml:21`: `ureq = "2"`. `crates/daku-core/Cargo.toml:16`: `rusqlite = { version = "0.37", features = ["bundled"] }` (latest in the local index: 0.40.2 — optional Step 4).

`crates/daku-core/src/servicenow.rs`:

```rust
// :39-41
pub trait HttpTransport: Send + Sync {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse>;
}

// :16-29  (HttpRequest { method: String, url: String, headers: Vec<(String,String)>, body: Option<String> };
//          HttpResponse { status: u16, headers: Vec<(String,String)>, body: String })

// :295-345
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        Self { agent }
    }
}

impl HttpTransport for UreqTransport {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let mut call = match request.method.as_str() {
            "GET" => self.agent.get(&request.url),
            "POST" => self.agent.post(&request.url),
            other => return Err(anyhow!("unsupported HTTP method {other}")),
        };
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let response = match &request.body {
            Some(body) => call.send_string(body),
            None => call.call(),
        };
        match response {
            Ok(response) | Err(ureq::Error::Status(_, response)) => read_ureq_response(response),
            Err(error) => Err(error.into()),
        }
    }
}

fn read_ureq_response(response: ureq::Response) -> anyhow::Result<HttpResponse> {
    let status = response.status();
    let headers = response
        .headers_names()
        .into_iter()
        .filter_map(|name| response.header(&name).map(|value| (name, value.to_owned())))
        .collect();
    let body = response.into_string().context("reading HTTP body")?;
    Ok(HttpResponse { status, headers, body })
}
```

Only these four `ureq::` references exist in the crate (`grep -n ureq crates/daku-core/src/*.rs` → lines 297, 302, 324, 330). Callers rely on: non-2xx statuses returned as `Ok(HttpResponse)` (401 triggers the OAuth refresh at `:108-118`, 429 triggers retry at `:193-205`), header lookup via `HttpResponse::header(name)` (case-insensitive — check its impl near `:31-37`), and body as `String`.

Dependency tree at HEAD (`cargo tree --workspace -i webpki-roots@0.26.11`): `webpki-roots 0.26.11 └── ureq 2.12.1 └── daku-core`; `webpki-roots 1.0.9` is pulled only by 0.26.11. `rustls-native-certs 0.8.4` is already present via `tungstenite` (both `daku-client` and `daku-core`).

**ureq 3 API (from the 3.1.4 crate source in the local cargo cache — latest is 3.4.0; the executor MUST confirm names against `cargo doc -p ureq --open` or docs.rs for the version resolved, this is a sketch, not gospel):**

- Features: default = `["rustls", "gzip"]`; `platform-verifier` enables `RootCerts::PlatformVerifier` (uses `rustls-platform-verifier`, i.e. the macOS trust store); `native-tls` is the alternative (Secure Transport roots).
- `ureq::Agent::config_builder().timeout_global(Some(Duration)).http_status_as_error(false).tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build()).build()` → `Config`; `ureq::Agent::new_with_config(config)`.
- `redirect_auth_headers` defaults to `RedirectAuthHeaders::Never` (config.rs:880 in 3.1.4) — same as the ureq 2 behaviour we rely on; assert it in a comment, do not change it.
- `agent.get(&url)` → `RequestBuilder<WithoutBody>` with `.header(k, v)` and `.call()`; `agent.post(&url)` → `RequestBuilder<WithBody>` with `.header(k, v)` and `.send(&str)` (`AsSendBody` is implemented for `&str`/`String`).
- Response is `http::Response<ureq::Body>`: `response.status().as_u16()`, `response.headers().iter()` (`HeaderName`/`HeaderValue`, `value.to_str()`), `response.body_mut().read_to_string()` (or `into_body().read_to_string()`).
- With `http_status_as_error(false)`, 4xx/5xx come back as `Ok(response)` — that replaces the `Err(ureq::Error::Status(_, response))` arm.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Docs for the resolved ureq | `cargo doc -p ureq --no-deps` then open `target/doc/ureq/index.html` | builds |
| Check | `cargo check --workspace --all-targets` | exit 0 |
| Client tests | `cargo test -p daku-core servicenow` | all pass (fake transports; unchanged) |
| Roots gone | `cargo tree --workspace -i webpki-roots` | not found, or only via a non-daku path |
| Verifier present | `cargo tree --workspace -i rustls-platform-verifier` | reachable from `ureq` ← `daku-core` |
| Operator smoke (needs a real Environment + Keychain item) | `cargo run -p daku-daemon -- probe-availability` | `availability probe complete`; snapshot `reachability: reachable` |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/Cargo.toml` (`ureq` line; optionally `rusqlite`)
- `crates/daku-core/src/servicenow.rs` (`UreqTransport`, `read_ureq_response` only)
- `Cargo.lock` (regenerated)
- `crates/daku-core/README.md` (one clause if you mention TLS roots — optional)
- `plans/README.md` (status row)

**Out of scope**:
- `HttpTransport`, `HttpRequest`, `HttpResponse`, `ServiceNowClient` and every test — must be untouched.
- `crates/daku-client` tungstenite TLS config — already native roots.
- Proxy support (`https_proxy`), custom CA files — not requested; ureq 3 honours proxy env vars by default (verify, note in Maintenance).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Move ServiceNow transport to ureq 3 with platform root certificates.`

## Steps

### Step 1: Bump the manifest

`crates/daku-core/Cargo.toml`: replace `ureq = "2"` with

```toml
ureq = { version = "3", default-features = false, features = ["rustls", "gzip", "platform-verifier"] }
```

Run `cargo check -p daku-core` — it will fail on the four `ureq::` sites; that is expected. Read the resolved version's docs now (`cargo doc -p ureq --no-deps`).

**Verify**: `grep -n '^ureq' crates/daku-core/Cargo.toml` → the line above; `grep -n -A1 '^name = "ureq"$' Cargo.lock` → `version = "3.x.y"`.

### Step 2: Rewrite `UreqTransport`

Replace `Default for UreqTransport`, `HttpTransport for UreqTransport`, and `read_ureq_response` with the ureq 3 equivalents. Target shape (adjust names to the docs you read in Step 1):

```rust
impl Default for UreqTransport {
    fn default() -> Self {
        // Platform verifier = macOS Keychain roots, so Environments behind
        // corporate TLS interception / private CAs work like the WS client.
        // redirect_auth_headers stays at ureq's default `Never`.
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Self { agent: ureq::Agent::new_with_config(config) }
    }
}

impl HttpTransport for UreqTransport {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let response = match request.method.as_str() {
            "GET" => {
                let mut call = self.agent.get(&request.url);
                for (name, value) in &request.headers {
                    call = call.header(name.as_str(), value.as_str());
                }
                call.call()
            }
            "POST" => {
                let mut call = self.agent.post(&request.url);
                for (name, value) in &request.headers {
                    call = call.header(name.as_str(), value.as_str());
                }
                call.send(request.body.as_deref().unwrap_or(""))
            }
            other => return Err(anyhow!("unsupported HTTP method {other}")),
        };
        read_ureq_response(response?)
    }
}

fn read_ureq_response(mut response: http::Response<ureq::Body>) -> anyhow::Result<HttpResponse> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned())))
        .collect();
    let body = response.body_mut().read_to_string().context("reading HTTP body")?;
    Ok(HttpResponse { status, headers, body })
}
```

`http` is re-exported by ureq 3 (`ureq::http`) — use `ureq::http::Response` if the `http` crate is not a direct dependency (do **not** add `http` to `Cargo.toml` if `ureq::http` works). Keep the transport-error path returning `Err` (timeouts, TLS failures) — that is what `send()` propagates and what collectors persist as `down`.

**Verify**: `cargo check --workspace --all-targets` → exit 0. `cargo test -p daku-core` → all pass (no test touches `UreqTransport`; confirm with `grep -n UreqTransport crates/daku-core/src/*.rs` — only `collector.rs` constructs it for the real loop).

### Step 3: Confirm the tree and behaviour

**Verify**: `cargo tree --workspace -i webpki-roots` → no `daku-core` path (if `rustls-platform-verifier` itself pulls a `webpki-roots` fallback, note it — that is acceptable); `cargo tree --workspace -i rustls-platform-verifier` → reachable via `ureq`. Operator-local smoke (only if you have a configured Environment): `cargo run -p daku-daemon -- probe-availability` → `availability probe complete` and the availability snapshot's payload shows `"reachability":"reachable"` (e.g. `sqlite3 ~/.daku/app.db "select payload_json from signal_snapshots where signal_id='availability'"`). If no Environment is configured, record "smoke not run" in the status row.

### Step 4 (optional, only if trivial): `rusqlite` bump

Change `rusqlite = { version = "0.37", …}` to `"0.40"` and run `cargo check --workspace --all-targets`. If it compiles and `cargo test -p daku-core` passes unchanged, keep it; if any code change is required, **revert** to 0.37 and note it (out of scope here).

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- No new tests: every ServiceNow test uses `ScriptedTransport`/fixture transports and must pass unchanged (`cargo test -p daku-core servicenow` and the collector tests).
- Manual: Operator-local `probe-availability` against a real Environment (records reachability through the new TLS stack).

## Done criteria

- [ ] `grep -n -A1 '^name = "ureq"$' Cargo.lock` → `version = "3.`
- [ ] `grep -n 'AgentBuilder\|send_string\|into_string\|headers_names\|Error::Status' crates/daku-core/src/servicenow.rs` → no matches
- [ ] `grep -n 'PlatformVerifier' crates/daku-core/src/servicenow.rs` → 1 match
- [ ] `cargo tree --workspace -i webpki-roots` shows no path through `daku-core` (or a documented verifier fallback)
- [ ] `cargo test -p daku-core` → 0 failed, no test files modified (`git status` shows only `Cargo.toml`, `Cargo.lock`, `servicenow.rs`, `plans/README.md`)
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 034 updated (including whether the Operator smoke ran)

## STOP conditions

- The resolved ureq 3.x has no `platform-verifier` feature / `RootCerts::PlatformVerifier` (API moved) — report the docs you found instead of improvising an alternative TLS setup.
- `redirect_auth_headers` default is not `Never` in the resolved version — set it explicitly to `Never` and note; if the type is missing, STOP.
- Any existing `servicenow`/collector test needs modification to pass.
- Step 4 (`rusqlite`) needs any Rust code change — revert it.

## Maintenance notes

- ureq 3 reads `https_proxy`/`HTTP_PROXY` by default (verify in the docs of the resolved version); that is the standard convention and matches how the WS client behaves — document it in README if an Operator asks.
- If a private-CA-only Environment still fails, `RootCerts::Specific` accepts an explicit PEM list — that would be a new `~/.daku` setting, not a code default.
- Reviewers: check that non-2xx statuses still return `Ok(HttpResponse)` (OAuth 401 refresh and 429 retry depend on it) and that the 30 s timeout is global, not per-call.
