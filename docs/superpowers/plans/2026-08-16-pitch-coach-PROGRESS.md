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

**Phase 1 — backend.** Not started.

- [ ] Task 1 — YIN detector (`audio/yin.rs`)
- [ ] Task 2 — decimation to 8 kHz (`audio/decimate.rs`)
- [ ] Task 3 — pitch frames: gating, smoothing, timestamps (`audio/pitch.rs`)
- [ ] Task 4 — parity guard vs. `sidecars/hum_to_midi.py`
- [ ] Task 5 — **InputHub** (`audio/engine.rs`) ← the risky one
- [ ] Task 6 — commands, events, schemas (+ owner must register in `lib.rs`)
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

## Open items needing the owner

1. **`src-tauri/src/lib.rs` is FROZEN.** Task 6 needs five commands
   registered there, Task 14 needs three more. Ask; do not edit, do not
   work around.
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
