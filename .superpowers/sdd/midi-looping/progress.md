# MIDI clip looping — progress log

Solo overnight session (knobo asleep). Spec:
`docs/superpowers/specs/2026-08-13-midi-clip-looping-design.md`. Plan:
`docs/superpowers/plans/2026-08-14-midi-clip-looping.md`. Baselines at
ae7c868 (origin/main, PR #1 merge): 346 backend tests / 80 frontend tests.

Branch: `midi-clip-looping`, off `origin/main` @ `ae7c868`.

## Best-effort rulings (no one to ask — logged, not blocking)

- **Accessor naming**: `MidiClip::content_length_ticks` is the wire *field*
  (`Option<u64>`); the "give me the effective length" helper is named
  `effective_content_length_ticks()` rather than reusing the field name, to
  keep field-vs-method reads unambiguous at call sites (both compile fine in
  Rust, chose clarity over cleverness).
- **Drag persistence cadence**: `midi_set_clip_bounds` is NOT invoked on
  every `pointermove` (would flood IPC). Frontend keeps the existing
  optimistic-local-update-per-move pattern and commits to the backend once,
  on `pointerup` (mirrors the D-03 "one invoke per edit gesture" rule used
  by `midi_set_notes`).
- **Keyboard nudge**: `MidiClipView`'s ArrowLeft/ArrowRight nudge (pre-existing,
  frontend-only) now also commits through `midi_set_clip_bounds` after each
  press — it's a discrete gesture already (D-03), and the spec's "drag-move is
  frontend-only, never reaches the scheduler" hole applies to it too.
- **Paste target track ("the selected track")**: the codebase has no
  standalone "selected track" concept — only `midi.selectedClipId`. Ruling:
  paste/duplicate target track = `midi.selectedClip?.trackId`, falling back
  to the copied clip's own original track when nothing is selected on the
  timeline (duplicate always uses the source's track, since it has no
  competing "selected" other than the source itself).
- **Copy/paste/duplicate note identity**: confirmed no new backend surface is
  needed for "stamped copies always mint fresh note ids" — `midi_set_notes`'s
  existing keep-rule (`assign_incoming_note_ids`) already mints fresh ids for
  every note whenever the target clip's existing note set is empty (true for
  a freshly `midi_add_clip`'d clip), because the keep-rule requires the
  incoming id to already belong to *that* clip. The frontend `MidiNote` TS
  type has no `noteId` field at all yet (pre-existing, unrelated to this
  task), so outgoing JSON never carries one anyway — belt and suspenders.
- **App.svelte edge-jump tempo-map fix**: no dedicated test harness exists
  for `App.svelte` (no component test infra, confirmed against
  `src/**/*.test.ts`); fixed inline per spec §6, verified by reading the
  diff carefully rather than a new test (consistent with how the rest of
  App.svelte is already untested).
- **Gesture/rendering layers** (edge-drag, repeat separators, demo.ts
  modulo): spec §7 says these have no component-test infra and are
  "verified live in the app." No interactive display in this sandboxed
  session tonight — verified by careful code reading + `svelte-check` +
  `vitest run` + `cargo test`/`cargo build` compiling cleanly instead. Flagged
  here for a human to eyeball live before merge.

## Milestones

- Tasks 1-5 (backend: data model, schedule.rs repeat expansion, persistence
  round-trip, AMT content-length validation, midi_set_clip_bounds command)
  landed, one commit each, full backend suite green after every commit.
  Backend count after Task 5: 358 (346 baseline + 12 new tests).
- Noticed once, not reproduced: `plugins::host::tests::plugin_main_thread_slots_and_tickers`
  failed ONE full-suite run after Task 5, passed in isolation and on the
  immediate full-suite rerun. Unrelated to any file this branch touches
  (plugin host main-thread ticker) — logged as a pre-existing flaky/timing
  test under parallel execution, not a regression from this work.
- Tasks 6-11 (frontend: types/store wiring, edge-drag gesture + repeat
  rendering, demo.ts modulo, clip stamping store logic + keyboard wiring,
  piano roll content-length bounds) landed, one commit each. Frontend count
  after Task 11: 88 (80 baseline + 8 new: 1 clip-edit-loop content-length
  case + 7 midi-stamp.test.ts cases). `svelte-check` clean (0 errors, 0
  warnings) after every frontend task.
- ALL 11 PLAN TASKS COMPLETE. Final gates: backend 358/358 passing
  (`cargo test --manifest-path src-tauri/Cargo.toml`), frontend 88/88
  passing (`npx vitest run`), `svelte-check` clean. Branch pushed at every
  task boundary; PR opened.
- Not done tonight, by design: live-in-app visual verification of the
  edge-drag gesture, repeat rendering (separator lines), and the piano
  roll's playhead-wrap — no interactive display in this sandboxed session.
  Everything in that category was verified by careful code reading +
  svelte-check + the full vitest regression suite (see the "gesture/
  rendering layers" ruling above). A human should eyeball these live before
  merge:
  - Drag a MIDI clip's right edge past its content and confirm it loops
    visually (separator lines at each content boundary) and audibly.
  - Confirm the piano-roll playhead re-enters at the left edge on each
    repeat instead of disappearing.
  - Confirm Ctrl+C/V/D on a selected timeline clip.

(see the plan file's checkboxes — all checked — for the authoritative,
completed task list)
