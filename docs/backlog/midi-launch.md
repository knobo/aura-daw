# MIDI launch map

**v0.1 is on `main`** (PR #42 squash `92f0d03`, follow-up PR #50
`1b38821`: play-from-0, overlay all-notes-off, Delete only on the
panel). Mark regions or clips,
bind them to notes, fire them from hardware or from a MIDI clip that
*uses* a named launcher. Drive playback does not steal the arrangement
loop or playhead (shadow playhead). GATE vs ONE-SHOT per launcher.
Piano-roll **▶ play** runs that clip once. Clips whose track is gone
do not drive.

User-facing: `docs/midi-input.md` § Launch. Trace: `AURA_LAUNCH_TRACE=1`.

## RESOLVED — a pad fires far more often than it looks pressed (2026-08-30, closed 2026-09-01)

**Owner's answer: ~40.** He confirmed pressing that fast is expected, and
asked for the collapse-not-queue behaviour the "~40" branch below already
named as the fix. Landed: `ClockTable::fire_at` (`src-tauri/src/audio/clock.rs`)
now no-ops a press against a clock that is already armed (`pending_at !=
NO_PENDING`) instead of overwriting `start`/`end`/`looping`/`gain`/`at` —
one gate, shared by both `fire_scene` and `player_fire_with_velocity`
since both call through `fire_at`. A double-tap ahead of the boundary is
dropped, not queued and not re-armed; the first press of an arm-cycle
still wins. Test: `a_repeat_press_on_an_armed_pad_is_dropped` in
`clock.rs`. Found while ear-checking #141.

### What the owner reported

Pressing a quantized launcher pad repeatedly "spreads the presses out over
time": the pad keeps sounding after he stops pressing, other controls
(stop) do not respond meanwhile, and Escape has to be pressed several
times to get quiet.

### What the trace actually shows

Run with `AURA_LAUNCH_TRACE=1`. Three bursts, 75 `launch: fire` lines
total, every one of them the SAME binding and every one `origin=Preview`
— the UI's own path (`launch.preview` → `launch_fire` → `launch_preview`),
not the drive thread's. The third burst:

```
18:02:50  3 fires      18:02:56  4 fires
18:02:51  4 fires      18:02:57  3 fires
18:02:52  3 fires      18:02:58  4 fires
18:02:53  4 fires      18:02:59  4 fires
18:02:54  4 fires      18:03:00  3 fires
18:02:55  4 fires
```

Ten seconds, a steady 3–4 per second, no gap. Only 5 `drive stop` lines in
the whole log.

**The coincidence that has to be resolved:** the bound clip is 12 679
samples = **0.26 s**, i.e. 3.8 per second — exactly the fire rate. Either
the owner clicked in time with what he heard, or something re-fires each
time the scene ends.

### The question that decides it

**In that ten-second burst, did the owner press ~40 times or ~5?**

* **~40** — the backend did exactly what it was asked. What he is hearing
  is that each press restarts a 0.26 s scene while quantize collapses the
  presses onto beats, so the sound lags the finger. Not a bug, but not good
  behaviour either: the fix is that pressing a pad whose fire is already
  ARMED should be a no-op rather than a re-arm (`fire_at` currently
  overwrites, which is right for a retrigger and wrong for a double-tap).
* **~5** — something re-fires itself, and it is a real bug. Almost
  certainly PRE-EXISTING: `origin=Preview` is the UI path, and #141 changed
  nothing in it.

### What has already been ruled out

Read and found clean — do not re-read these first:

* `Pad.svelte`'s press path: `pointerdown` fires once, and
  `firedFromPointer` swallows the synthetic `click` that follows. No
  double-fire, no repeat.
* `SurfacePanel.pressPad` → `surface.fireBinding` / `fireClip` →
  `launch.preview`: one call per press, no queue, no retry.
* `launch://fired`'s handler in `launch.svelte.ts`: sets `overlay`, never
  fires anything.
* Every other caller of `launch.preview` (`LaunchMapPanel`'s row
  double-click and play button) — no loop.
* The Rust side: one `launch: fire` per IPC call, no queue, and `fire_at`
  overwrites a pending fire rather than stacking one.

**If the answer is ~5, the next step is instrumenting the FRONTEND** (log
every `preview` call with a stack), because nothing on the Rust side can
produce a call the UI did not make.

### A second thing the trace showed, worth its own look

The owner's two pads — "Drum 1" and "symbal" — both borrow the SAME track
(`9ab20e15`). Two scenes over one track is last-writer-wins by V-14, so
they tear the track away from each other on every press. That is
as-designed, but it may be half of what "I cannot press anything else"
feels like.

## Launch quantize — landed 2026-08-30 (PR #141)

A binding carries a `quantize` division and fires on that boundary of the
arrangement's grid instead of on the press. Same field, same wire form and
same chip as a player pad's (`audio::player::Quantize`), deliberately one
type rather than two: a deck has pads of both kinds side by side and
"Q 1/4" has to mean the same thing on either.

The clock machinery came from Plan V's V3 (PR #137) unchanged — `fire_at`
and `arm_pending` live in `ClockTable` and were never bound to the player
range — so this cut is entirely about the SCENE path, and about the two
places where "off" used to be the same question as "over":

* **`GraphTables::release_finished_scenes`** handed a waiting scene's
  borrowed tracks back, because a clock armed for a beat is off. The fire
  then started with nothing bound to it: a pad that lights up and sounds
  nothing.
* **the drive thread's release edge** (`launch.rs`, the 8 ms poll) read the
  same "off" as an ending and announced — and cut — a scene before it had
  begun.

Both now ask `ClockTable::is_live` (running *or* waiting on a beat), which
is the rename of V3's `holds_voice`; the voice cap asks the same question
for its own reason.

**The tracks are bound at PRESS time even though the fire waits**, and that
is deliberate rather than an oversight: a slot on a scene clock that is off
falls back to the arrangement (`mixer::node_playhead`'s fourth case), so the
borrowed tracks keep playing the song until the beat arrives and the binding
is already in place when it does. Binding from `arm_pending` instead is not
available — that runs on the audio thread, and which slots a scene names is
a control-side map.

**The migrated-binding trap.** V2 migrated launch bindings onto players, and
`launch_fire_from` returns early into `player_fire` for such a binding — so
for a `player`-targeted binding the division that governs the press is the
PLAYER's, and writing the binding's own field there sets a value nothing
reads. The surface chip resolves through the binding's target for exactly
this reason (`SurfacePanel.padQuantize` / `cyclePadQuantize`), and a DOM
test pins it.

**Deliberately NOT in scope: choke and velocity on a scene.** A scene
borrows real arrangement tracks, so a choke would cut them mid-song and a
velocity would duck them for as long as the scene ran. Both are different
features from the pad ones that share their names, and neither has been
asked for. Note that the mixer's velocity multiply already reads any
non-transport clock's gain, so a future scene velocity would need no engine
work — only a decision.

**Known gaps.**

- `LaunchFired { playing: true }` is emitted at the PRESS, not at the beat,
  so the launcher lights up while the scene is still waiting. Emitting on
  the beat needs a signal back from the audio thread (`arm_pending` runs
  there); an "armed" state distinct from "playing" is the UI half of the
  same thing. Neither is built.
- Block-accurate, not sample-accurate, exactly as a player's press is
  (V-21): ~5 ms worst case at 512 frames / 48 kHz.
- The chip is per-binding. A GLOBAL launch quantize — one setting for the
  whole launcher, which is how Ableton frames it — would sit on `LaunchMap`
  beside `play_mode`, and nothing here forecloses it.

## Not in v0.1 — next

### Hardware GATE

Drive clips honour GATE (note-off cuts the scene) vs ONE-SHOT (plays to
the marked end). Hardware pads do **not**: a matching note-on seeks,
loops, and plays the region; note-off only clears the held-pad debounce.
Hardware GATE (hold the pad to keep the scene, lift to stop) is not in
v0.1.

### Sustain (overlapping voices) — SUPERSEDED by Plan V, cut V3

**Do not implement this here.** As of 2026-08-26 overlapping voices are
V3 of [`plan-v-players.md`](plan-v-players.md), because the same single
atomic set (`launch_on`/`launch_pos` in `audio/rt.rs`) is what blocks a
polyphonic pad deck, a per-player automation clock and everything else the
owner asked the control surface for. Plan V **retires** this mechanism
rather than adding a third play mode to it: see
[the design's §2](../superpowers/specs/2026-08-26-plan-v-players-design.md)
for the four properties (one playhead, transport hijack on hardware fire,
MIDI-only clip target, automation read on the transport's clock) that make
it a prototype. The text below is kept because the *behaviour* it describes
is still what V3 must deliver.

Today ONE-SHOT restarts the single shadow playhead, and GATE cuts on
note-off. There is no way to trigger the same scene again and let the
previous one ring out.

**Sustain:** each trigger starts a *new* voice. The previous one keeps
playing to its marked end. Notes can overlap — a roll of kicks, or
stacked scene hits, without cutting the earlier one.

This is a third play mode on the launcher (GATE / ONE-SHOT / SUSTAIN),
not a replacement. Implementation sketch: N concurrent overlay
playheads (small fixed cap, not one `launch_pos`), mixer sums launched
tracks at each voice's position. Cut-off when the cap is hit: oldest
voice dies first. Hardware pads and drive clips share the same mode.

Do **not** start this until v0.1 has been used for a bit. The
single-voice overlay is the whole of the current engine hook.
