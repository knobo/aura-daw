# Landed tracks A–F / Plan F

Extracted from `next-prompt.md` on 2026-08-16. This is the historical
record. **Do not restart these.** Leftovers live in each "Still owed" /
carry-forward list and in `docs/PHASE4-PLAN.md` handoffs, not as a new
parallel track.

Historical plans called this `next-prompt.md` §3.

## State of the world (when this was extracted)

The project is **AURA**, an AI-native DAW: Tauri v2 + Svelte 5 around a
lock-free real-time Rust engine (`src-tauri/`), local AI sidecars, and an
embedded MCP server so agents mutate the session alongside the user.

**Plan F (history storage) is MERGED** (`6af46dd`, PR #23). Tasks 1–13
plus the persist/clipboard review follow-up. A fresh session branching
from `origin/main` gets the published `SessionSnapshot`, lock-free
rebuild assembly, version graph, panic rollback, journal reader
(detection only), and placement offsets. Full handoff:
`docs/PHASE4-PLAN.md`'s **"Plan F handoff"**.

Plan E is already on `main` (squash `27911d8`, PR #12). Its follow-up
PR #18 is also merged. Branch `plan-e-side-channels` is **kept** so SHAs
cited in the Plan E handoff still resolve. The landed inventory is
`docs/SIDE-CHANNEL-INVENTORY.md`: R-3/L-4/L-5 CLOSED (Plan F); residuals
are R-1, R-4, and the F-6 outgoing-persist skip.

**PR #17** (`midi-input-ports`) is **merged** (`3340aa8`) — that was
Track B's prerequisite. Tracks B/C/D/E/F have their own landed PRs
(#21/#22/#20/library-browser/`modulation-system`). Nothing further to
do about those merges.

Test counts live in `README.md` / `CONTRIBUTING.md` (dated-count
convention). Measure before writing a leftover; do not trust a number
copied here. Known flakes: `midi_out` under parallel lib runs;
`apply_hum_clip_commits_synchronously_and_announces_project_changed`.

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

## Track A — Plan F: history storage — LANDED 2026-08-16

**Done, branch `plan-f-history`, PR #23** (13 tasks, plan
`docs/superpowers/plans/2026-08-14-plan-f-history-storage.md`). Full
handoff — rulings F-1..F-12, lifted carry-forwards, new carry-forwards —
is **`docs/PHASE4-PLAN.md`'s "Plan F handoff"**; read that before touching
`SessionSnapshot`, `engine::rebuild`, the journal reader, or the version
graph. Implementation note on ADR 0005 (per-clip Arc, tree deferred) is
under Consequences. Branch `plan-f-history` is **kept** so task SHAs
cited in the handoff still resolve.

What landed: published snapshot + lock-free rebuild assembly, version
graph (64 KB replay-only, janitor), panic rollback in `transact`, journal
reader (detection only), rev-ordered undo stack, R-3 closed, MIDI
placement offsets (including clipboard copy/paste), persist_gate +
re-dirty on a stale write. The Plan A `engine.rs` sequencing note is
**resolved** — assembly no longer holds the session lock.

Carry-forwards for later rounds live in the handoff: live-document B-tree
(trigger = note-delta op), I-1 option-(a) residual, no auto-apply of
journal tails, seeded-PRNG for future random ops, version-graph product
surface unbuilt.

Baseline after this track: **900 backend (866 lib + 34 integration, plus
2 `#[ignore]`) + 456 frontend**, measured on `plan-f-history` 2026-08-16
after the PR #23 review follow-up (rebased onto main).

## Track B — MIDI slice 2: routing, recording, clock/sync out — LANDED 2026-08-15

**Done, branch `midi-slice-2`, PR #21** (12 tasks, plan doc
`docs/superpowers/plans/2026-08-14-midi-slice-2.md`). Full handoff — the
scope rulings later rounds inherit, the standing hazards, the deferred
minors and the three ear checks the owner still owes — is
**`docs/PHASE4-PLAN.md`'s "Track B handoff"** section; read that, not this
paragraph, before touching hardware MIDI.

A `MidiInHub` (`src-tauri/src/audio/midi_in.rs`) rings hardware MIDI from
the midir callback thread into the RT output callback, where the mixer
dispatches it into the target track's own live node — so a keyboard plays
the track's real instrument, playing or stopped. A take is captured
control-side and registered as ONE `Actor::Engine` transaction:
`Op::MidiClipAdd` rides inside the existing `commit_recording_finalize`
alongside the audio `ClipAdd`s, so a take is one undo entry. No new op
kind, no shape change, `OP_FORMAT_VERSION` stays 2. MIDI OUT is a
dedicated non-RT `aura-midi-out` thread (`src-tauri/src/midi_out.rs`) over
`SharedRt` + a tempo/event snapshot — 24 PPQN clock, Start/Stop/Continue/
SPP, and note-out from one chosen MIDI track — and it touches `engine.rs`
not at all. User docs: `docs/midi-input.md`, `docs/midi-output.md`.

**Rulings a later round inherits** (all in the plan, ADR 0007-marked): (1)
the MIDI-in target track is app config behind a `ControlPlane` seam, NOT
derived from `TrackState.armed` — the frontend calls it from the same arm
click; (4) the clock's musical position is `SharedRt::position` + the
`TempoMap`, not `steady`; (5) a backward jump re-cues the slave with
`Stop`/SPP/`Continue`; (9) MIDI-in timestamps are quantised to the audio
block; (10) note-out is a routing carve-out, not a track field; (11) no
persistence of ports/targets across restarts while port ids are unstable.

**What is left**, in priority order: the owner's **three ear checks**
(note-out to real gear, Hydrogen sync, a real keyboard through
arm→record→undo→reopen) — nothing in the test suite substitutes; then
**recording under an active loop, which silently produces a musically
wrong take** (mechanism, both candidate fixes and why the plan's non-goals
do not cover it: `docs/backlog/hardware-midi-io.md`, "Still open after
slice 2" — the choice between the two fixes is a behaviour decision for
the owner); then sub-block timestamping, port persistence, MIDI thru, CC/
pitch-bend, a MIDI-out latency offset, and the document-model
external-instrument target.

## Track C — Multi-clip selection, group-drag, cross-track paste + cross-instance clipboard — LANDED 2026-08-15

**Done, branch `multiclip-clipboard`, PR #22** (13 tasks, plan doc
`docs/superpowers/plans/2026-08-14-multiclip-selection-paste.md`). Full
handoff — all eight scope rulings, the mid-flight rulings, the
deferred-minors roll-up and what is still OWED — is
**`docs/PHASE4-PLAN.md`'s "Track C handoff"** section; read that, not this
paragraph, before touching selection, group drag or the clipboard.

The timeline has a real multi-selection (click / shift / ctrl / marquee) as
**viewer state** — a `Set<"<kind>:<id>">` that never enters the document and
that the backend never reads. A group drag or MIDI group resize is ONE
gesture and ONE undo entry through the new `move_clips`; copy/paste rides a
backend-built `application/x-aura-clips` JSON envelope through two `arboard`
commands, so it works across two running AURA instances. Five new additive
commands: **`move_clips`, `clips_copy`, `clips_paste`,
`os_clipboard_write_text`, `os_clipboard_read_text`**. Keys: Ctrl+C,
Ctrl+V, Ctrl+Shift+V (paste to new tracks), Ctrl+Shift+M (export the
selection as .mid).

**Rulings other tracks must not contradict:** no new op kind, no new
`PropPath`/`ObjectRef` variant, `OP_FORMAT_VERSION` still **2** (paste
composes `TrackAdd`/`ClipAdd`/`MidiClipAdd` in one transaction); selection
is viewer state and never document state; the clipboard wire's tempo domain
is the NOMINAL 48 kHz rate; SMF is export-only (the OS clipboard slot this
app can own is plain text); `engine.rs` untouched.

**Still owed, and it belongs to the owner:** the manual **cross-instance
check** (copy in instance A, Ctrl+V in instance B) — the one link no test in
this repo can make, with a **binding no-input-automation rule**; procedure
and rationale in the handoff. Also read the handoff's **clipboard size
ceiling** before promising anyone large-selection copy between windows: a
failed large copy empties the system clipboard for every application.

**Left deliberately undone:** multi-clip delete/duplicate (Delete and the
`×` from PR #26 stay single-clip — a batch `clips_remove` in ONE transaction
is the prerequisite, see the handoff), arrow-key nudge over a selection,
audio-clip resize, true content instancing, SMF paste, and the hardcoded MCP
port 41717 that real two-instance workflows still hit.

Baseline after this track: **729 backend (700 lib + 29 integration) + 359
frontend**, measured on `multiclip-clipboard` 2026-08-15.

## Track D — Automation audible + lane UI — LANDED 2026-08-14

**Done, branch `automation-audible`, PR #20** (10 tasks, plan doc
`docs/superpowers/plans/2026-08-14-automation-audible.md`). Full handoff —
all eight scope rulings, the non-goals, the mid-flight rulings and the
deferred-minors roll-up — is **`docs/PHASE4-PLAN.md`'s "Track D
handoff"** section; read that, not this paragraph, before touching
automation.

`engine::rebuild` now reads `session.automation`: track-gain lanes compile
into a slot-indexed `RtGraph::gain_ramps` table applied at the mixer's
per-track gain stage (NOT by wrapping nodes in `GainAutomatedNode` — see
ruling 1), and plugin-param lanes are driven on the engine control thread's
≤2 ms tick, host-only, never through the document (ruling 2). The REBUILD
PIN in `control/session.rs` is resolved. The timeline gained an in-lane
overlay for drawing/dragging/deleting points, plus a plugin-param target
picker. Gestures grew a persist deferral, so a drag is one undo entry and
one `project.json` write. Song export compiles the same gain ramps.

Three Plan E findings closed on the way: **I-3 + M-6**, **I-8** (inventory
row 13) and the **frontend M-3** — struck from
`docs/handoff/plan-e-review.md`.

Still open and named there: the **owner's ear check** (nobody has heard it
yet), **plugin-param automation in a bounce**, the **non-blocking CLAP
param path**, and holding a knob against a flat automated param.

**Superseded for the document model by Track F** (below): lanes become
curve+binding pairs under `session.modulation` / project v4; the lane IPC
remains as a compatibility facade. Track D's engine rulings 1, 2, 6 and
gesture rulings 4/7 still stand.

Baseline after this track: **566 backend (537 lib + 29 integration) + 258
frontend**, measured on `automation-audible` 2026-08-15 (superseded by
Track F's 731 + 299).

## Track F — Modulation system — LANDED 2026-08-16

**Done, branch `modulation-system`** (11 tasks, design
`docs/superpowers/specs/2026-08-15-modulation-system-design.md`, plan
`docs/superpowers/plans/2026-08-15-modulation-system.md`, **ADR 0008**).
Full handoff — controller rulings, non-goals, divergences — is
**`docs/PHASE4-PLAN.md`'s "Track F handoff"**; read that before touching
modulation, automation lanes, or project v4.

What the user can do now: several curves per track (gain, pan, plugin
params), an automation-track kind routed to many targets, and clip
envelopes that loop with MIDI content. What is *not* done yet is the
finished graph — **path is design §8**, linked from `next-prompt.md`
(R2: keep that pointer findable; do not restate §8).

Baseline after this track: **731 backend (702 lib + 29 integration) + 299
frontend**, measured on `modulation-system` 2026-08-16.

## Track E — Library & browser panel — LANDED 2026-08-14

**Done, branch `library-browser` (9 tasks, plan doc
`docs/superpowers/plans/2026-08-14-library-browser.md`).** The LIB dock tab
browses three roots: SAMPLES (default library folder + user-configured
folders, click-to-audition without touching the project), CLIPS (the open
project's audio/MIDI clips, draggable back onto tracks), PRESETS (loaded
sampler instruments + ZynAddSubFX bank patches). Four additive backend
commands: `library_scan`, `library_default_root`, `library_audition`,
`library_audition_stop`. Every drag-out lands through the existing,
already-channel-routed import/clip-add commands, so it's undoable for
free — no new document state, no engine.rs touch. The folder list is a
frontend preference: the plan added a new `pathList` preference kind to
`src/lib/prefs/schema.ts` (Preferences → LIBRARY), not a backend config
file.

Deferrals (recursive background scan, metadata probe, waveform
thumbnails, on-disk `.sfz`/`.xiz` browsing, SMF drag-in, tagging/
favourites/search, drag-out to the OS) are recorded explicitly, not
silently, in `docs/backlog/library-and-browser.md`'s updated "Suggested
cut" section — each one line, each dated. The plan doc's scope rulings
1-11 are the reasoning behind each deferral; they still need to be copied
into `docs/PHASE4-PLAN.md`'s Track E handoff section when one is written
(not done as part of this track — `PHASE4-PLAN.md` wasn't touched here).

Baseline after this track: 538 backend + 234 frontend tests, all green
(counted 2026-08-14 on `library-browser`; superseded by Track D's
566 + 258 above, which includes this track).
