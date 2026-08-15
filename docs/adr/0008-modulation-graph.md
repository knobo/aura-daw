# ADR 0008 — Modulation graph: tagged `TargetRef` as a step toward ports

## Status

**Accepted** (2026-08-15, project owner). Distilled from
`docs/superpowers/specs/2026-08-15-modulation-system-design.md` (accepted
through brainstorming the same day). Owner rulings R1–R5 in that document
are the source of this decision; remaining implementer choices under R5 are
recorded in `docs/PHASE4-PLAN.md`'s Track F handoff.

## Context

ADR 0004 closed with automation-lane identity as a live gap: string
`target_node` pairs cannot address faders, pan, sends, or macros, and the
fix was "explicitly assigned to the node-graph round". Track D
(`docs/PHASE4-PLAN.md` "Track D handoff") made the existing lane model
audible without changing that identity — one gain overlay per track, plugin
params host-driven at ≤2 ms, still named by `"track:<id>"` or a plugin uuid.

The owner then accepted a full **modulation graph** (R1): sources →
bindings → targets, where a drawn curve is one source kind among LFO,
macro, MIDI CC and envelope follower. Implementation is staged; the format
is designed once. Targets are a **tagged union now, ports later** (R2), on
the condition that the tagged form is a step toward the port model, not a
detour, and that the path to the finished system is written down in local
files (the design, `next-prompt.md`, and this ADR).

Shipping only string targets again, or inventing a parallel address space
that the node-graph round must rewrite, would violate that condition.

## Decision

1. **Document objects.** `Curve` (normalized 0..1 point data), `Binding`
   (source → target, with mode/depth/range), and `AutomationClip` (placement
   of a curve on an automation track) ship in project **schemaVersion 4**.
   `Modulator` and `Macro` are format-only this round (empty arrays). Point
   chunks stay AMEV.

2. **`TargetRef` is a tagged union now**, including the arms the node-graph
   round will need and the ones this round only resolves:

   | Arm | This round |
   |---|---|
   | `trackParam` (gain, pan; mute/send format-only) | gain/pan resolve; mute/send return `None` |
   | `pluginParam` | resolves |
   | `selfTrackParam` / `selfInstrumentParam` | clip envelopes only |
   | `macro` | reserved |
   | `port` (`nodeId`, `portId`) | reserved — node-graph round |

3. **Why this is a step, not a detour (R2).** When the node-graph round
   lands, the resolver keeps every existing arm and **maps them to ports
   internally**. Saved projects need no rewrite; new bindings may mint
   `port` refs directly. Adding the port arm touches the resolver only —
   that is the purchase of designing the union once. ADR 0004's reserved
   gap is this arm.

4. **Value semantics live on the binding** (R3): curves are shapes in
   `[0,1]`; mode/depth/range map into the target's normalized domain.
   Combination and arbitration are design §4; they re-express Track D's
   gain-as-multiplier and plugin-as-absolute without behaviour change on
   migration.

5. **The ordered path to the finished system is design §8** — ports,
   modulators, macros, curve shapes, recording, sample-accurate plugin
   params, lazy expansion, per-voice modulation. `next-prompt.md` links
   that section; it is not restated here.

## Consequences

- Project files go to v4 with a one-way v3 `automation[]` → `modulation{}`
  migration; reading v1–v3 remains forever.
- Several curves per track, automation tracks routed to many targets, and
  content-keyed clip envelopes become data-model properties rather than
  special UI cases.
- The node-graph round inherits a resolver it can extend in place; it does
  not inherit a second address space to migrate off.
- Non-goals this round (design §9) stay deliberate: LFO/macro/CC bodies,
  non-linear curve shapes, write/touch/latch recording, sample-accurate
  plugin params, mute/send targets, plugin-param bounce, per-voice
  modulation, and any change to Track D's gesture/undo shape.
