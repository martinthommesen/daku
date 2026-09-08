# daku for agents

daku state is queryable without the window: a local MCP server plus a
redacted JSON snapshot. Both serve the same fields and the same redaction
rule — **ids, labels, states, counts, summaries; never instance URLs,
payloads, or credentials**. Hostnames identify the Operator's estate;
syslog payloads carry message text; credentials live outside SQLite
entirely.

## MCP server

```sh
daku-daemon mcp
```

Speaks MCP over stdio (newline-delimited JSON-RPC): `initialize`,
`tools/list`, `tools/call`. No sockets, no tokens — stdio inherits the
invoking user's trust, like `doctor` and `digest`. `DAKU_DB_PATH` is
respected when the database lives elsewhere.

| Tool | Arguments | Reads |
|---|---|---|
| `environments_list` | — | id, label, platform, rolled-up health per Environment |
| `health_get` | `environment_id` | health, reachability, per-Signal votes |
| `signals_get` | `environment_id` | per-Signal states plus redacted drill-in rows (`list`, `count`, `names`, `truncated` — display names only, never message bodies, subjects, URLs, or payloads) |
| `events_recent` | `environment_id`, `limit` (default 20, max 100) | health + Signal transitions, newest first |
| `digest_week` | `environment_id`, `days` (default 7, max 90) | Markdown review |

Unknown Environments and tools answer JSON-RPC errors (`-32602`);
unknown methods answer `-32601`; malformed lines answer `-32700` and the
loop continues. Notifications (no `id`) get no reply.

## Copy Agent Context

App menu → Copy Agent Context (JSON), or the `copy-context` palette row:
the selected Environment as pretty JSON — environment, per-Signal states
with summaries, recent timeline text. Same redaction as the MCP tools.
