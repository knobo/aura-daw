# AURA — Phase 4: the core redesign build

**Status: ACTIVE (2026-08-13).** Implements
[`CORE-REDESIGN-ROUND-2.md`](CORE-REDESIGN-ROUND-2.md) (ACCEPTED) and ADRs
0001–0007 (all Accepted). This document is the orchestration plan: it cuts
the round into six sub-plans, fixes their order and gates, and records the
rules that hold across all of them. Detailed, bite-sized task plans live in
`docs/superpowers/plans/` and are written **just-in-time** — each sub-plan
is authored when its predecessor lands, against the tree as it then exists,
not speculatively.

Convention note: phases 2 and 3 used ownership zones for parallel agents
(`PHASE2-PLAN.md`, `PHASE3-PLAN.md`). Phase 4 keeps the zone idea but the
zones are *sequenced*, because this phase rebuilds the foundation the zones
would otherwise share.

---

## The six sub-plans

| # | Sub-plan | Implements | Depends on |
|---|---|---|---|
| **A** | Session + the transaction channel | round-2 §4.1–§4.4, §4.6 (Session over Store+MidiStore, `transact`/`Tx`, ops with inverses, actor/run, effect descriptors, revision, op-format v1) | — |
| **B** | Identity groundwork | §2 (typed id families, `note_id` + persisted watermark, source-keyed assets/cache, derived slots + versioned ParamTable/meter routing, MAX_TRACKS removal) | A (ops address ids) |
| **C** | Time + project v3 | §3 (`Ticks`/`Samples` newtypes, supertick tempo, meterMap, section table, steady_time, v2→v3 migration, wire/frontend bijection) | A (tempo edits are ops) |
| **D** | Content/placement split | §5 (`ContentId`, placements with `LaneId`, default lanes) — **ships inside C's v3 migration**: one format bump, not two | C (same migration) |
| **E** | The side-channel totality | §4.5 (all 14+ paths through the channel, engine-thread inversion, gesture begin/end IPC, `clip.move` op + command, id-preserving piano roll, mutating readers become pure, remaining three stores into Session) | A, B, C/D |
| **F** | History storage | §6 (COW B-tree session, version-graph retention, replay-only nodes at 64 KB, placement-offset routing, janitor thread, budget/eviction) | A; informs E's undo exposure |

Order of execution: **A → B → C+D → E → F**, with F's tree buildable as a
standalone module (property tests against `benches/bulkbench`'s reference
behavior) any time after A. E is last on purpose: it is the totality
migration, and its exit gate is the one that turns the op log on.

## Gates (from round-2 §7 — the definition of done)

- **Gate A:** property tests 1 (undo round-trip), 3 (atomicity) and 5
  (attribution) pass against the A-slice ops; nesting is
  compile-prevented in-tree and runtime-trapped across the handle.
- **Gate B:** test 2 (delete-then-undo preserves identity and inbound
  references) passes; slot aliasing test (delete→add→old graph still
  playing) passes; meter blocks carry graph generation.
- **Gate C/D:** test 6 (section-table bound < 64 samples), test 7 (tempo
  round-trip + lossless v2→v3 against a fixture corpus, including
  meterMap and the placement/content split); frontend consumes the
  shipped section table (TS TempoMap deleted).
- **Gate E:** the side-channel inventory inside the channel is **total**
  (checked against dossier 10 §2.3 + review additions, all 14+ paths);
  test 4 (Figma invariant — reads mutate nothing) passes; **only then
  does the op log/journal turn on.** This gate is the round's whole
  point; it does not get negotiated down.
- **Gate F:** delete-then-undo rides version retention; replay-only
  nodes reproduce byte-identical state (seeded PRNG, minted-id
  payloads); eviction respects the floor/ceiling; janitor keeps drops
  off-RT (test with `assert_no_alloc`, `disable_release` OFF).

## Rules that bind every sub-plan

1. **The op log stays dark until Gate E.** No journal file is written, no
   undo UI is exposed, before the inventory is total. (ADR 0003.)
2. **Thin renderer** (ADR 0006, owner-accepted): no new authoritative
   state, business logic, or time math lands frontend-side; hot rendering
   goes behind the texture-targeting renderer interface. Any frontend task
   in these plans is renderer/chrome work or op emission — nothing else.
3. **Frozen names stay frozen.** Command *names* and event names remain;
   bodies become wrappers over the channel. New commands are additive.
4. **Evidence policy** (ADR 0007): performance claims cite `benches/`;
   corrections to any doc are marked, never silent.
5. **Prepare-outside/commit-inside** for anything that does I/O; blocking
   engine round-trips inside a transaction are banned (round-2 §4.2/§4.4).
6. **Every sub-plan ends green:** `cargo test --manifest-path
   src-tauri/Cargo.toml` (277 baseline) and `npm test` (17 baseline) plus
   its own new tests; counts updated in README/CONTRIBUTING per the
   dated-count convention.

## Written plans

- **A:** [`docs/superpowers/plans/2026-08-13-plan-a-session-transaction-channel.md`](superpowers/plans/2026-08-13-plan-a-session-transaction-channel.md) — ready.
- B–F: authored when their predecessor lands (just-in-time, against the
  then-current tree). Each follows the same skill template as A.
