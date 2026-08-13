# AURA — Core redesign, design analysis round 1

**Status: DRAFT. Not authoritative, and nothing here binds code yet.**
A second review round over this analysis is planned before any
implementation begins. Treat this document as a proposal to be argued with,
not as a decision that has been taken.

Date: 2026-08-13. Written against commit `f632580`.
Evidence: [`docs/research/`](research/00-INDEX.md) — eleven dossiers from a
fourteen-agent research round, including first-hand storage benchmarks.

---

## 0. What this round is, and is not

This round does **not** build the node graph, takes, the history browser,
the crate split, per-voice modulation, or out-of-process plugins. It builds
the three things that are impossible to retrofit and that everything else
stands on — **identity, time, and the single mutation channel** — plus the
content/placement split, because that one touches every clip reference in
the project format and therefore has to land before the format is in use.

The test for whether the scope is right: every item in this round is
something where "we'll do it later" means "we migrate every project file
saved between now and then."

---

## 1. Identity

Three tiers, named in the code, with conversion only at defined boundaries.

| Tier | Lifetime | Used by |
|---|---|---|
| `ProjectId` | forever, never reused | project file, op log, MCP, scripts |
| `Handle` | one app run | references between model objects |
| `Slot` | one compiled graph | the RT schedule, param tables, meters |

A `Slot` never reaches the project file. A `ProjectId` never reaches the RT
thread. `ProjectId` is typed per family — `TrackId`, `ClipId`, `NoteId`,
`TakeId`, `PluginInstanceId`, `SourceId` — so a `ClipId` cannot be passed
where a `TrackId` is expected.

**IDs are never reused.** Blender #149899 records what happens otherwise: a
name reused across two consecutive undo steps caused "falsely detecting
other IDs using these as unchanged, leading to undo data corruption and
crashes."

**Every object also carries a `LineageId`** that survives extraction: a
fresh `ObjectId`, the same `LineageId`. That is what makes "the same thing,
across time and takes" expressible without ever having two live objects
sharing one identity.

**Deletion does not free.** The object moves to limbo with a refcount, and
the undo entry owns it. This is Zrythm 2's `UuidReference` and Figma's
client-side undo buffer, invented independently by both. Without it,
undoing a deletion is not possible — and Ardour, demonstrably, cannot.

### What this changes in existing code

- `MidiNote` gains a `u32` id, unique within its clip. The address
  `(ClipId, NoteId)` is globally unique because the clip already has one.
  Cost today: one `columnMask` bit in the AMEV chunk format. Cost after
  projects exist: migrating every event chunk ever written. Two independent
  research threads derived this requirement from different directions —
  per-voice modulation and MPE on one, undo addressing on the other.
- `Store::free_slot` stops reusing slots. Slots become pure derived state,
  reassigned wholesale on every rebuild, with the param table repopulated
  from the document. Today a freed slot is reused — with a test asserting
  it — which means undoing a track deletion cannot restore the track's slot.
- Audio is named by content (`audio/<sourceId>.wav`) and the decode cache is
  keyed by source rather than by clip. The current clip-keyed cache would
  serve the wrong audio the moment two clips share a source, with no
  diagnostic.

---

## 2. Time

**One tagged time type with two domains.** Ardour packs this into 62 bits
plus a flag; in Rust it is an enum and we skip the bit-fiddling.
Same-domain comparison is free; cross-domain goes through the tempo map and
is named explicitly as expensive.

**Durations carry their anchor** — `TimeCnt { distance, position }`. "Four
bars" is a different number of samples depending on where it starts. This
is the most commonly missed idea in DAW time models and it is not optional.

**Tempo is stored as an integer period with a start and an end**, not as a
BPM float. Ramps become "start ≠ end" for free, and round-tripping through
the file never drifts. BPM becomes a derived, lossy view.

**A section table is precomputed** — constant-tempo segments carrying
cumulative time, beat, ppq and bar number, with curved ramps subdivided
finely enough that the error is inaudible. Both directions are then exact
linear arithmetic within a segment.

**One tempo map per block**, passed explicitly into render and immutable for
the duration of the block. Ardour's own source warns against the
alternative: "Doing this here is problematic, since it can result in each
thread using a different tempo-map in a given cycle."

**A monotonic `steady_time`** separate from song position, for LFO phase and
delay lines that must not jump when the playhead does.

---

## 3. The mutation channel

`&mut Session` is reachable only inside a transaction, and the transaction
is a closure.

```rust
session.transact(meta, |tx| {
    tx.apply(Op::ClipMove { .. })?;
    tx.apply(Op::ClipMove { .. })?;
    Ok(())
})?;
```

`apply_raw` is private. The borrow checker makes "someone mutated without
recording a command" **unexpressible** — and that is precisely why we can
have a command log where Blender cannot. Blender chose whole-database
snapshots because its mutation surface is open (C operators, Python
handlers, depsgraph writeback); ours can be closed, and Rust can enforce it.

Nesting is impossible by construction. Ardour uses an ambient
`_current_trans` field, now `assert(false)`s on nesting, and carries the old
collapse branch as dead code; Krita independently concluded "transactions
can **not** be nested!". A closure-scoped `Tx` cannot be nested at all.

**Ops are property-addressed** wherever possible — `Set { object, path,
from, to }` — with named ops only for structural change. Ardour's
`StatefulDiffCommand`, JUCE's `SetPropertyAction` and Figma's
`Map<ObjectID, Map<Property, Value>>` are the same data shape, arrived at
independently in two industries. It gives coalescing and no-op elision for
free: move something and move it back, and the property drops out of the
history entirely.

**The inverse is produced by `apply`, never guessed.** Rollback on failure
is "run the collected inverses in reverse" — the same code path as undo, and
therefore covered by the same property tests.

**Every batch carries `actor` and `run`, non-optional, stamped at the
`ControlPlane` seam.** We are already a multi-actor system without a single
network hop, because the MCP door lets an agent mutate the session alongside
the user. Attribution cannot be reconstructed after the fact; this is the
one item on the whole list that cannot be deferred.

**One transaction produces one notification, one revision, and at most one
`Rebuild`.** The transaction folds the ops' engine effect and decides for
itself — replacing today's contract where every caller must remember to send
`ControlMsg::Rebuild`, which is exactly the opt-in discipline that failed
for Ardour.

**Gestures close on an explicit boundary first, with a timer as fallback.**
Tracktion's 350 ms timer refuses to close while a mouse button is held
anywhere, and a change of target set breaks the gesture regardless. Not
time alone.

---

## 4. Content and placement

A clip becomes a **placement** referencing a named, reusable content object.
This yields linked instances, the step sequencer and section-based arranging
from one decision, and it is the precondition for `Clip.take: TakeId` later
replacing `Clip.track_id` — so that extraction never has to rewrite
`track_id` at all.

FL Studio has had this since 1998 and it is the largest single workflow gap
between FL and Live. It is nearly free now and expensive later, because it
touches every clip reference in the format.

---

## 5. Two documented decisions that change before anyone builds on them

**`SCALABILITY.md` §1 — "preassigned buffer indices" becomes a pre-reserved
refcounted pool.** Static assignment is correct only for a single-threaded,
fixed-order schedule. The moment the graph goes multicore — stage 2 of our
own roadmap — execution order is nondeterministic and a statically assigned
buffer can be read by one node while another overwrites it.

**`SCALABILITY.md` §3 — the flat chunk rope becomes a summarising COW
B-tree.** Measured, on this hardware: the flat structure is Θ(√N) per
retained version against the tree's Θ(log N) — 24 KB versus 4.1 KB at the
flat structure's own optimum, with the gap widening as N grows. The tree
additionally answers a piano-roll viewport query over 1% of a million events
in 8.8 µs, which the flat structure cannot do at all.

---

## 6. The nine side channels

Nine mutation paths currently bypass the control plane: `remove_track`,
`set_track_instrument`, `open_project`, `save_project`, all MIDI commands,
all plugin commands, automation, the sampler, and two sidecar commands.
Three of them mutate from threads that are not command threads — the engine
control thread, the LoopJam watcher, and sidecar completion sinks.

These move inside. This is not tidying: an op log with holes in it is worse
than no op log, because it looks authoritative and lies.

**Open question for round 2:** whether this belongs in round 1 or splits
into its own round. Nine touch points in mature code is real work, and the
foundation could stand finished and tested first.

---

## 7. Testing

Four property tests that must exist before any of this counts as done:

1. **Undo round-trip** — random op sequence, apply all, undo all, assert
   byte-identical state.
2. **Delete then undo preserves identity and inbound references** — the
   property Ardour's architecture cannot satisfy and Zrythm 1 failed for a
   decade.
3. **Atomicity** — a failing batch leaves the session exactly as it was.
4. **The Figma invariant** — undo a lot, copy something, redo back to the
   present, and the document must not have changed. This fails the moment a
   non-mutating action enters the op log, which is why selection, playhead
   and scroll never do.

Plus, on the engine side, **block-size invariance**: the same project
rendered at block sizes 1, 17, 64, 128, 512 and 1024 must produce
bit-identical output. One test, enormous coverage — it catches state carried
across blocks, event-scheduling off-by-ones, filters reset at boundaries,
parameter smoothing tied to block rate, and loop-wrap bugs.

---

## 8. Deferred, deliberately

The node graph with ports and buses. Takes, variants and the history
browser. The crate split and its enforcement tooling. Per-voice modulation.
Out-of-process plugin hosting.

But `note_id`, `LineageId`, `actor`, `run` and the tagged time type land
**now**, unused if necessary, because they are free today and rewrites
later.

---

## 9. Open questions for round 2

1. Does the closure transaction survive contact with the 74 existing Tauri
   commands, or does some caller genuinely need a longer-lived handle?
2. Round 1 or its own round for §6?
3. Three frontend measurements before the render architecture is locked:
   `setPointerCapture` past the window edge on WebKitGTK, WebGL2 on real
   target hardware with a startup micro-benchmark, and keydown-to-paint
   latency on a canvas-only surface. None of these numbers have been
   published by anyone.
4. The storage design's own stated biggest risk — bulk-op amplification.
   Every measured per-version number came from *point* edits; a quantize
   over 100 000 notes rewrites ~780 leaves ≈ 1.6 MB in one history node,
   roughly 400× the mean. This needs measuring against a realistic gesture
   mix before the storage model is locked.
