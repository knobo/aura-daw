# Command, Mutation and Undo Architecture — research dossier

**Date:** 2026-08-13
**Status:** research input, not a decision. Nothing here binds code yet.
**Owner:** architecture round preceding the op-log / identity / time spec.

> This is document 04 of the research set gathered before designing AURA's
> mutation layer. It is deliberately long and quote-heavy: the value is in
> the *failure stories*, and failure stories evaporate when paraphrased.

---

## Why this document exists

AURA has no op log, no undo, and no revision concept in the code today. That
is not an oversight — it is written policy. `ARCHITECTURE.md` §10 constraint
9 says *"Do not implement undo/redo ad hoc (UI-local or engine-local). Undo
arrives with the batched op-log protocol… the prototype ships without undo
rather than with a throwaway one."* `SCALABILITY.md` §4–§5 specifies the
protocol, `docs/ipc-schemas/op-envelope.schema.json` drafts the envelope, and
`CONTRIBUTING.md` rule 4 repeats the prohibition to outside contributors.

So the mutation layer is about to be designed from scratch, once, with the
whole product ahead of it. That is the moment to spend a day reading how
everyone else got it wrong.

Four codebases were read at source level — **Ardour**, **Zrythm 1.x**,
**Zrythm 2.x** and **Blender** — plus AURA's own tree. **Figma**,
**ProseMirror**, **Krita**, **VS Code**, **REAPER**, **Ableton's LOM**,
**Yjs**, **Automerge**, **openDAW** and the **CLAP** headers come from primary
docs and specs. Everything is tagged **[F]** (fact, with citation) or **[J]**
(engineering judgment). Where a source could not be verified it says so
rather than dressing up a recollection.

**Provenance note.** Ardour's Mantis tracker is behind a proof-of-work wall
and could not be fetched; Ardour failure evidence therefore comes from
`discourse.ardour.org`, the manual, and git history. Quoted code was
extracted by a fetching model — spot-check exact wording against the linked
URL before quoting it anywhere public.

### The five findings that dominate everything below

1. **Blender measured it: serialization is not the cost, identity
   invalidation is.** ~2.5% of undo time goes to the data, ~97% to rebuilding
   everything downstream of invalidated pointers. Optimise identity
   stability, not diff format.
2. **Ardour cannot undo track deletion**, and its lead developer says plainly
   why: *"Our undo/redo model only operates on existing objects, it does not
   delete or recreate objects."*
3. **Zrythm 1 shipped a decade of undo corruption** because object addresses
   were paths — `{track_name_hash, lane_pos, at_idx, idx}` resolved by array
   indexing.
4. **Every survivor keeps large binary data out of the undo model.** Undo
   moves references, never samples.
5. **CRDT undo is unsolved enough that Automerge removed it** for 1.0.

---

## 1. The five command-pattern variants

Evaluated on four axes: memory per step, correctness pitfalls (especially
with large binary data), coalescing, and partial failure.

### 1.1 Command objects with an explicit stored inverse

Each command stores enough state to reverse itself: `SetTrackGain { track,
old, new }`.

**Memory:** O(size of change). Best in class — a fader move is ~32 bytes.
**[F]** Zrythm 2's `MoveArrangerObjectsCommand` stores `Vec<UuidReference> +
tick_delta + original_positions`; a delta, not a clone.

**Correctness pitfalls.** The inverse must be computed against the state at
*apply* time, not record time. **[F]** Ardour's `StatefulDiffCommand` gets
this right by capturing `get_changes_as_properties()` immediately after
mutation, then calling `PropertyList::invert()` on undo.

**Opt-in capture is a bug factory.** Ardour's protocol is `clear_changes()` →
mutate → `add_command(new StatefulDiffCommand(obj))`. Forget step 1 and you
record nothing. Call step 1 inside a loop and you erase earlier work — this
is literally Ardour commit `b501eaf4` (2024-08-13), *"Fix undo when removing
multiple regions on the same track"*, where a per-iteration `clear_changes()`
meant undo restored only the last region. **[F]** And `add_command` without an
open transaction is a silent log line, not an error.

**Lifetime is the hard part.** **[F]** Ardour's commands self-destruct when
their subject dies (`DropReferences` → `command_death` → transaction removes
command → if empty, transaction `delete`s itself). Robin Gareus, commit
`4b28e4ee` (2020):

> `~StatefulDiffCommand()` may trigger `UndoTransaction::command_death()`
> which may delete the `StatefulDiffCommand()` that's just being destroyed.
> This depends on the signal-connection order, which is undefined.

Self-deleting commands inside self-deleting transactions is a double-free.

**Large binary data:** excellent — *if* the command references immutable blobs
by ID rather than owning bytes. That is the whole game (§1.6).

**Coalescing:** native and clean. Qt's `QUndoCommand::mergeWith` is the
canonical shape. **[F]** Zrythm 2's `ChangeParameterValueCommand`:

```cpp
int id() const override { return 89453187; }
bool mergeWith(const QUndoCommand* other) override {
  if (other->id() != id()) return false;
  if (duration_cast<milliseconds>(now - last_redo_timestamp_).count() > 1'000)
      return false;
  value_after_ = other_cmd->value_after_;  // keep first before_, take last after_
  return true;
}
```

**A real bug lives here [J, from source].** `id()` is a single constant for
*all* parameters, and `mergeWith` never checks
`&other_cmd->param_ == &param_`. Since `QUndoStack::push` merges with the
stack top whenever ids match, dragging fader A then fader B within one second
merges B into A's command — the merged command holds A's `param_` and B's
`value_after_`. Redo would write B's value into A.

**Lesson: the merge key must include the target identity, not just the op
kind.** Zrythm knows this elsewhere — **[F]**
`ChangeUuidIdentifiableObjectPropertyCommand` carries a doc comment saying it
*"Uses a distinct command id … so the two variants never merge with each
other — merging a UUID-safe command into a raw command would lose the refcount
safety."*

**Partial failure:** the family's weakness. If command 3 of 5 fails you must
run `undo()` on 1–2 in reverse, which requires each undo to be total. In
practice you get that only by validating the whole batch before applying any
of it. **[F]** AURA already does exactly this in `control/ops.rs`:

```rust
for c in changes {
    if !store.tracks.iter().any(|t| t.id == c.track_id) {
        return Err(format!("unknown track: {}", c.track_id));
    }
}
// ...then apply
```

with a test named `batched_mix_is_atomic_on_unknown_track`.

### 1.2 Memento / snapshot

Store the whole before-state (or whole before+after).

**Memory:** O(document) per step. This is the family that dies at scale.

- **[F]** REAPER exposes a **"Maximum undo memory use"** preference in
  megabytes and a policy knob *"When approaching full undo memory, keep newest
  undo states"* — you only need those knobs if steps are snapshots.
- **[F]** Blender's sculpt undo `GEOMETRY` nodes *"push the entire state of the
  mesh to the undo stack… stores mesh before and after modification"*, and
  enabling dyntopo stores *"an entire copy of mesh"*. Blender now runs
  **background zstd compression** on sculpt undo nodes — an admission that raw
  snapshots are too big.
- **[F]** Zrythm 1's `ArrangerSelectionsAction` deep-cloned the selection
  *twice* (`sel` and `sel_after`), plus six 800-entry pointer arrays
  (~25 KB of mostly-null pointers) in *every* arranger action. Moving 200
  notes by one tick stored two full deep clones of 200 notes.

**Structural sharing is what makes snapshots survivable.**

- **[F]** Blender's `MemFileChunk` shares buffers positionally: on a `memcmp`
  match, `curchunk->buf = compchunk->buf; curchunk->is_identical = true;` and
  only new bytes count toward `memfile->size`.
- **[F]** Blender's 2024 implicit-sharing PR (#106903, Jacques Lucke) let undo
  steps take shared ownership instead of serializing: *"this turns an O(n)
  operation into O(1)"*, *"2-5 times faster"*, memory on a subdivided mesh
  *"1.03 GB now (compared to 1.62 GB)"*. Cost: memfile steps could no longer
  be dumped as `.blend`, making auto-save up to 3× slower.
- **[F]** Krita moved *away* from per-transaction mementos: the old design
  *"required two hash tables at least 4KiB each"* per memento; the replacement
  keeps *"three hash tables per data manager only"*.

**Correctness pitfalls.**

- **Positional diffing is fragile.** Blender's chunk comparison is strictly
  index-*i*-vs-index-*i* `memcmp`; any ID insertion desyncs the whole stream.
  The patches are **[F]** (a) flush the write buffer at every ID boundary —
  *"Very important to do it after every ID write now, otherwise we cannot know
  whether a specific ID changed or not"* — and (b) a `session_uid → first
  chunk` map to resync. Meanwhile `BLI_array_store`, in the same codebase and
  used by edit-mesh undo, does proper **content-defined chunking with
  hashing**. **[F]** Campbell Barton flagged in 2018 (#56163) that memfile
  should adopt it; it never happened.
- **Runtime fields poison the diff.** **[F]** Blender had to strip
  `id->recalc` from the stream because depsgraph state leaked in and caused
  false "changed" verdicts two steps later.
- **Identity collisions silently corrupt.** **[F]** Blender #149899: *"It is
  possible that a same ID name is reused for two different IDs in two
  different consecutive undo steps… Getting the same stable pointer in this
  case can lead to **falsely detecting other IDs using these as unchanged,
  leading to undo data corruption and crashes**."*

**Large binary data:** catastrophic without COW. **[F]** Krita's answer is the
reference implementation: tiled image data with copy-on-write tiles managed by
`KisMementoManager`, so an undo step retains only the tiles the stroke
actually touched.

**Coalescing:** awkward. A snapshot per intermediate value is unaffordable, so
you must suppress pushes during a gesture and snapshot only at boundaries —
which means the "before" snapshot must be taken *before* the gesture starts,
requiring a gesture-begin signal.

**Partial failure:** trivially handled — restore the snapshot. This is the
family's one real advantage, and it is why Blender chose it (§2.4).

### 1.3 Inverse-op generation (derive the inverse at apply time)

ProseMirror is the clean exemplar. **[F]** Every `Step` implements
`invert(doc)` — *"mapping enables inverting steps in a lossless way"*. The
inverse is a function of the step **and the document it applied to**.

**Memory:** O(change), like §1.1, but you can store the inverse eagerly (fast
undo, more memory) or re-derive it (less memory, requires the pre-state).

**Correctness pitfalls:** the inverse is only valid against the document
version it was derived from. If anything else mutated in between you must
**map** the inverse forward through the intervening changes — **[F]**
ProseMirror's `StepMap`/`Mapping`, *"including the inverse relations between
the maps in it"*. That machinery is what makes concurrent undo work at all
(§3), and **[F]** it *"enables steps to survive situations where content they
targeted was temporarily deleted but later reintroduced"*.

**Large binary data:** the inverse of "delete 400 MB take" is "insert 400 MB
take" — reference, never embed.

**Partial failure:** best in class. **[F]** ProseMirror steps *can fail*: *"if
a step would create invalid token balance or violate schema constraints, it
fails and returns an error message rather than modifying the document"*.
Failure is a value, not an exception, and the document is untouched.

### 1.4 Event sourcing with projections

Store only forward events; derive state by replay; undo = replay to N, or
append a compensating event.

**Memory:** O(total history), unbounded, unless snapshotted. **[F]** The
standard mitigation is periodic snapshots — *"instead of replaying 50,000
events… load a snapshot at event 49,500 and replay only the 500 after"*.

**Correctness pitfalls:** schema evolution is the killer. **[F]** *"Events are
immutable — schema evolution must be backward-compatible. Upcasters transform
old events to new format at read time… upcasters add latency to replay"*.
**[J]** For a DAW whose project format will evolve for a decade, requiring
every historical op kind to remain replayable forever is a serious tax.

**Where it does pay: journaling and crash recovery.** **[F]** AURA's
`SCALABILITY.md` §4 already states this correctly:

> **Journal, don't re-save.** Every committed user operation (see op-log, §5)
> is appended to `<project>/.aura/journal.ndjson` (fsync'd on a debounce, e.g.
> 500 ms idle or 5 s max). Crash recovery = last saved project.json + replay
> journal. This is cheap *because* ops are already first-class for undo and
> IPC — one design, three features.

**[F]** Ardour and REAPER both persist history to disk
(`<snapshot_name>.history`, `RPP-UNDO`).

### 1.5 Structural diffing / patching

Compute a diff between two document states; the inverse patch is the undo.

**Memory:** O(diff). Good.

**Correctness pitfalls:** **[J]** JSON Patch paths are *path addresses*
(`/tracks/3/clips/2/gain`) and inherit every pathology of §4.2 — index shifts
invalidate them. **[F]** Automerge's modern recommendation is this shape but
keyed on immutable version heads rather than paths: *"you can now produce
inverse patches using `Automerge.diff` to go between any two points. To
implement a reasonable undo… record whatever document heads you consider
useful undo points and then patch between them"*.

**Coalescing:** free — diff the gesture's start and end states, skip the
middle entirely. **[J]** Genuinely the most elegant coalescing story of the
five.

### 1.6 The binary-data question, answered once

**[F]** Every system surveyed converges on the same answer, and the ones that
deviated got burned.

| System | Mechanism |
|---|---|
| Ardour | `bounce_range()` writes the new file **before** `begin_reversible_command()`; only playlist/region property diffs enter the transaction. Unreferenced sources are quarantined to `dead/` by a *separate* Clean-up tool, and flushing the wastebasket **requires closing and reloading the session first**. |
| Zrythm 2 | `AudioPool::remove_unused()` — *"Removes and frees (and removes the files for) all clips not used by the project **or undo stacks**."* |
| Krita | Tile-level COW via `KisMementoManager`; only touched tiles retained. |
| Photoshop | History states spill to the scratch disk; "Purge History" is a user-facing memory-recovery action. |

**The deviation, with receipts. [F]** Zrythm 1 issue #4955 — *"Undoing
time-stretching operation doesn't revert original audio file"*: *"The audio
sounds distorted… The waveforms are visibly different."* Fixed only in
1.0.0-beta.4.13 by changing resize semantics to revert the original object.
And Zrythm 1's undo-stack eviction had a standing TODO to delete plugin state
files, so **evicting an action leaked state files on disk**.

> **[J] Rule.** Audio bytes are immutable, content-addressed, and refcounted.
> Deleting a clip decrements a refcount; undo increments it back. Actual file
> deletion is a GC pass that consults *both* the project and the undo stack,
> runs only past the undo horizon, and never happens inside a command.

### 1.7 Summary

| | Memory/step | Binary-data safety | Coalescing | Partial failure | Verdict for a DAW |
|---|---|---|---|---|---|
| do/undo commands | O(Δ) ★★★ | good if by-reference | native (`mergeWith`) | weak — needs pre-validation | **core mechanism** |
| memento/snapshot | O(doc) ★ | needs COW | awkward | trivial ★★★ | escape hatch only |
| inverse-op generation | O(Δ) ★★★ | good | native | best ★★★ | **the correctness model** |
| event sourcing | O(history) | good | needs compaction | n/a | **journal/recovery layer** |
| structural diff | O(Δ) ★★ | poor on blobs | free ★★★ | good | gesture capture |

**[J]** These are not exclusive. The right answer is layered: op-shaped
commands (§1.1) whose inverses are derived at apply time (§1.3), appended to a
journal (§1.4), with snapshots (§1.2) as a bounded fallback for ops too gnarly
to invert.

---

## 2. How real editors do it

### 2.1 Ardour — the state-diff system that cannot create or destroy

**Architecture [F].** The transaction layer was hoisted out of
`ARDOUR::Session` into `PBD::HistoryOwner` in commit `d30c8a12` (2024-06-28):

```cpp
class LIBPBD_API HistoryOwner {
  void begin_reversible_command (const std::string& cmd_name);
  void abort_reversible_command ();
  bool abort_empty_reversible_command ();
  void commit_reversible_command (PBD::Command* cmd = 0);
  void add_command (PBD::Command* const cmd);
  PBD::StatefulDiffCommand* add_stateful_diff_command (std::shared_ptr<PBD::StatefulDestructible>);
 protected:
  PBD::UndoHistory      _history;
  PBD::UndoTransaction* _current_trans;
  std::list<GQuark>     _current_trans_quarks;
};
```

`UndoTransaction` is itself a `Command` holding `list<Command*>`; `undo()`
iterates in reverse, `redo()` calls `operator()()` forward. `UndoHistory`
holds two lists, `_depth = 0` meaning **unlimited by default**, and `add()`
unconditionally destroys the redo list — strictly linear history.

Two command flavours **[F]**:

- `MementoCommand<T>` — before/after XML, applied via `set_state()`.
- `StatefulDiffCommand` — `weak_ptr<Stateful> _object` + `PropertyList*
  _changes`; undo is
  `PropertyList p = *_changes; p.invert(); s->apply_changes(p);`

The magic is in the property itself (`libs/pbd/pbd/properties.h`) **[F]**:

```cpp
void set (T const& v) {
    if (v != _current) {
        if (!_have_old) { _old = _current; _have_old = true; }
        else {
            if (v == _old) {
                /* value has been reset to the value at the start of a history
                   transaction... thus there is effectively no apparent history
                   for this property. */
                _have_old = false;
            }
        }
        _current = v;
    }
}
void invert () { T const tmp = _current; _current = _old; _old = tmp; }
```

Two behaviours fall out free: **coalescing** (200 `set()` calls in a
transaction → one `(from, to)` pair) and **no-op elision** (move it and move
it back → the property drops out entirely). And the base class states the dual
purpose explicitly **[F]**:

> Properties are used for two main reasons:
> - to handle current state (when serializing Stateful objects)
> - to handle history since some operation was started (when making
>   StatefulDiffCommands for undo)

The same `PropertyChange` set drives
`PBD::Signal<void(const PropertyChange&)> PropertyChanged` — so
**serialisation, undo, and change notification are one mechanism.**

#### Failure story 1 — nesting was tried for 15 years, then made a hard error

**[F]** In Ardour 6.9, nested `begin`/`commit` pairs collapsed into one
transaction. In today's `history_owner.cc`:

```cpp
void HistoryOwner::begin_reversible_command (GQuark q) {
  if (_current_trans) {
    cerr << "An UNDO transaction was started while a prior command was underway. "
            "Aborting command (...) and prior (" << _current_trans->name() << ")";
    abort_reversible_command();
    assert (false);
    return;
  }
  /* If nested begin/commit pairs are used, we create just one UndoTransaction ... */
  if (_current_trans == 0) { ... } else { /* DEAD CODE */ }
  _current_trans_quarks.push_front (q);
}
```

The comment describing nesting support, the `else` branch, and the quark-stack
bookkeeping in `commit_reversible_command` are all now **vestigial** —
`_current_trans` is always null when reached.

> **[J] Lesson.** An *ambient* transaction (a hidden `_current_trans` field
> that callers implicitly join) cannot be made compositional. The moment two
> independent code paths both want to be "the" transaction, you either
> collapse them (wrong grouping) or forbid it (`assert(false)`). An **explicit
> transaction handle passed as a parameter** does not have this problem, and
> Rust's borrow checker can enforce it.

#### Failure story 2 — the ambient transaction leaks into unrelated logic

**[F]** `Session::playlist_region_added` inspects `_current_trans_quarks` to
decide whether to update session range markers:

```cpp
list<GQuark> ops;
ops.push_back (Operations::capture);   ops.push_back (Operations::paste);
ops.push_back (Operations::duplicate_region); /* ...12 more... */
set_intersection (_current_trans_quarks.begin(), ..., back_inserter (in));
if (!in.empty ()) { maybe_update_session_range (r->position(), r->end_position()); }
```

**[J]** Once "which operation is in progress" is a global, unrelated code
starts branching on it. That hardcoded list of 13 quarks is unmaintainable by
construction — every new operation must remember to add itself.

#### Failure story 3 — you cannot undo deleting a track

**[F]** Paul Davis, verbatim
([discourse.ardour.org/t/88782](https://discourse.ardour.org/t/why-is-track-deletion-not-part-of-undo-history/88782)):

> **"Our undo/redo model only operates on existing objects, it does not delete
> or recreate objects."**
>
> "deleting a track means removing a bunch of resources associated with it —
> particularly input and output ports, any plugins or other processors in use,
> disk i/o buffers and so on."
>
> "That leaves the undo operation as actually recreating the track in its
> previous state, which is totally at odds with undo/redo as a state changing
> operation."
>
> Workaround: "Mark them as inactive and hide them instead."

Regions are the one exception, and only via two mechanisms **[F]**:

1. `RegionFactory::region_map` is a global `map<PBD::ID,
   shared_ptr<Region>>` that **never releases** — every region ever created
   lives for the session's lifetime.
2. `RegionListProperty` is a `SequenceProperty<list<shared_ptr<Region>>>`
   whose `ChangeRecord { added, removed }` holds **strong references**, so the
   command itself pins removed regions alive.

Playlist serialization can then store only IDs, with the comment *"All regions
(even those which are deleted) have their state saved by other code."*

**[J]** Ardour got the *right answer for regions* and never generalized it.
The generalization is exactly Zrythm 2's registry + `UuidReference` (§2.3).
The cost of not generalizing is a missing, universally-expected feature.

#### Failure story 4 — persistable history is a hard architectural ceiling

**[F]** `Session::restore_history` can reconstitute exactly six command shapes
(`MementoCommand` / `MementoUndoCommand` / `MementoRedoCommand`,
`TempoCommand`, the three MIDI diff commands, `StatefulDiffCommand`). Anything
else hits:

```cpp
error << "Couldn't figure out how to make a Command out of a %1 XMLNode." << endmsg;
```

**Every command class defined in `gtk2_ardour/` is therefore not
persistable.** And `memento_command_factory` resolves objects via a
hand-written `if/else` chain on **demangled C++ type-name strings**, with
`/* XXX: HACK! */` on the ID lookup, playlists resolved **by name rather than
ID**, and a `Session::registry` escape hatch (`map<PBD::ID,
StatefulDestructible*>`) for GUI objects libardour can't otherwise see.
Failure is an `info` log line — you get a **silently shorter undo history**.

> **[J] Lesson.** If undo commands must be serializable, they need a **closed,
> versioned, data-only op vocabulary** — not polymorphic C++ objects
> reconstituted by type-name string matching. This is precisely why AURA's
> `op-envelope.schema.json` (`kind: "track.setGain"`) is the right starting
> point.

#### Other Ardour facts **[F]**

- Separate undo stacks per view are **"110% intentional"** (Paul Davis) —
  `HistoryOwner` is subclassed by `Editor`, `EditingContext`, `CueEditor`,
  `MidiModel`, `MidiRegion`. User-visible cost: retargeting the pianoroll
  **throws its undo history away**.
- Non-linear undo was attempted and abandoned: *"both user interface and
  implementation are a nightmare."*
- `PBD::ID` is a per-process monotonic `uint64_t` restarted at 0 each launch,
  with `/* danger, will robinson: could result in non-unique ID */` on both
  the string and integer constructors.
- Ardour 6 removed destructive "tape mode" recording; the modes *"caused more
  confusion than the problem they were intended to solve."*
- Manual: *"Limit undo history … Unchecking will keep an endless memory of
  operations to undo, at the expense of memory."* Both undo limits are
  **counts**, not bytes.
- `Session::undo` refuses while recording:
  `void Session::undo (uint32_t n) { if (actively_recording()) { return; } ... }`
- The RCU retirement lesson, quoted in full because it is the single most
  valuable artifact in this whole dossier — `libs/pbd/pbd/rcu.h`:

```cpp
#if 0 // TODO find a good solition here...
        /* if we are not the only user, put the old value into dead_wood.
         * if we are the only user, then it is safe to drop it here.
         */
        if (1 != _current_write_old->use_count ()) {
                _dead_wood.push_back (*_current_write_old);
        }
#else
        /* above use_count() condition is subject to a race condition.
         *
         * Particulalry with JACK2 graph-order callbacks arriving
         * concurrently to processing, which can lead to heap-use-after-free
         * of the RouteList.
         *
         * std::shared_ptr<T>::use_count documetation reads:
         * > In multithreaded environment, the value returned by use_count is approximate
         * > (typical implementations use a memory_order_relaxed load).
         */
        _dead_wood.push_back (*_current_write_old);
#endif
```

  Ardour tried the obvious optimisation — "if nobody else holds it, free it
  right now" — and it produced a **heap-use-after-free of the `RouteList`**.
  The fix was to stop being clever and *always* retire.

  **[J]** Note *why* the same `use_count()` check is safe a few lines away, in
  `write_copy()`'s dead-wood scan: once an object is in `_dead_wood`,
  `managed_object` no longer points at it, so no reader can obtain a *new*
  reference and the count is monotonically non-increasing. **Reachability is
  the invariant that makes a refcount observation meaningful.** Encode that in
  your types, not in a comment.

- `rt_safe_delete` + the butler thread — a runtime-checked, per-object
  deferred free, used with a custom `shared_ptr` deleter at exactly the
  graph-swap site:

```cpp
/* However, the graph-chain may be in use (session process), and the last reference
 * be helf by the process-callback. So we delegate deletion to the butler thread. */
_graph_chain = std::shared_ptr<GraphChain> (new GraphChain (g, edges),
                 std::bind (&rt_safe_delete<GraphChain>, this, _1));
```

### 2.2 Zrythm 1.x — the path-addressing catastrophe

**[F]** `UndoableAction` is a C tagged union with exactly ten concrete
subclasses, dispatched by a macro-expanded `switch`:

```c
#define DO_ACTION(uc, sc, cc) \
  case UA_##uc: { ret = perform ? sc##_action_do ((cc##Action*) self, error) \
                                : sc##_action_undo ((cc##Action*) self, error); } break;
```

The same switch shape is repeated for `_free`, `_to_string`, `_contains_clip`,
`_get_plugins`, `_needs_pause` — **six parallel switches kept in sync by
hand.**

`UndoStack` is a fixed-capacity ring, depth from GSettings, serialized as **ten
parallel typed arrays** plus an `int stack_idx` per action to reconstruct
ordering (cyaml has no polymorphism).

**The root cause — `RegionIdentifier` [F]:**

```c
typedef struct RegionIdentifier {
  RegionType type; int link_group;
  unsigned int track_name_hash; int lane_pos; int at_idx; int idx;
} RegionIdentifier;
```

resolved by literal array indexing:

```c
lane = track->lanes[id->lane_pos];
return lane->regions[id->idx];
...
static ArrangerObject* find_midi_note (MidiNote* src) {
  ZRegion* r = region_find (&((ArrangerObject*)src)->region_id);
  return (ArrangerObject*) r->midi_notes[src->pos];   // no validation at all
}
```

**Every coordinate in that address is mutable.** Rename a track → new hash.
Delete a region → every later `idx` shifts. Delete a note → every later `pos`
shifts.

**The resulting bugs [F]:**

- **#3486** *"Deletion and Undo of MIDI notes leads to unexpected behaviour"*:
  delete a note, Ctrl+Z, select a note above it, press Up — *"**the wrong MIDI
  note will be moved**. This only happens with the MIDI notes above the
  previously deleted."*
- **#3537** *"crash when undoing movement of region from one lane to
  another"*, backtrace: `region_find → find_region → arranger_object_find →
  do_or_undo_move → undoable_action_undo → undo_manager_undo`.
- **#4164** *"undo history gets cleared when deleting a track"* — and the
  changelog oscillates: first *"Clear undo history when deleting channel slots
  or tracks with uninstantiated plugins"*, then *"Fix undo history getting
  cleared when deleting tracks."* When you can't reliably resurrect an object,
  nuking history is the fallback.
- Changelog, 2019-03-29 — they knew: *"Undo/redo technical improvement
  (**revert objects to their original IDs**)."*
- Changelog: *"Upgrade project format and **drop undo history** when loading
  older projects"*, *"Don't save undo history with backups"*, *"Fix error when
  sending a bug report for a project with many actions on the undo stack"*.

**[F]** And `find_region` **verifies positions match between the clone and the
resolved project object and logs warnings when they don't** — a tell that
stale addresses were routine, not exceptional.

### 2.3 Zrythm 2.x — the rewrite, and it is the reference design

**Typed UUIDs per family [F]:**

```cpp
template <typename Derived>
class UuidIdentifiableObject : public UuidIdentifiableBase {
  struct Uuid final : type_safe::strong_typedef<Uuid, QUuid> {};
  UuidIdentifiableObject(QObject* p = nullptr)
      : UuidIdentifiableBase(QUuid::createUuid(), p) {}
  friend void init_from (UuidIdentifiableObject& obj, const UuidIdentifiableObject& other,
                         utils::ObjectCloneType clone_type) {
    if (clone_type == ObjectCloneType::NewIdentity) obj.set_raw_uuid (QUuid::createUuid ());
    else                                            obj.set_raw_uuid (other.raw_uuid ());
  }
};
```

A `Track::Uuid` cannot be confused with an `ArrangerObject::Uuid`.
Clone-with-new-identity vs clone-preserving-identity is an **explicit
parameter** — compare Ardour's thread-local
`set_regenerate_xml_and_string_ids_in_this_thread` flag.

**A refcounted registry [F]:**

```cpp
class IObjectRegistry {
  void register_object (UuidIdentifiableBase& obj);
  void acquire_reference (const QUuid& id);
  void release_reference (const QUuid& id);
  [[gnu::hot]] UuidIdentifiableBase* find_by_raw_uuid (const QUuid& id) const;
};
```

> "Reference counting: acquire_reference()/release_reference() manage
> reference counts. When the count drops to zero, the registry may delete the
> object. UuidReference and TypedUuidReference handle this automatically via
> RAII."

**[F]** Note the threading discipline: `register`/`acquire`/`release` all
`assert_main_thread()`, while `find_by_raw_uuid` deliberately does not —
*"Lookups are read-only and safe from the Qt render sync thread (which runs
while the main thread is blocked)."*

**Undo of deletion becomes trivial [F]:**

```cpp
class RemoveArrangerObjectCommand : public QUndoCommand {
  void undo() override { object_owner_.add_object (object_ref_); }
  void redo() override { object_owner_.remove_object (object_ref_.id ()); }
  ArrangerObjectUuidReference object_ref_;   // the refcount pins the removed object
};
```

The design intent is written down **[F]**:

> holds a `utils::UuidReference` whose RAII refcount **prevents the
> UUID-identifiable object from being deleted while the command is on the undo
> stack**. This makes it safe for objects whose lifetime is tied to a mutating
> model (e.g. automation points that can be removed by other undo commands).

**`DeleteTracksCommand` shows the ordering discipline [F]:**

```cpp
struct TrackInfo { TrackUuidReference ref; int original_position;
                   std::optional<Track::Uuid> original_folder_parent;
                   bool original_expanded_state; };

void redo() { /* sort by position DESCENDING */
              for (i) collection_.remove_track (i.ref.id()); }

void undo() { /* sort by position ASCENDING  */
              for (i) collection_.insert_track (i.ref, i.original_position);
              /* then restore folder parents, then expanded states */
              collection_.notify_tracks_moved (deleted_uuids_); }
```

Remove in reverse position order, re-insert in forward position order — the
classic index-shift trap, solved. And **exactly one** notification at the end.

**The RT seam lives on the stack [F]:**

```cpp
void UndoStack::execute_with_engine_pause_if_needed (const QUndoCommand& cmd,
                                                     const std::function<void()>& action) {
  const auto recalc_graph = command_or_children_require_graph_recalculation (cmd);
  const auto pause_engine = command_or_children_require_engine_pause (cmd) || recalc_graph;
  if (pause_engine) callback_with_paused_engine_requester_ (action, recalc_graph);
  else              action ();
}
```

Commands declare `static constexpr int CommandId`; the stack checks the
command **and all its macro children recursively** against two whitelists.
**[J]** This is a regression from Zrythm 1's virtual `needs_pause()` — a new
command that mutates the graph and forgets to add itself to that array will
corrupt the running graph, silently.

**Macros are lazy [F]:**

> Macro creation is lazy: `beginMacro()` only records the macro text, and the
> macro is realized on the underlying `QUndoStack` when the first command is
> pushed. `endMacro()` without any intervening `push()` discards the macro, so
> **empty macros never pollute the undo stack** (QUndoStack itself has no API
> to remove an empty macro once begun).

This is Ardour's `abort_empty_reversible_command()` problem solved
structurally rather than by discipline.

**Sample-rate resilience [F].** `UndoableAction` stores `sample_rate_` and
`frames_per_tick_` at construction, and `init_loaded()` **rescales** by
`engine_sample_rate / sample_rate_` when the stack is reloaded. Sample rate is
an ambient property the undo stack adapts to, not one it can change.

**Engine interaction [F].** `AudioEngine::wait_for_pause()` calls
`panic_all()` (MIDI panic) **before** requesting the pause, then — after
`run_` is cleared and the processing lock taken and released — runs **one more
one-sample cycle**, commented `/* run 1 more time to flush panic messages */`.
`resume()` does
`transport_.move_playhead (transport_.playhead_ticks_before_pause (), false)`.
Undo asserts it is off the DSP thread
(`z_return_if_fail (ROUTER->is_processing_thread () == false)`) and serialises
actions behind `action_sem_`. `needs_pause()` defaults to `true`.

**[F] Verified negative:** Zrythm has **no** `free_later` / idle-deferred-free
mechanism, in either the C or C++ tree. `rechain_from_node_collection` calls
`release_node_resources()` then `graph_nodes_ = std::move (nodes)` — the old
collection is destroyed **inside the critical section**, potentially while the
audio thread is blocked.

**One regression [F]:** `undo_stack.h` reads `// persistence (TODO)`. Zrythm 1
serialized its undo stack into the project; Zrythm 2 does not yet.

### 2.4 Blender — the definitive scaling case study

**Architecture [F].** `UndoStack` / `UndoStep` / `UndoType` in
`BKE_undo_system.hh`:

- `UndoStack` holds `ListBaseT<UndoStep> steps`, plus `UndoStep *step_active`
  and — tellingly — **`UndoStep *step_active_memfile`**, *"last memfile state
  for library consistency."*
- `UndoStep` carries `name[64]`, a `const UndoType *type`, `size_t data_size`,
  and flags including `use_memfile_step` (*"stores global state for edge
  cases"*), `use_old_bmain_data` (*"allows reusing unchanged data-blocks"*),
  and `is_applied` (*"tracks accumulating changes (sculpt, painting)"*).
- `UndoType` is a vtable: `step_encode_init`, `step_encode`, `step_decode`,
  `step_free`, **`step_foreach_ID_ref`**. Blender has a **pluggable,
  polymorphic undo system** — multiple co-existing implementations on one
  stack.
- `UndoRefID` exists because *"pointers are not stable and may have changed
  when restoring the undo-step"* — restoring an undo step *reallocates the
  world*, so every cross-reference must be re-resolved.

From `undo_system.cc` **[F]**:

- Verbatim: **"Odd requirement of Blender that we always keep a memfile undo
  in the stack."** The developers' own comment calls their invariant *odd*.
- Verbatim: **"Make sure we don't apply edits on top of a newer memfile
  state"** — referencing bug #56163. There is an explicit ordering hazard
  between the local and global undo systems, discovered as a bug and patched
  with a guard.

**What a "memfile" is [F].** `MemFile` is a linked list of `MemFileChunk` plus
a total size. **An undo step is a serialised `.blend` file held in RAM.**
Chunk reuse is the mitigation: `BLO_memfile_write_init()` takes a reference to
the previous step and builds *"a mapping from ID session UIDs to their
corresponding memory chunks, allowing reuse even if data reordering occurs"*.
There are *two* identity flags, `is_identical` and `is_identical_future`,
because undo and redo are not symmetric — *"this is fine in redo case, but not
in undo case, where we need an extra flag defined when saving the next
(future) step after the one we want to restore."* `BLO_memfile_merge()`
transfers chunk ownership when a step is evicted.

The read path is a three-way dispatch **[F]**: identical → reuse the old ID
untouched; changed → read new content then `BKE_lib_id_swap_full` into the
**old address**; new → read at a new address. The rationale, verbatim:

> This allows us to keep the same pointer even for modified data, which helps
> reducing further detected changes by the depsgraph (since unchanged IDs
> remain fully unchanged, even if they are using/pointing to a changed one).

#### The failure story, quantified

**[F]** Brecht Van Lommel, 2018: *"It is effectively reloading the entire
scene yes… There is no project underway to improve undo performance
currently."*

**[F]** Bastien Montagne, 2019 — the load-bearing measurement:

> avoiding reading of unchanged IDs saves about 30% of the read process time…
> around 100ms (130 ms with current master code, 90ms with code in the
> branch), **when the actual undo step takes about 4 seconds from a user
> PoV**. So main optimization is clearly to be sought into the scene
> update/rebuild happening after undo 'memfile' has been read.

~2.5% of undo time on the data; ~97% on rebuilding downstream of invalidated
pointers.

**[F]** User reports from #60695:

| Scene | Undo time |
|---|---|
| "more or less big scene" (2.79) | **45 s** |
| 10–50 M polys, "tens of thousands of objects" | **"minutes"** |
| imported automotive CAD FBX | **"at least a couple of minutes"** |
| >12 M tris object mode | 33 s (2.82) → 1 s (2.90 alpha) |

And the adoption cost: *"It's not just that we need to wait 5-10 seconds for
the Undo, it's also all the times you avoid Undoing because you know it's
going to lock your computer for a while."* / *"I've switched to maya just
because of this."*

**[F] T60695 "Optimized per-datablock global undo" has been open since
2019-01-21 and is still open in 2026.** Step 4, "write only changed
datablocks", is not done.

#### The 2026 regression — same disease, new vector

**[F]** Blender 5.0 introduced stable-pointer generation at write time; undo
regressed. PR !155587 (mont29, 2026-03-12):

> Generating stable addresses for all pointers when writing memfile undo steps
> is not a practical option, as with certain user cases… it implies
> **generating millions of stable addresses, which translates into seconds of
> lag**.
>
> In practice, stable addresses are almost never necessary in undo case anyway
> - in fact, they can even have an **adverse effect, potentially preventing to
> detect an ID as modified**.

| Issue | 4.5 | main (5.1 dev) | after fix |
|---|---|---|---|
| #151827 pose mode, 51 actions × 1000 keys × 256 bones | 0.85 s | 1.15 s | 0.95 s |
| #150350 appended CAD geometry | **0.025 s** | **0.6 s** | 0.11 s |

**[F] The first memfile-undo unit tests and perf regression tests landed in
2026** — *after* the regression shipped. Blender's global undo went ~20 years
without them.

#### The two-tier leak — nine years of ordering bugs

**[F]**

- #50423 (2017) — hook added in edit mode breaks undo/redo.
- #56163 (2018, Campbell Barton) — *"Resolve bugs where **changes to data
  outside a 'mode' are ignored by the undo system**."* Estimate: *"possibly
  2-4 weeks."*
- #75013 (2020, still open) — edit a vertex, change object scale, Ctrl+Z →
  you undo the *vertex edits* first.
- **Auto-save collateral damage:** *"**auto-save will ignore any changes (undo
  steps) made in the following modes**: Edit Armature, Curve/Curves, Font,
  Lattice, MetaBall, Mesh; Paint Curve, Image; Sculpting; Particle Edit; Text
  Editing… IMHO, it's a stretch to call this 'auto-save' right now."*
- Fixed only in 2026 by PR !161566, whose change-detection scan costs
  *"~35ms"* per undo push — a scan that exists **only because Blender cannot
  know what an operator touched**.

#### Why Blender did *not* use a command pattern

**[F]** No document rejects it by name; there are three specific technical
reasons, all from Brecht:

> However, the problem is that we don't actually know for certain that there
> have been no changes relative to the current state. For a few reasons:
> - **Nearly all operators will do an undo push after making changes, but not
>   all.**
> - **Dependency graph evaluation may flush back some data to the original**,
>   and this happens after undo push.
> - **Python app handlers may modify the scene in arbitrary ways.**

And the decisive asymmetry:

> If it is not done correctly, then instead of a missing refresh there is a
> **more serious bug of not undoing all changes**.

Plus, on the abandoned scoped approach:

> there are **dependencies between datablocks in many situations** that make
> per-datablock undo problematic. **Bugs and unexpected behavior due to this
> is exactly why we moved away from it.**

> **[J] Lesson.** Blender's snapshot approach is a deliberate correctness
> trade against an **open, unbounded mutation surface** (C operators + Python
> addons + depsgraph writeback + modal tools) over a **globally shared mutable
> database with cross-datablock raw pointers**. A command log requires a
> *closed* set of mutations. Blender doesn't have one and, given its
> extensibility model, can never have one. **AURA can. That is the decisive
> difference, and it is why AURA should not copy Blender.**

And Brecht's own retrospective **[F]**, which points the opposite way from
what you'd expect:

> I also think that ideally **everything should be stored in the memfile undo
> stack, rather than having a single stack but still separate storage that
> continues to cause problems**. This is what I hoped the unified undo stack in
> 2.8 would be, but **it didn't go that far.**

Blender's conclusion after eight years is that the *two-tier split* is the
source of the pain, not snapshots per se.

#### Blender's eviction algorithm, and its identity machinery

**[F]** `BKE_undosys_stack_limit_steps_and_memory()` walks **newest → oldest**,
accumulating `data_size`, and cuts when the budget is blown. Three properties:

1. **The newest step is always kept, even if it alone exceeds the budget.**
   The limit is applied *after* the push, so a push never fails for budget
   reasons.
2. A hard floor of **two** steps ("keep at least two (original + other)").
3. Steps flagged `skip` don't count toward the step budget.

Defaults: `undo_steps` **32** (range 0–256), `undo_memory_limit` **0 =
unlimited**. `undosteps == 1` is silently coerced to 2, with the source
comment *"Do not allow 1 undo steps, useless and breaks undo/redo process (see
#42531)."*

**[F]** Blender's architecture doc names an axis worth borrowing: **Relative
vs Absolute** steps. *"Currently, Blender undo stack is fully relative"* — to
reach a given step you must replay everything between.

**[F]** `session_uid`: *"A session-wide unique identifier for a given ID, that
remain the same across potential re-allocations (e.g. due to undo/redo
steps)."* Introduced **because** the "never reuse a memory address" branch
failed: *"We are going to try and use session-wise uuids for data-blocks
instead."*

**[F]** bpy gotchas: *"you should assume that undo and redo **always
invalidates all `bpy.types.ID` instances**"*, and even the modern partial
behaviour comes with *"**there is no guarantee of any kind that it will be
safe and consistent. Use it at your own risk.**"*

**[F]** Crash classes 2025–26: no-undo IDs referencing undoable IDs;
shader-editor space data holding stale pointers; async icon-preview render
jobs holding pointers across the free; interior pointers (pose→Bone) needing a
bespoke `POSE_RECALC` special case. **Anything holding a pointer not reachable
via `foreach_id` is a latent undo crash.**

### 2.5 Figma — the multiplayer undo semantics

**[F]** Data model: `Map<ObjectID, Map<Property, Value>>` — equivalently *"a
database with rows that store `(ObjectID, Property, Value)` tuples"*.
**Children store links to parents**, not the reverse — this *"preserves object
identity during reparenting"* and avoids dropping concurrent edits. Parent
link + fractional position *"must both be stored as a single property so they
update atomically"*. LWW per property, resolved by server timestamp. OT was
rejected as *"unnecessarily complex"* and *"very complicated and hard to
implement correctly"*; full CRDTs were relaxed because a central server is the
authority. Object IDs embed a per-client ID so offline clients never collide.

**The undo principle, verbatim [F]**
([figma.com/blog/how-figmas-multiplayer-technology-works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/)):

> Undo history has a natural definition for single-player mode, but undo in a
> multiplayer environment is inherently confusing. If other people have edited
> the same objects that you edited and then undo, what should happen? Should
> your earlier edits be applied over their later edits? What about redo?
>
> We had a lot of trouble until we settled on a principle to help guide us:
> **if you undo a lot, copy something, and redo back to the present (a common
> operation), the document should not change.** This may seem obvious but the
> single-player implementation of redo means 'put back what I did' which may
> end up overwriting what other people did next if you're not careful. This is
> why in Figma an undo operation modifies redo history at the time of the
> undo, and likewise a redo operation modifies undo history at the time of the
> redo.

**[F]** And: deleted objects' properties *"are stored in the undo buffer of the
client that performed the delete"* rather than on the server — *"keeping
documents from growing indefinitely."*

> **[J]** This is Figma independently arriving at "the undo entry owns the
> deleted object" — the same conclusion as Zrythm 2's `UuidReference`, reached
> from a distributed-systems direction. And "undo modifies redo history at the
> time of the undo" is the precise statement of §1.3: **the inverse is derived
> against *current* state, not restored from a snapshot.**

### 2.6 Krita — the reference for large binary data

**[F]** `KisTransaction` is a thin wrapper owning a `KisTransactionData` (a
`QUndoCommand`). The lifetime rule: *"when you delete this object, it reports
to the data manager about its death, and the latter one free's all the
history."* Tiled data with copy-on-write; the memento manager retains only
touched tiles.

The memory motivation, verbatim: the old per-transaction memento design
required *"two hash tables at least 4KiB each"*; the centralized replacement
keeps *"three hash tables per data manager only"*.

Two quirks worth noting **[F]**:

- *"It's first invocation (the one that will be done by QUndoStack) does
  nothing!"* — the command was already applied optimistically, so the stack's
  first `redo()` must be a no-op. **This is the "apply-then-record" pattern,
  and it is a classic source of confusion.**
- *"transactions can **not** be nested! Be sure that you have finished
  previous transaction before requesting a new one."* — Krita reaches the same
  conclusion as Ardour 7, independently.

**[F]** Krita also ships the tiering primitive: `KisCumulativeUndoData` with
`excludeFromMerge = 10` — *the most recent 10 commands are never merged*, only
older history gets coalesced. Plus `mergeTimeout 5000ms`,
`maxGroupSeparation 1000ms`, `maxGroupDuration 5000ms`. Default **off**.

### 2.7 ProseMirror — the cleanest correctness model

**[F]** Documents are **values, not stateful objects** — *"every time you
update a document, you get a new document value."* `Step.apply(doc)` returns a
result that **can fail** rather than mutating. Every step produces a `StepMap`
for position translation; `Mapping` accumulates them *"including the inverse
relations between the maps in it"*. Transactions are atomic: *"all steps
applied at once"*, producing one new `EditorState` and one view update.

History grouping is time-based (`newGroupDelay`), with `closeHistory()` to
force a boundary and an **`addToHistory` meta flag to exclude a transaction
from undo entirely**.

> **[J]** `addToHistory: false` is the single most useful primitive nobody
> remembers to design in. In a DAW it is what lets a background render job, a
> meter update, or a plugin's own automation write commit through the same
> channel *without* becoming an undo entry.

### 2.8 VS Code, REAPER, Ableton LOM, CLAP

**VS Code [F]:** multi-file undo is a long-standing hole (issue #638) — *"When
you do a rename operation that makes edits in multiple files, undo does not
correctly undo the edits in files other than the active document… This affects
any extension which provides a WorkspaceEdit crossing multiple files."*
**[J]** Per-resource undo stacks cannot express a cross-resource transaction —
the same disease as Ardour's per-view `HistoryOwner`.

**[F]** VS Code's coalescing is worth copying: consecutive typing appends into
one element labelled "Typing", and `close()` **serializes the element to a
compact binary form** the moment it stops being appendable — an in-memory
compaction step triggered exactly when a node becomes immutable. Barriers are
explicit (`pushStackElement`/`popStackElement`; the extension API's
`undoStopBefore`/`undoStopAfter`).

**REAPER [F]:** a **"Maximum undo memory use"** setting in MB; a policy for
which states to drop when full; **undo trees** (multiple redo branches); undo
states that optionally include selection/cursor; and history persisted to
`RPP-UNDO` files — *"Even at some later date, you will still be able to revert
the project to an earlier state if you wish."* Branching, verbatim:

> Enabling the option to **Store multiple redo paths where possible** means
> that whenever you go back to an earlier point any actions you take from that
> point on will be stored as an alternate set: REAPER will remember both paths
> independently of each other.

The UI answer to "how do you display a DAG": REAPER doesn't. It shows a
**linear list** and annotates the branch point with **`(*2)`** — right-click to
choose the path.

**[F]** REAPER's changelog shows the cost of full-project snapshots: *"Undo:
improved memory use, **scan for common blocks in history when adding
states**"*; *"Undo: **incrementally updated RPP-UNDO files**, can make for much
faster save of undo history"*; *"Undo system: greatly reduced memory use when
loading undo history from file"*. And a per-plugin escape hatch for blob
bloat: **"Save minimal undo state"** and **"Avoid loading undo states where
possible"**.

**[J]** REAPER is the only surveyed DAW shipping persistent + byte-bounded +
non-linear undo. It is prior art, not a gap.

**Ableton LOM [F]:** `live.path` resolves a human path (`live_set tracks 0
mixer_device volume`) to an object ID; the ID *"remains unchanged as long as
the object exists"* — but *"When an object is deleted and a new object is
created at its place, it will get a new ID."* Paths are index-based, so
inserting a track shifts every downstream path. **This is exactly the trap
AURA's MCP surface must not fall into.**

**CLAP [F]:** `CLAP_EVENT_PARAM_GESTURE_BEGIN` / `_END` bracket parameter value
events — *"This is not mandatory… but this improves the user experience a lot
when recording automation or overriding automation playback."* The plugin API
already gives you the exact coalescing primitive; use it as the model for your
own gesture protocol rather than inventing timeouts.

**[F]** CLAP's draft shared-undo extension (`ext/draft/undo.h`) is worth
reading even though it is draft:

> This extension enables the plugin to merge its undo history with the host.
> This leads to a single undo history shared by the host and many plugins.
>
> Some changes are long running changes… the plugin will call
> `host->begin_change()` to indicate the beginning of a long running change and
> complete the change by calling `host->change_made()`.
>
> This leads to another important consideration: **starting a long running
> change without terminating is VERY BAD, because while a change is running it
> is impossible to call undo or redo.**
>
> Rationale: multiple designs were considered and this one has the benefit of
> having a single undo history. This simplifies the host implementation,
> leading to less bugs, a more robust design and maybe an easier experience for
> the user because there's a single undo context versus one for the host and
> one for each plugin instance.

Deltas are optional (`clap_undo_delta_properties { has_delta,
are_deltas_persistent, format_version }`) — falling back to "host snapshots
plugin state" when the plugin doesn't provide one. That fallback is exactly the
memento/diff hybrid Ardour uses.

### 2.9 Budget models, for reference

| Product | Bound | Notifies on eviction? |
|---|---|---|
| **Pro Tools** | 1–64 steps | **Yes — row turns red one step before eviction** |
| Emacs | `undo-limit` 160 K soft / `undo-strong-limit` 240 K hard bytes + per-command `undo-outer-limit` | Only for the per-command outer limit |
| REAPER | max undo memory (MB) | No — but the user *chooses* the eviction policy |
| GIMP | bytes/image (⅛ RAM) + floor of 5 steps | No |
| Blender | 32 steps + optional MB | No |
| Krita | 200 steps | No |
| Vim | 100 changes (1000 on Unix/VMS/Win32); Neovim 1000 everywhere | No |

**[F]** Pro Tools' warning, verbatim: *"When the oldest operation is one
operation away from being pushed out of the queue, **it is shown in red**."*
Pre-emptive, in-place, non-modal, zero chrome.

**[F]** Emacs' `amalgamating-undo-limit = 20`, gated on
`(eq this-command last-command)`.

**[J]** Byte-bounded with a count floor is strictly better than either alone.
One "import 400 clips" step should not evict 400 fader moves, and 400 fader
moves should not evict the import.

---

## 3. Undo in the presence of concurrency

### 3.1 The actual problem AURA has

**[J]** AURA is **not** a multiplayer editor. There is one authoritative
document, one process, one commit point. What it has is **multiple actors
sharing one linear history**:

| Actor | Character |
|---|---|
| GUI | interactive, gestural, expects "Ctrl+Z undoes what I just did" |
| MCP agent | batch, semantically large ("add a chorus section"), possibly long-running |
| Scripting | batch, may be a loop of thousands of ops |
| Internal processes | *unsolicited* — a recording stops, a generation job lands |

This is materially easier than Figma's problem (no concurrent divergent
replicas, no merge) and materially harder than a single-user editor
(mutations arrive that the user did not initiate).

**The key point: AURA is already a multi-actor system, with zero networking,
because the MCP front door lets an agent mutate the session alongside the
user.** Actor attribution is therefore a requirement *today*, not
future-proofing. Networked collaboration can wait; `origin` cannot.

### 3.2 What the literature says

**[F]** Selective undo in OT requires three "inverse properties" (IP1, IP2,
IP3), and the field's own summary is that *"It is still a great challenge to
design an efficient and correct OT algorithm capable of handling both normal
do operations and user-initiated undo operations because these two kinds of
operations can interfere with each other in various forms. Undoing operations
that are received and executed out-of-order at different sites leads to
divergence cases."* Papers run from 2000 to 2020+ and are still publishing.

Canonical citations, DOIs verified:

- Ellis & Gibbs, *"Concurrency control in groupware systems"*, ACM SIGMOD
  Record 18(2):399–407, 1989, **doi:10.1145/67544.66963** (GROVE — OT origin).
- Berlage, *"A selective undo mechanism for graphical user interfaces based on
  command objects"*, ACM TOCHI 1(3):269–294, 1994,
  **doi:10.1145/196699.196721**.
  ⚠️ **doi:10.1145/174630.174632 is *not* Berlage** — it resolves to Sears &
  Shneiderman's "Split menus". That wrong DOI circulates in secondary sources.
- Prakash & Knister, *"A framework for undoing actions in collaborative
  systems"*, ACM TOCHI 1(4):295–330, 1994, **doi:10.1145/198425.198427**.
- Ressel, Nitsche-Ruhland & Gunzenhäuser, *"An integrating,
  transformation-oriented approach to concurrency control and undo in group
  editors"*, CSCW '96, 288–297, **doi:10.1145/240080.240305** (adOPTed).
- Ressel & Gunzenhäuser, *"Reducing the problems of group undo"*, GROUP '99,
  131–139, **doi:10.1145/320297.320312** — the title alone is a citable
  statement that group undo is a problem.
- Sun, C., *"Undo as concurrent inverse in group editors"*, ACM TOCHI
  9(4):309–361, 2002, **doi:10.1145/586081.586085** — canonical; introduces
  IP1/IP2/IP3.
- Weiss, Urso & Molli, *"Logoot-Undo: Distributed Collaborative Editing System
  on P2P Networks"*, IEEE TPDS 21(8):1162, 2010,
  **doi:10.1109/TPDS.2009.173** — the CRDT-side equivalent; that undo needed
  its own paper on top of Logoot is itself the argument.

**[F]** Yjs ships a selective `UndoManager`:
`constructor(scope, {captureTimeout, trackedOrigins, deleteFilter,
ignoreRemoteAttributeChanges})`. It is *"a selective Undo/Redo manager. The
changes can be optionally scoped to transaction origins"*; by default it
tracks *"all local changes that don't specify a transaction origin"*; it
*"merges edits that are created within a certain `captureTimeout` (defaults to
500ms)"*, settable to 0, with `stopCapturing()` to force a boundary.

**[F]** Automerge **removed built-in undo/redo for 1.0**, having deemed it
unsatisfactory. Notable absence: `automerge.org`'s full sitemap contains **no
undo documentation at all** — the leading general-purpose CRDT library ships
no documented undo story.

**[F]** Figma, which needed multiplayer undo more than almost anyone, did not
build a CRDT undo.

### 3.3 Is a CRDT justified here?

**[J] No.** In order of weight:

1. **AURA has a single authoritative writer.** A CRDT's entire value
   proposition is convergence without coordination. AURA *has* coordination —
   one `Mutex<Store>`, one commit point. Full cost, none of the benefit.
2. **Automerge's maintainers could not make CRDT undo satisfactory** and
   removed it. Strongest available evidence not to attempt it as a side quest.
3. **Tombstone growth is a real cost** in a document holding 10⁶ MIDI notes;
   you'd add a GC problem you don't currently have.
4. **[F]** Figma explicitly relaxed CRDT guarantees once they had a central
   server: *"we removed this extra overhead and benefit from a faster and
   leaner implementation."*

**When to revisit [J]:** if AURA ever grows real-time multi-user co-editing
across machines. Even then, the Figma model (server-authoritative LWW per
property + fractional indexing for order + client-local undo) is a better fit
for a DAW than a general CRDT — a DAW's conflict domain is mostly disjoint
(different tracks, different clips).

**What to do instead [J]:** adopt the *shape* that makes a CRDT unnecessary
and a future one possible — stable UUIDs, ops that carry their target
identity, a monotonic `rev`, and no positional addressing anywhere. AURA's
draft `op-envelope.schema.json` already has `rev`, `baseRev` and `origin`,
which is exactly the right hedge.

### 3.4 The design for the four-actor problem

**[J]**, synthesized from Figma + Yjs + ProseMirror:

**(a) One stack, tagged by actor.** Not four stacks. VS Code's multi-file undo
hole and Ardour's pianoroll history loss are both consequences of splitting
the stack. Every committed transaction carries
`origin: Origin { actor: Actor, session_id }` where
`Actor ∈ {User, Agent(id), Script(id), System}`.

**(b) Default undo is chronological, not selective.** Ctrl+Z undoes the most
recent entry regardless of actor. Justification: the alternative — "undo my
last action, not theirs" — silently *skips* an agent's edit, leaving the user
with a document state that never existed in the history and that neither party
intended. This is the exact failure Figma guards against.

**(c) Make agent work visible and separately reversible, not silently
skipped.** The right UX primitive is not selective undo — it is a **labelled
group** plus a "Revert this agent action" affordance:

- Every MCP tool call opens exactly one transaction with a human label
  (*"Claude: add chorus section (12 clips)"*). AURA's op envelope already has
  `label`.
- The history panel shows actor attribution.
- "Revert" on a specific agent entry is implemented as **derive-inverse-
  against-current-state and commit it as a NEW forward transaction** — the
  Automerge/Figma answer — not as an in-place history edit. If the inverse
  cannot be derived because later ops depend on it, refuse with a clear reason
  rather than corrupt.
- **Dependency check before offering it.** With `W` = the run's write set, and
  later ops with write set `W'` and read set `R'`, the revert is clean iff
  `W ∩ (W' ∪ R') = ∅`. If not clean, never silently partial-revert — show the
  conflicting objects and offer three options, each with a stated consequence.

**(d) Non-undoable transactions are a first-class flag.** ProseMirror's
`addToHistory: false`. Transport position, meter state and (arguably)
recording-in-progress commit through the same channel but carry
`undoable: false`. **Do not** route them outside the channel — then you lose
journaling and remote observability for them.

**(e) A background job that lands mid-gesture must not split the gesture.**
Concretely: the user is dragging a fader (an open gesture) when a generation
job completes. Three rules:

1. The job's transaction commits normally, **after** the currently-open
   transaction, never interleaved. The commit point is serialized.
2. It gets its own undo entry with `Actor::System` — it does **not** merge into
   the user's gesture. This is why the merge key must include actor *and*
   target, not just op kind (§1.1's Zrythm bug).
3. The open gesture's inverse must still be valid. Since the gesture targets a
   specific parameter by UUID and the job touched different objects, it is.
   **This is only true because addresses are stable** — with Zrythm-1-style
   path addresses it would not be.

**(f) `baseRev` is for optimistic UI reconciliation, not conflict
resolution.** **Recommendation: do not build a rebase engine.** Use `baseRev`
for exactly one thing — letting a client detect that its optimistic local state
is stale and needs reconciliation from the committed stream. Rejecting on
mismatch is fine for a single-process app; ProseMirror-style rebasing is a
large, subtle subsystem you should not build until multiplayer actually
exists.

**(g) Ship the tree, do not ship selective undo. [J]** Selective undo requires
a commutation analysis, and for a DAW where commands touch overlapping time
ranges on shared tracks, **most interesting pairs will not commute.** REAPER
proves the branching *tree* is cheap and shippable; selective undo is the trap.

---

## 4. The addressable-object problem

This is where the survey is most unanimous, and it is the decision most
expensive to get wrong.

### 4.1 The five schemes

| Scheme | Stable across insert/delete? | Survives delete+undo? | Persistable? | Cross-process / scriptable? |
|---|---|---|---|---|
| Integer index (`tracks[3]`) | **No** | **No** | no | no |
| Monotonic integer ID | yes | only if never reused *and* the object is kept | yes | within one file |
| **UUID** | yes | yes, **if the registry cooperates** | yes | **yes** |
| Generational index / slotmap | yes for live objects | **No — by design** | no | no |
| Path (`track/3/plugin/2/param/cutoff`) | **No** | **No** | brittle | human-friendly |

### 4.2 Why path addressing fails — with receipts

**[F]** Zrythm 1's `RegionIdentifier` produced #3486 (wrong MIDI note moved
after delete+undo), #3537 (crash in `region_find` undoing a lane move), and a
`find_region` implementation that *validates positions against the clone and
warns on mismatch* because mismatches were routine.

**[F]** Ableton's LOM: `live_set tracks 0 devices` is index-based, and *"When
an object is deleted and a new object is created at its place, it will get a
new ID."*

**[F]** Ardour's `memento_command_factory` resolves playlists **by name**, not
ID — and rename breaks it.

**[J]** Paths are a *presentation* concern. They are excellent for an MCP tool
signature that a language model has to produce (`set_param("Bass", "Serum",
"cutoff", 0.4)`) and unacceptable as storage.

### 4.3 Why generational indices are the wrong primitive here

**[F]** From the slotmap docs: *"each slot in the vector is a (value, version)
tuple, and after insertion the returned key also contains a version — only
when the stored version and version in a key match is a key valid. This allows
reusing space in the vector after deletion without letting removed keys point
to spurious new elements."*

**[J] That is precisely the wrong semantics for undo.** Generational indices
exist to make stale handles *detectably invalid*. Undo needs stale handles to
become *valid again*. `slotmap` has no public API to reinsert at a chosen
(index, generation) pair; resurrection would require a fork or a parallel ID
map.

They are the right primitive for **RT-side, frame-scoped** structures (a mix
graph snapshot, a voice pool) where invalidation is what you want. Use them
below the document layer, never as document identity.

**[F]** This is also exactly Blender's abandoned branch:
`id-ensure-unique-memory-address` — "never reuse an address so pointers can
serve as cross-step UIDs" — was **abandoned** in favour of *"session-wise uuids
for data-blocks."*

### 4.4 The resurrection problem, and the three known solutions

**The requirement, precisely stated [J].** When a delete is undone, the object
must come back with (a) the *same* ID, (b) the *same* identity as far as every
other reference is concerned, and (c) *all* inbound references restored —
including references from objects that were *not* part of the delete.

**Solution A — the registry never releases (Ardour's regions). [F]**
`RegionFactory::region_map` holds `shared_ptr` for every region ever created;
nothing is ever removed. Works, and is why regions are the one Ardour object
type whose deletion is undoable. Cost: unbounded growth over a session.
**[J] Rejected** — a DAW session can create millions of transient objects.

**Solution B — the undo entry owns the deleted object (Zrythm 2, Figma). [F]**
`RemoveArrangerObjectCommand` holds an `ArrangerObjectUuidReference`; the RAII
refcount *"prevents the UUID-identifiable object from being deleted while the
command is on the undo stack."* Figma independently: deleted objects'
properties *"are stored in the undo buffer of the client that performed the
delete."* When the entry falls off the undo horizon, the refcount drops and
the object is finally freed. **[J] This is the correct answer.**

**Solution C — soft-delete tombstones + GC. [J]** Equivalent in effect;
strictly worse in that every read path must filter tombstones and every
invariant must account for them.

**And the reference-integrity half [J]**, which is where people still get it
wrong: solution B restores the *object*, but if some *other* object held a raw
pointer or index to it, that reference is still broken. The fix is structural,
and **[F]** Figma states it explicitly: **store references as IDs, resolved
through the registry at use time.** Then restoring the object into the
registry restores every inbound reference simultaneously, for free, including
references you never enumerated.

**[F]** Blender demonstrates the alternative's cost: because references are raw
`ID*`, it needs `BKE_libblock_remap` walking *every* ID pointer in the database
on every remap (O(total pointers), independent of edit size), plus `_multiple`
and `_raw` variants purely to amortise or strip that constant, plus bespoke
special-casing for interior pointers (*"our beloved Bone pointers from the
object's pose need their usual special treatment"*), plus a documented rule
that Python must assume all handles are invalid after undo.

### 4.5 The hybrid: stable ID + resolvable path

**[J]** The right shape, and it maps cleanly onto AURA's two front doors:

```
MCP / scripting surface:   path-shaped, human/LLM-friendly  ("Bass" / "Serum" / "cutoff")
        ↓ resolve ONCE, at the front door, before the transaction opens
Op-log / undo / storage:   UUID only                        (TrackId, PluginId, ParamId)
```

**[F]** Ableton does this (`live.path` → ID); its bug is that the resulting ID
is not stable across delete+recreate, which solution B fixes.

Two corollaries **[J]**:

- **Never journal a path.** A replayed journal must not re-resolve names, or
  replay is non-deterministic.
- **Return the resolved UUIDs to the agent** in every tool response, so a
  multi-step agent plan can address objects it just created without
  re-resolving by name — which is exactly where name collisions bite.

---

## 5. Transactions and invariants

### 5.1 Validate before applying — the AI-agent question

**[J]** AURA has an adversarial-ish input channel (an LLM emitting tool calls)
into a data structure with real invariants. Four layers, in order:

**Layer 1 — schema/type validation at the front door.** An op that doesn't
deserialize into a typed `Op` never reaches the core. **[F]** AURA already
emits JSON Schemas; `op-envelope.schema.json` deliberately keeps payloads open
(`additionalProperties: true`) so op kinds can be added without breaking
validators — good, but the *Rust* `Op` enum must be closed and total.

**Layer 2 — precondition validation over the whole batch, before any
mutation.** This is what `apply_track_mix` already does. Generalize it: every
op implements `validate(&Session) -> Result<(), OpError>`, and the transaction
runs all validations before any application.

Important subtlety **[J]**: **validation must be sequential-aware.**
`[create_track(X), add_clip_to(X)]` — op 2 fails a naive up-front check because
X doesn't exist yet. Two workable answers:

- *Speculative apply with rollback*: apply to a scratch copy or with a journal
  of inverses, and unwind on failure.
- *Staged validation*: validate op *i* against the state projected after ops
  *0..i-1*.

**[J] Recommendation: speculative apply with inverse-unwind.** You need the
inverses anyway for undo — so `apply` produces `Inverse` as a by-product, and
abort is "run the inverses collected so far, in reverse." **That makes
rollback share code with undo, which means it gets tested by the same property
tests (§6).** Ardour reaches for the same shape with
`abort_reversible_command()` calling `_current_trans->clear()`.

**Layer 3 — invariant assertion after applying, before committing.**
Structural invariants that no single op is responsible for: every clip's
`track_id` resolves; no clip extends past its source length; the tempo map is
monotonic; refcounts are consistent; no track exceeds `MAX_TRACKS`. Run these
in debug always and in release on agent-originated transactions specifically. A
failed invariant means "abort and unwind", not "panic" — and it should be
*loud*, because it indicates a missing precondition in layer 2.

**Layer 4 — policy.** **[F]** AURA's `mcp/policy.rs` gates per *tool name*
(`Allow`/`Deny`/`Confirm`). **[J]** Once ops exist, policy should move to per
*op kind* — otherwise a permissive tool becomes a universal write primitive.
`track.setGain` allow, `project.delete` confirm, `track.delete` confirm.

**[J] One more MCP consequence.** The current gate is `confirmDestructive`
with a 60 s timeout **per call**. A 40-op agent run would prompt 40 times, and
per-call confirmation is exactly what trains people to click through. The
*run* concept fixes it: **confirm the run's scope once** — *"this agent may
edit MIDI on 'Lead' between bars 1–32 for the next 10 minutes"* — a capability
grant rather than a per-call interrupt.

### 5.2 Atomicity of multi-part change

**[J]** The rule, in one line: **`&mut Session` is reachable only from inside a
transaction.** Everything else follows.

```rust
impl Session {
    pub fn transact<R>(&mut self, meta: TxMeta,
                       f: impl FnOnce(&mut Tx) -> Result<R, OpError>)
        -> Result<Committed<R>, OpError>;
}
```

`Tx` holds `&mut Session` privately and exposes only `apply(op)`. On `Err`,
`transact` unwinds the collected inverses and returns without emitting
anything. On `Ok`, it bumps `rev`, appends one entry to the undo stack, appends
one record to the journal, and emits **one** notification.

Why this and not Ardour's `begin`/`commit`:

| Property | How | Failure it avoids |
|---|---|---|
| No mutation outside a transaction | `apply_raw` is private; `&mut Session` never escapes | Ardour's "forgot `add_command`" silent history loss |
| No nesting | closure borrow — the borrow checker rejects it | Ardour's `assert(false)`; Krita's *"transactions can not be nested!"* |
| No empty entries | `applied.is_empty()` check | Ardour `abort_empty_reversible_command`; Zrythm 2's lazy macros |
| Rollback shares code with undo | `unwind()` runs `inverses` in reverse | one tested path instead of two |
| One notification, one Rebuild | folded at commit | AURA's current whole-project `project://changed` |
| Non-undoable but journaled | `meta.undoable` | ProseMirror `addToHistory: false` |

**Gestures span multiple `transact` calls**, so they need a separate primitive
**[J]**, modelled on CLAP's `PARAM_GESTURE_BEGIN`/`_END` rather than on a
timeout:

```rust
let g = session.begin_gesture("Fader: Bass");
// ...many transient updates, RT-visible immediately, NOT journaled, NOT undoable
session.end_gesture(g);   // ONE undo entry: {param, value_at_begin, value_at_end}
```

This is strictly better than Yjs's 500 ms `captureTimeout` or Zrythm's 1000 ms
merge window **because the UI actually knows when the mouse went down and up.**
Keep a timeout-based `mergeWith` only as the fallback for actors that can't
bracket (an agent hammering `set_param` in a loop) — and **key the merge on
`(op_kind, target_id, actor)`**, per the Zrythm bug in §1.1.

### 5.3 Exactly one notification per transaction

**[J]** Three rules, each with a cited failure behind it:

1. **Emit only at commit, never inside `apply`.** **[F]** Ardour has
   `Playlist::freeze()`/`thaw()` purely to batch notifications during
   multi-region edits; with a closure-scoped transaction you get this for free.
2. **The notification is the committed op list plus the new `rev`** — not the
   whole project. **[F]** AURA currently emits `project://changed` with the
   entire serialized `Project`, consumed by
   `project.svelte.ts::applyProject`. At 500 tracks and 10⁶ notes that is
   fatal, and `SCALABILITY.md` §5 already says so. **[F]** Zrythm 2's
   `DeleteTracksCommand` ends with exactly one
   `collection_.notify_tracks_moved(deleted_uuids_)` — one signal, carrying
   the affected set.
3. **Never re-enter.** Notification runs *after* the `&mut Session` borrow
   ends. Observers that want to mutate must submit a *new* transaction, which
   lands after the current one. **This is exactly Blender's Python-app-handler
   hazard made impossible by the type system.**

**Corollary for the RT engine [J].** The transaction should declare its engine
impact, Zrythm-2 style: `Op::engine_effect() -> EngineEffect { None |
ParamOnly | Rebuild }`; the commit path folds over all ops and sends at most
one `ControlMsg::Rebuild`. **[F]** AURA's `control/ops.rs` already documents
the split (*"mix changes are param-table writes (never graph rebuilds);
structural changes are the caller's cue to send `ControlMsg::Rebuild`"*) — the
transaction should own that decision instead of every caller, because
caller-opt-in is exactly the discipline that failed for Ardour.

---

## 6. Testability

**[J]** This is the strongest argument for the whole design, and it is worth
stating to the team in these terms: **the mutation channel is a test harness
that happens to also be the product.**

### 6.1 Replayable op logs as fixtures

Once every mutation is a serializable op batch, a bug report is a file.
`journal.ndjson` *is* the repro:

- `aura replay --journal bug-1234.ndjson --assert-final-hash <h>` reproduces a
  user's session deterministically without their audio.
- Golden-file tests: a fixture journal + a snapshot of the resulting `Session`,
  diffed on every commit.

**[F, cautionary]** Blender's global undo shipped for ~20 years with no unit
tests and no perf regression tests; both landed in 2026, *after* the 5.0
stable-pointer regression reached users. **Ship the harness with the feature.**

### 6.2 The property tests

**[J]** With `proptest`, generating `Vec<Op>` against a live session. Four that
matter, in priority order:

**P1 — Undo/redo round-trip.**

```
∀ ops: apply_all(ops); let h1 = hash(session);
       undo_all();      assert_eq!(hash(session), h0);
       redo_all();      assert_eq!(hash(session), h1);
```

Catches: forgotten inverse fields, non-total inverses, ordering bugs (Zrythm
2's remove-descending/insert-ascending would be caught the moment it
regressed), and resurrection failures.

**P2 — Delete/undo preserves identity and inbound references.**

```
∀ ops, ∀ target: let id = target.id();
       delete(target); undo();
       assert!(session.get(id).is_some());                    // same ID resurrected
       assert!(all_references_to(id).all(|r| r.resolves()));  // and every inbound ref
```

**This is the property Ardour's architecture cannot satisfy and Zrythm 1
failed for a decade. Write it first; it will drive the registry design.**

**P3 — Atomicity.**

```
∀ ops where apply_batch(ops).is_err():
       assert_eq!(hash(session), hash_before);
```

AURA already has the hand-written version
(`batched_mix_is_atomic_on_unknown_track`); property-testing it generalizes to
every op kind, including the "op 3 of 5 fails after ops 1–2 mutated" case that
hand-written tests never cover.

**P4 — Invariants hold after every commit.**

```
∀ ops: apply_all(ops); assert!(session.check_invariants().is_ok());
```

Cheap, and it is the one that makes agent-driven mutation safe to ship.

**P5 — Journal replay determinism.**

```
∀ ops: apply_all(ops); let a = hash(session);
       let b = hash(replay(journal_of(ops), from: empty));
       assert_eq!(a, b);
```

Catches paths leaking into the journal (§4.5), non-deterministic ID generation,
and time-dependent op semantics.

**[F]** `proptest-state-machine` exists for exactly this shape — *"the
framework generating test cases as sequences of operations, and shrinking test
cases by removing operations from the sequence."* Shrinking is the reason to
use property testing rather than a hand-rolled random loop: a 400-op failure
shrinks to the 3 ops that actually matter.

### 6.3 The Figma invariant as an acceptance test

**[J]** Adopt it verbatim: apply K ops, undo all K, perform a *read-only*
operation, redo all K, assert document **and rendered output** unchanged.
Stronger variant: after undoing all K, apply a *new* op, and assert the redo
stack behaves per your documented policy with no dangling references.

The generalisation past multiplayer **[J]**: in a DAW the "other participant"
is the *engine* and the user's *non-undoable* actions — recording wrote a
file, an AI job landed a clip, a plugin's internal state moved on. Undo and
redo must be rebased against what actually happened.

### 6.4 Fuzzing

**[J]** Two distinct targets, both cheap once ops are data:

1. **Op deserializer + validator** (`cargo-fuzz` on
   `&[u8] -> Op -> validate`): corpus is arbitrary bytes; oracle is "never
   panic, never violate an invariant, always either reject or produce a valid
   session." This is the defence against a malformed MCP payload.
2. **Journal reader**: replaying a truncated or corrupted journal after a crash
   must never panic and never produce a corrupt session. Crash recovery is
   exactly where you get half-written files.

### 6.5 The benefit that isn't obvious

**[J] Headless testing of the entire application, including the UI's intent,
without a UI.** Because the GUI's only way to mutate is the same op log the
MCP server uses, an integration test can drive the app exactly as a user
would, assert on committed op batches, and never start a WebView or an audio
device. AURA is unusually well placed for this because both front doors
already funnel through `ControlPlane` — the op log makes the funnel
*observable* as well as shared.

And the reverse: because agent actions are ops, **you can test the MCP surface
by asserting on the op log rather than on final state**, which makes tests
robust to unrelated model changes.

---

## 7. Collaboration — what arrived from the peer session

Included because it bears directly on undo, not because AURA is building
collaboration. **[J] AURA should not build multi-user collaboration now** —
see §7.5 for why.

### 7.1 Ardour persists its undo history as a replayable command log

**[F]** The strongest mainstream-DAW example:

- `libs/pbd/pbd/undo.h` — `UndoTransaction : public PBD::Command` holds
  `std::list<PBD::Command*> actions` + timestamp; `UndoHistory` exposes
  `XMLNode& get_state(int32_t depth)` and `void save_state()`.
- `libs/ardour/session_state.cc` — `Session::save_history()` writes
  `<snapshot_name>.history` XML into the session dir, bounded by
  `Config->get_saved_history_depth()`; `Session::restore_history()` reads it
  back and reconstructs typed commands by tag name (§2.1, failure story 4).
  Undo depth is a user pref (`history-depth`, `Session::set_history_depth`).
- `libs/pbd/pbd/stateful_diff_command.h` — verbatim doc comment: *"A Command
  which stores its action as the differences between the before and after state
  of a Stateful object."* Members `std::weak_ptr<Stateful> _object;` and
  `PBD::PropertyList* _changes;`.

**[J]** That is JUCE's `SetPropertyAction` (target + old + new) generalised to
a property list — and Ardour serialises it. **Durable, typed, named,
timestamped, replayable per-session command log across restarts.**

### 7.2 openDAW — the readable reference implementation

**[F]** André Michelle's (Audiotool founder) AGPLv3 web DAW — a live
implementation of nearly the architecture AURA is aiming at.

From `introduction.md`:

> `lib-box` […] The object graph that backs every project. Boxes are typed
> records with addressable fields, pointer fields form the edges.
> **Transactional editing with undo, an update protocol, and `sync-source` /
> `sync-target` for mirroring a graph across threads or peers.**

> Adapters wrap raw boxes into usable domain objects. A box is a dumb record,
> an adapter gives it behaviour.

> The graph is mirrored in through the `lib-box` sync protocol, so **the audio
> thread never reads main thread state directly.**

**Undo mechanics [F]** (`packages/lib/box/src/editing.ts`, `updates.ts`):

- `Modification` wraps `ReadonlyArray<Update>` with `forward(graph)` /
  `inverse(graph)`.
- Update types: `NewUpdate`, `DeleteUpdate`, `PointerUpdate`,
  `PrimitiveUpdate`; `PrimitiveUpdate` carries **oldValue AND newValue** —
  structurally the same as JUCE's `SetPropertyAction`.
- `optimizeUpdates()` — their coalescing analogue; strips objects created *and*
  deleted inside one transaction.
- Gesture boundaries, verbatim source comment:

  > The only thing ever left unmarked in `#pending` is UI-state (selection,
  > edit pointers) or a gesture still building its step; both belong WITH the
  > edit they precede, not as their own phantom undo entry. Gestures that must
  > stay a distinct step already self-seal with an explicit `mark()` at their
  > boundaries (knob/slider drags, recording).

- **Multiplayer undo:** in `undo()`/`redo()`, if applying an inverse throws
  they abort the graph transaction, re-apply what was already undone, restore
  `historyIndex`, and surface
  `RuntimeNotifier.notify({message: "History changed by another participant."})`.
  Their answer: try the local inverse; if the graph no longer supports it, roll
  back and refuse with a visible notice. Worth contrasting with Figma's
  history-rewriting approach.

**[F]** A production bug worth internalising, from
`errors/P2-undo-rollback-pointerfield-missing.md`:

> `BoxGraph.#rollback` replayed the **raw, un-optimized** transaction updates
> in reverse, with deferred pointer updates appended at the **end** of the list
> — out of chronological order… the reverse replay inverts a pointer update of
> a box that is already unstaged → the exact production panic `Could not find
> PointerField at <uuid>/2`

Their fix collapsed create+delete pairs via `optimizeUpdates` before replaying,
because *"phantom create+delete pairs net to nothing; replaying them raw is
what resurrected edges."*

And the companion constraint, from `sync.ts` **[F]**:

> `primitiveType` carries the field's codec captured at emission time: a task
> stream is forward-only and self-contained, so serialization must never
> re-resolve the field against a live graph that a later task in the same batch
> may have deleted (e.g. **undo trims a region, then unstages it** — #287).

> **[J] The most transferable lesson for a snapshot architecture: an undo delta
> must be self-contained and order-correct at emission time.** Anything
> resolved lazily against a graph the rest of the delta is mutating is a bomb
> that fires only on undo, only sometimes.

**[F]** CRDT layer: `packages/studio/core/src/ysync/` (Reconcile.ts,
YMapper.ts, YService.ts, YSync.ts). `YSync` imports yjs and mirrors the box
graph into a `Y.Map` "boxes"; `YMapper.createBoxMap(box)` → `Y.Map{name,
fields:deepMap}`. **Per-field granularity — the same granularity choice as
Figma, on a real CRDT.** Separately, an explicit commit log lives in
`sync-log/` (Commit.ts, SyncLogReader.ts, SyncLogWriter.ts).

**[F]** openDAW also carries a live-mirror integrity check: the
`Synchronization` interface has `checksum(value: Int8Array): Promise<void>`
beside `sendUpdates`, and `crates/boxgraph/src/checksum.rs` implements a
rolling 32-byte XOR checksum described as *"used to validate the mirror after
a transaction"*. **[J]** In AURA's architecture the "mirror" is the published
snapshot: after every publish (debug/CI always, release sampled), have the RT
side checksum what it is actually rendering from and compare. That turns "undo
desynced the engine" from a mystery into a localised assertion.

### 7.3 Figma's per-property LWW, and Yjs's UndoManager

Covered in §2.5 and §3.2. The point worth repeating here: **Figma's
`Map<ObjectID, Map<Property, Value>>` with per-property last-writer-wins is
Ardour's `StatefulDiffCommand` and JUCE's `SetPropertyAction` data shape,
arrived at independently in a different industry.** A property-diff undo model
is one step from a collaborative model.

**[F]** JUCE ships `ValueTreeSynchroniser` — one-way replication with **no
merge**:

> This class can be used to watch for all changes to the state of a ValueTree,
> and to convert them to a transmittable binary encoding. The purpose of this
> class is to allow two or more ValueTrees to be remotely synchronised by
> transmitting encoded changes over some kind of transport mechanism.

API: virtual `stateChanged(const void* encodedChange, size_t)`,
`sendFullSyncCallback()`, static `applyChange(ValueTree& target, const void*,
size_t, UndoManager*)`. No conflict resolution, no causality, no vector clocks.
**[J] State this plainly: ValueTree gives you replication for free;
collaboration is exactly the part it does NOT give you.** Dave Rowland closed
his ADC'17 deck (slide 58, "Future Thoughts") with *"ValueTrees can be kept in
sync using a ValueTreeSynchroniser / Can keep apps on different devices in
sync… Leads to interesting ideas about remote clients such as cloud…"* — named
as future work in 2017; as far as can be found it was never built into
Tracktion.

**[F]** Automerge's merge rule is worth stating precisely because it is often
misquoted: *"If both `A` and `B` set the key `x` to some value then randomly
choose one value"*, where "randomly" means arbitrary but **identical on all
nodes** — a deterministic arbitrary winner, **not** wall-clock LWW.

### 7.4 The DAW collaboration landscape

**[F]**

| Product | Model |
|---|---|
| **Ableton Live** | **No multi-user collab.** Ableton Cloud is single-account Note/Move→Live sync. Link Audio is LAN audio streaming only. ⚠️ Secondary blogs claiming "Live 12 introduces cloud-based real-time collaboration" have **no** primary source — do not repeat. |
| **Bitwig** | None shipped. Officially *"Collaboration features are planned for future versions… Network support is already built into the core of the software and the Bitwig Studio project file format is designed bearing collaboration features in mind."* They have said so since roughly 1.0 — itself evidence of difficulty. |
| **Pro Tools / Avid Cloud Collaboration** | Explicit per-track upload/download, pull-based, **not realtime**. Audio stored WavPack-compressed. Dropbox-with-track-granularity. |
| **Splice Studio** | Snapshot version control for DAW projects; **shut down June 2023.** CEO: *"this feature hasn't been a focus for us since 2017… we haven't been able to provide the quality of experience of which we can be proud."* |
| **BandLab** | Three models: forking (async, git-like, attribution preserved), invited collaboration with revisions (up to 50 collaborators), and Live Sessions (genuine realtime, web Mix Editor only). Merge model undocumented. |
| **Soundtrap** (Spotify) | Genuine realtime + autosave since Aug 2022. Merge model undocumented. |
| **Audiotool** | Genuine realtime multiplayer; NEXUS SDK with a transactional `await nexus.modify((t) => t.create(...))` API over protobuf. Merge model undocumented. |
| **openDAW** | Genuine realtime, fully open — the readable reference (§7.2). |

**[F]** Bitwig and PreSonus's **DAWproject** is the industry's only attempt to
write down the shared object model. Notable: it includes Arrangement *and*
Scenes, nested clips and warp maps — so those are consensus — and has **no
representation at all for modulation routings, per-voice anything, or modular
patches**; devices serialize as opaque `<State>` blobs.

### 7.5 The verdict

**[J] Do not build multi-user collaboration now.** Bitwig has claimed a
collaboration-ready format since 1.0 and still hasn't shipped it. Splice
Studio — the one mainstream "git for DAW projects" — was free, unmonetized,
and got killed.

**But AURA is already a multi-actor system**, with zero networking, because of
the MCP door. So `origin`/`actor` on every op is not future-proofing we can
defer — it is a requirement we have today. **Networked collaboration can wait;
actor attribution cannot**, because attribution cannot be reconstructed after
the fact.

---

## 8. What this means for AURA

### 8.1 Where AURA already is

Credit where due — the existing design docs anticipate most of this **[F]**:

- `ARCHITECTURE.md` §10 rule 9: *"Do not implement undo/redo ad hoc… the
  prototype ships without undo rather than with a throwaway one."*
- `SCALABILITY.md` §4 already specifies command-pattern-at-the-op-level, ops
  carrying inverses, one batch per gesture, journal-don't-re-save, COW at the
  bulk level, and *"undo/redo replays ops through the normal control-plane
  path… The RT thread has no concept of undo."*
- `op-envelope.schema.json` already has `rev`, `baseRev`, `origin`, `label`,
  `transient`, and namespaced `kind`.
- `control::ops` is already the "single implementation" seam, and
  `apply_track_mix` already validates-then-applies atomically.

**The research says this plan is right.** What follows is the concrete typing,
plus the three things the plan does not yet address.

### 8.2 Three traps in the code today

**Trap 1 — `MidiNote` has no id.**

```rust
pub struct MidiNote {
    pub tick: u32, pub length_ticks: u32,
    pub key: u8, pub velocity: u8, pub channel: u8,
}
```

with `MidiClip.notes` documented as *"sorted by (tick, key)"*. **Notes are
addressed positionally.** This is Zrythm 1's `r->midi_notes[src->pos]`
exactly, and it will produce Zrythm 1's #3486 exactly.

**Recommendation: a `u32` per-clip counter, never reused within the clip.**

| Option | Cost | Verdict |
|---|---|---|
| Full `Uuid` per note | +16 B/note; 10⁶ notes = +16 MB; inflates the AMEV chunk | Overkill |
| **`u32` per-clip counter** | **+4 B/note; address is `(ClipId, NoteId)`** | **Recommended** |
| No ID (status quo) | 0 B | Zrythm 1 #3486, guaranteed |

A note is never referenced from outside its clip, so `(ClipId, NoteId)` is
globally unique; the clip already has a UUID; and 4 bytes fits the fixed
16-byte-record discipline the AMEV format already uses. **The AMEV
`columnMask` mechanism is already the right one** — *"old readers skip unknown
columns, never break"* — so adding a note-id column now costs **one
`columnMask` bit**. Adding it after projects exist costs a migration of every
event chunk ever written. Note that per-voice modulation, per-note expression
and MPE all need this same identity for unrelated reasons.

**Trap 2 — `Store::free_slot` reuses freed RT slots.**

```rust
pub fn free_slot(&mut self, track_id: &str) {
    if let Some(slot) = self.slots.remove(track_id) { self.slot_used[slot] = false; }
}
```

with a test asserting `assert_eq!(c, a, "freed slot is reused")`. Delete track
A (frees slot 3) → add track C (takes slot 3) → undo the delete → A needs slot
3 and cannot have it.

**Recommendation: do not pin the slot. Make slots pure derived state**,
reassigned wholesale on every rebuild with the `ParamTable` repopulated from
the document. That makes slots not-identity, which is what they should be
(§4.3: generational/dense indices belong below the document layer).

**Trap 3 — track and clip ids are untyped `String`.**

Correct in substance, wrong in type: nothing stops a `clip_id` being passed
where a `track_id` is expected. Zrythm 2's per-family strong typedef is the
fix, and in Rust it is nearly free.

### 8.3 The recommended Rust types

```rust
// ---------- identity ----------
macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);
        impl $name { pub fn new() -> Self { Self(Uuid::new_v4()) } }
    };
}
typed_id!(TrackId);  typed_id!(ClipId);   typed_id!(PluginId);
typed_id!(ParamId);  typed_id!(LaneId);   typed_id!(SourceId);

/// Per-clip, never reused within the clip. Address is (ClipId, NoteId).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct NoteId(u32);

/// Every document object lives here. Nothing else owns one.
/// `limbo` is Zrythm 2's `UuidReference` / Figma's undo buffer, in Rust.
pub struct Registry<K: Copy + Eq + Hash, V> {
    live:  HashMap<K, V>,
    limbo: HashMap<K, (V, u32)>,   // non-zero refcount == "resurrectable"
}

impl<K, V> Registry<K, V> {
    pub fn get(&self, k: K) -> Option<&V>;
    pub fn insert(&mut self, k: K, v: V);            // caller supplies the key
    pub fn remove_to_limbo(&mut self, k: K) -> Option<Handle<K, V>>;
    pub fn restore(&mut self, h: &Handle<K, V>);     // exact same key, exact same value
}

/// RAII pin held by an undo entry. Dropping the last handle frees for good.
pub struct Handle<K, V> { key: K, reg: Arc<Mutex<Registry<K, V>>> }

// ---------- ops ----------
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Op {
    #[serde(rename = "track.add")]    TrackAdd    { id: TrackId, name: String, at: usize },
    #[serde(rename = "track.remove")] TrackRemove { id: TrackId },
    #[serde(rename = "track.setMix")] TrackSetMix { id: TrackId, gain_db: Option<f64>,
                                                    pan: Option<f64>, muted: Option<bool> },
    #[serde(rename = "clip.move")]    ClipMove    { id: ClipId, to_track: TrackId, start: Ticks },
    #[serde(rename = "note.add")]     NoteAdd     { clip: ClipId, id: NoteId, note: MidiNote },
    #[serde(rename = "note.remove")]  NoteRemove  { clip: ClipId, id: NoteId },
    #[serde(rename = "param.set")]    ParamSet    { plugin: PluginId, param: ParamId, value: f64 },
}

/// What must happen to undo one applied Op. Produced BY apply, never guessed.
pub enum Inverse {
    Op(Op),
    /// For ops whose true inverse needs owned state (resurrection).
    RestoreTrack { handle: Handle<TrackId, Track>, at: usize },
    RestoreClip  { handle: Handle<ClipId,  Clip>,  at: Ticks },
    /// Escape hatch: a bounded snapshot for ops not worth inverting analytically.
    Snapshot(Box<SubtreeSnapshot>),
}

pub enum EngineEffect { None, ParamOnly, Rebuild }

impl Op {
    pub fn validate(&self, s: &Session) -> Result<(), OpError>;
    pub fn engine_effect(&self) -> EngineEffect;
    /// Merge key. MUST include the target, or you get Zrythm's cross-param merge bug.
    pub fn coalesce_key(&self) -> Option<(&'static str, u128)>;
}

// ---------- transactions ----------
pub enum Actor {
    Human  { session: SessionId },
    Agent  { agent: String, run: RunId },
    System { reason: &'static str },   // recording landed, job completed, migration
}

pub struct TxMeta {
    pub label:    String,   // "Move 12 clips" — shown in history
    pub actor:    Actor,    // NON-optional, stamped at the ControlPlane seam
    pub undoable: bool,     // ProseMirror's addToHistory
}
```

**Non-negotiables, each traceable to a cited failure:**

- `insert` takes the key. A registry that only mints keys (`slotmap`) cannot
  resurrect (§4.3).
- Keys are **never reused**, ever (Blender #149899).
- Every inter-object reference is a typed ID, **never** an index, name, or
  pointer (Figma; Blender's `BKE_libblock_remap` cost; Zrythm 1 #3486).
- Clone-with-new-identity vs clone-preserving-identity is an explicit
  parameter, not a mode flag (Zrythm 2's `ObjectCloneType`; Ardour's
  thread-local flag as the counter-example).
- The merge key includes the target and the actor (Zrythm 2's
  `id() == 89453187` bug).

### 8.4 The three decisions that must not be gotten wrong

> **1. Identity: typed UUIDs, a central registry, refcounted handles, IDs never
> reused.**
>
> Get it wrong and you cannot undo deletion. Ardour cannot, and says so.
> Zrythm 1 spent a decade shipping "wrong MIDI note moved" and then rewrote
> from scratch onto a UUID registry. Blender tried "never reuse a memory
> address", abandoned it, and retrofitted `session_uid`.
>
> **This is the least reversible decision in the document** — identity is
> embedded in the file format, the IPC schemas, the MCP tool signatures, and
> every journal ever written.

> **2. The transaction is the only path to `&mut`, and it is a closure, not a
> begin/commit pair.**
>
> Ardour's opt-in protocol produced commit `b501eaf4` (per-iteration
> `clear_changes()` erasing earlier records) and commit `4b28e4ee`
> (order-dependent double-free), and its `begin_reversible_command` now
> `assert(false)`s on nesting. Krita independently landed on *"transactions can
> not be nested!"*
>
> And the deeper reason to enforce it in the type system: **Blender's entire
> architectural choice was forced by not being able to.** AURA can have a
> closed mutation surface, and Rust can enforce it. That is the whole reason
> this design is available to us and not to them.

> **3. Bulk data never enters the undo stack — content-addressed pool,
> refcounts, GC past the horizon.**
>
> Blender's memfile undo is O(whole database) per push, open as a design task
> since 2019, with users reporting 45-second undos and at least one who left
> for Maya. Zrythm 1 shipped time-stretch undo that *didn't revert the audio*
> and leaked plugin state files on eviction.
>
> Every system that got this right did the same thing: immutable,
> content-addressed blobs; refcounts held by both document and history;
> deferred quarantine-not-delete GC.

**Two more that are cheap now and expensive later [J]:**

**4. One undo stack per project, not per view or per actor.** Ardour's per-view
`HistoryOwner` costs users their pianoroll history on every retarget; VS Code's
per-resource stacks make multi-file refactor undo broken to this day.
Attribution (`actor`) and grouping (`label`) give you everything actor-scoped
stacks promise, without the divergence.

**5. Ship the property tests with the feature, not after.** Blender's global
undo went ~20 years without unit or perf regression tests; both landed in 2026,
*after* the regression reached users. P1 and P2 are ~40 lines of `proptest`
each and will find the resurrection bugs before users do.

---

## Sources

**Ardour** — [`libs/pbd/pbd/history_owner.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/history_owner.h) ·
[`libs/pbd/history_owner.cc`](https://github.com/Ardour/ardour/blob/master/libs/pbd/history_owner.cc) ·
[`libs/pbd/undo.cc`](https://github.com/Ardour/ardour/blob/master/libs/pbd/undo.cc) ·
[`libs/pbd/pbd/command.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/command.h) ·
[`libs/pbd/stateful_diff_command.cc`](https://github.com/Ardour/ardour/blob/master/libs/pbd/stateful_diff_command.cc) ·
[`libs/pbd/pbd/properties.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/properties.h) ·
[`libs/pbd/pbd/property_basics.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/property_basics.h) ·
[`libs/pbd/pbd/rcu.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/rcu.h) ·
[`libs/pbd/pbd/sequence_property.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/sequence_property.h) ·
[`libs/ardour/session_command.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/session_command.cc) ·
[`libs/ardour/session_state.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/session_state.cc) ·
[`libs/ardour/session.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/session.cc) ·
[`libs/ardour/route.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/route.cc) ·
[`libs/ardour/region_factory.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/region_factory.cc) ·
[`libs/ardour/ardour/rt_safe_delete.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/rt_safe_delete.h) ·
commits `d30c8a12`, `b501eaf4`, `4b28e4ee` ·
[Why is track deletion not part of undo history](https://discourse.ardour.org/t/why-is-track-deletion-not-part-of-undo-history/88782) ·
[Separate undo/redo history in the pianoroll](https://discourse.ardour.org/t/separate-undo-redo-history-in-the-pianoroll/113383) ·
[Manual: Preferences](https://manual.ardour.org/preferences-and-session-properties/preferences-dialog/) ·
[Manual: Cleaning up sessions](https://manual.ardour.org/working-with-sessions/cleaning_up/)

**Zrythm 1** — `inc/actions/{undoable_action,undo_manager,undo_stack,arranger_selections}.h`,
`src/actions/*.c`, `inc/dsp/region_identifier.h`, `src/dsp/region.c`,
`src/gui/backend/arranger_object.c`. Issues
[#3486](https://gitlab.zrythm.org/zrythm/zrythm/-/work_items/3486),
[#3537](https://gitlab.zrythm.org/zrythm/zrythm/-/work_items/3537),
[#4164](https://gitlab.zrythm.org/zrythm/zrythm/-/work_items/4164),
[#4955](https://gitlab.zrythm.org/zrythm/zrythm/-/work_items/4955)

**Zrythm 2** — `src/utils/{uuid_identifiable_object,iobject_registry,uuid_reference,typed_uuid_reference}.h` ·
`src/undo/undo_stack.{h,cpp}` ·
`src/commands/{delete_tracks_command,remove_arranger_object_command,change_parameter_value_command,change_uuid_identifiable_object_property_command}.h` ·
`src/dsp/{engine,graph_dispatcher,graph_scheduler}.cpp` ·
`src/gui/backend/legacy_actions/{undoable_action,undo_manager,mixer_selections_action}.cpp` ·
`src/dsp/audio_pool.h`

**Blender** — [#60695](https://projects.blender.org/blender/blender/issues/60695) ·
[#56163](https://projects.blender.org/blender/blender/issues/56163) ·
[#75013](https://projects.blender.org/blender/blender/issues/75013) ·
[#149899](https://projects.blender.org/blender/blender/issues/149899) ·
[PR #106903 implicit sharing](https://projects.blender.org/blender/blender/pulls/106903) ·
[PR !155587 pointer stability](https://projects.blender.org/blender/blender/pulls/155587) ·
[PR !161566 mixing memfile with other undo](https://projects.blender.org/blender/blender/pulls/161566) ·
[devtalk: Undo performance future](https://devtalk.blender.org/t/undo-performance-future/2384) ·
[devtalk: Undo Performance Feedback](https://devtalk.blender.org/t/undo-performance-feedback/2554) ·
[Undo architecture docs](https://developer.blender.org/docs/features/core/undo/) ·
[bpy gotchas](https://docs.blender.org/api/4.0/info_gotcha.html) ·
source: `BKE_undo_system.hh`, `intern/undo_system.cc`, `BLO_undofile.hh`,
`intern/{undofile,writefile,readfile}.cc`, `editors/undo/memfile_undo.cc`,
`blenlib/intern/array_store.cc`, `makesdna/DNA_ID.h`, `BKE_lib_remap.hh`

**Others** — [Figma: How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/) ·
[ProseMirror Guide](https://prosemirror.net/docs/guide/) ·
[Krita Transactions Design](https://community.kde.org/Krita/Transactions_Design) ·
[Krita Tile Data Format](https://community.kde.org/Krita/Tile_Data_Format) ·
[Y.UndoManager](https://docs.yjs.dev/api/undo-manager) ·
[Automerge undo/revert #985](https://github.com/automerge/automerge/issues/985) ·
[Automerge merge rules](https://automerge.org/docs/reference/under-the-hood/merge-rules/) ·
[VS Code multi-file undo #638](https://github.com/microsoft/vscode/issues/638) ·
[Ableton LOM](https://docs.cycling74.com/max8/vignettes/live_object_model) ·
[CLAP params.h](https://github.com/free-audio/clap/blob/main/include/clap/ext/params.h) ·
[CLAP draft undo.h](https://github.com/free-audio/clap/blob/main/include/clap/ext/draft/undo.h) ·
[REAPER User Guide](https://www.reaper.fm/userguide.php) ·
[openDAW](https://github.com/andremichelle/openDAW) ·
[Bitwig on collaboration](https://www.bitwig.com/support/technical_support/when-will-the-collaboration-features-become-available-16/) ·
[Splice Studio shutdown](https://splice.com/blog/studio-shutdown/) ·
[DAWproject](https://github.com/bitwig/dawproject) ·
[slotmap docs](https://docs.rs/slotmap/) ·
[proptest-state-machine](https://docs.rs/proptest-state-machine/) ·
[Undo as Concurrent Inverse in Group Editors](https://dl.acm.org/doi/10.1145/586081.586085)

**AURA** — `src-tauri/src/audio/types.rs` · `src-tauri/src/control/{ops.rs,mod.rs}` ·
`src-tauri/src/audio/{project.rs,engine.rs,rt.rs}` · `src-tauri/src/midi/{types.rs,events.rs}` ·
`src-tauri/src/mcp/{policy.rs,tools.rs}` · `src/lib/state/project.svelte.ts` ·
`docs/ARCHITECTURE.md` §10 rule 9, §11 · `docs/SCALABILITY.md` §3–§5 ·
`docs/ipc-schemas/op-envelope.schema.json`

**Not verified — do not cite as fact:** REAPER's undo internals beyond the user
guide; Pro Tools / Logic / Cubase / Studio One / Bitwig undo internals;
Ardour's Mantis tracker (proof-of-work wall); Tower's undo implementation;
Audiotool/Soundtrap/BandLab merge algorithms.
