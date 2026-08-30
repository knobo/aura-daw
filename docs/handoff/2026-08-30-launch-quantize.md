# Handoff — launch quantize (#141), and what was left open

**Written 2026-08-30, mid-session, for whoever picks this up next.**

## State of the work

* **PR #137 (Plan V — V3: polyphony) is MERGED** (`42fe769`). Voice cap,
  choke groups, quantized start, velocity. Rulings V-18…V-21 and its four
  known gaps are in [`../backlog/plan-v-players.md`](../backlog/plan-v-players.md).
* **PR #141 (`feat/launch-quantize`) is OPEN, ready for review, mergeable,
  CI green.** Worktree: `.worktrees/launch-quantize`. Nothing is
  half-finished in it — suite, docs, counts and the perf pair are all
  done. It needs a reviewer and a merge, not more work.
* The claim row for #141 was already removed from `next-prompt.md` (it
  went in with the docs commit). The PR is the live record until it
  merges.

## The one thing that is genuinely unfinished

**A pad fires far more often than it looks pressed.** Full write-up,
evidence and the ruled-out list: the OPEN section at the top of
[`../backlog/midi-launch.md`](../backlog/midi-launch.md).

It is blocked on ONE question to the owner, and asking it is the first
thing to do:

> In the ten-second burst at 18:02:50–18:03:00, did you press the pad
> about 40 times, or about 5?

Do not start reading code before that answer. The two answers lead to two
completely different jobs — a small behaviour change in `fire_at` on one
hand, a frontend instrumentation hunt for a self-retrigger on the other.

## Owner ear-checks still owed

Neither #137's nor #141's ear-check has been completed; the owner
confirmed only that the `Q` chip appears on a launcher pad. Both lists are
in their backlog files. The two that no test can stand in for:

* a choke group of two on a sample with body — listen for a CLICK on the
  cut (`ClockTable::stop` is a hard cut; a click is a real finding, and
  the fix is a release ramp);
* quantize 1/4 pressed off the beat against a metronome — listen for a
  FLAM (the press is block-accurate, ~5 ms worst case at 512/48k).

## Two findings worth not losing

**The MCP surface has no Plan V tools at all.** The roster is
`add_track`, `create_project`, `import_audio_clip`, `set_track_mix`,
`transport_control`, `record_take`, `run_sidecar_job`,
`get_project_state`, `read_meters`. Nothing for players, launch bindings,
pads, choke groups or the surface — so an agent cannot set up a pad test
for the owner, which is exactly what was asked for and could not be done.

**Nothing turns WAVs you already have into an instrument.** `audio/sampler.rs`
is a working SFZ-subset sampler (key/velocity zones, loop points, amp
envelope, 64 voices) and `sidecars/stable_audio_sfz_worker.py` generates an
instrument from a text PROMPT — but there is no path from existing files.
Verified by searching for `build_sfz`, `sfz_from`, `instrument_from_clip`:
none exist.

## Where the owner's thinking was heading

He observed — correctly — that what he actually wants is: a pad plays a
MIDI note that gets RECORDED onto a track, and a knob writes an
AUTOMATION curve. That is V6 (with V5 for the knobs), already specced.
His own read was that he had built the deck before the bridge to the
arrangement, which is fair: today nothing played on the deck is kept
(V-15 keeps players out of the bounce, and V6 does not exist).

The recommendation given, **not decided and not claimed**:

1. a small "make an instrument from these WAVs" cut first — it uses the
   sampler and the frozen SFZ subset that already exist, and it is what
   makes "build a synth from a clip" true;
2. then the main line, V4 → V6.

He was asked whether to write both up as backlog rows and had not
answered when the session ended.
