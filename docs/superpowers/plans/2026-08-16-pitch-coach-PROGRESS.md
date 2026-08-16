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
| Worktree | `/home/knobo/prog/dav/.claude/worktrees/pitch-coach` |
| Branch | `feat/pitch-coach` |
| Branched from | `origin/main` @ `bcdc481`, merged `origin/main` @ `d0866ad` |
| Baseline at merge | 792 Rust tests pass, 440 frontend tests pass, 0 failures |

Run everything from the worktree. Do **not** `cd` to `/home/knobo/prog/dav`.

## Status

**Phase 0 — design.** Done.

- [x] SingStar/UltraStar scoring + pitch-detection research (agent)
- [x] Codebase architecture survey (agent)
- [x] Owner rulings R1–R6 captured (spec §2)
- [x] Spec written, self-reviewed, committed — `4e9d684`
- [x] Pre-existing red test on main fixed by the owner in PR #45, merged in
- [x] Implementation plan written and self-reviewed

**Phase 1 — backend.** In progress; the pure-DSP half is done.

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
      before any UI is built

**Phase 2 — panel.** Not started. Tasks 7–11.

**Phase 3 — scoring.** Not started. Tasks 12–16.

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

## Open items needing the owner

1. **`src-tauri/src/lib.rs` was FROZEN.** Owner later authorised additive
   command registration (new names only). Task 6 may edit `lib.rs` for
   the five new pitch commands; do not rename existing ones.
2. **Speaker bleed is mitigated, not solved.** Without headphones the
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
