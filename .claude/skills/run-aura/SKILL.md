---
name: run-aura
description: Use when launching, restarting, or driving the AURA desktop app in this repo — building it, giving it access to the .venv-sidecars Python stack, or connecting an MCP client (Claude Code, Claude Desktop) to its embedded MCP server. Covers the exact commands; the "why" lives in CONTRIBUTING.md and docs/mcp-usage.md.
---

# Running AURA

AURA is a Tauri desktop app (Rust backend, Svelte frontend) with an embedded
MCP server. This skill is the verified cold-start path — full rationale is in
[CONTRIBUTING.md](../../../CONTRIBUTING.md) and
[docs/mcp-usage.md](../../../docs/mcp-usage.md); this file is just the
commands, in order.

## 1. Launch (or restart) the app

```sh
PATH="$PWD/.venv-sidecars/bin:$PATH" npm run tauri dev
```

Always prepend `.venv-sidecars/bin`, even if you don't think you need the AI
sidecars this session — `resolve_python()` (`src-tauri/src/sidecars/jobs.rs`)
picks the first `python3`/`python` on `PATH`, so leaving it off silently
falls back to system Python (missing `anticipation`/`torch`/`transformers`/
`mido`/`demucs`) instead of failing loudly.

No `.venv-sidecars` yet, or don't need real models this session? Set
`AURA_SIDECAR_SIMULATE=1` instead — every AI worker returns deterministic
placeholder output.

Rust changes need a restart (no hot reload): stop the process
(Ctrl-C / kill the pid group) and re-run the command above. Frontend-only
changes hot-reload via Vite.

## 2. Restarting after a `git pull`

```sh
git status                       # confirm nothing local would be discarded
git pull --ff-only origin main   # stop and ask the user if this can't fast-forward
# stop the running app, then:
PATH="$PWD/.venv-sidecars/bin:$PATH" npm run tauri dev
```

The MCP bearer token rotates on every launch, so any connected MCP client
needs reconnecting after this (step 4).

## 3. Confirm it's up

```
grep 'mcp: streamable-HTTP server' <your captured log>
```

or just check the process:

```sh
ps aux | grep target/debug/aura
```

## 4. Connect an MCP client

Token lives at `~/.local/share/aura/mcp-token` (0600), regenerated per
launch.

```sh
claude mcp add --transport http aura http://127.0.0.1:41717/mcp \
  --header "Authorization: Bearer $(cat ~/.local/share/aura/mcp-token)"
```

Claude Desktop / other clients: [docs/mcp-usage.md](../../../docs/mcp-usage.md)
has the `mcp-remote` config and the full tool roster, policy modes
(`readOnly`/`confirmDestructive`/`full`), and the transport/security notes
(loopback-only, Origin validation, `Content-Length`-only bodies).

## Why this is a skill and not just docs

The docs (CONTRIBUTING.md, docs/mcp-usage.md) explain *why* each piece works
the way it does and are the source of truth if this skill and the docs ever
disagree. This skill exists because the MCP server itself can't bootstrap
its own connection: it isn't reachable until the app is built and launched
with the right `PATH`, and its token is only readable from the filesystem
before you're connected. That bootstrapping step is exactly what a skill is
for — everything reachable *through* the MCP once connected (project state,
job status, tool roster) doesn't need to be duplicated here.
