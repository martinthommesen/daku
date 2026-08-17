# ServiceNow API access for an external read-only poller

Research note for [#3](https://github.com/martinthommesen/daku/issues/3) (map: #1). Question: how should daku, a headless external monitor, talk to three ServiceNow instances (prod / test / dev) safely?

Sources are primary unless marked **[S]** (secondary: ServiceNow Community, third party). ServiceNow docs are the Fluid Topics site `servicenow.com/docs`; the current indexed release is "Australia" (Zurich+1); topic bodies were read through the site's content API and are cited by canonical URL. Now SDK topics are cited as `now-sdk explain <topic>`. Anything not traceable to a primary source is listed under "Unverified" at the end.

## TL;DR recommendation

1. **Auth: OAuth 2.0 client credentials** against `POST /oauth_token.do` (`grant_type=client_credentials`), one OAuth API endpoint per instance, mapped to a dedicated *OAuth Application User* that is a `sys_user` with **Web service access only** (Identity type = Machine) and only the read roles/ACLs it needs. Cache the access token (default 30 min lifetime, no refresh token in this grant, just re-request). Requires the instance property `glide.oauth.inbound.client.credential.grant_type.enabled=true` (Washington DC+). Upgrade path with no shared secret: **inbound JWT bearer** grant. Fallback for a quick PDI spike only: basic auth on a WSAO account.
2. **Roles:** there is no single "monitoring" role. Give the service account (a) `snc_read_only` as a hard write-guard, plus (b) the smallest existing read role per signal family (`itil` for task/incident/change; most platform-health tables like `syslog`, `sys_trigger`, `sys_cluster_state`, `sys_properties`, `sys_email` are `admin`-read out of the box), or better (c) one custom role `x_daku_reader` with explicit read ACLs on exactly the tables daku polls. Verify with Access Analyzer / `sys_security_acl` per instance because ACLs differ per instance.
3. **Rate limits:** no OOB per-user quota is enforced unless an admin creates a rule; rules are per REST resource, per hour, per user/role/all users, in `sys_rate_limit_rules`; observed via `X-RateLimit-Limit`, `X-RateLimit-Reset`, `X-RateLimit-Rule`, and on denial `429` + `Retry-After`. Poll politely anyway (`sysparm_fields`, `sysparm_limit`, `sysparm_query=sys_updated_on>…`, `sysparm_no_count`, Aggregate API for counts).
4. **PDIs:** same platform, same auth options in principle, but a PDI hibernates after ~6 h without interactive activity, returns an HTML "hibernating" page to HTTP calls, has no programmatic wake, and (policy since 2026-07-11) is reclaimed if ≥90 days old with no explicit UI login for 10 days — background/API activity does not count. Fine as a dev target for daku, unfit as a monitored "environment" you rely on.
5. **Instance-health APIs:** none that a customer can call generically. Instance Scan (config quality, REST under `/api/sn_cicd/instance_scan/*`), Impact Instance Observer (paid, portal), Log Export Service (Kafka pull for `syslog`/`sys_audit`), Health Log Analytics (ITOM licence). Reading tables via the Table/Aggregate API remains the practical route for daku's signals.

---

## 1. Inbound REST authentication options

| Method | Status / how | Notes for a headless poller |
|---|---|---|
| **Basic** | Supported ("By default, ServiceNow REST APIs use basic authentication or OAuth"). Since Yokohama MFA is not required by default for basic-auth REST. Docs now call it a **legacy** method, "strongly discouraged over token-based authentication methods"; new (zBoot) instances restrict it by default. Restriction feature: `glide.authenticate.basic_auth.restriction.enforce`, `glide.authenticate.basic_auth.restriction.default_decision`; when enforced, basic-auth is blocked unless the account is Web Services Access Only, presents an MFA OTP, or has role `snc_basic_auth_api_access`. | Works everywhere today (incl. PDIs), simplest to spike. Password on every request; expect it to be blocked on new/hardened instances. If used at all: WSAO account. |
| **OAuth 2.0 – client credentials** | Supported inbound since Washington DC **[S]** (docs exist from Xanadu on). Docs: "Use the OAuth client credentials grant type for back-end services or automated integrations that access ServiceNow APIs without user interaction." Requires: OAuth 2.0 plugin; property `glide.oauth.inbound.client.credential.grant_type.enabled=true` (must be created, default absent/false); an OAuth API endpoint for external clients (`oauth_entity`) with Grant type incl. Client Credentials, Public Client = false, and the **OAuth Application User** field set — "Any authorization request with the Grant Type as Client Credentials, Client ID, and Secret is passed for the associated OAuth Application User"; if it isn't set or the property is false "the authorization request isn't passed". "You must use the REST API Auth Scope with client credentials grant type to control the access provided to the 3rd party client." Token: `POST /oauth_token.do` `grant_type=client_credentials&client_id&client_secret[&scope]`. | **Recommended.** Access token default 30 min (1800 s); no refresh token in this grant — fetch a new one. Now SDK's own CI mode uses exactly this flow (`SN_SDK_AUTH_TYPE=oauth`, hits `/oauth_token.do` once per run). |
| **OAuth 2.0 – JWT bearer (inbound)** | Supported. Application Registry > "Create an OAuth JWT API endpoint for external clients"; RS256/384/512, ES256/384/512; claims `iss` (client id), `sub` (mapped to a user via User Field), `aud`, `exp`, `iat`; JWT Verifier Map (kid + shared key or `sys_certificate`); `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion=…`. Docs: "either as itself or on behalf of a user … without requiring user interaction or storing a shared secret"; positioned as "a more secure alternative to the client credentials grant". | Best security posture (private key never leaves daku); more setup (key pair + verifier map per instance). Upgrade path from client credentials. |
| **OAuth 2.0 – authorization code (+PKCE) / refresh** | Supported; auth-code and password flows return `refresh_token` ("can be used to request additional access tokens"); refresh token default lifespan 100 days (8 640 000 s), access token 30 min. Both lifespans are fields on `oauth_entity`. | Needs a browser + human once per 100 days per instance; wrong shape for a daemon. |
| **OAuth 2.0 – resource owner password (ROPC)** | Supported but "recommended only in legacy or controlled environments"; hardening kill-switch `glide.oauth.inbound.ropc.grant_type.disabled` (default false, recommended true; "OAuth 2.1 has deprecated ROPC"). | Avoid. |
| **API key (`x-sn-apikey`)** | Plugin "API Key and HMAC Authentication" (`com.glide.tokenbased_auth`, deps `com.glide.rest.auth.scope`, `com.glide.rest.policy`, `com.glide.auth.scope`); roles `api_service_admin`, `adaptive_auth_policy_admin`. Setup: Inbound Authentication Profile (Auth Parameter `x-sn-apikey: Auth Header` or `Query Parameter`) → REST API Key record (User, Auth Scope, Token, Expiry — "Empty value means no expiration") → a **REST API Access Policy per API/version/method** referencing the profile; "Token Based Auth isn't allowed in the Global REST API Policy". Introduced Washington DC **[S]**. | Simplest token option, but needs one access policy per API used (Table API, Aggregate API) per instance. Reasonable second choice. |
| **Mutual TLS / client certificate (inbound)** | Plugin Certificate-based authentication (`com.glide.auth.mutual`): "Enable mutual authentication for inbound web services … mutually authenticate requests to access ServiceNow REST and SOAP APIs." Requires ADCv2 load balancer, CA chain registered (`sys_ca_certificate`), client PEM mapped to a user (`sys_user_certificate`); not for on-prem/self-hosted or Edge Encryption. The older "Configure mutual authentication" (JKS/protocol profile) is **outbound only**. | Works but heavy (LB prerequisite, cert lifecycle per instance). Not needed for a read-only poller. |

Additional guardrails worth using regardless of grant:

- **Web service access only** (`web_service_access_only`, from the Non-Interactive Sessions plugin, default on): "Non-interactive users can only use their credentials to authorize API connections … They can't log in to the ServiceNow UI." Auto-set when Identity type = Machine on the user form. Machine Identity Console flags integration accounts without it as a finding, and "Machine identity access controls" (per-API/table restrictions) apply to WSAO users. Note from `now-sdk explain ci-integration`: the SDK's own CI flow needs Identity type = Human because it also drives `/angular.do`; a pure Table/Aggregate API client does not, so Machine is correct for daku.
- **REST API access policies** can pin an API to an auth type + IP range ("allows only OAuth 2.0 authentication type from a specified range of IP addresses").
- "Internal Integration User" is a different flag (bypasses WS-Security for MID/ODBC); do not use it for daku.

Sources:
- REST API basics / basic auth / MFA: https://www.servicenow.com/docs/r/zurich/api-reference/rest-api-explorer/c_RESTAPI.html
- Basic authentication (legacy, zBoot restriction): https://www.servicenow.com/docs/r/platform-security/authentication/basic-authentication.html ; restriction feature: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/basic-auth-restriction.html
- OAuth inbound overview (grants, guidance): https://www.servicenow.com/docs/r/zurich/platform-security/authentication/oauth-inbound.html
- Client credentials grant: https://www.servicenow.com/docs/r/platform-security/authentication/client-credential-grant.html ; property: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/create-cc-sys-prop.html ; OAuth Application User: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/add-oauth-application-user.html ; workflow: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/client-credentials-grant-workflow.html ; introduced in Washington DC **[S]**: https://www.servicenow.com/community/developer-blog/up-your-oauth2-0-game-inbound-client-credentials-with-washington/ba-p/2816891
- Token lifetimes (`oauth_entity`, `oauth_credential`): https://www.servicenow.com/docs/r/zurich/platform-security/authentication/t_CreateEndpointforExternalClients.html ; https://www.servicenow.com/docs/r/zurich/platform-security/authentication/t_ManageTokens.html
- JWT bearer inbound: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/create-jwt-endpoint.html
- Auth code + refresh: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/c_OAuthAuthorizationCodeFlow.html
- ROPC hardening: https://www.servicenow.com/docs/r/zurich/platform-security/instance-security-hardening-settings/sc-disable-resource-owner-password-credentials-ropc-in-oauth-2-token-grants.html
- API key & HMAC: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/api-key-and-hmac-rest-apis.html ; configure: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/configure-api-key.html
- Certificate-based (inbound mTLS): https://www.servicenow.com/docs/r/zurich/platform-security/certificate-based-authentication/set-up-mutual-auth.html ; outbound-only mutual auth: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/c_MutualAuthentication.html
- WSAO / non-interactive sessions: https://www.servicenow.com/docs/r/zurich/platform-administration/user-administration/c_NonInteractiveSessions.html ; https://www.servicenow.com/docs/r/zurich/platform-administration/user-administration/t_CreateAUser.html ; Machine identity: https://www.servicenow.com/docs/r/zurich/platform-security/identity/integration-accounts-wsa-false.html ; https://www.servicenow.com/docs/r/zurich/platform-security/access-control/machine-identity-access-controls.html
- REST API access policies: https://www.servicenow.com/docs/r/zurich/platform-security/authentication/inbound-authentication-profile.html
- Now SDK: `now-sdk explain ci-integration` (auth types basic/oauth, `SN_SDK_AUTH_TYPE`, `/oauth_token.do`, OAuth Application User, KB1645212), `now-sdk explain alias-template-guide` (platform's own list of auth templates: Basic, API Key, OAuth auth code, client credentials, JWT bearer).

## 2. Minimum roles for read-only monitoring

Facts:

- **`snc_read_only`** (plugin `com.snc.read_only.role`) "restricts a user … to read-only access on the tables to which the user already has access. This role is designed to complement other roles" — it grants nothing by itself; it blocks insert/update/delete everywhere (even when impersonating admin), plugin activation, SQL, XML upload, background scripts. Exemption properties `glide.security.snc_read_only_role.tables.exempt_{create,write,delete}`. Assign only to users. → Perfect belt-and-braces for daku's account, not a source of read access.
- **`admin`** "can override access control list (ACL) rules and pass all role checks" — never give it to the poller.
- **`itil`**: "ITIL users can open, update, close incidents, problems, changes, and read some rules, definitions, and CIs related to CMDB features. This role is the base system technician role." It contains `cmdb_read`, `snc_platform_rest_api_access`, `sn_incident_write`, `sn_change_write`, … — i.e. it is a *write* role for the task family; pair it with `snc_read_only` if used.
- **`snc_platform_rest_api_access`** (Base system roles: "Allows access to Platform Rest APIs" — Table, Import Set, Aggregate, Attachment API; contained in `itil`). Whether it is *enforced* depends on the per-API `REST_Endpoint` execute ACL, which is inactive by default and can be activated per API (**[S]** community + KB2976437). Give it to the poller account regardless; it grants nothing else.
- `snc_read_only` default exemption lists already include `sys_user_session`, `sysevent`, `syslog`, `syslog_transaction`, `sys_user_preference` so a read-only account's own login/logging still works.
- Flow Designer read roles exist: `flow_operator` (view executions/dashboards/logs), `flow_report_viewer`, `fd_read_operations` / `fd_read_operations_all`.
- Table API rule: "The calling user must have sufficient roles to access the data in the table specified in the request"; `sysparm_limit` is applied **before** ACL evaluation, so ACL-filtered pages can come back short or empty — a symptom of missing read ACLs, not of missing data.
- Rate-limit admin role is `rate_limit_admin`; OAuth setup `oauth_admin`; API key setup `api_service_admin` (see §1, §3).
- ACLs are **per instance** and customers change them. Verify with **Access Analyzer** ("analyze and view the permissions of users, groups, roles for a table … and REST endpoints"; impersonates the identity, reports allowed/denied and why), **Debug Security Rules** (System Security > Debugging), or by reading `sys_security_acl` filtered on table + operation `read`, rather than trusting a static table.

Per signal family (base-system defaults; **verify per instance**):

| Signal family | Tables | Read access in base system |
|---|---|---|
| Work / ITSM volume | `task`, `incident`, `problem`, `change_request`, `sysapproval_approver` | `itil` |
| CMDB | `cmdb_ci*` | `cmdb_read` (contained in `itil` and `asset`) |
| Users | `sys_user`, `sys_user_has_role` | `user_admin` for full; `sys_user` limited read for any role |
| Scheduled jobs | `sys_trigger`, `sysauto_script` | `admin` |
| System / transaction logs | `syslog`, `syslog_transaction` | not documented; `admin` in practice (no OOB "log reader" role in the base roles list) |
| Email | `sys_email` | `admin` |
| MID Server / ECC | `ecc_agent`, `ecc_queue` | `mid_server` / `admin` |
| Import / integration | `sys_import_set`, `sys_import_set_run`, `sys_data_source` | `import_admin` / `import_transformer` |
| Flow Designer executions | `sys_flow_context` | `flow_operator`, `fd_read_operations[_all]`, `flow_report_viewer` |
| Update sets / upgrades | `sys_update_set`, `sys_upgrade_history` | `admin` |
| Node / cluster | `sys_cluster_state` | `admin` |
| Events | `sysevent` | `admin` |
| Properties / dictionary | `sys_properties`, `sys_dictionary`, `sys_db_object` | `admin`; `personalize_dictionary` for dictionary |
| Rate limits | `sys_rate_limit_rules`, counts/violations related lists | `rate_limit_admin` |
| Instance Scan results | `scan_*` tables | `scan_user` (per Instance Scan roles page **[S]** snippet) |

Recommendation: create **one custom role `x_daku_reader`** on each instance with explicit `read` ACLs (table-level, no field ACLs) on exactly the tables above that daku's signals need, add `snc_read_only`, set Identity type Machine / WSAO, and use it as the OAuth Application User. Where a customer prefers OOB roles, `itil` covers the ITSM family but the platform-health tables still need `admin`-level reads, which is exactly what the custom role avoids. Only `admin`-read tables that daku genuinely needs should get ACLs; everything else stays closed.

Sources:
- Read-only role: https://www.servicenow.com/docs/r/platform-administration/user-administration/c_ReadOnlyRole.html
- Base system roles (admin, itil, snc_platform_rest_api_access, cmdb_read, mid_server, import_admin, …): https://www.servicenow.com/docs/r/platform-administration/user-administration/r_BaseSystemRoles.html
- Flow Designer access roles: https://www.servicenow.com/docs/r/build-workflows/workflow-studio/user-access-flow-designer.html
- MID Server user/role: https://www.servicenow.com/docs/r/servicenow-platform/mid-server/t_SetupMIDServerRole.html
- Access Analyzer: https://www.servicenow.com/docs/r/platform-security/access-control/explore-access-analyzer.html ; ACL debugging: https://www.servicenow.com/docs/r/platform-security/access-control/c_AccessControlRulesDebug.html ; ACL types (execute = REST endpoint): https://www.servicenow.com/docs/r/platform-security/access-control/acl-rule-types.html
- Table API (roles note, sysparm_limit before ACL): https://www.servicenow.com/docs/r/zurich/api-reference/rest-apis/c_TableAPI.html
- `snc_platform_rest_api_access` **[S]**: https://support.servicenow.com/kb?id=kb_article_view&sysparm_article=KB2976437 ; https://www.servicenow.com/community/developer-forum/rest-api-access/m-p/1525721
- ACL/role model for a custom role: `now-sdk explain security-guide`, `now-sdk explain acl-api`, `now-sdk explain role-api`

## 3. Inbound REST rate limits

- Feature: "Inbound REST API rate limiting" — "set rules that limit the number of inbound REST API requests processed per hour … for specific users, users with specific roles, or all users." Rules live in **`sys_rate_limit_rules`** (System Web Services > REST > Rate Limit Rules; role `rate_limit_admin`). Fields: REST API + Version + Resource (Table API rules can target a specific table), Request limit per hour, Apply to (Single user / Users with role / All users).
- Enforcement is per user, counted per node and committed to DB every 30 s ("a rate limit rule may not take effect for up to 30 seconds"). Precedence: single-user rule > role rule > all-users rule; a user matching several role rules gets the **lowest** limit.
- **Headers** on any request that matches a rule: `X-RateLimit-Limit` (requests/hour), `X-RateLimit-Reset` (UNIX timestamp of next window reset), `X-RateLimit-Rule` (sys_id of the rule). On denial: HTTP **429 Too Many Requests**, `Retry-After` (seconds), body `{"error":{"message":"Rate limit exceeded","detail":"Rate limit of N requests per hour for Table API exceeded"},"status":"failure"}`.
- Observability on the instance: rule record's related lists **Rate Limit Counts** (cleared daily) and **Rate Limit Violations** (cleared biweekly); modules Rate Limits / Rate Limit Violations; Reset clears count + current-hour violations. Changing "Request limit per hour" resets the count.
- There is **no `X-RateLimit-Remaining`** header documented. No OOB rule is documented as enabled by default; with no rule matching, no `X-RateLimit-*` headers appear.
- Platform ceilings that apply anyway: **transaction quota** "REST Table API request timeout — prevents inbound REST Table API transactions from running for longer than 60 seconds" (same 60 s for Aggregate, Import Set, Attachment API; System Definition > Transaction Quota Rules); **semaphores** — API calls go to the `API_INT` pool, "typically 4 max per node"; when the queue fills "additional transactions will be rejected with a 429 error". So a 429 can mean rate-limit rule *or* semaphore exhaustion — distinguish by presence of `X-RateLimit-*`/`Retry-After`. Design daku for 429 + backoff regardless.
- Table API paging facts: `sysparm_limit` default **10000** ("Unusually large sysparm_limit values can impact system performance"), `sysparm_offset`, `Link` header with `rel="next"|"prev"|"first"|"last"` (suppress with `sysparm_suppress_pagination_header=true`), `X-Total-Count` header, `sysparm_no_count=true` skips the `count(*)`, `sysparm_fields`, `sysparm_exclude_reference_link=true`, `sysparm_display_value=false`; invalid query parts are silently ignored unless `glide.invalid_query.returns_no_rows=true`.
- Polite polling recipe: `sysparm_query=sys_updated_on>…^ORDERBYsys_updated_on`, `sysparm_fields=…`, `sysparm_exclude_reference_link=true`, `sysparm_no_count=true`, modest `sysparm_limit`, follow `Link rel="next"`, keep each request under the 60 s quota; use the **Aggregate API** (`/api/now/stats/{table}`) for counts instead of pulling rows.

Sources:
- https://www.servicenow.com/docs/r/api-reference/rest-api-explorer/inbound-REST-API-rate-limiting.html (headers, 429, precedence, `sys_rate_limit_rules`)
- Create: https://www.servicenow.com/docs/r/api-reference/rest-api-explorer/create-REST-API-rate-limits.html ; Monitor: https://www.servicenow.com/docs/r/api-reference/rest-api-explorer/monitor-rate-limits.html ; Investigate: https://www.servicenow.com/docs/r/api-reference/rest-api-explorer/investigate-rate-limit-violations.html
- Table API reference: https://www.servicenow.com/docs/r/zurich/api-reference/rest-apis/c_TableAPI.html ; https://developer.servicenow.com/dev.do#!/reference/api/latest/rest/c_TableAPI
- Transaction quotas: https://www.servicenow.com/docs/r/platform-administration/platform-performance/c_DefaultQuotaRules.html
- Semaphores (API_INT, 429 on queue overflow): https://www.servicenow.com/docs/r/impact/io-semaphores-performance-metrics.html ; https://www.servicenow.com/docs/r/xanadu/platform-administration/platform-performance/monitoring-semaphore-activity.html

## 4. Personal Developer Instances

- **Same platform, same APIs.** No primary source lists any inbound auth option (OAuth, API keys, rate-limit rules, cert auth) as unavailable on PDIs; PDIs give full admin and let you activate plugins from the Developer Site. Documented limits: no clone source/target, no team development, no App Repo/Store publishing, no ML/IDR/MetricBase, many Store apps not installable, "PDIs do NOT have the performance or reliability of a paid ServiceNow instance", no support.
- **Hibernation:** "When a PDI hibernates, the database and application server shut down … All your data is preserved." Roughly 6 h of inactivity triggers it; the timer is reset by "any activity in an interactive session" — REST calls are not documented as counting. Hibernated instance answers HTTP with a "hibernating" notice page (community: HTTP 200 + HTML **[S]**); wake is via Developer Site login or Manage my Instance > Wake Instance (3–5 min, up to 20). No documented programmatic wake. Scheduled jobs don't run while hibernated.
- **Reclamation (policy from 2026-07-11):** reclaimed when the PDI is ≥90 days old **and** has had no explicit PDI login in 10 days; "Background processes and developer site logins don't count toward activity." Reclaimed = reset and reassigned; data gone.
- Terms of Use: non-production instances "solely for your own internal use to evaluate the ServiceNow Products", "may not use … with production data or to provide services to others".
- Consequence for daku: a PDI is a fine place to *develop and test daku's ServiceNow client*, and can stand in for the "dev" environment in a demo, but the poller must treat "hibernating" (HTML instead of JSON, or non-2xx) as a distinct *unreachable/asleep* state, not an outage, and must not attempt keep-alive. Real prod/test/dev must be customer instances.

Sources:
- PDI guide (Understanding PDIs): https://developer.servicenow.com/dev.do#!/guides/xanadu/developer-program/pdi-guide/understanding-pdis
- PDI FAQ: https://developer.servicenow.com/dev.do#!/guides/yokohama/now-platform/pdi-faq
- Hibernation blog (Developer Program): https://developer.servicenow.com/blog.do?p=%2Fpost%2Fhibernation-and-developer-instances%2F
- Reclamation policy 2026: https://www.servicenow.com/community/developer-blog/keeping-pdis-available-for-active-builders-upcoming-pdi/ba-p/3572058 (posted by ServiceNow) ; KB3140725: https://support.servicenow.com/kb?id=kb_article_view&sysparm_article=KB3140725
- Terms of Use: https://www.servicenow.com/terms-of-use.html
- Hibernation vs REST **[S]**: https://www.servicenow.com/community/developer-forum/how-to-wake-up-my-developer-instance-through-rest-api-call/m-p/1980648

## 5. Official instance-health / observability surfaces

| Surface | What it is | Usable by daku? |
|---|---|---|
| **Instance Scan** (`com.glide.instance_scan`) | Configuration-quality scanner ("interrogate your instance for configurations that indicate health issues"); checks/suites/findings; role `scan_user`. REST trigger via CI/CD API `/api/sn_cicd/instance_scan/{full_scan,point_scan,suite_scan/...}` (used by ServiceNow's own GitHub Action). Results are tables readable via Table API. | Yes, as a low-frequency "config health" signal, not uptime. |
| **System Diagnostics** (`stats.do`, `xmlstats.do`, `threads.do`, System Events / Scheduled Jobs dashboards) | Admin UI pages; `xmlstats.do` returns XML node stats. Not documented as a supported external interface; admin-only. | Avoid; use Table API on the underlying tables instead. |
| **`sys_cluster_state`** | Node status table. | Readable via Table API (admin ACL). |
| **Now Support portal** | Per-instance info; "P1-Free does not represent instance availability". Portal-only, no public REST API found. | No. |
| **Impact Instance Observer** | Near-real-time instance health/availability, only for Impact Advanced/Total customers, via CSM portal. | No (paid, portal). |
| **Log Export Service (LES)** | Store app; exports `syslog`, `sys_audit` (+delete/relation) via Kafka (Hermes) to MID/Kafka consumers/SIEM; roles `sn_logstoanalytics.admin`, `admin` for setup. | Alternative to polling `syslog` if a customer already runs it; Kafka pull, not HTTP. Not v1. |
| **Health Log Analytics** | ITOM Health store app; can ingest the instance's own syslog ("ServiceNow System Logs Retriever"). Licence + MID Server. | No for v1. |
| **Event Management self-health**, Instance Troubleshooter (no longer deployed since Xanadu), Upgrade Monitor, HealthScan (Now Support/Impact-run) | Narrow or retired. | No. |
| Generic health endpoint | None documented (`/api/now/.../health` does not exist). Practical liveness probe: `GET /api/now/table/sys_properties?sysparm_query=name=glide.buildname&sysparm_fields=value` (also yields version). | Yes, as daku's "up + version" check. |

Sources:
- Instance Scan: https://www.servicenow.com/docs/r/platform-administration/instance-scan/hs-landing-page.html ; CI/CD API: https://www.servicenow.com/docs/r/api-reference/rest-apis/cicd-api.html ; ServiceNow GitHub Action: https://github.com/ServiceNow/sncicd-instance-scan
- System Diagnostics: https://www.servicenow.com/docs/r/platform-administration/platform-performance/c_RunSystemDiagnostics.html
- Now Support availability: https://support.servicenow.com/kb?id=kb_article_view&sysparm_article=KB0547242
- Impact Instance Observer: https://www.servicenow.com/docs/r/zurich/impact/io-overview.html
- LES: https://www.servicenow.com/docs/r/platform-security/servicenow-ai-platform-security/les-landing-page.html ; roles: https://www.servicenow.com/docs/r/platform-security/servicenow-ai-platform-security/les-roles.html
- HLA self-logs: https://www.servicenow.com/docs/r/yokohama/it-operations-management/health-log-analytics/hla-data-input-glide-syslog.html
- Instance Troubleshooter retired: https://support.servicenow.com/kb?id=kb_article_view&sysparm_article=KB0966883

## 6. Concrete recommendation for daku v1

Per monitored instance (prod / test / dev):

1. Admin creates `sys_user` `daku.monitor` — Identity type Machine (⇒ Web service access only), roles: `x_daku_reader` (custom, read ACLs on the signal tables) + `snc_read_only`. No `admin`, no `itil` unless the ITSM family is in scope and the customer prefers OOB roles.
2. Admin sets `glide.oauth.inbound.client.credential.grant_type.enabled=true`, creates an OAuth API endpoint for external clients (Public Client false, Grant type Client Credentials, OAuth Application User = `daku.monitor`, optionally an Auth Scope restricting to Table + Aggregate API), hands daku `client_id`/`client_secret` + instance URL.
3. Optionally: a REST API access policy pinning Table/Aggregate API for that profile to OAuth + daku's egress IPs; a `sys_rate_limit_rules` rule for `daku.monitor` so daku's own budget is explicit and observable via `X-RateLimit-*`.
4. daku: token from `/oauth_token.do`, cache until `expires_in`−60 s, retry once on 401; honour `429`/`Retry-After`; treat non-JSON 200 (PDI hibernation page) as "asleep"; liveness = `sys_properties` `glide.buildname` read; poll incrementally with `sysparm_query=sys_updated_on>…`, `sysparm_fields`, `sysparm_limit`, `sysparm_no_count`, Aggregate API for counts.
5. Config shape: `{ instance_url, client_id, client_secret }` per environment (basic `{ user, password }` accepted only as a dev/PDI convenience, matching Now SDK's own guidance "PDIs, sandbox, fast iteration"). JWT bearer, API key and mTLS are documented alternatives; not built in v1.

## Unverified / open

- Exact release that introduced inbound client credentials and API keys (community says Washington DC; docs indexed from Xanadu on).
- `snc_platform_rest_api_access` requirement and its per-API "REST API ACL" toggle: KB/community only this session.
- Which base-system role reads `syslog`, `sys_trigger`, `sys_email`, `sysevent`, `sys_cluster_state`, `sys_update_set`, `sys_upgrade_history`, `sys_properties`, `sys_transaction` is not stated in docs; `admin` in practice — check with Access Analyzer.
- Table names of the rate-limit counts/violations related lists (only `sys_rate_limit_rules` is named); API_INT queue depth default; whether any rate-limit rule ships enabled.
- HTTP status returned by a hibernated PDI; whether REST calls reset the hibernation timer.
- PDI availability of `com.glide.tokenbased_auth` / `com.glide.auth.mutual` and ADCv2 on PDIs.
- Instance Scan CI/CD endpoint role requirements; any REST API on Now Support for instance status.
