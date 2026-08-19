# AURA docs — reading map

| Document | What it is / why you'd read it |
|---|---|
| [../next-prompt.md](../next-prompt.md) | Fresh-session briefing: what to do now, standing rules, leftover pointers. History lives in [handoff/](handoff/). |
| [handoff/](handoff/) | Landed-track log and Plan E review log — opened only when the job touches that area. |
| [ARCHITECTURE.md](ARCHITECTURE.md) | The system design: agent ownership zones and frozen files (§0), the real-time rules every audio-path change must obey (§2), IPC strategy, threading, sidecars, the control-plane seam (§11), the embedded MCP server (§12), musical time (§13), sampler (§14), and plugin hosting (§15). Read §0 and §2 before writing any code. |
| [SCALABILITY.md](SCALABILITY.md) | The path from prototype to full DAW: mixer graph, PDC, plugin isolation, project format, undo — ending in the **debt register** (D-01…D-12), the honest list of frozen decisions that will need managed migration, with which are already paid down. |
| [PHASE2-PLAN.md](PHASE2-PLAN.md) | The executed plan for wave 2 (control plane, MCP server, MIDI/tempo map, sampler): ownership zones, contracts, and acceptance gates. Kept as process history. |
| [PHASE3-PLAN.md](PHASE3-PLAN.md) | The executed plan for wave 3 (CLAP/LV2 plugin hosting, live instrument nodes): the acceptance contract that made ZynAddSubFX the gating test. Kept as process history. |
| [pitch-track.md](pitch-track.md) | Normative design and delivery plan for persistent pitch curves, the arrangement pitch lane, audio-to-MIDI extraction, MIDI targets, non-destructive manual pitch editing, offline correction, and later live auto-tune. |\n| [mcp-usage.md](mcp-usage.md) | How to connect an MCP client (Claude Code / Claude Desktop) to a running AURA: endpoint, per-launch token, tool roster, policy modes, and the transport/security notes. |
| [synth-compatibility.md](synth-compatibility.md) | The synth compatibility sweep: verdict table for every synth driven through the acceptance harness, and what it proved about co-hosting risk (D-11). |
| [themes.md](themes.md) | Theme system guide: built-in themes, accessibility (WCAG AA/AAA), custom JSON theme format, token reference, and export workflow. |
| [ipc-schemas/](ipc-schemas/) | The JSON Schema wire contracts for every IPC payload (project, clips, transport, meters, sidecar jobs, MCP policy, plugin state, user themes…). v1 schemas are frozen; v2 schemas are additive emitter contracts. |
| [screenshots/](screenshots/) | The screenshots and captures used by the top-level README. |
