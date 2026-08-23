# The side-channel inventory — LANDED (Gate E)

**Status: TOTAL.** Every mutation path that reaches the AURA document now
goes through the one transaction channel (`Session::transact` →
`Committer::commit_with_rebuild`), except the sanctioned epoch functions and
the two carve-outs recorded below (R-3 CLOSED in Plan F Task 10). Every
declared read path is pure.

This document is the landed form of the 34-row inventory in
`docs/superpowers/plans/2026-08-14-plan-e-side-channel-totality.md`
("The re-derived inventory"), which was itself re-derived from dossier 10
§2.3 plus the round-2 review's additions. It is what Gate E's "checked
against dossier 10 §2.3 + review additions" clause audits.

Verified at commit `73bb9a7` (Plan E Task 17, 2026-08-14). All `file:line`
anchors below were grepped against that tree; the anchors did not move when
the op log landed in `f984ae2` (the log is additive to every path here) —
`src-tauri/src/control/mod.rs` line numbers shifted by the insertions,
so treat its anchors as "the named function", not a byte offset.

The machine-checked half of this claim is
`src-tauri/tests/figma_invariant.rs` (§7 test 4): a scripted mixed session
over every family in this table, undone through new commits, swept by every
declared read path, redone, and asserted byte-identical.

---

## The table

"Mechanism" is where the path ended up; "Anchor" is the landed
wrapper/op/epoch function.

| # | Path (as of the plan's HEAD anchor) | Mechanism that closed it | Anchor |
|---|---|---|---|
| 1 | `add_track`, `remove_track`, `set_track_*`/`set_track_mix` | already the channel (Plan A/B): `Op::TrackAdd`/`TrackRemove`/`Set` | `src-tauri/src/control/mod.rs:1258` (`add_track`), `:1288` (`remove_track`), `:1318` (`set_track_mix`) |
| 2 | `set_track_instrument` — direct write + Rebuild, no event | `Op::Set{Track, InstrumentId}` through `commit` | `src-tauri/src/control/mod.rs:1408`; command wrapper `src-tauri/src/audio/mod.rs:550` |
| 3 | audio clip move (frontend-only; "worst offender": lost on save) | `Op::Set{Clip, TimelineStartSamples}` + the additive `move_clip` command | `src-tauri/src/control/mod.rs:1387` (method), `:2449` (command), `src/lib/tauri.ts:426` (binding) |
| 4 | `set_tempo_map` — bypass, two non-atomic phases incl. `transport.tempo_bpm` | `Op::TempoSet` (ONE atomic apply incl. the bpm mirror) | `src-tauri/src/midi/mod.rs:306` (`set_tempo_map_core`); op `src-tauri/src/control/op.rs:54` |
| 5 | `midi_add_clip` — bypass | `Op::MidiClipAdd` | `src-tauri/src/midi/mod.rs:339` (`midi_add_clip_core`) |
| 6 | `midi_set_notes` — bypass, wholesale replace | `Op::MidiSetNotes` (§4.4 value-replacement wrapper, coalescable) | `src-tauri/src/midi/mod.rs:406` (`midi_set_notes_core`) |
| 7 | `midi_set_clip_bounds` (new since dossier 10 — PR #8) | `Op::Set{MidiClip, TimelineStartTicks/LengthTicks/ContentLengthTicks}`, all three in one commit | `src-tauri/src/midi/mod.rs:494` (`midi_set_clip_bounds_core`) |
| 8 | `midi_import_file` — 3 lock acquisitions, `ops::add_track` under the lock | prepare-outside parse → ONE composite transaction (`add_track_tx` + optional `TempoSet` + `MidiClipAdd` per clip) | `src-tauri/src/midi/mod.rs:572` (`midi_import_file_core`) |
| 9 | mutating readers `midi_get_clips`/`midi_export_file` (lazy resync could re-instantiate live plugins) | pure reads; adopt happens EAGERLY at epochs only | `src-tauri/src/midi/mod.rs:244` (`read_midi`), `:559`, `:667`; eager adopt at `src-tauri/src/control/mod.rs:2353` (create) / `:2420` (open) |
| 10 | mutating reader `automation_get` | pure read (session-lock clone, no sync/no disk) | `src-tauri/src/plugins/automation.rs:805`; body `src-tauri/src/control/mod.rs:1299` (`automation_lanes`) |
| 11 | `automation_set` — own store, own persistence | `AutomationStore` moved INTO `Session` + `Op::AutomationSetLane`; persistence is a `PersistEffect` | `src-tauri/src/plugins/automation.rs:818`; op arm `src-tauri/src/control/session.rs:763` |
| 12 | `plugin_instantiate`/`plugin_remove` — bypass + auto-persist | registry doc-half into `Session`; `Op::PluginAdd`/`Op::PluginRemove` (restore-from-blob inverse) | `src-tauri/src/plugins/mod.rs:276`, `:302`; op arms `src-tauri/src/control/session.rs:799`, `:832` |
| 13 | `plugin_set_param` — project.json rewritten per knob gesture | `Op::Set{Plugin, Param}` + host-forward effect + `PersistEffect`, folded into `gesture_begin`/`gesture_end` with the persist DEFERRED to gesture close (Track D — the review's I-8: before that, the rewrite had only moved off the lock, one full `project.json` read-modify-write still ran per rAF batch) | `src-tauri/src/plugins/mod.rs:353`; op arm `src-tauri/src/control/session.rs:922` |
| 14 | `zyn_load_patch` — unmanaged global, patch lost on save | `Op::PluginSetState` (blob inverse) | `src-tauri/src/plugins/patches.rs:322`; op arm `src-tauri/src/control/session.rs:892` |
| 15 | plugin auto-persist `state::persist_after_mutation` (unserialized project.json RMW) | DELETED; replaced by `PersistEffect` executed post-lock | `src-tauri/src/control/mod.rs:538` (`Committer::execute_persist`); the retired call site is documented at `:603` |
| 16 | sink `wrap_sink_with_import`/`do_import` — `add_track` + `clips.push` under two locks | prepare-outside + ONE `Actor::System` tx (`add_track_tx` + `Op::ClipAdd`) | `src-tauri/src/control/import.rs:360` (`do_import`), `:439` (sink) |
| 17 | sink `wrap_sink_with_stem_import` — track via channel, clip not | fully channel: per-stem tx (`Op::ClipAdd`) | `src-tauri/src/control/import.rs:523` |
| 18 | sinks `wrap_sink_with_hum_apply`/`wrap_sink_with_accompany_apply` → `apply_hum_clip` (midi disk write under the lock) | prepare-outside + `Actor::System` tx (`Op::MidiClipAdd`), persist as an effect | `src-tauri/src/control/hum.rs:402` (`apply_hum_clip`), `:435`, `:527` (sinks) |
| 19 | sink `wrap_sink_with_instrument_register` — touches only the global `SamplerBank` | **CARVE-OUT 5**: live resource, not document — out of op scope, recorded | `src-tauri/src/control/mod.rs:2383` |
| 20 | LoopJam watcher `apply` — `replace_region_clips` + push, no undo | `Actor::System` tx: `ClipRemove`/`Set{Clip,…}`/`ClipAdd` batch, one commit | `src-tauri/src/control/loopjam.rs:542` |
| 21 | engine thread site 1: `open_output` sample-rate writeback | `Actor::Engine` TRANSIENT tx via the shared `Committer` | `src-tauri/src/audio/engine.rs:740` (in `open_output`, `:645`) |
| 22 | engine thread site 2: auto-stop | `Actor::Engine` transient transport tx | `src-tauri/src/audio/engine.rs:1130` (in `apply_end_policy`, `:1076`) |
| 23 | engine thread sites 3+4: recording start/finalize (`clips.extend` was never an op) | `Actor::Engine` txs; finalize = `Op::ClipAdd` ops, created AFTER the I/O completes | `src-tauri/src/audio/engine.rs:1328` (start), `:1434`/`:1459` (finalize: state mirror split from clip registration) |
| 24 | engine thread site 5: `ensure_project` — direct + emit | **epoch function** (carve-out 4); the engine now calls the ONE sanctioned epoch fn through an installed closure | `src-tauri/src/audio/engine.rs:1485`; closure installed at `src-tauri/src/lib.rs:95`; swap site `src-tauri/src/audio/project.rs:118` |
| 25 | `open_project` — wholesale swap | **epoch boundary** (carve-out 4): sanctioned epoch fn, single-lock swap, eager adopt of midi/plugins/automation | `src-tauri/src/control/mod.rs:1759` (`open_project_epoch`), marker at `:1787` |
| 26 | `save_project`/`save_project_as`/`create_project` (save-as wrote midi under the lock) | **snapshot mark / epoch boundaries** (carve-out 4), all I/O outside the lock | `src-tauri/src/control/mod.rs:1684` (`create_project_at`, marker `:1718`), `:1834` (`save_project_as_epoch`, marker `:1848`), `:1884` (`save_project_mark`, marker `:1893` — deliberately NOT an epoch bump) |
| 27 | `seed_demo_project` — 3 direct `add_track`s + midi pushes + save under the lock; Zyn plugin rows (R-3, CLOSED Plan F Task 10) | routed through ONE channel tx (tx-tier `add_track_tx` + `Set{InstrumentId}` + `MidiClipAdd` + `Op::PluginAdd`/`Op::PluginSetState`) | `src-tauri/src/control/mod.rs:1974` |
| 28 | transport family (`transport_play/stop/set_loop/set_stop_at_end`) — direct store writes | transient transport ops through `commit_with` (§4.2's "auto-stop produces a transport op") | `src-tauri/src/control/mod.rs:1042` (`ControlPlane::transport`) |
| 29 | device selection — `EngineHandle` direct | **CARVE-OUT (app config, not document)**: behind a `ControlPlane` method for attribution, no op | `src-tauri/src/control/mod.rs:1229`, `:1240` |
| 30 | frontend stems demo materialization + track reorder (webview-only clips) | DELETED; the frontend calls the real backend split-stems import | `src/lib/state/split-stems.test.ts`, `src/lib/state/jobs.svelte.ts` |
| 31 | mix-fader drags: one `TxMeta::user` tx per `input` event, no boundaries | `gesture_begin`/`gesture_end` IPC + transient coalescing; ONE history-bound batch per drag, recorded into `History` by `close_gesture` DIRECTLY (no drop-window) | `src-tauri/src/control/mod.rs:1519`, `:1531`, `:1563` (`close_gesture`), `:804` (`commit_transient_and_fold`) |
| 32 | `fold_ops` merged `Set`s across intervening structural ops | structural barrier in the fold | `src-tauri/src/control/session.rs:1235` (see the `barrier` at `:1248`/`:1270`) |
| 33 | piano-roll note-id churn (frontend `MidiNote` had no `noteId`) | `noteId` in the TS type + explicit mint sentinel (0) | `src/lib/types/ipc.ts:301`; keep-rule `src-tauri/src/midi/mod.rs:454` |
| 34 | `steady_time` per-node counter reset on rebind | engine-global steady clock (`SharedRt::steady`, advanced once per block prologue) | `src-tauri/src/audio/engine.rs:294`; regression test `:1685` |

---

## Carve-outs and binding rulings (restated)

These are decisions, marked per ADR 0007, not omissions.

**Ruling 1 — op kind naming keeps the landed style** (`"set"`, `"trackAdd"`,
`"tempoSet"`, `"automationSetLane"`, `"pluginAdd"`, …). D-03's draft
envelope schema prescribed dotted kinds (`clip.move`); no journal had ever
been written, so the format is genuinely born at Task 17's first line.
`docs/ipc-schemas/op-envelope.schema.json`'s `kind` pattern is corrected to
`^[a-z][a-zA-Z0-9]*$` as a MARKED correction, with a `$comment` saying so.
Round-2's "`clip.move` op" therefore landed as
`Op::Set{Clip, TimelineStartSamples}` plus the additive `move_clip` command
— the capability round-2 demanded, under the house naming.

**Ruling 2 — transport ops are transient: through the channel, never
journaled, never undoable.** `transport.state` / the loop window /
`stop_at_end` writes are `Op::Set` on `ObjectRef::Transport` paths, so they
are attributed, atomic and single-writer — but they carry
`TxMeta.transient = true`: no history entry, no journal line (no DAW undoes
play/stop). `transport_seek` stays a pure RT atomic (position is ENGINE
state, not document state), and the `sample_rate` writeback is likewise
transient (a device property mirrored into the document so it survives a
save).

*Corollary the Figma test pins*: `ops::transport_snapshot`
(`src-tauri/src/control/ops.rs:139`) composes the store's transport mirror
with LIVE RT atomics, and three of those — `position`, `sample_rate`,
`song_end` — are written by the engine on its own turn
(`src-tauri/src/audio/engine.rs:330`, `:705`, `:864`) with no document write
involved. They are engine state visible through a document-shaped read, and
a document oracle must exclude them.

**Ruling 3 — the undo-round-trip oracle masks the note-id watermark.**
ADR 0001 makes `next_note_id` monotonic and never-rewinding; an undo of
`MidiSetNotes` restores the notes but NOT the watermark (ids are never
reused). Both round-trip oracles (`tests/channel_properties.rs`,
`tests/figma_invariant.rs`) therefore normalize `next_note_id` to 0 in both
snapshots before comparing, and separately assert it never decreases. This
is the test honoring the ADR, not the test being weakened.

**Ruling 4 — epoch boundaries are not ops.** `open_project` is a log epoch
boundary (document swap, history root); `save_project`/`save_project_as` are
snapshot marks; `create_project` and the engine's `ensure_project` are epoch
boundaries too (document birth). They are *sanctioned* epoch functions:
single-lock swap discipline, eager adopt of midi/plugins/automation, history
and redo stack cleared, journal rotated — but they never appear as ops in
the log. `save_project_mark` deliberately does NOT bump `Session::epoch`
(same document, same content) and therefore does not clear history; it
writes a `"save"` mark record to the journal instead.

**Ruling 5 — `SamplerBank` stays outside `Session`.** At HEAD the bank has
no document half: loaded instruments are never persisted
(`load_into_registered_bank` writes only the process-global bank; nothing
lands in project.json). It is exactly the "live plugin host handles / loaded
voice data" class §4.1 keeps outside the document.

**Carve-out — device selection is attributed config, not document.**
`select_input_device`/`select_output_device` go through a `ControlPlane`
method so the choice is attributed and single-entry, but produce no op: the
selected device is app configuration, not part of the project.

**Carve-out — `midi_export_file` is a read plus an export ARTIFACT.** It
reads the document and writes a file OUTSIDE the project document. Writing
an artifact is not a document mutation; the read half is pure (row 9).

---

## The log itself (Task 17)

The log the whole plan exists for, for completeness of this document:

* `src-tauri/src/control/history.rs` — `History` (bounded undo/redo stacks,
  the 350 ms boundary-less same-key merge, cleared at epochs) and
  `JournalWriter` (append-only `<project dir>/journal.ndjson`, line-buffered,
  no fsync), behind one `HistoryLog` shared by BOTH `Committer` instances.
* The single write point: `Committer::commit_with_rebuild`, after the
  effects, `if !committed.meta.transient`. That one condition IS ruling 2's
  enforcement. A batch that folded to ZERO ops is dropped inside
  `HistoryLog::record_commit` itself (fix round 1): `fold_ops` elides a
  net-no-op `Set` group, so an empty `Committed` is an ordinary outcome
  (`move_clip` to the sample a clip already sits on, a gain write of the
  current value) — it must produce neither a phantom undo step nor an
  `"ops":[]` journal line.
* REDO SOUNDNESS RESTS ON A RULE ABOUT `transient` (fix round 1, recorded
  on `HistoryMode`): transient writes must never touch document fields an
  entry's `ops` can address, or a pending redo silently lands on a
  different state. Today's transient writers stay clear by construction
  (transport `Set`s address `ObjectRef::Transport` only; mid-gesture folds
  are superseded by the gesture batch that closes over them). **This is now
  CHECKED** (Plan E follow-up, M-3): `debug_assert_transient_invariant`
  runs in the commit path and fails a debug build on any transient batch
  whose ops address something other than `ObjectRef::Transport`, unless it
  is a mid-gesture fold (marked by a debug-only thread-local set across
  `commit_transient_and_fold`). Still a rule about what may be MARKED
  transient — but a rule with teeth in debug builds instead of a comment.
* `undo`/`redo` (`ControlPlane::undo`/`redo`, Tauri commands `undo`/`redo`,
  Ctrl+Z / Ctrl+Shift+Z in `src/App.svelte` guarded against text entry)
  commit through the NORMAL path with `HistoryMode::Replay`: journaled
  (they are mutations), no new entry — the original entry migrates between
  the stacks.
* `OP_FORMAT_VERSION` is LOAD-BEARING from the first journal line. Additive
  `#[serde(default)]` fields stay non-breaking and need no bump; anything
  else needs a bump plus a reader that understands both shapes. It is at
  **2** since the Plan E follow-up (I-5): plugin state blobs
  (`PluginRemove.state`, `PluginSetState.state`) are base64 strings on the
  wire, not JSON number arrays — the old encoding cost roughly 4x on blobs
  that are routinely hundreds of kilobytes, written synchronously into the
  journal and held in every `HistoryEntry`. That bump deliberately shipped
  WITHOUT a dual-shape reader: the journal is still write-only, so every
  version-1 line is data no code will ever parse. The same bump covers
  L-1. This freedom ends the moment Plan F ships a replayer.
* EPOCH GUARD AT BOTH SINKS (Plan E follow-up, C-1). `HistoryLog` owns the
  epoch its two streams describe: advanced by `epoch_boundary`, left alone
  by `snapshot_mark`, and CHECKED by
  `record_commit`/`record_gesture`/`snapshot_mark`, which drop the record
  with a warn when it has moved. Without it, an epoch function landing in
  the effect window — which contains blocking plugin round-trips and disk
  I/O — produced a journal line in the new project's file and a poppable
  undo entry for a document that was no longer open. Same shape and same
  justification as `execute_persist`'s long-standing guard; the epoch is a
  mutex held across check AND write, because an atomic read would leave
  exactly the window being closed.

  ONE NARROW INVERSE WINDOW, RECORDED AS BENIGN: an epoch function bumps
  `session.epoch` INSIDE its swap block but calls `epoch_boundary` only
  after releasing the session lock (journal I/O may not run under it). A
  commit whose `transact` lands in that gap captures the NEW epoch while
  `HistoryLog` still holds the OLD one, so its batch is dropped rather
  than recorded. That is the guard erring toward silence instead of
  corruption — the commit wrote into a document the swap is in the middle
  of replacing, and its `execute_persist` is dropped by the same reasoning
  — so the log stays consistent with what survives. Recorded because it is
  a real gap and a future reader of the guard will otherwise wonder: it
  costs a dropped record, never a wrong one.

## Recorded replay limitations (known, not fixed here)

Recorded so a future replay/collaboration round finds them stated rather
than rediscovers them.

**L-1 — CLOSED (Plan E follow-up).** `Op::PluginRemove.params` carries the
row's param mirror so a journaled op is self-describing, but until the
follow-up nothing read it: the undo path restored the mirror from the
PARKED in-memory copy in `session.plugins.params`, which a COLD REPLAY
(fresh process, journal only) does not have, so a replayed undo-of-remove
restored the row without its params. `apply_raw`'s `PluginRemove` arm now
SEEDS the mirror from the op's own field when the in-memory one is absent
or empty. In-process behaviour is unchanged — a populated mirror still
wins, so an undo restores the user's real values, not whatever the op
recorded. Landed under the same `OP_FORMAT_VERSION` bump as the base64
blob change, while the format was one day old and unread.

**L-2 — `Op::MidiSetNotes` note ids are absolute in value but its
`noteId: 0` entries are MINT SENTINELS.** A replay re-mints. Within a
complete history this is invisible whenever the redo of `Op::MidiClipAdd`
restores the clip row (watermark included) before the notes op replays —
which is what `figma_invariant.rs` exercises. A redo that does NOT
re-create the clip re-mints from the CURRENT watermark, so re-added notes
get fresh ids. That is ADR 0001 behaving as designed (ids are never reused
and never resurrect a deleted note's identity), the same asymmetry ruling 3
masks for the watermark itself.

(Plan F, 2026-08-14, ruling F-9): tail replay is still benign. Minting is
deterministic from the clip's persisted `next_note_id` watermark, and that
watermark is part of the on-disk snapshot the tail replays from. Replaying
the same op sequence from the same base reproduces the same minted ids.
Pinned by `tests/journal_replay.rs::
a_cold_tail_replay_reproduces_the_crashed_sessions_document_byte_identically`
— byte-identical documents, no id normalization. If a future op breaks
this, that test is the signal that L-2 stopped being benign.

**L-3 — replay is faithful only against a COMPLETE log.** An unrecorded
mutation that attaches content to an object an earlier op created is lost
on redo: `Op::TrackAdd` replays with its ORIGINAL (empty) `clips`, so a
clip added later by an unlogged path never comes back. This is the reason
`History::record` takes EVERY non-transient commit, with no exceptions
beyond `TxMeta.transient`.

**L-4 — CLOSED (Plan F, 2026-08-14, ruling F-4; Tasks 9 + 11).** Journal
FILE order is still not `rev` order under concurrency — that is now a
documented format rule, not a bug: "line order is not rev order; consumers
must sort." The reader (`control/replay.rs`) sorts by `(epoch, rev)`. The
undo stack is rev-ordered (`HistoryEntry.rev`, `History::record` inserts
in rev order, `VecDeque` eviction at the low end). A commit-sequence lock
and out-of-order buffering in `record_commit` were both rejected (they
couple command latency to plugin I/O, and transient commits make rev gaps
ordinary). File order remains unordered BY DOCUMENTED RULE.

**L-5 — CLOSED (Plan F, 2026-08-14, ruling F-3; Task 8).**
`Session::transact` wraps the closure in `catch_unwind`. On panic it
restores the live document from the pre-transaction published snapshot and
returns `Err("transaction panicked: …")`. The log never records a half-
applied batch. Containment is a crash-consistency net, not a license —
closures must still not panic. Residual: a panic inside the Err arm's
`expect("rollback must not fail")` is outside this net.

---

## Residual documented non-op document writes

Two sites still write document fields without an op (R-3 CLOSED in Plan F
Task 10 — see below). Both are documented at the site, both are under the
session lock, and neither is a user-visible edit — recorded here so the
totality claim stays honest rather than absolute-sounding. (Plan F,
2026-08-14.)

**R-1 — `ControlPlane::set_plugin_pending_state`**
(`src-tauri/src/control/mod.rs:1465`). Seeds
`session.plugins.pending_state[id]` with bytes the caller already obtained
from the live host, so the following `Op::PluginSetState` commit computes
its inverse from real truth instead of a stale/absent blob. This is the
prepare-outside pattern's hand-off, not an edit: it is always immediately
followed by the op that makes it document truth.

**R-2 — the adopt-install helpers**
(`src-tauri/src/plugins/state.rs:665`/`:754`,
`src-tauri/src/plugins/automation.rs:400`/`:405`). They install a project's
persisted plugin rows / automation lanes into the session at an EPOCH
boundary. Sanctioned by ruling 4 — an epoch is where the document is
replaced wholesale, and nothing about that replacement is an op.

**R-3 — CLOSED (Plan F, 2026-08-14, Task 10, ruling F-12).** The demo's
Zyn bootstrap is no longer a direct session push. `try_seed_zyn_demo_instruments`
only PREPARES (host instantiate + patch load + captured post-load state —
prepare-outside, no session lock). The seed's one commit applies
`Op::PluginAdd` + `Op::PluginSetState` per instance. Persistence rides
`PersistEffect`; the demo is one undoable step including its instruments;
a cold journal replay reconstructs the plugins. The old direct-push
paragraphs are retired.

**R-4 — `Committer::execute_host_forward`'s `Instantiate` writeback**
(`src-tauri/src/control/mod.rs`, `apply_instantiate_writeback`). After the
host round-trip returns, the arm writes `status = "active"` and fills an
empty param mirror for the instance. `status` IS document state (it
round-trips through `PluginInstanceInfo`, is carried by `Op::PluginAdd`, and
is persisted), so this is a genuine non-op document write — found by the
Plan E whole-branch review as I-3. It stays a carve-out rather than becoming
an op because both op-shaped routes are closed: a TRANSIENT batch addressing
`ObjectRef::Plugin` trips `debug_assert_transient_invariant` (the M-3 redo
invariant), and a NON-transient one would push a phantom undo entry for every
plugin instantiate and every undo-of-a-remove. What the write now has is an
EPOCH GUARD, the same one `execute_persist` uses: a document swapped between
the commit and the host's return is a different document, and this commit's
host result is not about it.

**F-6 residual (Plan F, 2026-08-14) — outgoing persist on open is dropped.**
I-1 landed as option (b): Save-As writes plugin/automation/modulation into
the new dir. Option (a) — epoch functions flush the *outgoing* document's
pending persist before the swap — was deferred. `open_project_epoch` still
writes nothing for the project being closed; an in-flight persist for that
document is skipped with a warn (`execute_persist`'s epoch guard). Ctrl+S
(M-2) is the user recovery path. The epoch-guard comment must not be read
as "the new epoch saves the old project."

---

## Verified non-writers

Checked against the tree at `73bb9a7` and RE-VERIFIED line by line on
`automation-audible` (Track D close-out) — this document's value is its
navigability, so every citation in it was re-resolved against the current
tree after Track D grew `plugins/automation.rs`, `control/mod.rs`,
`control/session.rs` and `audio/engine.rs`. Re-check them again whenever
those files move:

* `sidecar_split_stems` / `sidecar_transcribe` — job submits only
  (`src-tauri/src/sidecars/mod.rs`).
* the export thread — deep-copies the store under ONE lock and never writes
  (`src-tauri/src/control/export.rs:531` — store, midi, plugin rows and
  (Track D) automation lanes).
* MCP handlers — all mutations go through `ControlPlane`
  (`src-tauri/src/mcp/handler.rs`, `src-tauri/src/mcp/server.rs`).
* the plugin scan worker (`src-tauri/src/plugins/scan_worker.rs`), the
  sampler preview path (`src-tauri/src/audio/sampler_preview.rs`), the
  recorder thread (`src-tauri/src/audio/recorder.rs`) and the meter cache
  (`LatestMeterCache`, `src-tauri/src/control/mod.rs:850`) — none of them
  holds a `Session` handle at all.
* `hum.rs:314`/`:356`, `import.rs:237`, `loopjam.rs:230`/`:554`,
  `midi/mod.rs:248`, `audio/mod.rs:94`/`:353`/`:401`/`:568` — all
  read-only session locks (snapshot-then-drop), verified line by line.
* **`engine::Control::drive_param_automation`** (Track D) — writes plugin
  PARAMS ON THE HOST, at the engine control thread's ≤2 ms tick, and writes
  NO document field. It is therefore not a residual, but it IS a host-write
  site outside `execute_host_forward`, so the grep gate's readers need to
  know it exists. Deliberate: automation overrides the stored knob value
  during playback while the document keeps what the user set. Consequences,
  recorded so they are not mistaken for bugs:
  - After playing an automated section the plugin's live value can differ
    from the document's stored value until the next project load or user
    edit; the document is authoritative on save. The param panel no longer
    hides that: the driver's writes are published on the meter frame
    (`MeterFrame.drivenParams`, `pump_meter_frames`) and the panel paints
    them with an AUTO flag while the transport rolls, falling back to the
    document the moment it stops. Display only — nothing reads that
    read-back back into the document.
    - The asymmetry that remains, unchanged by that panel work: a STOP
      clears the read-back, so the panel returns to the document value while
      the PLUGIN still holds the last automated one. The panel is right about
      what will be saved and wrong about what the plugin has, until something
      writes the param. Closing it needs the driver to hand the param back on
      stop (a host write per automated param, plus a rule for which value
      wins), which is the same read/write-arbitration question
      fader-follows-automation and write/touch/latch raise.
  - A flat automated param is RE-ASSERTED every ~0.5 s
    (`ParamAutomationDriver::REASSERT_TICKS`), because the host is not ours
    alone — the plugin's own GUI, a patch load or a knob drag can move the
    param behind our back and a flat lane would otherwise never correct it.
    While the user HOLDS a knob on such a param during playback, the
    plugin's GUI and the audio therefore snap back at 2 Hz — and AURA's own
    panel now shows the re-asserted value rather than the knob's, so the
    snap-back is visible instead of silent. This is the gap write/touch/latch
    automation modes fill (landed for track gain, PR #85; still open for
    plugin params).
  - **Export does not evaluate plugin-param lanes at all**, and a bounce
    captures whatever value the live host instance holds — most recently,
    whatever the last playthrough left there. There is one copy of a plugin
    instance in the process, so a bounce cannot drive it without writing the
    export's automation into the user's live plugin. The fix needs private
    per-render plugin instances (a `clap_host`/`lv2_host` API addition); see
    the KNOWN DIVERGENCE note on `audio::offline::build_graph` and the Track
    D handoff in `docs/PHASE4-PLAN.md`. TRACK-GAIN lanes, by contrast, ARE
    compiled into the offline graph and a bounce follows them exactly.

## The grep gate

```
grep -rn '\.lock()' src-tauri/src --include=*.rs
```

Every DOCUMENT-writing session-lock site in the tree is one of:
`src-tauri/src/control/session.rs` (`Session::transact`/`apply_raw` — the
channel itself), `Committer::commit_with_rebuild`/`execute_persist`/`execute_host_forward`
(`src-tauri/src/control/mod.rs` — effect execution and the
`midi.dirty`/`dirty_state` persist bookkeeping), the five sanctioned epoch
functions (rows 24-26), the adopt-install helpers (R-2), the snapshot-
republish sites listed below, or the two recorded residuals R-1 and R-4
(R-3 CLOSED in Plan F Task 10).

**Snapshot republish sites** (Plan F, 2026-08-14, Task 5). Every sanctioned
non-op writer that mutates the live document outside `transact` is marked
`// snapshot republish:` and must call `republish_full` under the same
lock as the write. Grep gate: `rg -n 'snapshot republish:' src-tauri/src`.
Current sites (re-counted 2026-08-16 on `plan-f-history`):

* `control/mod.rs` — R-4 instantiate writeback; R-1 pending-state seed;
  modulation adopt; create/open/save-as epoch swaps
* `control/session.rs` — panic restore
* `audio/project.rs` — `ensure_default_project` document birth
* `plugins/state.rs` — adopt install + I-7 clear
* `plugins/automation.rs` — adopt replace + I-7 clear

**Every production `session.lock()` in `src-tauri/src/audio/engine.rs` is a
documented short read** (Plan F, 2026-08-14, Task 6). `rebuild`'s graph
*assembly* and `ensure_loaded` no longer take the session lock — they read
the published `SessionSnapshot`. Survivors (re-anchored 2026-08-16):

* `:1377` — rebuild PHASE 2 only: param VALUES + slot map + automation
  compile (short; no clip assembly, no decode)
* `:1716` — meter fold (display order)
* `:1821` — `transport_snapshot` (store.transport + RT atomics)
* `:1909`, `:1998`, `:2007`, `:2224` — recording resolution (project dir,
  take numbering, target validation, tempo/ppq)

The engine's WRITE sites still do not take the session lock themselves —
they go through `Committer::commit_with_rebuild`. `:184` is
`published_handle()` at control-thread start, not a live-document read.
