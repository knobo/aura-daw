#!/usr/bin/env python3
"""AURA sidecar: Demucs stem separation.

Standalone CLI spoken to by the Rust job runner (src-tauri/src/sidecars/)
over the NDJSON stdout protocol (docs/ARCHITECTURE.md §6):

    {"type":"progress","progress":0.42,"stage":"separating"}
    {"type":"done","result":{"kind":"stemSplit","stems":{...},"elapsedSeconds":...}}
    {"type":"error","message":"..."}

Usage:
    demucs_split.py --input song.wav --out-dir DIR [--model htdemucs]
                    [--stems vocals,drums] [--two-stems vocals] [--simulate]

Requires `pip install demucs` for real runs (first run downloads the model).
`--simulate` produces fake progress + silent stem WAVs so the full pipeline
is demoable without the model.
"""

import argparse
import json
import os
import sys
import time
import traceback

ALL_STEMS = ["drums", "bass", "other", "vocals"]

# Keep a private handle on the real stdout for protocol lines, and point
# sys.stdout at stderr so stray library prints can't corrupt the NDJSON.
_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr


def emit(obj):
    _NDJSON.write(json.dumps(obj) + "\n")
    _NDJSON.flush()


def progress(fraction, stage):
    emit({"type": "progress", "progress": round(max(0.0, min(1.0, fraction)), 4),
          "stage": stage})


def fail(message, code=1):
    emit({"type": "error", "message": message})
    sys.exit(code)


def parse_args(argv=None):
    p = argparse.ArgumentParser(description="AURA Demucs stem-split sidecar")
    p.add_argument("--input", required=True, help="source audio file (wav/flac/mp3)")
    p.add_argument("--out-dir", required=True, help="output directory for stem WAVs")
    p.add_argument("--model", default="htdemucs", help="demucs model name")
    p.add_argument("--stems", default=None,
                   help="comma-separated subset of vocals,drums,bass,other")
    p.add_argument("--two-stems", default=None, choices=ALL_STEMS,
                   help="demucs two-stems mode (alias for --stems <name>)")
    p.add_argument("--simulate", action="store_true",
                   help="fake progress + silent stem WAVs (no demucs needed)")
    return p.parse_args(argv)


def requested_stems(args):
    if args.two_stems:
        return [args.two_stems]
    if args.stems:
        stems = [s.strip() for s in args.stems.split(",") if s.strip()]
        bad = [s for s in stems if s not in ALL_STEMS]
        if bad:
            fail("invalid stem(s) %s — expected a subset of %s"
                 % (",".join(bad), ",".join(ALL_STEMS)))
        return stems or ALL_STEMS
    return list(ALL_STEMS)


def write_silent_wav(path, seconds=1.0, sample_rate=44100, channels=2):
    import wave
    with wave.open(path, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(b"\x00\x00" * channels * int(sample_rate * seconds))


def run_simulate(args, stems, t0):
    progress(0.02, "loading model")
    time.sleep(0.2)
    for i in range(1, 9):
        time.sleep(0.12)
        progress(0.05 + 0.80 * i / 8.0, "separating")
    progress(0.9, "writing stems")
    os.makedirs(args.out_dir, exist_ok=True)
    produced = {}
    for name in stems:
        path = os.path.abspath(os.path.join(args.out_dir, name + ".wav"))
        write_silent_wav(path)
        produced[name] = path
    emit({"type": "done",
          "result": {"kind": "stemSplit", "stems": produced,
                     "elapsedSeconds": round(time.monotonic() - t0, 2)}})


def run_real(args, stems, t0):
    progress(0.02, "loading model")
    try:
        from demucs.api import Separator, save_audio  # noqa: import check
    except ImportError:
        fail("demucs is not installed — install it with: pip install demucs "
             "(first run also downloads the '%s' model)" % args.model)

    state = {"last": 0.0}

    def callback(data):
        # demucs.api progress callback: coarse fraction over bag models.
        try:
            models = max(1, int(data.get("models", 1) or 1))
            model_idx = int(data.get("model_idx_in_bag", 0) or 0)
            length = max(1, int(data.get("audio_length", 1) or 1))
            offset = int(data.get("segment_offset", 0) or 0)
            frac = (model_idx + min(offset / length, 1.0)) / models
            frac = 0.05 + 0.80 * max(0.0, min(1.0, frac))
            if frac - state["last"] >= 0.01:  # keep well under 10 Hz
                state["last"] = frac
                progress(frac, "separating")
        except Exception:
            pass

    try:
        separator = Separator(model=args.model, callback=callback)
        progress(0.05, "separating")
        _origin, separated = separator.separate_audio_file(args.input)
        progress(0.90, "writing stems")
        os.makedirs(args.out_dir, exist_ok=True)
        produced = {}
        for name, tensor in separated.items():
            if name not in stems:
                continue
            path = os.path.abspath(os.path.join(args.out_dir, name + ".wav"))
            save_audio(tensor, path, samplerate=separator.samplerate)
            produced[name] = path
        missing = [s for s in stems if s not in produced]
        if missing:
            fail("model '%s' did not produce requested stem(s): %s"
                 % (args.model, ", ".join(missing)))
        emit({"type": "done",
              "result": {"kind": "stemSplit", "stems": produced,
                         "elapsedSeconds": round(time.monotonic() - t0, 2)}})
    except SystemExit:
        raise
    except Exception as e:
        traceback.print_exc(file=sys.stderr)
        fail("demucs separation failed: %s" % e)


def main(argv=None):
    args = parse_args(argv)
    t0 = time.monotonic()
    if not os.path.isfile(args.input):
        fail("input file not found: %s" % args.input)
    stems = requested_stems(args)
    if args.simulate:
        run_simulate(args, stems, t0)
    else:
        run_real(args, stems, t0)


if __name__ == "__main__":
    main()
