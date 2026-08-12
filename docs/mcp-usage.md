# Using AURA's embedded MCP server

AURA runs a Model Context Protocol (MCP) **streamable-HTTP** server inside the
app so agents (Claude Code, Claude Desktop via `mcp-remote`, any MCP SDK) can
inspect, edit, mix, record and generate in the open session.

* **Endpoint:** `http://127.0.0.1:41717/mcp` (loopback only — never a LAN
  interface). The port is configurable via policy (below).
* **Lifecycle:** starts with the app (`mcp::init`); a bind failure disables
  MCP for the session but never aborts startup. Check `mcp_get_status` /
  the log line `mcp: streamable-HTTP server on http://127.0.0.1:<port>/mcp`.

## Authentication: the session token

A fresh 256-bit bearer token is generated **on every app launch** (the
previous token stops working). It is never logged in full — the log/UI show
an 8-char fingerprint — and the full token is written with `0600`
permissions to the app-data dir:

```
~/.local/share/aura/mcp-token        # Linux (dirs::data_dir()/aura/mcp-token)
~/Library/Application Support/aura/mcp-token   # macOS
```

Every HTTP request must carry it: `Authorization: Bearer <token>`.

### Claude Code

```sh
claude mcp add --transport http aura http://127.0.0.1:41717/mcp \
  --header "Authorization: Bearer $(cat ~/.local/share/aura/mcp-token)"
```

Re-run after an AURA restart (the token rotates per launch).

### Claude Desktop (via `mcp-remote`)

`claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "aura": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://127.0.0.1:41717/mcp",
        "--header",
        "Authorization: Bearer ${AURA_MCP_TOKEN}"
      ],
      "env": {
        "AURA_MCP_TOKEN": "<paste the contents of ~/.local/share/aura/mcp-token>"
      }
    }
  }
}
```

## Tool roster

Read-only (always allowed unless overridden): `get_project_state`,
`read_meters` (the agent's "ears" — latest ~60 Hz peak/RMS frame),
`get_job_status`.

Destructive (policy-gated): `create_project`, `add_track`, `set_track_mix`
(batched), `transport_control`, `record_take`, `import_audio_clip`,
`run_sidecar_job` (kinds: `aceStepGenerate`, `aceStepRepaint`,
`aceStepAudio2Audio`, `elevenLabsMusic`, `amtInfill`, `stableAudioSfz`).

`run_sidecar_job` extras applied by the control plane:

* params with `importToTrackId` (`null`/`""` = auto-create a track) and
  optional `importAtSamples` auto-import a finished job's `outputPath` onto
  the timeline;
* a finished `stableAudioSfz` job auto-loads its `sfzPath` into the sampler
  bank, ready for `sampler_preview_note` / `set_track_instrument`.

## Policy modes

Wire shape: `docs/ipc-schemas/mcp-policy.schema.json`; change it with the
`mcp_set_policy` command (UI/dev console), inspect with `mcp_get_status`.

| Mode | Effect |
|---|---|
| `readOnly` | Only the read-only tools run; destructive tools are denied. |
| `confirmDestructive` (**default**) | Read-only tools run freely; every destructive call parks as a pending confirmation, emits `mcp://confirm-requested` to the AURA UI, and runs only when the user approves (`mcp_confirm_pending`). Timeout **60 s ⇒ deny**. |
| `full` | Everything runs without confirmation (explicit user opt-in). |

Per-tool overrides (`toolOverrides: { "record_take": "deny", ... }` with
`allow` / `confirm` / `deny`) beat the mode; unknown tool names are always
denied.

**Port:** `mcp_set_policy` with a different `port` takes effect on the next
server start (next app launch). The bind address is not configurable.

## Transport notes

* Requests must be sized JSON bodies with a **`Content-Length` header** —
  chunked transfer encoding is rejected with `411` (MCP SDK clients send
  sized bodies).
* Sessions follow the streamable-HTTP spec: `Mcp-Session-Id` is issued on
  `initialize` and required on subsequent POSTs; `DELETE` terminates the
  session; the optional standalone GET SSE stream is not offered (the spec
  allows answering it with `405`).
* Security hardening (all mandatory, see `ARCHITECTURE.md` §12.3): loopback
  bind, constant-time token compare, and Origin validation on every request
  — browser-originated requests are rejected unless the Origin is loopback
  (DNS-rebinding defense).
