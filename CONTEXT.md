# daku

Infrastructure monitoring for platforms the owner/team runs — ServiceNow first.

## Language

**Platform**:
A product or system family daku can monitor. v1 is ServiceNow only.
_Avoid_: integration, tool, vendor

**Environment**:
One concrete deployable instance of a Platform (e.g. ServiceNow prod, test, or dev). The Operator's config defines the list — defaults to prod/test/dev; not a fixed ceiling. A Personal Developer Instance (PDI) is a temporary stand-in for building daku, not a monitored Environment the team relies on.
_Avoid_: instance (ambiguous with "ServiceNow instance" in casual speech — prefer Environment when talking about daku's model), stage, tier

**Signal**:
A named observation daku collects from an Environment (availability, job backlog, error rate, …). v1 ships seven for ServiceNow (availability/build, scheduled jobs, MID/ECC, syslog error rate, version/plugin drift, last-clone, outbound/integration failures).
_Avoid_: metric, check, probe, KPI

**Credential**:
Secrets that let daku read an Environment. Live in the macOS Keychain (daku-owned service); Environment URLs/labels live in `~/.daku/`. Never in git. Real Environments use OAuth client credentials; basic auth is only for PDI stand-ins.
_Avoid_: API key (too specific — auth method varies), token

**Operator**:
The person running daku. In v1 this is the platform owner on their own machine; not a multi-user role model.
_Avoid_: user, admin, viewer (those imply daku-side accounts we are not building in v1)

**Environment health**:
A rolled-up status for an Environment derived from its Signals: **healthy**, **degraded**, or **down**. v1 uses hard-coded defaults (not Operator-configured alert rules).
_Avoid_: severity, priority, alert state
