# ADR 0003 — The mutation channel: closure transactions over one Session

## Status

**Proposed** (2026-08-13). Distilled from `docs/CORE-REDESIGN-ROUND-2.md` §4
(DRAFT); the owner accepts this ADR, not its authors.

## Context

Mutations today reach state through ~14+ distinct paths (round-2 §4.5, O-8):
ten bypassing command paths, five sidecar completion sinks, the engine
control thread writing the store directly from five sites, two *read*
commands that mutate (lazy disk resync), and frontend-only mutations — of
which clip-move, the most common gesture in the product, reaches no backend
channel at all. Five uncoordinated stores forbid cross-store atomicity by
their own lock discipline. Undo, journaling, attribution, and multi-window
sync all require a single ordered mutation stream (SCALABILITY §5, D-03);
dossier 04 supplies the gesture and inverse machinery.

## Decision

- **One `Session`** subsumes the five stores behind one lock; `&mut Session`
  is reachable only inside `session.transact(meta, |tx| …)`; every reusable
  mutator takes `&mut Tx`. Nested `transact` is runtime-checked (panic, not
  deadlock). Graph rebuilds read an immutable snapshot, never the lock
  (round-2 §4.1, §4.3).
- **The engine thread submits, it never holds**: recording finalize,
  auto-stop, and friends become ops submitted as **`Actor::Engine`**
  transactions through the same channel as everyone else; blocking engine
  round-trips inside a transaction are banned (round-2 §4.2).
- **Gestures get IPC primitives** — `begin_gesture(target)` /
  `end_gesture()` from the frontend, CLAP-style; between them, updates flow
  as transient ops (RT-visible, not journaled, coalesced by
  `(op_kind, target, actor)`), and `end_gesture` commits one batch. The
  350 ms backend timer is the fallback for boundary-less callers, not the
  mechanism (round-2 §4.4).
- **The op log is a persisted, versioned format from day one**: ops are the
  journal (`journal.ndjson`); op schemas carry versions; the envelope
  inherits D-03's draft (`op-envelope.schema.json` — `rev`/`baseRev`/
  `transient`/`origin`); property paths live in one Rust module so drift is
  a compile error (round-2 §4.6). `actor`/`run` stamping is the one
  attribution-irreversible item and cannot be deferred.
- **The log stays dark** — no journal, no undo exposure — **until the
  side-channel inventory is total** (round-2 §4.5). An op log with holes
  looks authoritative and lies.

## Consequences

- Cross-store batches become atomic; the §7 gate tests (undo round-trip,
  atomicity, the Figma invariant, attribution) become expressible.
- The mutating readers become pure reads; clip-move gets a real
  `clip.move` op in this round or the log lies from the first drag.
- Renaming a property path becomes a file-format break, and is treated as
  one.
- Prepare-outside/commit-inside keeps long I/O (import, decode, plugin
  instantiation) out of the serialized channel.
