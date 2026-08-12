#!/usr/bin/env python3
"""AURA sidecar: Anticipatory Music Transformer MIDI infilling
(phase 2, zone C).

Job kind: amtInfill.

    amt_infill_worker.py --params-json '{...}' [--simulate]

Params: docs/ipc-schemas/sidecar-job-v2.schema.json $defs amtInfillParams —
context notes + the tick region to infill, all musical positions in TICKS at
the given ppq (never seconds/samples; debt D-02 rules apply to sidecars too).
The Rust caller additionally passes `bpm` (additive field, D-06) so this
worker can convert ticks <-> the model's seconds domain; default 120.
Result: $defs amtInfillResult ({"kind":"amtInfill","notes":[MidiNote...]}),
notes relative to the clip start, same shape as midi-clip.schema.json.
Merging into the clip happens Rust-side (midi::amt::merge_infill) — this
worker only proposes notes for the region.

Real mode requires the Anticipatory Music Transformer stack (CPU is fine):

    pip install anticipation torch transformers mido

The checkpoint (default stanford-crfm/music-medium-800k, override with the
AURA_AMT_MODEL env var) is downloaded from Hugging Face on first use.
`--simulate` produces a plausible, fully deterministic arpeggio instead.
"""

import argparse
import json
import os
import sys
import tempfile
import time
import traceback

_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr

INSTALL_HINT = (
    "AMT infilling needs the Anticipatory Music Transformer stack: "
    "`pip install anticipation torch transformers mido` (CPU works; the "
    "checkpoint downloads from Hugging Face on first run — default "
    "stanford-crfm/music-medium-800k, override via AURA_AMT_MODEL). "
    "For dev/CI without models use --simulate / AURA_SIDECAR_SIMULATE=1."
)

DEFAULT_MODEL = "stanford-crfm/music-medium-800k"


def emit(obj):
    _NDJSON.write(json.dumps(obj) + "\n")
    _NDJSON.flush()


def fail(message):
    emit({"type": "error", "message": message})
    return 1


def parse_params(params):
    """Validate amtInfillParams; returns (ppq, bpm, context, start, end,
    temperature, top_p) or raises ValueError."""
    ppq = int(params.get("ppq", 960))
    bpm = float(params.get("bpm", 120.0))
    start = int(params.get("regionStartTicks", -1))
    end = int(params.get("regionEndTicks", -1))
    context = params.get("contextNotes", [])
    if ppq < 24:
        raise ValueError(f"ppq {ppq} out of range")
    if not (bpm > 0):
        raise ValueError(f"bpm {bpm} out of range")
    if start < 0 or end <= start:
        raise ValueError(
            f"invalid region [{start}, {end}) — need 0 <= start < end")
    if not isinstance(context, list):
        raise ValueError("contextNotes must be an array")
    temperature = float(params.get("temperature", 1.0))
    top_p = float(params.get("topP", 0.98))
    return ppq, bpm, context, start, end, temperature, top_p


# ---------------------------------------------------------------------------
# Simulate mode: deterministic eighth-note arpeggio filling the region
# ---------------------------------------------------------------------------

def run_simulate(ppq, start, end):
    emit({"type": "progress", "progress": 0.3, "stage": "loadingModel"})
    time.sleep(0.02)
    emit({"type": "progress", "progress": 0.7, "stage": "sampling"})
    step = max(1, ppq // 2)
    pattern = [0, 4, 7, 12]
    notes = []
    t, i = start, 0
    while t < end:
        notes.append({
            "tick": t,
            "lengthTicks": max(1, min(step, end - t)),
            "key": 60 + pattern[i % 4],
            "velocity": 96,
            "channel": 0,
        })
        i += 1
        t += step
    emit({"type": "done", "result": {
        "kind": "amtInfill", "ppq": ppq, "notes": notes, "simulated": True,
    }})
    return 0


# ---------------------------------------------------------------------------
# Real mode: anticipation library
# ---------------------------------------------------------------------------

def context_to_midifile(mido, context, ppq, bpm, path):
    """Write the context notes to a .mid file at (ppq, bpm)."""
    mid = mido.MidiFile(ticks_per_beat=ppq)
    track = mido.MidiTrack()
    mid.tracks.append(track)
    track.append(mido.MetaMessage(
        "set_tempo", tempo=int(round(60_000_000 / bpm)), time=0))
    edges = []  # (tick, order, msg-kind, key, vel, ch)
    for n in context:
        tick = int(n["tick"])
        length = max(1, int(n["lengthTicks"]))
        key = int(n["key"])
        vel = max(1, min(127, int(n["velocity"])))
        ch = int(n.get("channel", 0)) % 16
        edges.append((tick, 1, key, vel, ch))
        edges.append((tick + length, 0, key, 0, ch))
    edges.sort(key=lambda e: (e[0], e[1]))
    prev = 0
    for tick, order, key, vel, ch in edges:
        delta = tick - prev
        prev = tick
        kind = "note_on" if order == 1 else "note_off"
        track.append(mido.Message(
            kind, note=key, velocity=vel, channel=ch, time=delta))
    mid.save(path)


def midifile_to_notes(mido, path, ppq, bpm):
    """Parse a .mid file into MidiNote dicts at our (ppq, bpm), robust to
    whatever internal division/tempo the anticipation encoder used (walks the
    file in seconds)."""
    mid = mido.MidiFile(path)
    tpb = mid.ticks_per_beat or 480
    sec_per_file_tick = 500_000 / (tpb * 1e6)  # MIDI default 120 bpm
    now_s = 0.0
    pending = {}  # (ch, key) -> [ (start_s, vel) ]
    notes = []

    def sec_to_tick(s):
        return int(round(s * bpm * ppq / 60.0))

    for msg in mido.merge_tracks(mid.tracks):
        now_s += msg.time * sec_per_file_tick
        if msg.type == "set_tempo":
            sec_per_file_tick = msg.tempo / (tpb * 1e6)
        elif msg.type == "note_on" and msg.velocity > 0:
            pending.setdefault((msg.channel, msg.note), []).append(
                (now_s, msg.velocity))
        elif msg.type in ("note_off", "note_on"):
            stack = pending.get((msg.channel, msg.note))
            if stack:
                start_s, vel = stack.pop(0)
                on_t = sec_to_tick(start_s)
                notes.append({
                    "tick": on_t,
                    "lengthTicks": max(1, sec_to_tick(now_s) - on_t),
                    "key": msg.note,
                    "velocity": max(1, min(127, vel)),
                    "channel": msg.channel % 16,
                })
    notes.sort(key=lambda n: (n["tick"], n["key"]))
    return notes


def run_real(params):
    ppq, bpm, context, start, end, _temperature, top_p = params
    try:
        import mido  # noqa: F401
        import torch  # noqa: F401
        from transformers import AutoModelForCausalLM
        from anticipation import ops
        from anticipation.sample import generate
        from anticipation.convert import events_to_midi, midi_to_events
        from anticipation.vocab import CONTROL_OFFSET
    except ImportError as e:
        return fail(f"missing dependency ({e}). {INSTALL_HINT}")

    try:
        def t2s(ticks):
            return ticks * 60.0 / (bpm * ppq)

        start_s, end_s = t2s(start), t2s(end)

        emit({"type": "progress", "progress": 0.05, "stage": "loadingModel"})
        model_name = os.environ.get("AURA_AMT_MODEL", DEFAULT_MODEL)
        model = AutoModelForCausalLM.from_pretrained(model_name)
        model.eval()

        emit({"type": "progress", "progress": 0.35, "stage": "tokenizing"})
        with tempfile.TemporaryDirectory(prefix="aura-amt-") as tmp:
            ctx_mid = os.path.join(tmp, "context.mid")
            context_to_midifile(mido, context, ppq, bpm, ctx_mid)
            events = midi_to_events(ctx_mid) if context else []

            # Condition on the past as history and on the future as
            # anticipated controls, then infill the region in between.
            history = ops.clip(events, 0, start_s, clip_duration=False)
            future = ops.clip(events, end_s, max(
                end_s, ops.max_time(events, seconds=True)), clip_duration=False)
            anticipated = [CONTROL_OFFSET + tok for tok in future]

            emit({"type": "progress", "progress": 0.45, "stage": "sampling"})
            proposal = generate(
                model, start_s, end_s,
                inputs=history, controls=anticipated, top_p=top_p)

            emit({"type": "progress", "progress": 0.85, "stage": "decoding"})
            out_mid = os.path.join(tmp, "proposal.mid")
            mid = events_to_midi(proposal)
            mid.save(out_mid)
            all_notes = midifile_to_notes(mido, out_mid, ppq, bpm)

        # Only the region is this worker's proposal; Rust merges.
        notes = [n for n in all_notes if start <= n["tick"] < end]
        for n in notes:
            n["lengthTicks"] = max(1, min(n["lengthTicks"], end - n["tick"]))

        emit({"type": "done", "result": {
            "kind": "amtInfill", "ppq": ppq, "notes": notes,
            "simulated": False,
        }})
        return 0
    except Exception as e:  # clean NDJSON error, never a stack-trace crash
        tb = traceback.format_exc(limit=3)
        print(tb, file=sys.stderr)
        return fail(f"AMT inference failed: {e!r}. {INSTALL_HINT}")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--params-json", required=True)
    p.add_argument("--simulate", action="store_true")
    args = p.parse_args()
    try:
        raw = json.loads(args.params_json)
    except ValueError as e:
        return fail(f"bad --params-json: {e}")
    try:
        params = parse_params(raw)
    except (ValueError, TypeError, KeyError) as e:
        return fail(f"bad amtInfillParams: {e}")

    ppq, _bpm, _context, start, end, _temp, _top_p = params
    if args.simulate:
        return run_simulate(ppq, start, end)
    return run_real(params)


if __name__ == "__main__":
    sys.exit(main())
