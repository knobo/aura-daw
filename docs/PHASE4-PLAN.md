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

## Plan A handoff (2026-08-13, post whole-branch review)

Whole-branch review of Plan A found round-2 §4.1's clause "the graph
rebuild reads an immutable snapshot rather than holding the Session lock"
is **not** delivered: `engine::rebuild` (`src-tauri/src/audio/engine.rs`)
still takes `self.session.lock()` and holds it for the entire graph build
(track/clip walk, RT-clip assembly, live-node binding). This is a widened
lock footprint versus pre-merge — every command now blocks on that same
merged `Session` lock during a rebuild, where before the split stores gave
some commands a narrower lock to contend with. Plan A's own written plan
never named this clause on an exclusion list, so per ADR 0007
(no-silent-corrections) it is recorded here explicitly rather than left
implicit: **snapshot-based rebuild is deferred, not delivered, by Plan A.**
It requires Plan F's copy-on-write history store (§6) to give `rebuild` an
immutable view it can read without holding the live document lock; until
Plan F lands, rebuild latency (proportional to track/clip count) sits
directly in the critical path of every command that touches the session.

The following are binding carry-forwards for later plans, recorded here
so they aren't lost between sub-plan hand-offs:

- Plan E first item: `with_synced_store`/`apply_hum_clip` hold the
  document lock across midi disk I/O — move persistence outside the lock.
- Before Gate E turns the log on: `fold_ops` coalesces Sets across
  intervening structural ops — harmless dark, wrong for replay; constrain
  it before `Committed.ops` is ever persisted.
- Plan B: add the duplicate-id guard on TrackAdd (rollback removes-by-id
  can mis-target); make clip restore positional or define canonical clip
  order (gate test 2 will fail on interleaved clip vecs otherwise); delete
  dead `ops::apply_track_mix`; add the clamp round-trip test (999→24).
- Standing caveat: `transact` has no panic rollback — transaction
  closures must stay non-panicking until Plan F's snapshot rollback.
- Op wire form: `ObjectRef` ids are raw Strings; typed id families must
  serialize transparently or bump OP_FORMAT_VERSION.
- Clip carriage is asymmetric by design: `TrackAdd.clips` authoritative,
  `TrackRemove.clips` advisory (store truth wins).

## Plan B handoff (2026-08-14, post whole-branch review)

Plan B (identity groundwork, round-2 §2 + ADR 0001) is **IMPLEMENTED**
(`a31104f..a70bf67`): typed id families (`TrackId`/`ClipId`/`ContentId`/
`SourceId`/`NoteId`) end to end through `ObjectRef` and the op wire form
(`OP_FORMAT_VERSION` unchanged — transparent serialization held); persisted
note identity (`note_id` + per-clip `next_note_id` watermark, AMEV
columnMask-honest reader); source-keyed asset naming and decode cache;
`Store`-owned slots deleted in favor of per-graph derived slots
(`derive_slots`) with a `GraphTables`/`SharedGraphTables` publish under the
session lock (round-2 §2.4, O-13's aliasing window closed by construction,
not by discipline); meter blocks stamped with the graph generation and
folded through `GenerationMaps`; `MAX_TRACKS` removed, chunked meter blocks
replace the fixed-array/u64-mask assumption. Gate B green: delete-then-undo
identity property, the slot-aliasing pin, and the meter-generation pin all
pass (`src-tauri/tests/identity_properties.rs`). Suites:
**340 backend + 73 frontend**, all green, counts dated in
README/CONTRIBUTING.

The following are binding carry-forwards for later plans, recorded here so
they aren't lost between sub-plan hand-offs:

- **Plan E:** the id-preserving piano roll gesture path
  (`midi_set_notes` still replaces arrays wholesale; ids churn on every
  replace — the *allocator/keep-rule* is correct per Task 3/round-2 §2.1
  (`assign_incoming_note_ids` in `src-tauri/src/midi/mod.rs` keeps an
  incoming id iff it is non-zero, unique in the payload, and currently
  live), but the frontend's `MidiNote` TS type (`src/lib/types/ipc.ts`)
  carries no `noteId` field at all, so every gesture round-trip mints
  fresh ids in practice — wiring the id through the wire type and gesture
  path so preservation is actually observable is Plan E's job). Midi
  mutations still bypass the transaction channel entirely; note-id
  allocation is lock-serialized (correct, never reused) but not yet
  op-attributed — no `Op` records a note mutation, so it carries no actor/
  run and produces no inverse.
- **Plan C/D:** `next_note_id` moves from `MidiClip` to the content object
  in the v3 split, semantics unchanged (still a persisted, monotonic,
  never-reset watermark); `ContentId` is declared (Task 2) but unpopulated
  — nothing mints one yet. Round-2 §2.1's split/merge/copy rules bind the
  content ops when they exist and not before: split keeps both halves' ids
  and records the partition, merge remints the absorbed side, copy mints
  fresh. No such operation exists yet; C/D's content-op work is bound by
  these rules from the day it lands, not retrofitted after.
- **Ledgered, not taken:** the waveform pyramid cache is still keyed by
  clip id, not source id — a dedup-by-source opportunity Plan B's
  source-keyed decode cache (Task 4) does NOT extend to the pyramid cache;
  left for whichever later round touches waveform rendering. Pyramids are
  now built from resampled engine-rate source data
  (`AudioEngine::ensure_loaded`, `src-tauri/src/audio/engine.rs`) rather
  than a fresh file read — this is visual-only (peak/RMS overview, not
  audio-critical) and intentional, not a latent bug.
  `GraphTables` under a `parking_lot::Mutex` is fine at today's track
  counts and rebuild rates — revisit only with a measurement (round-2
  §10.3's rule: no speculative lock-free rewrite without a bench showing
  contention). `GenerationMaps::resolve` is unused API surface (no
  production call site, only tests) — kept because Plan E's per-op
  attribution will need to resolve `(generation, slot)` back to a
  `TrackId`; deferred, not dead. `RtGraph`'s manual `impl Default` is
  test/placeholder convenience only (documented as such in-tree);
  production code always goes through `RtGraph::new` — deferred cleanup,
  not a correctness gap. `create_project` and `save_project_as`
  (`src-tauri/src/control/mod.rs`) do not reset `session.midi.dirty`;
  deferred — does not affect correctness today since both paths persist
  midi state as part of the same operation, but a future audit of dirty-
  flag invariants should account for it.
- **Coverage note:** `channel_properties.rs`'s clip-extension property
  (Gate A) is a same-track-contiguous oracle; it cannot independently
  catch positional restore bugs across interleaved clip vecs. Dedicated
  coverage for that lives in `src-tauri/tests/identity_properties.rs`
  instead (documented in its module comment) — this is by design, not a
  gap to backfill in `channel_properties.rs`.
- **Standing:** snapshot-rebuild deferral unchanged from the Plan A
  handoff above — `engine::rebuild` still holds the session lock for the
  whole graph build; Plan B's per-graph `GraphTables` changes what gets
  published, not when the lock is held. Still requires Plan F's
  copy-on-write history store. No panic rollback in `transact` (Plan F);
  transaction closures must stay non-panicking. Split/merge/copy remint
  rules (round-2 §2.1) bind future content ops from the day they land —
  see the Plan C/D item above; repeated here because it binds Plan E's
  content-adjacent work too, not only C/D's.
