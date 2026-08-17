# Pitch Coach — live progress / handoff

**Purpose:** if this session runs out of context and another agent takes
over, this file plus the spec and the plan are everything needed to
continue. Read all three, in this order:

1. `docs/superpowers/specs/2026-08-16-pitch-coach-design.md` — what and why
2. `docs/superpowers/plans/2026-08-16-pitch-coach.md` — the 16 tasks
3. this file — where we actually are

**Reply to the user in Norwegian.** They write Norwegian; the repo
documentation and all commits are English.

---

## Where the work lives

| | |
|---|---|
| On `main` | PR #49 `84b0313` (phase 1) + PR #54 (off-RT split, listen mid-take, `pitch_check`) |
| On `main` | **Phase 2**: PR #58 `7c3cb87` (panel, frame bus, lane geometry, prefs, `pitch_unsubscribe`) |
| Worktrees | stale — `pitch-coach`, `pitch-rt`, `pitch-phase2`. Keep the BRANCHES: this file cites their per-commit SHAs |
| **In flight** | **Phase 3** on branch `feat/pitch-coach-scoring`, worktree `.claude/worktrees/pitch-phase3`, branched from `origin/main` `cf224ce`. Not pushed, no PR yet |
| Next | **Task 14's command half** — see the phase 3 checklist below |

## Status

**Phase 0 — design.** Done.

- [x] SingStar/UltraStar scoring + pitch-detection research (agent)
- [x] Codebase architecture survey (agent)
- [x] Owner rulings R1–R6 captured (spec §2)
- [x] Spec written, self-reviewed, committed — `4e9d684`
- [x] Pre-existing red test on main fixed by the owner in PR #45, merged in
- [x] Implementation plan written and self-reviewed

**Phase 1 — backend. DONE.** PR #49, plus PR #54 for the two review
follow-ups and the R3 instrument.

- [x] Task 1 — YIN detector (`audio/yin.rs`) — `eb0d47b`, 799 tests green
- [x] Task 2 — decimation to 8 kHz (`audio/decimate.rs`) — `190316a`, 5/5
      module tests pass. Reviewed inline (no reviewer agent dispatched): the
      fractional-phase interpolation is correct — `t = 1 - pos/step` is the
      right fraction of the `[prev, current]` interval — filter state
      persists across chunks, and `process` allocates nothing.
- [x] Task 3 — pitch frames: gating, smoothing, timestamps (`audio/pitch.rs`)
      — `c579003`, 9/9 module tests pass, 215/215 `audio::` green
- [x] Task 4 — parity guard vs. `sidecars/hum_to_midi.py` — `c7e9555`, 2/2
      pass. Rust and sidecar agree to **3.3 cents**.
- [x] Task 5 — **InputHub** (`audio/engine.rs`) — `fc3b60e`. Listen/record
      lifetime split, rehearse-hold silence, 5/5 plan tests + 71/71
      `audio::engine` green. Hub is `listen_input` + per-take `inputs`.
- [x] Task 6 — commands, events, schemas + `lib.rs` registration (additive
      names only). `pitch_listen_{start,stop}`, `pitch_subscribe`,
      `set_rehearse_hold`, `pitch_set_reference`; `pitch://state`; schemas
      stay open (D-06). 227/227 `audio::` green.
- [ ] **Owner checkpoint** (spec R3): demonstrate detected pitch numerically
      before any UI is built. **Run it with:**

      ```sh
      cargo run --manifest-path src-tauri/Cargo.toml --example pitch_check -- 15 A3 --tone=0.35
      ```

      **Measured 2026-08-16**, speaker -> microphone, 44.1 kHz stereo input:

      | | |
      |---|---|
      | target | 220.00 Hz (A3) |
      | median detected | 220.02 Hz (+0.2 cents) |
      | \|error\| | median 0.4 cents |
      | within | 100 % <= 25 cents |
      | voiced | 100 % from first sound to last |
      | clarity | median 1.00 |
      | frame rate | 99-100 Hz |

      `--tone` plays the target out of the speakers so nothing has to be
      sung: the owner cannot reliably hit a pitch, and scoring a run against
      a note a person is aiming at mixes their error with the detector's
      with no way to separate them. Level matters — at the default 0.1 the
      same run came back 55 % voiced with readings at exactly half the
      target (YIN's sub-harmonic under poor SNR) and detections down at
      70 Hz that were room rumble. That was the acoustic path, not the
      detector; at 0.35 it is half a cent.

      **Whistle run, same session.** ~811 Hz (G#5) held for ten unbroken
      seconds: every 500 ms window between 810.1 and 813.6 Hz, clarity 0.98,
      81 % voiced with the gaps falling exactly where the owner drew breath.
      The reported -40 cents against G#5 is the WHISTLER, not the detector —
      811 Hz genuinely is 40 cents flat of 830.6 Hz, and reporting that is
      the entire product.

      Two lessons for whoever builds the panel:

      * "Distance to nearest note" saturates near 50 cents for anyone
        sitting midway between two semitones, which is where a person who
        cannot hit a pitch lives. It reads as a failure and is not one.
        `pitch_check` therefore also reports the jitter of the longest
        unbroken voiced run — the detector measured against itself. On a
        synthetic tone that is 0.1 cents, which is the measurement floor to
        compare any voice run against.
      * The note LABEL flips across a semitone boundary while the underlying
        pitch barely moves (G5 at 795 Hz, G#5 at 811 Hz, from one 500 ms
        window to the next). Phase 2 should draw the midi float, never the
        rounded label, or the trail will strobe for exactly the users this
        feature exists for.

      **Voice run — the case that actually tests the detector.** A sustained
      open vowel at ~100 Hz (G2), 15 s, input at -32 dBFS:

      | | |
      |---|---|
      | voiced | 100 % from first sound to last (1312 frames) |
      | longest unbroken run | 13.1 s — the whole vocalisation, no dropouts |
      | detected range | 92.4 .. 101.9 Hz |
      | jitter | median 9.6 cents over that run |
      | clarity | median 0.99 |

      **No octave errors in 1312 frames.** That is the result worth keeping:
      a ~100 Hz vowel carries strong harmonics at 200/300/400 Hz, which is
      exactly where YIN reports the wrong octave, and the detected range
      never leaves 92-102 Hz. The 9.6 cents of jitter is the VOICE — the
      synthetic tone through the identical chain reads 0.1 cents, and ~10
      cents is ordinary human variation on a held vowel.

      Practical note for whoever runs this next: an open "aaaa" is the test.
      A closed-lip hum loses most of its energy before it reaches the
      microphone — an earlier attempt at one came back 0.3 % voiced with the
      level barely above the room floor, which read as a detector failure
      and was not one.

      **What none of this settles:** one voice, one vowel, one room.

      The plan's checkpoint text said to drive the running app. That is not
      possible: the five pitch commands are registered in `lib.rs`, but
      `src/lib/tauri.ts` has no bindings (that is Task 7, phase 2),
      `withGlobalTauri` is unset so `window.__TAURI__` does not exist in
      devtools, and no MCP tool exposes them. `examples/pitch_check.rs` opens
      the default capture device and runs the real `PitchTap` +
      `PitchWorker`, printing Hz, nearest note, cents error, voiced fraction
      and clarity. It does NOT exercise the Tauri command layer or the 60 Hz
      batching — that is phase 2's to prove.

**Phase 2 — panel. DONE and merged** — PR #58, squashed as `7c3cb87`.

- [x] Task 7 — wire types + backend bindings — `c757b8c`. Five bindings,
      not the plan's four: `pitchSetReference` is the fifth command Task 6
      registered and Task 11's picker needs it. `pitch://state` joined
      `AuraEventMap`; `recording://state` grew the optional `rehearseSpans`
      the backend already emits. **`vitest` does not type-check** — it
      strips types — so `npx svelte-check` is what proves these types, and
      the test file's runtime half compares the interface keys against the
      published schemas' properties.
- [x] Task 8 — non-reactive frame bus (`state/pitch.svelte.ts`) — `60d7ca3`,
      10/10. Fixed 3000-frame ring. `startPitchStream` subscribes to BOTH
      halves of the wire: the batch channel (frames + the two flags that
      must not lag) and `pitch://state` (`referenceTrackId`, which rides on
      nothing else).
- [x] Task 9 — lane geometry (`pitch/lane.ts`) — `514866b`, 20/20.
- [x] Task 10 — five preferences + `TOLERANCE_CENTS` — `809b3b5`, 25/25 in
      the prefs suite. `NumberDef.unit` grew an `"ms"` case.
- [x] Task 11 — the panel — `7cc1aa5`. 547 frontend tests green,
      `svelte-check` clean, `npm run build` green.
- [x] Review round on PR #58 — `a590136`. Nine findings, all fixed; the
      branch is no longer frontend-only (see the log).

**Phase 3 — scoring. IN PROGRESS** on `feat/pitch-coach-scoring`. Tasks
12–16: shared repeat-expansion helper, scoring, pitch track on disk, the
report, docs + PR.

- [x] Task 12 — `clip_notes` + `AbsNote` in `midi/schedule.rs` — `aff2564`,
      14/14 `midi::schedule`, 151/151 `midi::`. `clip_events` and
      `clip_notes` now share one `expand_notes`, and
      `clip_notes_and_clip_events_agree_on_timing` fails if they drift.
      `AbsNote.note_id` is the repo's `NoteId`, not the plan's bare `u32`;
      `clip_id_hash` is FNV-1a because `DefaultHasher` is not stable across
      Rust releases and this value reaches the frontend.
- [x] Task 13 — `control/pitch_coach.rs`: `reference_melody` + `score` —
      `78d0e32`, 14/14. Two deliberate deviations from the plan, both
      because the plan disagreed with its own tests: `vibrato_extent_cents`
      is PEAK-TO-PEAK (the plan said half of it, then asserted > 50 cents on
      a 90 cent wobble), and the hit hysteresis counts the frames that
      trigger a transition as part of the state they trigger (charging them
      to the singer caps a flawless take at 97.8 %, and the plan's own first
      test wants > 99).
- [x] Task 14, **first half** — `audio/pitch_store.rs` (`APTF`, `PitchFolder`,
      `analyze_interleaved`) + the recorder fold — `9f21522`, 10/10 store,
      5/5 recorder.
- [ ] Task 14, **second half** — the three commands (`pitch_score`,
      `pitch_track`, `pitch_analyze_clip`), `pitch-score-report.schema.json`,
      and their registration in `lib.rs` (additive names only, which the
      owner has already authorised).
- [ ] Task 15 — the report UI (`PitchReport.svelte`, `pitch/report.ts`).
- [ ] Task 16 — `docs/pitch-coach.md`, ARCHITECTURE, the count docs, PR.

`panel-logic.targetNotesFor` still expands repeats frontend-side with the
timeline preview's rule. Task 12 built the backend replacement but nothing
consumes it yet — retiring the frontend copy is Task 15's business.

**Test counts are NOT yet updated** in README/CONTRIBUTING (the dated-count
convention). Phase 3 has added 3 + 14 + 10 + 2 = 29 Rust lib tests so far;
measure, do not copy that number, and land the update with Task 16.

### What Task 14's second half needs to know

Written down because the first half made the decisions:

- **`APTF` positions are TAKE-LOCAL**, in the take's own sample rate, and
  they snap onto a `first_sample + i * hop` grid. The scorer therefore has
  to map them onto the timeline itself:
  `timeline = clip.timeline_start_samples + (sample - clip.offset_samples)
  * project_rate / clip.source_sample_rate`. The reference notes from
  `clip_notes` are already in project samples.
- **The header carries `first_sample`** because the analyser timestamps the
  CENTRE of its window (~15 ms in). Deriving positions from a bare
  `i * hop` would slide every curve early by that much.
- **Provenance is `CausalMedian` for both producers today.** The offline
  path reuses the live detector; being offline is not the same as being
  centred. Decision (a) of `pitch-as-data.md` is satisfied by the byte
  existing, not by a centred pass existing.
- **`score()` leaves `reference_track_id` empty** — the command stamps it.
- Missing cache: the intended shape is for `pitch_score` to analyse and
  write the track when `cache/pitch/<clipId>.bin` is absent, so the UI
  never has to make two round trips. Analysis runs at roughly 100× real
  time, so a 3-minute take costs ~2 s on the command thread.

**Before implementing anything more of Task 14, read
[`docs/backlog/pitch-as-data.md`](../../backlog/pitch-as-data.md).** Its
four decisions are what the format above already encodes; (b) (do not widen
the format on speculation) and (d) (the curve is read-only) are still live
constraints on the rest of the task.

### What phase 2 must not get wrong

Both of these came out of actually running the detector against a voice
(see the log), not out of reading the plan:

- **Draw the `midi` float, never the rounded note name.** The label flips
  across a semitone boundary while the pitch barely moves — G2 to G#2 on a
  0.9 Hz change, seen on both a whistle and a held vowel. Drawing the label
  makes the trail strobe for exactly the users this feature exists for.
- **Do not make "distance to the nearest note" the headline number.** It
  saturates near 50 cents for anyone sitting midway between two semitones,
  which is where a person who cannot hit a pitch lives. It reads as a
  failure and is not one. Stability — how steadily the reading holds — is
  the honest measure of whether the detector is tracking someone.

### Still open, beyond the numbered tasks

- **The Tauri command layer is still unproven against the real engine.**
  Phase 2 drives all five commands and the batch channel end to end, but
  only against `DemoBackend`'s synthetic singer in a browser. Nothing has
  yet run the panel against `pitch_subscribe` in a live Tauri build — that
  is the owner's ear-check, and it is the first thing to do with this
  branch.
- **One voice, one vowel, one room.** No woman's voice, no falsetto, no
  vibrato, no backing track under it.
- **Speaker bleed is mitigated, not solved** (see below). The panel says so
  once, plainly.

## Decisions already made (do not relitigate)

- **Tauri + Rust + Svelte 5**, not Electron. No Web Audio, no
  `getUserMedia`. All audio processing is Rust — owner instruction, and
  ADR 0006.
- **The mic opens on an explicit listen toggle or while the Pitch Coach
  panel is open** — *not* on track arm (owner ruling R6).
- **Rehearse-hold is both** a held `H` key (configurable) and a
  press-and-hold button in the panel (R5). It writes **silence** into the
  take rather than skipping, so the take stays sample-aligned.
- **The input stream is rebuilt when the consumer set changes**, not
  mutated in place. A command ring into the RT callback is the "correct"
  mechanism and the upgrade path, but rebuild-on-change is behaviourally
  equivalent, happens only on control-thread transitions, and is far
  cheaper to get right in the repo's most sensitive file. Accepted cost: a
  few ms of pitch blackout at record start. See spec §3.1.
- **Detector constants are copied verbatim** from `sidecars/hum_to_midi.py`
  so the live trainer and hum-to-MIDI never disagree. Task 4 is the test
  that keeps them honest.
- **Scores are never persisted**, only the pitch track. Recomputing means
  the report stays correct after the reference melody is edited.
- **Chord resolution (top note + ambiguous flag) is Rust-side.** An earlier
  plan draft put it in the panel; that was an ADR 0006 violation and was
  removed. Phase 2 draws every note of the reference track; phase 3 dims
  the ones the backend flagged.

## Environment warning (read before running the suite)

This box is under memory pressure: **swap is fully consumed (15/15 GB)** and
other worktrees run `cargo test` concurrently. A full `cargo test` therefore
fails *differently each run*, always in unrelated ALSA MIDI-loopback tests
(`midi_out::tests::*`) or `plugins::host::tests::plugin_main_thread_slots_and_tickers`.
Do not chase those as regressions without first confirming the machine is
quiet. Verify a task with its own module tests (`cargo test audio::<mod>`),
which are deterministic, and treat a full-suite run as valid only when no
other agent is building.

Root filesystem was at 100% during Task 2; the implementer freed this
worktree's own `target/` and it is now at 82% (133 GB free). Roughly 194 GB
of stale `target/` directories remain across the other worktrees — cleaning
them is the owner's call, not an agent's.

Also: `cargo fmt` without a path argument reformats the whole crate here.
Always check `git status` before committing after running it.

## Review follow-ups (PR #49)

Merge-blocker bugs from the review are fixed on `main`. Issues 6 and 7
are done on `feat/pitch-rt-thread` (see the log below); nothing from the
review is left open.

- ~~**Issue 6:** spec §3.2 wants YIN / RMS gate / median / jump limiter on
  a dedicated non-RT pitch thread.~~ Done — `audio/pitch_thread.rs`.
- ~~**Issue 7:** `pitch_listen_start` mid-take that already owns the pitch
  device sets `wants_listening` but does not attach a `PitchTap`.~~ Done —
  the take carries a dormant tap and the shared flag wakes it.

## Open items needing the owner

1. **R3 is answered on substance; the box is the owner's to tick.** Three
   runs, recorded under the checkpoint above: a synthetic tone at 0.4
   cents, a whistle held 10 s, and — the one that matters — a sustained
   vowel at ~100 Hz with **no octave errors in 1312 frames**. A vowel
   carries strong harmonics at 200/300/400 Hz, which is where YIN reports
   the wrong octave, and the detected range never left 92–102 Hz.
2. **`src-tauri/src/lib.rs` was FROZEN.** Owner later authorised additive
   command registration (new names only). Task 6 may edit `lib.rs` for
   the five new pitch commands; do not rename existing ones.
3. **Speaker bleed is mitigated, not solved.** Without headphones the
   backing track is detected as pitch. The panel says so once, plainly.

## Conventions

- Conventional Commits with a scope: `feat(pitch): ...`, `fix(audio): ...`.
- Every commit message ends with
  `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
- Pre-PR gate, all four:
  `cd src-tauri && cargo test`, `npm test`, `npx svelte-check`, `npm run build`.
- Never claim a step passed without pasting the command output.

## Update this file

After finishing each task: tick its box, and add a line under the log below
with the commit sha and anything the next agent would be surprised by.

## Log

- 2026-08-17 — Phase 3 started on `feat/pitch-coach-scoring` (worktree
  `.claude/worktrees/pitch-phase3`, from `origin/main` `cf224ce`). Tasks 12,
  13 and the first half of 14 are committed but NOT pushed. Six things the
  next agent would otherwise rediscover the hard way:
  1. **`cargo fmt`/`rustfmt` on a whole file reformats the repo's existing
     code.** `rustfmt src/midi/schedule.rs` turned a 175-line diff into a
     407-line one, all of it reflowing untouched test code — this crate has
     never been rustfmt-clean. Hand-match the surrounding style instead, and
     never run `cargo fmt` without a path (the phase 1 log says the same).
  2. **Frame positions out of `PitchAnalyzer` are NOT exactly hop-spaced.**
     Its integer timestamp maths re-anchors at every push, so a chunk
     boundary shifts a frame by up to ~4 samples. Bounded, not cumulative,
     and under 0.2 ms against a 10 ms hop — but a test asserting exact
     spacing fails, and the file format snaps them back onto the grid.
  3. **`PitchAnalyzer::push` stops appending at 2048 frames.** That cap
     exists for the RT path; anything folding a whole take must drain the
     out-vec after every push or silently lose everything past 20 s.
  4. **A ring drain can split an interleaved frame down the middle.** The
     recorder hands the writer two slices from one rtrb chunk, and the split
     is at a raw sample index. `PitchFolder` carries the remainder to the
     next push — a decimator that swallowed a half frame would swap the
     channels for the rest of the take.
  5. **`RecSpec` grew `pitch_path: Option<PathBuf>`.** `None` means "fold
     nothing" and is what the engine's two error-path tests pass.
  6. Verification ran per module (`cargo test --lib <module>`) with
     `CARGO_TARGET_DIR` pointed at the main checkout's target dir, per the
     box's disk constraint. The full suite has NOT been run on this branch
     yet; it is owed before the PR, single-threaded.

- 2026-08-17 — **Owner ear-check done, and it found a real one.** The panel
  tracked a real voice well, but the reference notes "did not fall into
  place until I pressed record". Cause: the panel drew the LANE only while
  `transport.isPlaying` and the TUNER otherwise, so selecting a reference
  track while stopped showed the tuner — no target notes at all. On top of
  that, `fixed` mode pinned the lane to the playhead, so a melody starting
  at bar 5 sat off-screen even once the lane appeared. Fixed:
  1. **The lane is drawn whenever a reference melody exists**, stopped
     included. The tuner is for having no melody, or for asking (there is a
     TUNER chip now) — never for hiding one.
  2. **`laneWindowFor` replaces `laneScrollFor`** and gives the lane its own
     zoom: a readable span derived from the melody's own length (4–12 s),
     framing the melody when stopped outside it, pinning the playhead when
     rolling or when stopped inside it. The zoom does NOT depend on
     `playing`, so record is not a jump. `laneScrollFor` stays for now but
     nothing uses it.
  3. **The picker draws the user's choice immediately** instead of waiting
     for `pitch_set_reference` to round-trip through the control thread. A
     display-only echo, dropped the moment `pitch://state` arrives — the
     truth still comes from the engine.
  Also, the owner asked whether the detector can drive auto-tune. Answer and
  staged path: [`docs/backlog/pitch-correction-autotune.md`](../../backlog/pitch-correction-autotune.md).
  Detection is the done third; the shifter and the correction policy do not
  exist. Offline correction of a take is Stage A.
- 2026-08-16 — **PR #58 reviewed and fixed** (`a590136`). Rust 1020
  (984 lib + 36 integration), frontend 564. Four things worth carrying
  forward:
  1. **`pitch_unsubscribe` exists now** — additive, keyed on
     `Channel::id()`. It had to: `pitch_subscribe` appends a sink and the
     engine only retires one when `send_batch` fails, which a live Tauri
     `Channel` never does no matter how dead its JS end is. Every open of
     the panel leaked a sink. Anything else that subscribes per-mount
     (rather than once for the app's lifetime, like meters) needs the same
     treatment — `subscribeMeters`' mute-only unsubscribe is NOT a pattern
     to copy.
  2. **`PitchFrame.sample` is a PROJECT sample position**, not device-rate.
     It is anchored to `shared.position` and only offset within one
     callback buffer. The old doc comment and schema said "device-rate
     samples" and the panel believed them, which made the latency offset
     ~8% short on a 44.1 kHz mic in a 48 kHz project. Both now say so.
  3. **Rehearse-hold is refcounted** in `state/rehearse.svelte.ts`. Two
     controls, one engine boolean: with a flag per control, releasing one
     while the other is down ended the hold and the take started writing
     real audio mid-rehearsal.
  4. **The full Rust suite is only trustworthy single-threaded on this
     box.** Three parallel runs failed a DIFFERENT set each time
     (`midi_out::tests::*`, `mcp::server::tests::read_meters_hears_the_headless_engine`
     — a 3 s deadline), each passing in isolation; `--test-threads=1` is
     1020/1020. Load average was 5–6 with 4 GB free.
- 2026-08-16 — **Phase 2 complete** on `feat/pitch-coach-panel`
  (`c757b8c`, `60d7ca3`, `514866b`, `809b3b5`, `7cc1aa5`). 547 frontend
  tests, `svelte-check` 0 errors, `npm run build` green. `cargo test` was
  NOT re-run: the branch touches zero files under `src-tauri/`
  (`git diff --name-only origin/main` is all `src/` plus the two count
  docs), and the box is at 91% disk with no target dir in this worktree.
  Seven things the next agent should know:
  1. **Canvas `font` does not resolve `var(--font-mono)`.** An unparsable
     font string is dropped whole and every label silently renders at 10px
     sans — the panel looked "designed small" until the stacks were
     inlined. Found by driving the demo engine in a headless browser, not
     by reading the code. Any new canvas text in this repo needs a literal
     font stack.
  2. **The plan's `autoFitRange` example and its implementation note
     contradict each other** ({58, 66} vs "minimum span 12"). The minimum
     won; the test says why.
  3. **The plan's `laneScrollFor` example computes -44000 samples** — a
     lane starting before time zero. The clamp is now its own test and the
     example uses a position that measures the formula.
  4. **With no reference melody the lane fits the SINGER**, re-centring
     only when they leave it. A fixed C4 octave pins the trail to an edge
     for anyone who does not sing near middle C, and re-fitting every
     frame makes the whole lane breathe with the vibrato.
  5. **`h` was already the hum dock's key.** Rehearse-hold claims it only
     while a take is running or the coach is open, and releases on keyup
     AND on window blur — a window that loses focus mid-hold never
     delivers the keyup, and the take would go on recording silence.
  6. **`DemoBackend` now synthesizes a singer** (drift + vibrato + breath
     gaps) so the lane is developable under `vite dev`. It is a mock; its
     numbers say nothing about the detector.
  7. **The panel opens the mic itself** (`pitch_listen_start` in an
     `$effect`, closed on destroy) — ruling R6's "or while the panel is
     open" half. The listen toggle is the other half.
- 2026-08-16 — Review follow-ups 6 and 7 done on `feat/pitch-rt-thread`
  (`007d346`, `af9b1ea`). Five things the next agent should know:
  1. **The capture callback no longer detects anything.** `audio/pitch_thread.rs`
     owns the split: `PitchTap` (decimate, hand over) on the RT side,
     `PitchWorker` (YIN, gate, median, jump limiter) on a worker thread.
     Two rings, not one — the samples ring plus a descriptor ring carrying
     each chunk's device position and rate, because `PitchAnalyzer::push`
     anchors its timestamps to those. Samples are written BEFORE the
     descriptor, and a chunk that does not fit anywhere is dropped whole;
     a half-written chunk would mistime every frame after it.
  2. **`PitchWorker::pump` is synchronous on purpose.** Tests drive the
     whole chain without a thread. `spawn_pitch_worker` is production's
     entry point, and the handle joins the thread on drop — an
     `InputBundle` declares it after `_stream`, so the callback stops
     before the worker is joined.
  3. **Taps are gated on a shared `Control::pitch_active` flag**, not on
     their own existence. A take on the pitch device now carries a tap
     whether or not anyone was listening when it started, and
     `set_listening` is what wakes it. That is the whole of issue 7: a
     recording stream cannot be rebuilt mid-take without losing audio.
     A dormant tap costs one relaxed atomic load per capture buffer.
  4. **`SAMPLE_RING_SLOTS` must hold one maximal chunk** (`a_maximal_chunk_always_fits`).
     Shrink it and any device the decimator upsamples from goes
     permanently dark instead of dropping a few frames.
  5. **A dropped chunk owes the analyser a reset.** Dropping whole keeps
     the two rings aligned; it does NOT keep the detector aligned, because
     it carries most of an analysis frame across the hole and then
     timestamps the splice at the new position. Found by rewriting the
     alignment test to feed a staircase instead of a steady tone — against
     a steady tone a desync is invisible, which is why the first version of
     that test passed while the bug was there.
- 2026-08-16 — PR #49 review merge-blockers fixed (Issues 1–5, 8). Failed
  take-start restores the listen hub; listen-only capture no longer pushes
  `base_slot == 0` meter blocks; NaN on the capture path is unvoiced (no
  `unwrap`/`expect`); `rehearse_open` resets to this take's start; 
  `SelectInput` emits `pitch://state` and restores the previous stream (or
  clears `wants_listening`) on open failure; pitch batches stay at ~60 Hz
  with no meter subscriber. **Issue 6 is still open** — do not move YIN
  off the callback in this PR. Issue 7 (listen-on mid-take) also remains.
- 2026-08-16 — spec committed `4e9d684`; merged `origin/main` (`d0866ad`,
  the transport fix) into the branch; baseline green; plan written.
- 2026-08-16 — Tasks 2 (review), 3 and 4 done. Three things the next agent
  should know:
  1. **The live median is causal; the sidecar's is centred.** The live pitch
     track therefore lags `hum_to_midi.py` by exactly the median half-kernel
     — 2 frames, 20 ms. This is not a bug and must not be "fixed": a live
     detector cannot look ahead. It is asserted in
     `tests/pitch_sidecar_parity.rs`, and Phase 2 should not be surprised
     when the trail sits 20 ms behind on fast vibrato.
  2. With that lag accounted for the two detectors agree to **3.3 cents**,
     not the 30 the plan budgeted. The tolerance is now 10 cents, which is
     tight enough to actually catch a drifted constant.
  3. Integration tests import **`aura_lib::`**, not `aura::` — the lib
     target is renamed in Cargo.toml. The plan said `aura::`.
- 2026-08-16 — Rebased onto `origin/main` `6af46dd` (Plan F) with no
  conflicts. Snapshot `rebuild` and InputHub both present. Pushed
  `feat/pitch-coach`; PR #49. Task 5 `c039cc4`, Task 6 `736c4c8`.
- 2026-08-16 — Task 6 done. Five additive commands registered in `lib.rs`
  next to `subscribe_meters`. Frames batch on the existing 60 Hz
  `last_frame` tick (`pump_pitch_frames` runs just before
  `pump_meter_frames`). New schemas are draft-07 and do not set
  `additionalProperties: false`. `recording://state` gained optional
  `rehearseSpans`. Frontend types/bindings are Task 7.
- 2026-08-16 — Task 5 done (`fc3b60e`). Input stream lifetime is no longer tied to the
  take: `set_listening` opens/closes a listen-only hub, a take on the same
  device carries the analyser and drops the listen stream, and stop hands
  the mic back. Rehearse-hold writes zeros for the held span (same sample
  count) and `recording://state` grows an optional `rehearseSpans`. Tests
  stub cpal via `stub_input` so they assert on hub presence without a
  microphone. `InputCb::capture` stays RT-safe: pitch scratch is reserved
  at open, `clear()` keeps capacity, surplus past 8192 frames is dropped.
  `drain_pitch` is in place for Task 6's 60 Hz pump. Do not commit
  `sidecars/__pycache__`.
- 2026-08-16 — Task 1 done (`eb0d47b`). Two plan corrections came out of its
  review, both committed: the difference function must sum a full `w` terms per
  lag (`ae338c1`), and the detector's effective pitch floor is 65.04 Hz, not
  `FMIN` (`93c7d3d`). The second is a real, documented property — `tau_max`
  truncates, so a tone at exactly 65 Hz is unreachable, and the Python sidecar
  behaves identically. Parity with the sidecar outranks widening the range.
