#!/usr/bin/env python3
"""AURA sidecar: vocal transcription via whisper.cpp (+ naive pitch track).

Wraps the whisper.cpp CLI (`whisper-cli`, or $WHISPER_CPP_BIN) and normalizes
its output to the NDJSON stdout protocol (docs/ARCHITECTURE.md §6) with the
`transcribeResult` shape from docs/ipc-schemas/sidecar-job.schema.json:

    {"type":"progress","progress":0.42,"stage":"transcribing"}
    {"type":"done","result":{"kind":"transcribe","language":"en",
                             "segments":[{"startSeconds":..,"endSeconds":..,
                                          "text":"..","words":[...]}],
                             "transcriptPath":"/abs/....json"}}
    {"type":"error","message":"..."}

Usage:
    whisper_transcribe.py --input vocal.wav --model ggml-base.en.bin
                          [--language en] [--with-pitch]
                          [--output-json /path/transcript.json] [--simulate]

`--with-pitch` derives a naive per-word median-f0 (MIDI-ready) pitch via
librosa (pyin) or aubio when available; otherwise pitch is marked unavailable
(pitchHz = null) and a log line says how to enable it.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import traceback

# Private handle on the real stdout for protocol lines; stray prints → stderr.
_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr

WHISPER_BIN_CANDIDATES = ["whisper-cli", "whisper-cpp", "whisper.cpp", "whisper"]
TARGET_SR = 16000


def emit(obj):
    _NDJSON.write(json.dumps(obj) + "\n")
    _NDJSON.flush()


def progress(fraction, stage):
    emit({"type": "progress", "progress": round(max(0.0, min(1.0, fraction)), 4),
          "stage": stage})


def log(line):
    emit({"type": "log", "line": line})


def fail(message, code=1):
    emit({"type": "error", "message": message})
    sys.exit(code)


def parse_args(argv=None):
    p = argparse.ArgumentParser(description="AURA whisper.cpp transcription sidecar")
    p.add_argument("--input", required=True, help="source audio file")
    p.add_argument("--model", default="ggml-base.en.bin",
                   help="whisper.cpp GGML/GGUF model file name or path")
    p.add_argument("--language", default=None, help="ISO-639-1 hint, e.g. en/no")
    p.add_argument("--with-pitch", action="store_true",
                   help="derive per-word median f0 (librosa/aubio if available)")
    p.add_argument("--output-json", default=None,
                   help="where to write the transcript JSON "
                        "(default: <input dir>/<name>.transcript.json)")
    p.add_argument("--simulate", action="store_true",
                   help="fake transcription (no whisper.cpp needed)")
    return p.parse_args(argv)


def transcript_path(args):
    if args.output_json:
        return os.path.abspath(args.output_json)
    base = os.path.splitext(os.path.basename(args.input))[0]
    return os.path.abspath(os.path.join(os.path.dirname(args.input),
                                        base + ".transcript.json"))


# ---------------------------------------------------------------------------
# Tool / model resolution
# ---------------------------------------------------------------------------

def find_whisper_bin():
    env = os.environ.get("WHISPER_CPP_BIN")
    if env:
        if os.path.isfile(env) and os.access(env, os.X_OK):
            return env
        fail("WHISPER_CPP_BIN=%r is not an executable file" % env)
    for name in WHISPER_BIN_CANDIDATES:
        path = shutil.which(name)
        if path:
            return path
    fail("whisper.cpp binary not found on PATH (looked for: %s). Build "
         "whisper.cpp and put `whisper-cli` on PATH, or set WHISPER_CPP_BIN."
         % ", ".join(WHISPER_BIN_CANDIDATES))


def find_model(model):
    candidates = [model]
    if not os.path.isabs(model):
        for base in [os.environ.get("WHISPER_MODEL_DIR"),
                     os.path.expanduser("~/.cache/whisper"),
                     os.path.dirname(os.path.abspath(__file__))]:
            if base:
                candidates.append(os.path.join(base, model))
    for c in candidates:
        if c and os.path.isfile(c):
            return os.path.abspath(c)
    fail("whisper model %r not found (tried: %s). Download a GGML model, e.g. "
         "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin, "
         "and pass its path or set WHISPER_MODEL_DIR." % (model, ", ".join(candidates)))


# ---------------------------------------------------------------------------
# Audio preparation (whisper.cpp wants 16 kHz mono 16-bit WAV)
# ---------------------------------------------------------------------------

def prepare_audio(input_path, tmp_dir):
    """Return path to a 16 kHz mono s16 WAV (ffmpeg, else pure-python)."""
    out = os.path.join(tmp_dir, "whisper-input.wav")
    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg:
        r = subprocess.run(
            [ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-i", input_path,
             "-ar", str(TARGET_SR), "-ac", "1", "-sample_fmt", "s16", out],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        if r.returncode != 0:
            fail("ffmpeg failed to convert input: %s"
                 % r.stderr.decode(errors="replace").strip()[-500:])
        return out
    # Pure-python fallback: 16-bit PCM WAV input only.
    import wave, array
    try:
        with wave.open(input_path, "rb") as w:
            if w.getsampwidth() != 2:
                fail("input is not 16-bit PCM WAV and ffmpeg is not installed — "
                     "install ffmpeg to transcribe this file")
            channels, sr, n = w.getnchannels(), w.getframerate(), w.getnframes()
            raw = w.readframes(n)
    except wave.Error as e:
        fail("cannot read input (%s) and ffmpeg is not installed — "
             "install ffmpeg for non-WAV input" % e)
    samples = array.array("h")
    samples.frombytes(raw)
    if sys.byteorder == "big":
        samples.byteswap()
    # Downmix to mono.
    if channels > 1:
        mono = array.array("h", (0 for _ in range(len(samples) // channels)))
        for i in range(len(mono)):
            acc = 0
            base = i * channels
            for c in range(channels):
                acc += samples[base + c]
            mono[i] = acc // channels
        samples = mono
    # Naive linear resample.
    if sr != TARGET_SR:
        ratio = sr / TARGET_SR
        out_len = int(len(samples) / ratio)
        res = array.array("h", (0 for _ in range(out_len)))
        for i in range(out_len):
            pos = i * ratio
            j = int(pos)
            frac = pos - j
            a = samples[j]
            b = samples[j + 1] if j + 1 < len(samples) else a
            res[i] = int(a + (b - a) * frac)
        samples = res
    with wave.open(out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(TARGET_SR)
        w.writeframes(samples.tobytes())
    return out


# ---------------------------------------------------------------------------
# whisper.cpp invocation + output normalization
# ---------------------------------------------------------------------------

PROGRESS_RE = re.compile(r"progress\s*=\s*(\d+)%")
SPECIAL_TOKEN_RE = re.compile(r"^\[_.*_\]$")


def run_whisper(binary, model, wav_path, language, tmp_dir):
    prefix = os.path.join(tmp_dir, "whisper-out")
    cmd = [binary, "-m", model, "-f", wav_path,
           "--output-json-full", "-of", prefix, "--print-progress"]
    if language:
        cmd += ["-l", language]
    progress(0.25, "transcribing")
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                            stderr=subprocess.PIPE, text=True,
                            errors="replace")
    tail = []
    for line in proc.stderr:
        line = line.rstrip("\n")
        tail.append(line)
        tail[:] = tail[-20:]
        m = PROGRESS_RE.search(line)
        if m:
            pct = int(m.group(1))
            progress(0.25 + 0.60 * min(pct, 100) / 100.0, "transcribing")
    proc.wait()
    if proc.returncode != 0:
        fail("whisper.cpp failed (exit %d): %s"
             % (proc.returncode, "\n".join(tail[-5:])))
    json_path = prefix + ".json"
    if not os.path.isfile(json_path):
        fail("whisper.cpp did not produce JSON output (%s missing)" % json_path)
    with open(json_path, "r", encoding="utf-8", errors="replace") as f:
        return json.load(f)


def tokens_to_words(tokens):
    """Merge whisper.cpp full-JSON tokens into word entries."""
    words = []
    cur = None
    for tok in tokens or []:
        text = tok.get("text", "")
        if SPECIAL_TOKEN_RE.match(text.strip()):
            continue
        offsets = tok.get("offsets") or {}
        t_from = offsets.get("from", 0) / 1000.0
        t_to = offsets.get("to", 0) / 1000.0
        starts_word = text.startswith(" ") or cur is None
        if starts_word:
            if cur and cur["word"]:
                words.append(cur)
            cur = {"word": text.strip(), "startSeconds": round(t_from, 3),
                   "endSeconds": round(t_to, 3)}
        else:
            cur["word"] += text
            cur["endSeconds"] = round(t_to, 3)
    if cur and cur["word"]:
        words.append(cur)
    return words


def normalize_output(raw, with_words):
    language = ((raw.get("result") or {}).get("language")
                or (raw.get("params") or {}).get("language") or "auto")
    segments = []
    for seg in raw.get("transcription") or []:
        offsets = seg.get("offsets") or {}
        entry = {
            "startSeconds": round(offsets.get("from", 0) / 1000.0, 3),
            "endSeconds": round(offsets.get("to", 0) / 1000.0, 3),
            "text": (seg.get("text") or "").strip(),
        }
        if with_words:
            entry["words"] = tokens_to_words(seg.get("tokens"))
        segments.append(entry)
    return language, segments


# ---------------------------------------------------------------------------
# Naive pitch (median f0 per word → MIDI-ready)
# ---------------------------------------------------------------------------

def load_f0_track(wav_path):
    """Return (times, f0) arrays or None if no pitch backend is available."""
    try:
        import numpy as np
        import librosa
        y, sr = librosa.load(wav_path, sr=TARGET_SR, mono=True)
        f0, _voiced, _prob = librosa.pyin(
            y, fmin=65.0, fmax=1000.0, sr=sr, frame_length=2048)
        times = librosa.times_like(f0, sr=sr, hop_length=512)
        return times.tolist(), [None if (v is None or np.isnan(v)) else float(v)
                                for v in f0]
    except ImportError:
        pass
    except Exception as e:
        log("librosa pitch extraction failed: %s" % e)
    try:
        import numpy as np
        import aubio
        import wave, array
        with wave.open(wav_path, "rb") as w:
            sr = w.getframerate()
            raw = w.readframes(w.getnframes())
        samples = array.array("h")
        samples.frombytes(raw)
        buf = np.asarray(samples, dtype=np.float32) / 32768.0
        hop = 256
        det = aubio.pitch("yin", 1024, hop, sr)
        det.set_unit("Hz")
        det.set_tolerance(0.8)
        times, f0 = [], []
        for i in range(0, len(buf) - hop, hop):
            hz = float(det(buf[i:i + hop])[0])
            conf = det.get_confidence()
            times.append(i / sr)
            f0.append(hz if hz > 0 and conf > 0.5 else None)
        return times, f0
    except ImportError:
        return None
    except Exception as e:
        log("aubio pitch extraction failed: %s" % e)
        return None


def hz_to_midi(hz):
    import math
    return 69.0 + 12.0 * math.log2(hz / 440.0)


def annotate_pitch(segments, wav_path):
    """Attach pitchHz (median f0) to every word; also build a note track."""
    track = load_f0_track(wav_path)
    if track is None:
        log("pitch unavailable — install librosa (pip install librosa) or "
            "aubio for vocal-to-MIDI; words carry pitchHz: null")
        for seg in segments:
            for w in seg.get("words") or []:
                w["pitchHz"] = None
        return None
    times, f0 = track
    notes = []
    for seg in segments:
        for w in seg.get("words") or []:
            voiced = [f for t, f in zip(times, f0)
                      if f is not None and w["startSeconds"] <= t <= w["endSeconds"]]
            if voiced:
                voiced.sort()
                median = voiced[len(voiced) // 2]
                w["pitchHz"] = round(median, 2)
                notes.append({"startSeconds": w["startSeconds"],
                              "endSeconds": w["endSeconds"],
                              "midiNote": int(round(hz_to_midi(median))),
                              "pitchHz": round(median, 2),
                              "word": w["word"]})
            else:
                w["pitchHz"] = None
    return notes


# ---------------------------------------------------------------------------
# Simulate mode
# ---------------------------------------------------------------------------

def run_simulate(args, t0):
    progress(0.1, "loading model")
    time.sleep(0.15)
    progress(0.3, "preparing audio")
    time.sleep(0.15)
    for i in range(1, 6):
        time.sleep(0.1)
        progress(0.3 + 0.55 * i / 5.0, "transcribing")
    words1 = [
        {"word": "hello", "startSeconds": 0.32, "endSeconds": 0.61, "pitchHz": 220.0},
        {"word": "aura", "startSeconds": 0.70, "endSeconds": 1.10, "pitchHz": 246.94},
    ]
    words2 = [
        {"word": "singing", "startSeconds": 1.52, "endSeconds": 2.01, "pitchHz": 293.66},
        {"word": "along", "startSeconds": 2.10, "endSeconds": 2.55, "pitchHz": None},
    ]
    if not args.with_pitch:
        for w in words1 + words2:
            w.pop("pitchHz", None)
    segments = [
        {"startSeconds": 0.32, "endSeconds": 1.10, "text": "hello aura", "words": words1},
        {"startSeconds": 1.52, "endSeconds": 2.55, "text": "singing along", "words": words2},
    ]
    finish(args, "en", segments,
           notes=[{"startSeconds": 0.32, "endSeconds": 0.61, "midiNote": 57,
                   "pitchHz": 220.0, "word": "hello"}] if args.with_pitch else None,
           t0=t0)


def finish(args, language, segments, notes, t0):
    progress(0.95, "writing transcript")
    result = {"kind": "transcribe", "language": language, "segments": segments}
    out_path = transcript_path(args)
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    doc = dict(result)
    doc["sourcePath"] = os.path.abspath(args.input)
    doc["elapsedSeconds"] = round(time.monotonic() - t0, 2)
    if notes is not None:
        doc["noteTrack"] = notes  # naive vocal-to-MIDI alignment (cache only)
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=2)
    result["transcriptPath"] = out_path
    emit({"type": "done", "result": result})


def run_real(args, t0):
    binary = find_whisper_bin()
    model = find_model(args.model)
    progress(0.05, "loading model")
    tmp_dir = tempfile.mkdtemp(prefix="aura-whisper-")
    try:
        progress(0.1, "preparing audio")
        wav = prepare_audio(args.input, tmp_dir)
        raw = run_whisper(binary, model, wav, args.language, tmp_dir)
        progress(0.88, "aligning")
        language, segments = normalize_output(raw, with_words=True)
        notes = None
        if args.with_pitch:
            progress(0.9, "estimating pitch")
            notes = annotate_pitch(segments, wav)
        finish(args, language, segments, notes, t0)
    except SystemExit:
        raise
    except Exception as e:
        traceback.print_exc(file=sys.stderr)
        fail("transcription failed: %s" % e)
    finally:
        shutil.rmtree(tmp_dir, ignore_errors=True)


def main(argv=None):
    args = parse_args(argv)
    t0 = time.monotonic()
    if not os.path.isfile(args.input):
        fail("input file not found: %s" % args.input)
    if args.simulate:
        run_simulate(args, t0)
    else:
        run_real(args, t0)


if __name__ == "__main__":
    main()
