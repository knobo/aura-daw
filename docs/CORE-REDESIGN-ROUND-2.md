# AURA — Core redesign, round 2

**Status: DRAFT. Not authoritative, and nothing here binds code yet.**
This document supersedes [`CORE-REDESIGN-ROUND-1.md`](CORE-REDESIGN-ROUND-1.md),
which is kept for the record. Round 1 was subjected to an adversarial review
on 2026-08-13 — six independent attack agents plus a naive-questions pass —
and this round folds in what survived. Where the review **overturned** a
round-1 position, that is said explicitly in §0.1 rather than silently
edited, because the point of these documents is that the reasoning can be
checked.

Date: 2026-08-13. Written against commit `35d801d` (code identical to
`f632580` for `src-tauri`).
Evidence: [`docs/research/`](research/00-INDEX.md), plus the review findings
summarized inline. Benchmark crates: `benches/pdsbench/`,
`benches/bulkbench/` (in-repo since this round; byte counts are
hardware-independent, timings ±40 %).

---

## 0. What the review did to round 1

Round 1 had the right skeleton: identity families, never-reused IDs, a
single closed mutation channel, the content/placement split, integer tempo,
the COW tree. All of that survives. What did not survive was the
*compression*: round 1 kept the dossiers' conclusions and dropped their
caveats, and the caveats turned out to be load-bearing. Three findings were
internal contradictions (§1 vs §4 note addressing; cross-domain comparison
vs per-block maps; the §6 hedge vs the doc's own "a log with holes lies");
several "facts" about the codebase were wrong (74 commands, the decode-cache
failure mode, the slot-change rationale); and the "free today" doctrine of
round 1 §8 was exactly backwards for a codebase whose settled D-06 policy
makes late *addition* cheap and early *mis-population* the only
unrecoverable error.

### 0.1 The overturned-decisions register

| # | Round-1 position | Round-2 position | Why |
|---|---|---|---|
| O-1 | §6 side channels: "open question — round 1 or its own round" | **In round 1. The split was fake.** | actor/run is stamped at a seam the channels bypass; building `Session` *is* the channel migration; a log with holes must not be enabled, so a §6-less round 1 has no verifiable deliverable. Three reviewers and Sokrates converged on this independently. |
| O-2 | Three ID tiers (ProjectId / Handle / Slot) | **Two tiers. Handle is dropped.** | No consumer exists; its research provenance cites a dossier that does not exist; dossier 04 §4.3 warns per-run generational keys are "precisely the wrong semantics for undo". Revisit as an interning optimization behind a measurement, later. |
| O-3 | `LineageId` on every object, landed now | **Deferred to the takes round.** | Its consumers are all deferred; its value is the propagation rules (duplicate, copy, linked placement), which are the takes design; unpopulated it retrofits byte-identically (`lineage := object_id`), mis-populated it is unrecoverable. Blanket per-note lineage would cost +16 MB per 10⁶ notes for nothing v1 uses. |
| O-4 | One runtime-tagged time type (enum with two domains) | **Two compile-time newtypes** (`Ticks`, `Samples`). | AURA is statically domained (MIDI ticks, audio samples). Newtypes give strictly more safety than a runtime enum unless per-object domain locking (Ardour's requirement) is declared — it is not. This also dissolves the `Ord`-needs-a-tempo-map contradiction. |
| O-5 | `TimeCnt { distance, position }` as the stored duration type | **Durations are unanchored distances; the anchor is an argument at the tempo-map API.** | A stored anchor goes stale under our own property-addressed ops (`Set { path: "timeline_start" }` silently invalidates a sibling field's anchor). Zero current call sites need a stored anchor; only Ardour ships one, and dossier 02's own judgment prefers Tracktion's model. |
| O-6 | §5: buffer pool reversal in this round | **Out. Recorded as a constraint on the node-graph round.** | No serialized state, no code to validate against (there is no buffer machinery today), and the five-word spec had two readings on opposite sides of the RT-deallocation rule. SCALABILITY §1 stage 2 already names a simpler alternative (per-node buffer ownership) that must be weighed when the schedule compiler is real. |
| O-7 | "74 existing Tauri commands" | **66.** | Fact-checked at HEAD: 66 `#[tauri::command]` functions, all registered. 74 is a grep artifact (8 doc-comment mentions). Dossier 10's "57" was also wrong. |
| O-8 | "Nine side channels" | **A corrected inventory of ~14+ paths — see §4.5.** | The nine undercounted by a third and misattributed the cross-thread writers. Worst finds: two *read* commands mutate (lazy disk resync), and the most common gesture of all — moving a clip — never reaches the backend. |
| O-9 | Tempo as integer period (presented as a green-field choice) | **Kept — but declared as a reversal of the *shipped* v2 `tempoMap` format, with a v2→v3 migration and a full precision spec.** | Real project files with `{tick, bpm: f64}` exist since phase 2. Round 1 reversed a shipped format without saying so, and named no unit or resolution — which is the entire engineering content. |
| O-10 | §0 scope test: "in the round iff deferral means migrating every saved file" | **Three admission classes — see §1.** | Half of round 1's own content failed its single test (both §5 reversals touch no file), while two items that pass it sat in the deferred list (the placement's lane reference; the time-signature map). |
| O-11 | Block-size invariance test gates this round | **Moved to the engine round.** | It cannot pass today for reasons this round does not touch (no parameter smoothing exists — gain steps land at block boundaries by construction). Property tests 1–4 remain the gate. |
| O-12 | "The clip-keyed decode cache would serve the wrong audio" | **Justification corrected** (the change stands). | Each entry decodes from its own clip's `source_path`; sharing yields duplicate *correct* decodes. The real defects: duplicated decode memory, asset-GC coupling, and staleness when `source_path` changes under an unchanged clip id. |
| O-13 | "Undoing a track deletion cannot restore the track's slot" as the slot-reuse defect | **Rationale corrected** (the change stands, with a new obligation). | Slots are derived state; nothing "restores" one. The real defect is slot **aliasing in the async rebuild window**: `remove_track` frees the slot, then rebuilds; an `alloc_slot` in between reuses it while the old graph still plays the deleted track under that slot. The new obligation: param table and meter routing must be **versioned with the graph snapshot** (§2.4), or wholesale reassignment trades an identity bug for an audible per-edit glitch. |
| O-14 | Limbo-plus-refcount for all deleted objects, entry-owned | **Scoped and reconciled — see §2.3.** | As written it specified a leak (no decrement path under a forever-log) and violated dossier 07 rules 10/17/18 (plugin instances must never die by refcount). With persistent version retention (§6), "limbo" for plain model objects *is* the old versions; a separate refcount registry is only needed if the tree does not land. |

Everything not listed above carries over from round 1 unchanged in
substance: typed per-family ids, never-reuse, content/placement, ops as the
only mutation path, inverse-from-apply, actor/run stamping, one
notification/revision per transaction, the tempo section table, per-block
map immutability, steady_time, the COW B-tree direction, property tests 1–4.

---

## 1. Scope, restated honestly

Round 1's single admission test was rhetoric. There are **three** distinct
irreversibilities, and every item in this round is labeled with the one
that admits it:

- **[F] File-format irreversibility.** Deferral means migrating every
  project saved in between. The strongest class.
- **[A] Attribution irreversibility.** The value exists only at event time
  and cannot be reconstructed later (who did this, from which take, with
  which random seed).
- **[E] Engine-invariant irreversibility.** A discipline that must hold
  from the first line of a subsystem because retrofit means rewrite
  (RT rules, the closed mutation surface).

**In round 1 (now "the round"):**

1. Identity: two tiers, typed families **including `ContentId`**, note ids
   with a persisted allocator, source-keyed assets. **[F]**
2. Content/placement split, with the field-level schema in §5 — including
   the lane/take *reference shape*, decided now precisely because the
   feature is deferred. **[F]**
3. Time: `Ticks`/`Samples` newtypes **[E]**; integer-period tempo + minimal
   time-signature map as the v3 format **[F]**; section table, per-block
   map, steady_time **[E]** — except the section table's *subdivision
   rule*, which is versioned format semantics and therefore **[F]**
   (§3.4).
4. The mutation channel **including the side-channel migration** (§4), with
   actor/run stamping **[A]** and the op log declared a persisted,
   versioned format from day one **[F]**.
5. Storage: the summarising COW B-tree as the in-memory session structure,
   with replay-only nodes as a first-class mechanism **[E]** — explicitly
   *not* justified by a file-format deadline (§6 retires that argument).

**Moved out, with the reason:**

- Buffer pool → node-graph round (O-6). The constraint it was trying to
  state is recorded in §8.
- `LineageId` → takes round (O-3), except that §5 records the extraction
  property it must eventually satisfy.
- Handle tier → dropped until a measurement asks for it (O-2).
- Block-size invariance test → engine round (O-11).

---

## 2. Identity

Two tiers, named in the code, with conversion only at defined boundaries.

| Tier | Lifetime | Used by |
|---|---|---|
| `ProjectId` | forever, never reused | project file, op log, MCP, scripts, **all inter-object references** |
| `Slot` | one compiled graph | the RT schedule, param tables, meters |

A `Slot` never reaches the project file. A `ProjectId` never reaches the RT
thread. `ProjectId` is typed per family — `TrackId`, `ClipId` (placements,
§5), `ContentId`, `NoteId`, `TakeId`, `LaneId`, `PluginInstanceId`,
`SourceId`, `LineageId` (reserved, unpopulated until the takes round) — so a
`ClipId` cannot be passed where a `TrackId` is expected. `PortId`/`BusId`
are deliberately absent: new families are additive, and nothing in this
round persists routing.

**IDs are never reused** (Blender #149899 — noting honestly that Blender's
corruption mechanism lives in a snapshot-diff architecture we reject; the
lesson generalizes, the mechanism does not). Family ids are 128-bit UUIDs,
which also answers the copied-project question: two copies of a project
mint ids independently and cannot collide. The one *sequential* id space —
`note_id`, below — is scoped per clip content, and two copies of a project
can legitimately mint the same `(ContentId, NoteId)` for different notes;
that only matters under a cross-project merge, which is not an operation
this design supports. If it ever becomes one, the merge remints and records
the mapping — noted here so nobody discovers it as a surprise.

### 2.1 Note identity

`MidiNote` gains a `u32` id, unique within its **content object** — the
address is `(ContentId, NoteId)`, *not* `(ClipId, NoteId)`: round 1's
address was written against the pre-split model and was invalidated by its
own §4 (review finding). Requirements the review forced into the open:

- **The allocator is persisted.** A per-content `next_note_id` watermark
  lives in the AMEV chunk header (one u32). Recomputing `max+1` on load
  would reuse the id of the highest deleted note after save/reload,
  violating never-reuse against a persistent op log.
- **Allocation happens inside the transaction** (§4), which also settles
  multi-actor races — an MCP agent and the user cannot mint the same id.
- **Split/merge/copy are defined:** splitting a content object mints a new
  `ContentId` for the second half and *keeps* note ids (the op records the
  partition, so log entries resolve); merging remints the ids of the
  absorbed side and the op records the mapping; copying always mints fresh
  ids. Ops in the log are never retargeted.
- **The document `NoteId` is not the CLAP note id.** CLAP's per-voice id is
  an `i32` note-*instance* id, voice-scoped (Slot-tier by our taxonomy);
  a looped note retriggers as new instances. The document id may *seed* the
  wire id but is not it, and a u32 above `i32::MAX` must never cross the
  CLAP boundary.
- Frontend contract: the piano roll's selection is index-based today and
  `midi_set_notes` replaces whole arrays. The gesture path must become
  id-preserving (§4.4), or the op log lies about note identity. This is
  frontend work and it is in this round's cost, stated rather than hidden.

### 2.2 Sources and the decode cache

Audio is named by **source identity** — `audio/<sourceId>.wav` — and the
decode cache is keyed by source. (Round 1 said "named by content", which
conflates identity with content-addressing; blob-level content addressing
is the storage layer's concern, dossier 06 §1.4.) Justification per O-12:
deduplicated decode memory, sane asset GC, and source-change invalidation —
not the "wrong audio" claim, which was false.

### 2.3 Deletion, limbo, and who actually owns retention

The **contract** is normative and lands with this round's property test 2:
*delete then undo preserves identity and every inbound reference.*

The **mechanism** is reconciled with storage (O-14): once the session lives
in persistent structures (§6), a deleted object simply stops being
referenced by the current version while history versions keep it alive —
retention *is* the version graph, exactly as dossier 06 §1.1 states
("Undo never un-allocates an ID … No resurrection problem"). No separate
limbo registry, no per-object refcount. Said plainly, because it must be
checkable: **this sets aside dossier 04 §4.4's own verdict** (which
crowned the entry-owned refcount, Solution B, "the correct answer") — that
verdict was written before the storage dossier existed, and the refcount
variant leaks under a forever-log (the round-1 review's blocker). The two
dossiers disagreed; round 2 sides with 06 and says so. Two scoping rules
survive from the dossiers and are binding:

- **Plain model objects only.** Plugin instances are never kept alive or
  destroyed by history reachability: destruction follows the plugin
  format's state machine with a bounded keep-alive window and a state-blob
  fallback (dossier 07 rules 10, 17, 18). The history retains the *state
  blob*, never the live instance.
- **Truncation releases.** The in-memory undo window is bounded
  (bytes ceiling, steps floor — dossier 06 §1.5); eviction drops
  materialized sessions and their exclusive retained bytes. The op log
  (persisted, §4.6) retains *semantics* forever; it holds serialized
  payloads, not live objects, so "forever" does not leak memory.
- The RT schedule is an independent strong owner of whatever it plays
  (`Arc` payloads, retire-queue drop on the control side) — history
  truncation can never free something a stale graph still renders, even
  when a rebuild was dropped on a full queue.

### 2.4 Slots

`Store::free_slot` and slot reuse go away; slots become pure derived state
assigned at graph-compile time. Corrected rationale per O-13: the defect is
the alias window between free and rebuild, demonstrated by the current
code's own sequence (`remove_track` frees, then rebuilds, while the old
graph still plays the deleted track under the freed slot).

The obligation that comes with wholesale reassignment: **the param table
and meter routing version with the graph snapshot.** Each compiled graph
carries its own param table, populated at build time from the document;
meter blocks carry the graph generation so the control thread folds them
under the right slot map; smoothing state (when it exists) is keyed by
`TrackId` and moved across rebuilds like live-node state already is. The
current single shared `ParamTable` written at knob rate cannot survive
per-rebuild renumbering, and pretending otherwise would trade an identity
bug for an audible glitch on every structural edit.

While slot allocation is being rebuilt anyway, the compile-time
`MAX_TRACKS = 64` ceiling (presence masks, fixed atomic arrays) moves to
per-graph sizing. Redesigning slot assignment while silently keeping the
64-cap would be a miss against the product's own scale ambition.

---

## 3. Time

### 3.1 Two newtypes, not a tagged enum (O-4)

`Ticks(u64)` and `Samples(u64)`, compile-time distinct, zero-cost. MIDI and
musical positions are `Ticks`; audio clip anchors and engine positions are
`Samples`. Cross-domain conversion is a **named, explicit call on the tempo
map** — `map.samples_at(t: Ticks) -> Samples` — and nothing else converts.
There is deliberately **no `Ord` across domains and no ambient global map**:
Ardour's cross-domain `operator<` works only because of a process-global
RCU map, the exact pattern its own source warns about and round 1 quoted.
Mixed-domain collections are a compile error; where a mixed comparison is
genuinely needed it is `cmp_in(&self, other, &TempoMap)`.

If per-object domain *locking* (a clip the user re-anchors from musical to
absolute time, Ardour-style) ever becomes a requirement, the upgrade path
is enum-over-newtypes at the placement fields only. That requirement is not
declared today, and a runtime enum without it is strictly worse: every
match arm is a potential wrong-domain branch, serde wire formats break
(§3.6), and the 16-byte AMEV record would bloat.

### 3.2 Durations (O-5)

Durations are unanchored distances (`Ticks` or `Samples`). The
tempo-dependence of "four bars from here" is real and lives **in the API**:
`map.distance_samples(d: Ticks, at: Ticks) -> Samples`. The anchor is a
function argument, never stored state — a stored anchor goes stale the
moment a property-addressed op moves the owner, which is this design's own
§4 op model.

### 3.3 Tempo: integer period, fully specified, as a v3 migration (O-9)

Tempo is stored as an **integer period in superticks per quarter note**,
where the supertick is a sub-sample unit fixed at
**508 032 000 per second** (Ardour's superclock constant: 2¹⁰·3⁴·5³·7²,
divisible by every common rate and PPQ). Properties, which are the actual
content of the decision:

- **Sample-rate independent and persisted in the file.** Changing the
  project rate never rewrites tempo data.
- **Round-trip guarantee, stated:** a user-typed BPM is quantized once, at
  entry, to the nearest integer period (max error ~2.4×10⁻⁷ BPM at 120.5 —
  displays as 120.5 forever); storage and all derived math are exact
  thereafter. Round 1's unstated version (integer samples-per-beat) would
  have displayed 120.502 on day one.
- **Ramps:** a tempo event carries `period_start` and `period_end`
  (constant = equal). The interpolation law is **linear in period** (i.e.
  linear in seconds-per-beat), chosen and named so that the section table
  (§3.4) and any future curve UI agree; a Tracktion-style curvature
  parameter is additive later.
- **This is a v2→v3 format migration of shipped data** — real `tempoMap:
  [{tick, bpm: f64}]` files exist. Migration: each `bpm` maps to the
  nearest integer period (error below any f64 the user ever saw), with
  `project.json.v2.bak` per the established chain. Round 1 presented this
  as green-field; it is not, and pretending otherwise is how formats rot.

**The time-signature map joins the round** as part of the same v3 bump
(O-10): a minimal `meterMap: [{tick, num, den}]`, default `[{0,4,4}]`. It
is forced twice over — the section table below carries bar numbers, which
are undefined without it, and today's code **silently clobbers the user's
time signature to 4/4 on every save** (dossier 10 trap 3), which is active
data loss in the persisted time model. Meter UI can wait; the format and
the honest write cannot.

### 3.4 The section table

Precomputed constant-tempo segments carrying cumulative superticks,
samples, beat, and bar (bar from the meter map). Three specs the review
forced from hand-waves to numbers:

- **Error bound, not adjective:** curved-ramp subdivision is derived from a
  property-tested bound — max deviation from the true curve integral
  **< 64 samples (~1.3 ms at 48 kHz)** against high-resolution numeric
  integration. "Inaudible" is the test's name, not its definition.
- **The subdivision rule is format semantics and is versioned.** Change it
  and every event after a curved ramp moves on upgrade. It lives next to
  the schema version, not in a code comment.
- **Suffix invalidation:** an edit at tick T rebuilds only segments ≥ T
  (the prefix-sum structure already supports this). Full-table rebuild per
  mouse-move during a ramp drag — with every clip and lane recompiled
  downstream — is the naive implementation and is explicitly rejected.

### 3.5 Per-block map and steady_time

One immutable `Arc<TempoMap>` (with its section table) per render block,
swapped at block start; staleness of at most one block is accepted. What
this actually changes on the RT thread is now stated instead of implied:
the pre-compiled `AbsNoteEvent` pipeline **stays** (the RT thread still
never does tempo math for scheduling); the map parameter exists to serve
tempo-synced DSP and the CLAP transport block, which currently gets `None`
(dossier 10 gap 14). Those are reads, not conversions-in-anger, and they
are the seam through which "RT never converts" relaxes deliberately rather
than by accident.

`steady_time` is **one engine-global u64**, owned by the audio callback,
incremented by `frames` per callback, never reset on seek/loop/stop, and
carried across graph adoption. Offline renders define origin 0 at bounce
start so exports are deterministic. This replaces the current per-node
counter, which survives ordinary rebuilds (the node cell is reused when
its registry key is unchanged) but **resets to 0 whenever the node is
re-created** — instrument rebind, sample-rate change, a track leaving and
re-entering the live set — where a plugin *instance* that logically
continues would see steady time jump backward, violating CLAP's
monotonicity contract. One global counter removes the hazard class
instead of enumerating its triggers, and it is three sentences of spec
that round 1 left out. *(Corrected after the consistency read: the
original text said the counter resets "on rebuild", which overstated it.)*

### 3.6 The wire and the frontend

The IPC boundary keeps **flat numbers with domain fixed by field name**
(`positionSamples`, `startTicks`) — no serde enum encodings; all 18 schemas
keep their shapes, v3 adds fields under the settled D-06 additive policy.

The frontend's four coordinate systems collapse against **one shipped
bijection**: the compiled section table is pushed to the UI as data (it is
derived state, versioned with the map revision), and the TypeScript
`TempoMap` duplicate plus the three inline `samplesPerBeat` derivations are
deleted. The known live bug this kills: snapping performed on a
constant-tempo grid against a piecewise map. Whether the conversion helpers
are TS-over-the-table or shared Rust-via-wasm is a frontend implementation
choice (dossier 09 §11 flags wasm as unmeasured); the *contract* — exactly
one bijection implementation feeding every surface — is this round's.

---

## 4. The mutation channel

The shape survives review: `&mut Session` reachable only inside
`session.transact(meta, |tx| …)`, private `apply_raw`, property-addressed
ops, inverses produced by apply, mandatory actor/run, one
notification/revision per transaction. The review's verdict on the open
question was a **qualified yes**: nothing in the codebase needs a
longer-lived `&mut` than a closure — the piano roll already buffers drags
and commits once, every sidecar sink applies in one call, no command holds
a lock across an await. The qualifications are five, and they are now part
of the design rather than discoveries waiting to happen.

### 4.1 Session is defined (qualification 1)

`Session` **subsumes the five uncoordinated stores** — Store, MidiStore,
AutomationStore, PluginRegistry's document half, SamplerBank's document
half — as one document object behind one lock. Today's lock discipline
literally forbids cross-store atomicity ("store first, then midi, never
both at once"), so `set_tempo_map` is two non-atomic phases and a batch
spanning tracks+midi+plugins cannot be atomic at all. Consolidation is not
a nicety; it *is* the migration (§4.5), which is why O-1 folded §6 into
this round.

Registry state that is *not* document state (live plugin host handles, the
sampler's loaded voice data) stays outside Session, referenced by id. The
graph rebuild reads an **immutable snapshot** (`Arc` clone of the persistent
session, §6) rather than holding the Session lock — the merged lock must
not sit on the rebuild path.

### 4.2 The engine thread submits, it never holds (qualification 2)

The engine control thread today writes the store directly from five sites
(recording start and finalize — two sites, auto-stop, sample-rate
writeback, auto-project) and one of them deadlocks the naive design: `Stop` blocks on an engine
round-trip while the engine's handler takes the store lock. The inversion
is explicit:

- The engine thread **never touches Session.** It finalizes I/O and submits
  ops as `Actor::Engine` transactions through the same channel as everyone
  else (recording produces a `ClipAdd` op; auto-stop produces a transport
  op).
- **Blocking request-reply into the engine is banned inside a
  transaction.** Transactions compute state and effects; engine round-trips
  happen before (prepare) or after (effects), never under the lock.
- Op-specific RT write orderings (the park-before-playing-cleared dance)
  live in effect descriptors (§4.4), not in transaction code.

### 4.3 Two API tiers, and honest anti-nesting (qualification 3)

Every reusable mutator exists in a `&mut Tx`-taking form
(`ops::add_track(tx, …)`); `session.transact` is the only place a `Tx` is
born. Composition is therefore compile-time safe *within a call tree*. The
guarantee across the shared handle is stated honestly: `transact` on
`&self` behind a lock means a nested `transact` call **deadlocks rather
than being unexpressible** — so it is also runtime-checked (a thread-local
in-transaction flag that panics with a real message instead of hanging).
Round 1's "impossible by construction" claim was true only for the
single-reference case Rust can see.

### 4.4 Gestures, effects, and inverses — the dropped dossier machinery, restored

- **Gestures get IPC primitives (qualification 4):** `begin_gesture(target)` /
  `end_gesture()` from the frontend, CLAP-style, exactly as dossier 04
  prescribed and round 1 dropped. Between the two, updates flow as
  `transient` ops — RT-visible immediately, not journaled, not undoable,
  coalesced by merge key `(op_kind, target, actor)` — and `end_gesture`
  commits one batch. The backend timer (350 ms, refuses to close while a
  boundary is plausibly still open, target-set change breaks the gesture)
  is the **fallback for legacy/boundary-less callers**, not the mechanism:
  the backend cannot see the mouse button, so the frontend must say. Fader
  and knob drags — today boundary-free at 200 events/s — are the first
  migration targets.
- **Engine effects are descriptors, not a boolean fold.** A transaction
  folds its ops into an effect set that includes: none, param-write,
  host-forward (plugin param rings), ordered-RT-atomic sequences (park
  handshake), rebuild. Effects execute **after** the Session borrow ends.
  Effects that depend on runtime engine state (Stop-while-recording) are
  resolved by the op's effect descriptor reading the shared atomics, not by
  ad-hoc caller logic. The engine's two self-originated rebuilds (recording
  finalize, device open) enter the same accounting as `Actor::Engine`
  transactions, so "at most one Rebuild per transaction" is a claim about
  *all* rebuilds. The same folding must hold for the *undo* of a
  transaction — one snapshot per undo step (dossier 07).
- **Inverses: the escape hatch is named.** `apply` produces inverses for
  document ops. Ops whose forward effect destroys an external resource
  (`plugin_remove`) carry a **restore-from-blob** inverse: the transaction
  captures the state blob before teardown, and undo re-instantiates through
  the plugin host's state machine. Ops that reference completed I/O
  (a recorded take) are *created after* the I/O completes — the op is the
  registration, never the recording itself.
- **Prepare-outside/commit-inside** (qualification 5): import, project
  open, plugin instantiation do their unbounded work (file copy, decode,
  waveform pyramid, host round-trip) **before** the transaction, then
  commit a short apply. A failed prepare leaves no document trace,
  preserving atomicity without stalling every actor behind one serialized
  channel for seconds. Auto-created side effects (import's track creation)
  move inside the same final transaction as the clip registration, so the
  §7 atomicity test covers the once-failing path.
- **Where the diff happens:** the frontend emits per-entity ops (it has
  the ids from §2.1); the value-replacement wire commands
  (`midi_set_notes` whole-array style) survive only as compatibility
  wrappers that diff against current state server-side, with their op
  output marked coalescable. New surfaces are born per-entity.

### 4.5 The side-channel migration (was §6; O-1, O-8)

The authoritative inventory is dossier 10 §2.3 **plus the review's
additions**, not round 1's "nine": ten bypassing command paths (device
selection included), **five** sidecar completion sinks (`import`,
`stem_import`, `hum_apply`, `accompany_apply`, `instrument_register`), the
engine control thread (§4.2), the LoopJam watcher, `zyn_load_patch`'s
unmanaged global, plugin auto-persist, **two mutating readers**
(`midi_get_clips` and `automation_get` lazily rewrite state from disk —
a direct §7-test-4 violation), and the frontend-only mutations, of which
clip-move is the worst: **the most common editing gesture in the product
currently reaches no backend channel at all.** A `clip.move` op and its
command are part of this round, or the log is a lie from the first drag.

Two carve-outs, category-corrected: `open_project` is a **log epoch
boundary** (document swap, history root), `save_project` is a **snapshot
mark** — neither is an op *in* the log. Device selection mutates app
config, not the document; it stays outside the op log but moves behind the
ControlPlane for attribution.

Migration is staged *inside* the round, in dependency order: Session
consolidation → channel + effect descriptors → `remove_track` first (the
worst asymmetry) → command families → sinks and threads → mutating readers
become pure reads (resync becomes an explicit `Actor::Engine` transaction
at open/watch time). **The op log does not turn on** — no journal, no undo
exposure — **until the inventory inside is total.** An op log with holes
looks authoritative and lies; that judgment was round 1's best sentence and
it now has teeth.

### 4.6 The op log is a persisted format, versioned from day one

Ops are the journal (`journal.ndjson`, per SCALABILITY §4) — that decision
is now explicit, because it makes op kinds, property paths, actor/run and
meta a **file format**: renaming a property path breaks replay of every
journal ever written. Op schemas carry a version; the envelope inherits
D-03's draft (`op-envelope.schema.json`) including `rev`/`baseRev`, whose
`transient`/`origin` machinery §4.4 uses rather than reinvents. Property
paths are declared in one Rust module (the same place `apply` dispatches
on), so path drift is a compile error, not a silent journal break.

`actor` and `run` remain the one thing that cannot be deferred **[A]** —
and unlike round 1, the seam they are stamped at now actually sees every
mutation (§4.5), which is what makes the stamp worth the bytes. Sokrates'
version of this finding stands as the test: a stamp nobody can
cross-examine is the same lie as the holey log, so the §7 suite includes
attribution assertions from the first enabled day.

---

## 5. Content and placement

Round 1 gave the highest-blast-radius item two paragraphs; here is the
schema-level decision. A **placement** (today's clip) references a
**content object**; content owns the data, placement owns the position.

| | Placement (`ClipId`) | Content (`ContentId`) |
|---|---|---|
| position (`startTicks` / `startSamples`) | ● | |
| length (may crop content) | ● | |
| lane/track reference | ● | |
| mute, color, name-override | ● | |
| transpose / velocity offset (MIDI) | ● | |
| gain, fades (audio placement) | ● | |
| note events (AMEV ref + `next_note_id`) | | ● |
| audio source reference (`SourceId`) | | ● |
| loop/native length, name | | ● |

- **Audio clips are content-backed too** (a thin content object wrapping a
  `SourceId`), so the placement schema is uniform and SCALABILITY §3's
  `contentRef` union survives. Instancing arrives free for MIDI; for audio
  it is a byproduct, not a goal.
- **The lane reference lands now** [F]: placements reference a `LaneId`
  **and nothing else** — the placement carries no `track_id`; the track is
  reached through the lane (`LaneId → TrackId`), and every track gets one
  default lane created with it. This is the deferred take feature's
  file-format hook — deferring the *reference shape* would mean rewriting
  every placement in every saved file when takes ship, which is exactly
  the class of mistake §1 exists to catch. Takes, lanes-UI and comping
  stay deferred; only the indirection ships. (The "lane/track reference"
  row in the table above means this single `LaneId` field.)
- **The sounding-instance address** (what per-voice modulation will need)
  is recorded now: `(ClipId placement, ContentId, NoteId)` resolves the
  document note; the *voice* is Slot-tier per §2.1. Round 3 does not get to
  discover this shape was never written down.
- Existing `midiClips` rows migrate mechanically in the same v3 bump as
  §3.3 (one content object per clip, one placement referencing it).
- The extraction property `LineageId` must eventually satisfy is recorded
  for the takes round: extraction mints new object ids for placements and
  contents while lineage carries continuity; linked placements share
  `ContentId` *by construction* and need no lineage to express "same
  content".

Automation-lane identity — persisted `target_node: String` that cannot
address faders because "tracks are not nodes" — is acknowledged as a live
identity gap in the shipped format (review finding), and is assigned to the
node-graph round, where targets become ports. It is listed so the deferral
is a decision, not an omission.

---

## 6. Storage

**The flat chunk rope becomes a summarising COW B-tree — confirmed, with
the honest framing the review demanded.** The memory evidence reproduced
byte-exact (4 114 B vs 23 969 B per retained point-edit version at 10⁶
events, Θ(log N) vs Θ(√N)); the viewport claim is corrected — the flat
rope's per-chunk summaries *can* answer it in a few µs (round 1's "cannot
do at all" belonged to `imbl::Vector`); and the deadline argument is
retired: the AMEV file format and the in-memory tree are separable by the
dossier's own words. The tree is in this round because it is cheapest
before event code exists and because sorted insert (what a piano roll *is*)
measured 114× faster than the mutable baseline **while retaining history**
— an argument that needs no false urgency.

**Bulk-op amplification (round 1's open question 4) is measured and
reframed** — see the corrections block in
[`06-time-travel-storage.md`](research/06-time-travel-storage.md):

- Contiguous bulk is fine: quantize-100k retains 1.66 MB (418×), *under*
  the dossier's own 2 MB p99 line — though at the 20-byte record that §2.1
  mandates it becomes ~2.07 MB, over it.
- **The true worst case is scattered selections** (~1.5–2.3 KB/note vs
  ~17 B contiguous): 1 000 scattered notes retain 2.3 MB; humanize-10k
  scattered retains 15.3 MB. "Select all C3s and transpose" is a normal
  gesture, and an **agent-driven editor makes bulk transforms the common
  case, not the tail** — the product's own MCP premise contradicts the
  human-point-edit gesture mix the budget was calibrated on.
- Therefore **replay-only history nodes are a first-class mechanism of the
  design, not a falsifier fallback**: ops classified bulk-or-scattered
  store op + inverse payload instead of a materialized snapshot
  (replay of the 100k quantize measured at ~600 µs with
  `benches/bulkbench` — single-shot timing, ±40 % per dossier 06's
  corrected methodology note; still two orders of magnitude inside a
  hover budget). Inverse-payload rules: quantize ≈ 4 B/note
  (delta-codable), transpose O(1)+selection, humanize O(1) **iff the PRNG
  seed is stored — random ops must be seeded, which is hereby an
  implementation constraint.** Deletes need no special case (measured
  ~1.1×: dropped subtrees are free). One exclusion the mechanism needs
  stated: **ops that mint non-deterministic ids** (paste, duplicate —
  fresh `ClipId`/`ContentId` UUIDs) either carry their minted ids in the
  op payload or are excluded from replay-only; re-minting on replay would
  break every later log entry that addresses them. (`NoteId`s are safe —
  the watermark makes them deterministic.)
- Retention thresholds are restated as **absolute per-op-class caps**,
  replacing the undecidable weighted-mean falsifier — and they are now
  **measured, not estimated** (10 000-gesture weighted simulations, HUMAN
  and AGENT profiles, per-pattern trees, 20-byte records;
  `benches/bulkbench/RESULTS.md`): point class ≤ 8 KB/node (measured p99
  4.9–5.1 KB — ~60 % headroom); bulk ≤ 256 KB/node (a whole 5 000-event
  clip rewrite measures ≤ 132 KB under per-pattern granularity);
  **replay-only kicks in class-based at 64 KB of own-created bytes** — at
  the 256 KB this document originally proposed, the rule was a measured
  no-op (0.04–0.7 % saving) because whole-clip ops sit below it; at 64 KB
  it saves 21 % on the AGENT profile with a worst replay chain of 23
  clip-sized transforms, sub-ms.
- Two findings from the simulation that reshape the mechanism's role:
  **(a) the capture effect** — a later materialized node re-captures a
  replay-only op's surviving output via structural sharing, so in a mixed
  stream the bytes are retained either way; replay-only *bounds node
  charges* and saves consecutive-rewrite bursts (agent iteration), while
  **the budget is defended by eviction** (the GIMP-style ceiling), not by
  replay-only. **(b) The biggest lever is not storage at all:**
  transpose/velocity-class gestures **MUST** route through §5's placement
  offset fields (a map-row edit, no leaf rewrite) — measured 5.4× on the
  HUMAN profile (550 → 101 MB per 10 k gestures; song-wide transposes
  were 82 % of the bill) and 1 536 → 1 047 MB on AGENT.
- What the budget actually buys, measured: HUMAN ~54 700 retained steps
  in 512 MB with the settings above — a working day fits outright; AGENT
  ~6 550 steps before coarsening begins. Dossier 06's "85 000 steps" was
  point-edit arithmetic and overstates agent-driven capacity ~13×; the
  budget *mechanism* holds.
- Per-pattern granularity caps within-clip scatter (15.3 MB → ≤ 126 KB)
  but not cross-clip scattered selections (~2.7 MB per 1 000 notes across
  clips) — those stay in the replay-only class. Whole-pattern deletes
  (1.5 KB) and COW duplicates (1.0 KB) are map-path-only, confirming §5's
  linked-content economics.

Delete-then-undo retention rides the version graph (§2.3), which is why
the mechanism question and the storage question are one question, answered
together here rather than twice in conflict (O-14).

The janitor thread (retire-queue drops off-RT; measured worst drop 83.8 ms
≈ 32 buffers) remains mandatory infrastructure, unchanged from the dossier.

---

## 7. Testing

The gate for this round:

1. **Undo round-trip** — random op sequence, apply all, undo all,
   byte-identical state.
2. **Delete then undo preserves identity and inbound references.**
3. **Atomicity** — a failing batch leaves the session exactly as it was,
   including the prepare-outside paths (§4.4).
4. **The Figma invariant** — undo, copy, redo: document unchanged. This
   test currently *fails by design* via the mutating readers; §4.5 makes
   them pure before the log enables.
5. **Attribution** — every committed batch carries a resolvable
   `actor`/`run`; `Actor::Engine` and MCP actors appear where expected in a
   scripted mixed session (§4.6).
6. **Section-table bound** — property test against numeric integration,
   < 64 samples deviation (§3.4).
7. **Tempo round-trip** — typed BPM → period → display is stable across
   save/load/rate-change; v2→v3 migration is lossless against a fixture
   corpus.

Block-size invariance moves to the engine round (O-11) — it tests
parameter smoothing and event scheduling this round does not build, and a
gate that cannot pass gates nothing.

---

## 8. Constraints recorded for later rounds

- **Node-graph round:** buffer lifetime under nondeterministic multicore
  order must be solved by per-node ownership or a wait-free arena pool with
  control-side release — *decided when the schedule compiler is designed*,
  against dossier 07's rule that the RT thread never runs a nontrivial
  destructor (O-6). Graph partitions must be maximal plugin runs with no
  host-side interleaving, or out-of-process hosting arrives at Davis's
  cost instead of Bique's (dossier 09 §2.1). Automation targets become
  ports here (§5).
- **Takes round:** `LineageId` semantics per creating op (record, import,
  paste, duplicate, generation), seeded `lineage := object_id` migration,
  extraction lineage per §5.
- **Engine round:** parameter smoothing, then the block-size-invariance
  test; PDC before sends ship.

---

## 9. The UI-stack question

*(Added this round at the owner's request: is Tauri good enough to start
with, does the frontend/backend separation leave a real exit to a
Rust-native UI, and is WebKitGTK likely to close its gaps in time?)*

### 9.1 What is already known, from our own dossier

Dossier 09 is blunt: the Tauri maintainers "completely stopped defending
webkitgtk" and warn against the stack "for projects where Linux is a
serious target"; measured WebKitGTK canvas paths have run at ~5 FPS against
17–30 in Chromium on identical code; the slow path cannot be feature-
detected (llvmpipe reports success and the renderer string is masked); the
compositor is not vblank-synced, which is a scrolling-playhead problem; and
**no shipping desktop DAW has ever used a WebView for its whole UI** — we
would be first. Against that: Igalia's Skia rework delivered +91 % to
+1034 % on MotionMark canvas subtests in a year, JUCE 8 ships WebView UIs
as a first-class feature (so WebKitGTK pain now lands on a much larger
constituency than ours), and the three decisive measurements
(`setPointerCapture` past the window edge, WebGL2-with-microbenchmark on
real hardware, keydown-to-paint) **have now been run — all three pass; see
§10.1 for the numbers and `benches/ui-probe/` for the harness.** The gate
they guarded is open: the render architecture may assume a WebView-hosted
arranger on this evidence, with the pacing self-check and DMA-BUF env-var
mitigation shipped as §10.1 records.

### 9.2 The architectural answer: this round is the exit door

The honest observation is that **every item in §4 and §3.6 makes the UI
thinner, and a thin UI is what makes any migration affordable.** After this
round:

- Every mutation is an op through one channel; the frontend holds no
  authoritative state (clip-move's frontend-only life ends, §4.5).
- Time math exists once, backend-side; the UI consumes a shipped section
  table (§3.6) instead of owning a second bijection.
- The wire is schema-defined flat JSON/binary with versioned formats —
  a contract any native shell can speak. `ops_apply`/`ops_subscribe` is
  transport-shaped, not DOM-shaped.
- The heavy surfaces (arranger, piano roll, meters) are already
  canvas-painted from typed data, not DOM — the *drawing model* ports; only
  the shell (docking, menus, text input) is genuinely web-bound.

The rule this implies is cheap and binding now: **no new authoritative
state, business logic, or time math lands frontend-side.** The UI renders
pushed state and emits ops/gestures. That rule costs nothing this round
(it is §4's discipline restated) and it is the entire price of keeping the
exit open.

### 9.3 Judgment

Tauri is good enough to *start* with, on today's evidence: the failure
modes are performance-shaped, not correctness-shaped, they are measurable
(round-3 items), and the alternative — pausing the product for a UI-stack
rewrite before the core exists — is strictly worse. The realistic posture
is: ship the three measurements, hold the thin-renderer rule (§9.2, plus
the renderer-interface clause in §9.4), and treat WebKitGTK improvements
as upside rather than plan-of-record.

### 9.4 Appendix: the platform research (2026-08-13)

A dedicated research pass verified the trajectory and the native
candidates against primary sources (release notes, bug trackers,
maintainer statements). Findings that bear on the decision:

**WebKitGTK: the 2D gap is narrowing on a reliable cadence; the WebGPU gap
is not narrowing at all.** Verified: 2.46 (2024) replaced Cairo with Skia,
GPU rendering default, ~4× MotionMark on desktop GPUs; 2.48 (2025) moved
tiling to GPU worker threads; 2.50 (2025) enabled damage propagation to
the compositor *by default*; 2.52 (2026) batched canvas
recording/replay and async-scrolling work; 2.54 (due 2026-09) removes
Cairo entirely. That is a real, funded, shipping trajectory for exactly
our workload shape (canvas). Against it: **WebGPU does not exist as a
WebKitGTK work item** — absent from every 2025–2026 release note and
Igalia periodical, "nobody is working on it" per the webkit-gtk list, and
the gpuweb implementation-status wiki lists no GTK/WPE port at all. The
NVIDIA/DMABUF instability class (blank windows, resize crashes) has
recurred for 4+ years and the env-var workaround ladder is now official
Tauri documentation. **Ship that workaround ladder and a startup perf
self-test regardless of everything else** — WebKitGTK's WebGL context
reports success even when software-rasterized.

**Tauri has no near-term escape hatch of its own.** Verso (Servo webview)
was archived 2025-10 ("no longer maintained"); Servo-in-Tauri is unfunded
backlog by the maintainers' own 2026 statements; the CEF backend is
unreleased and may land as a commercial offering. The maintainers state
plainly that they cannot recommend Tauri for Linux-serious projects today
— and the asymmetry is Linux-specific (WebView2 is evergreen Chromium;
Safari shipped WebGPU in 26.0), which is the wrong shape for a Linux-first
DAW. The webview we have is the webview we will have.

**Rust-native stacks: plugin scale is proven, arranger scale is not.** As
of 2026-08, no shipping timeline/arranger application uses egui, iced,
Vizia, Slint, or gpui. Meadowlark — the one serious Rust-DAW-frontend
attempt — tried iced, then Vizia, documented why each failed at timeline
scale (no damage tracking; full-tree traversal; immediate-mode rejected
outright for thousands-of-elements surfaces), started writing its own
renderer, and is now dormant. Zrythm's own v2 rewrite chose C++/Qt6/QML.
Ranked for a future AURA shell anyway: **egui** is the least-risk
whole-app choice (real docking, AccessKit on by default, Rerun as a
GPU-heavy shipped flagship, fastest release cadence) *provided* the
timeline/piano-roll are custom wgpu paint layers rather than
widget-per-note — the immediate-mode critique applies to widgets, not to
a custom-painted canvas in an egui shell. **gpui + gpui-component** is
the higher-ceiling alternative (120 FPS custom elements are its native
idiom — Zed is exactly that; Apache-2.0; real docking) at the cost of
zero accessibility and git-dependency life. **Vizia** is right-sized for
plugin GUIs (nih-plug lineage), not a DAW shell. **iced** lacks any
accessibility story (open since 2020) and its IME shipped stable only
2025-12. **Slint** is ruled out on license mechanics alone: the
royalty-free tier forbids exposing Slint APIs from the app — toxic to any
future plugin-UI SDK.

**The migration blueprint is Figma/1Password, not Zed.** Zed's path
(build a whole UI framework) took years; Figma swapped WebGL→WebGPU
incrementally because rendering sat behind one abstraction, and
1Password survived a full shell swap because the core was UI-agnostic.
The concrete clause this adds to §9.2's rule: **all hot rendering
(arranger, piano roll, meters) goes behind one renderer interface that
targets a texture/canvas — WebGL2 today, WebGPU-capable by design — and
Svelte keeps only chrome** (menus, inspectors, dialogs). Prototyping that
interface once against wgpu (which both egui and gpui can host) is cheap
insurance; then the day the Linux webview becomes untenable, what moves
is one renderer backend and some chrome — not the product.

---

## 10. Open questions for round 3

1. ~~The three frontend measurements (§9.1)~~ **CLOSED (2026-08-13,
   `benches/ui-probe/RESULTS.md` — wry 0.55.1 / WebKitGTK 2.52.3, the
   exact versions in Cargo.lock):**
   - **`setPointerCapture` past the window edge: PASS (3/3).** Moves
     stream with coordinates up to 683 px beyond the window, ≤ 16 ms
     gaps, `pointerup` delivered outside the window. Without capture:
     clamped and lost. Drag-based arranger editing is safe; capture on
     pointerdown is mandatory.
   - **WebGL2: real hardware, order-of-magnitude throughput margin.**
     10k instanced quads ≈ 2–5 ms; 200k ≈ 10 ms GPU-side. The renderer
     string is a hardcoded lie ("Apple GPU") — software detection MUST be
     timing-based. The one genuine risk found: default DMA-BUF frame
     pacing jittered at 27–48 FPS *independent of load* on this dual-GPU
     (reverse-PRIME) box, while `WEBKIT_DISABLE_DMABUF_RENDERER=1` locked
     a flat 62 FPS — ship a runtime pacing self-check plus the env-var
     mitigation; retest on single-GPU/Wayland.
   - **keydown→paint-commit: p50 22 ms / p95 30 ms** (heavy full-canvas
     repaint adds only ~3 ms — latency is frame-scheduling-bound,
     ~1.3–2 frames). In-page commit, not photon latency.
   **Gate verdict: nothing invalidates a WebView-hosted arranger.** §9.3's
   posture stands on measurement now, not judgment.
2. ~~The weighted storage gesture mix with an *agent-editing profile*~~
   **CLOSED (2026-08-13, `benches/bulkbench/RESULTS.md`):** replay-only
   threshold 64 KB own-created bytes (256 KB was a measured no-op);
   point-cap 8 KB confirmed with headroom; transpose/velocity gestures
   must route through placement offsets (the single biggest lever, 5.4×);
   512 MB holds ~54 700 HUMAN steps / ~6 550 AGENT steps before
   coarsening. Folded into §6.
3. Session lock granularity: §4.1 merges five locks; if profiling shows
   contention (MCP agent + UI + engine submissions), the escape is
   persistent-structure snapshots for readers, not lock splitting —
   verify.
4. The op-schema versioning story meets the journal GC: how long must a
   v(N) app replay v(N−k) journals, and when does a snapshot mark permit
   dropping old op versions?
5. Whether the wire keeps JSON for op batches or adopts the binary path
   for high-rate transient ops during gestures.
