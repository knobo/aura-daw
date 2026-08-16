#!/usr/bin/env python3
"""AURA sidecar: hum/voice recording -> monophonic MIDI melody ("hum-to-song").

    hum_to_midi.py --params-json '{...}' [--simulate]
    hum_to_midi.py --self-test

Params (JSON object; musical positions in TICKS at `ppq`, debt D-02 rules):
    inputPath     absolute path to a WAV recording of a hum/voice (required
                  unless --simulate). PCM 16/24/32 and float32 WAVs are read
                  natively (AURA records float32).
    ppq           ticks per quarter note (default 960).
    bpm           tempo used for seconds -> ticks (default 120).
    quantizeGrid  optional light grid snap: 4 = quarters, 8 = eighths,
                  16 = sixteenths... Onsets snap to the nearest grid line
                  (offsets stay put). Absent/0 = no snapping.
    minNoteMs     drop detected notes shorter than this (default 80).

NDJSON stdout protocol (docs/ARCHITECTURE.md §6):
    {"type":"progress","progress":0.42,"stage":"trackingPitch"}
    {"type":"done","result":{"kind":"humToMidi","ppq":960,"bpm":120,
        "notes":[{"tick":0,"lengthTicks":480,"key":69,"velocity":96,
                  "channel":0,"pitchHz":440.1}, ...],
        "lengthTicks":3840,"stats":{...},"simulated":false}}
    {"type":"error","message":"..."}

`notes` matches the midi-clip.schema.json MidiNote wire shape (tick /
lengthTicks / key / velocity / channel); `pitchHz` is an additive diagnostic
field (D-06: consumers ignore unknown fields).

Pitch tracking is a self-contained YIN (cumulative-mean normalized difference
+ parabolic interpolation) over 8 kHz mono frames — numpy is used when
importable (fast), with a pure-python fallback so the worker runs on a bare
python3 (PREFERRED degradation: no venv, no librosa needed). Breath noise and
silence are rejected by an RMS gate + aperiodicity threshold; the per-frame
pitch track is median-filtered, segmented into stable-pitch notes, and each
note is quantized to the nearest semitone (key = round of the segment median).

`--simulate` emits a deterministic, plausible 2-bar melody at (bpm, ppq).
`--self-test` synthesizes hummed-like input (vibrato + noise + silences) and
verifies the pipeline end to end, printing pitch-accuracy numbers to stderr.
"""

import argparse
import json
import math
import os
import struct
import sys
import time

# Private handle on the real stdout for protocol lines; stray prints → stderr.
_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr

try:
    import numpy as _np
except ImportError:  # pure-python fallback below
    _np = None

TARGET_SR = 8000          # analysis rate: fmax 1000 Hz << Nyquist, cheap
FRAME_HOP_S = 0.010       # 10 ms hop
FMIN, FMAX = 65.0, 1000.0
YIN_THRESHOLD = 0.20      # CMND aperiodicity threshold (voiced if below)
RMS_GATE_REL = 0.05       # voiced needs rms > 5% of the loud reference
SPLIT_SEMITONES = 0.8     # pitch move that splits a note
MIN_GAP_FRAMES = 2        # unvoiced frames that end a note
MEDIAN_KERNEL = 5


def emit(obj):
    _NDJSON.write(json.dumps(obj) + "\n")
    _NDJSON.flush()


def progress(fraction, stage):
    emit({"type": "progress",
          "progress": round(max(0.0, min(1.0, fraction)), 4), "stage": stage})


def log(line):
    emit({"type": "log", "line": line})


def fail(message):
    emit({"type": "error", "message": message})
    return 1


# ---------------------------------------------------------------------------
# WAV reading (self-contained: PCM 16/24/32 + IEEE float32, mono downmix)
# ---------------------------------------------------------------------------

def read_wav(path):
    """Return (sample_rate, [float mono samples in -1..1])."""
    with open(path, "rb") as f:
        data = f.read()
    if len(data) < 12 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("not a RIFF/WAVE file")
    pos, fmt, raw = 12, None, None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        (size,) = struct.unpack_from("<I", data, pos + 4)
        body = data[pos + 8: pos + 8 + size]
        if cid == b"fmt ":
            fmt = body
        elif cid == b"data":
            raw = body
        pos += 8 + size + (size & 1)
    if fmt is None or raw is None:
        raise ValueError("WAV missing fmt/data chunk")
    tag, channels, sr, _br, _ba, bits = struct.unpack_from("<HHIIHH", fmt, 0)
    if tag == 0xFFFE and len(fmt) >= 26:  # WAVE_FORMAT_EXTENSIBLE
        (tag,) = struct.unpack_from("<H", fmt, 24)
    if channels < 1:
        raise ValueError("WAV has no channels")
    if tag == 3 and bits == 32:
        n = len(raw) // 4
        samples = list(struct.unpack("<%df" % n, raw[: n * 4]))
    elif tag == 1 and bits == 16:
        n = len(raw) // 2
        ints = struct.unpack("<%dh" % n, raw[: n * 2])
        samples = [v / 32768.0 for v in ints]
    elif tag == 1 and bits == 32:
        n = len(raw) // 4
        ints = struct.unpack("<%di" % n, raw[: n * 4])
        samples = [v / 2147483648.0 for v in ints]
    elif tag == 1 and bits == 24:
        n = len(raw) // 3
        samples = []
        for i in range(n):
            b = raw[3 * i: 3 * i + 3]
            v = int.from_bytes(b, "little", signed=True)
            samples.append(v / 8388608.0)
    else:
        raise ValueError(
            "unsupported WAV encoding (format tag %d, %d-bit) — expected PCM "
            "16/24/32 or float32" % (tag, bits))
    if channels > 1:
        mono = []
        for i in range(0, len(samples) - channels + 1, channels):
            acc = 0.0
            for c in range(channels):
                acc += samples[i + c]
            mono.append(acc / channels)
        samples = mono
    return sr, samples


def resample_linear(samples, sr, target_sr):
    if sr == target_sr or not samples:
        return samples
    ratio = sr / target_sr
    out_len = max(1, int(len(samples) / ratio))
    if _np is not None:
        x = _np.asarray(samples, dtype=_np.float64)
        pos = _np.arange(out_len) * ratio
        j = _np.minimum(pos.astype(_np.int64), len(samples) - 1)
        j1 = _np.minimum(j + 1, len(samples) - 1)
        frac = pos - j
        return (x[j] * (1.0 - frac) + x[j1] * frac).tolist()
    out = []
    for i in range(out_len):
        pos = i * ratio
        j = int(pos)
        frac = pos - j
        a = samples[j]
        b = samples[j + 1] if j + 1 < len(samples) else a
        out.append(a + (b - a) * frac)
    return out


# ---------------------------------------------------------------------------
# Pitch tracking: YIN (CMND) per frame
# ---------------------------------------------------------------------------

def _frame_pitch_np(frame, sr, tau_min, tau_max, w):
    x = frame
    d = _np.empty(tau_max + 1)
    d[0] = 1.0
    base = x[:w]
    for tau in range(1, tau_max + 1):
        diff = base - x[tau:tau + w]
        d[tau] = float(_np.dot(diff, diff))
    return _cmnd_pick(d, sr, tau_min, tau_max)


def _frame_pitch_py(frame, sr, tau_min, tau_max, w):
    d = [1.0] * (tau_max + 1)
    for tau in range(1, tau_max + 1):
        acc = 0.0
        for i in range(w):
            diff = frame[i] - frame[i + tau]
            acc += diff * diff
        d[tau] = acc
    return _cmnd_pick(d, sr, tau_min, tau_max)


def _cmnd_pick(d, sr, tau_min, tau_max):
    """Cumulative-mean-normalize d, pick the first sub-threshold dip, refine
    with parabolic interpolation. Returns (f0_hz or None, aperiodicity)."""
    cmnd = [1.0] * (tau_max + 1)
    running = 0.0
    for tau in range(1, tau_max + 1):
        running += d[tau]
        cmnd[tau] = d[tau] * tau / running if running > 0 else 1.0
    tau_est = None
    for tau in range(tau_min, tau_max):
        if cmnd[tau] < YIN_THRESHOLD:
            while tau + 1 <= tau_max - 1 and cmnd[tau + 1] < cmnd[tau]:
                tau += 1
            tau_est = tau
            break
    if tau_est is None:  # no dip under threshold: take the global min anyway
        tau_est = min(range(tau_min, tau_max + 1), key=lambda t: cmnd[t])
    aperiodicity = cmnd[tau_est]
    # Parabolic interpolation around the minimum for fractional lag.
    t = tau_est
    if 1 <= t < tau_max:
        a, b, c = cmnd[t - 1], cmnd[t], cmnd[t + 1]
        denom = a - 2 * b + c
        if abs(denom) > 1e-12:
            t = t + 0.5 * (a - c) / denom
    if t <= 0:
        return None, aperiodicity
    return sr / t, aperiodicity


def pitch_track(samples, sr, on_progress=None):
    """Per-frame (time_s, f0_hz_or_None, rms). 10 ms hop."""
    tau_min = max(2, int(sr / FMAX))
    tau_max = int(sr / FMIN)
    w = 2 * tau_max          # integration window: >= 2 periods of FMIN
    frame_len = w + tau_max
    hop = max(1, int(sr * FRAME_HOP_S))
    n = len(samples)
    if n < frame_len:
        return []
    use_np = _np is not None
    if use_np:
        arr = _np.asarray(samples, dtype=_np.float64)
    frames = []
    starts = list(range(0, n - frame_len + 1, hop))
    for idx, start in enumerate(starts):
        if use_np:
            frame = arr[start:start + frame_len]
            rms = float(_np.sqrt(_np.mean(frame[:w] * frame[:w])))
        else:
            frame = samples[start:start + frame_len]
            rms = math.sqrt(sum(v * v for v in frame[:w]) / w)
        if rms < 1e-5:
            f0, ap = None, 1.0
        elif use_np:
            f0, ap = _frame_pitch_np(frame, sr, tau_min, tau_max, w)
        else:
            f0, ap = _frame_pitch_py(frame, sr, tau_min, tau_max, w)
        if f0 is not None and (f0 < FMIN or f0 > FMAX or ap > YIN_THRESHOLD):
            f0 = None
        frames.append((start / sr, f0, rms))
        if on_progress and idx % 25 == 0:
            on_progress(idx / max(1, len(starts)))
    # RMS gate relative to the loud reference (95th percentile), so breath
    # noise between phrases does not become notes.
    loud = sorted(f[2] for f in frames)
    ref = loud[int(0.95 * (len(loud) - 1))] if loud else 0.0
    gate = max(ref * RMS_GATE_REL, 1e-4)
    return [(t, (f0 if rms >= gate else None), rms) for (t, f0, rms) in frames]


def hz_to_midi(hz):
    return 69.0 + 12.0 * math.log2(hz / 440.0)


def median(values):
    s = sorted(values)
    m = len(s) // 2
    return s[m] if len(s) % 2 else 0.5 * (s[m - 1] + s[m])


def median_filter_track(track, kernel=MEDIAN_KERNEL):
    """None-aware median filter over the midi-value track (octave-blip
    killer). A frame stays voiced/unvoiced as it was; only values smooth."""
    half = kernel // 2
    out = []
    for i, (t, v, rms) in enumerate(track):
        if v is None:
            out.append((t, None, rms))
            continue
        window = [track[j][1] for j in range(max(0, i - half),
                                             min(len(track), i + half + 1))
                  if track[j][1] is not None]
        out.append((t, median(window), rms))
    return out


# ---------------------------------------------------------------------------
# Note segmentation
# ---------------------------------------------------------------------------

def segment_notes(track, min_note_s):
    """track: (time_s, midi_float_or_None, rms) -> list of note dicts
    {startS, endS, midi (float median), rms}."""
    hop = track[1][0] - track[0][0] if len(track) > 1 else FRAME_HOP_S
    notes = []
    cur = None  # {"start": s, "vals": [...], "rms": [...], "last": s}
    gap = 0

    def close(end_s):
        nonlocal cur
        if cur is not None:
            dur = end_s - cur["start"]
            if dur >= min_note_s and cur["vals"]:
                notes.append({
                    "startS": cur["start"],
                    "endS": end_s,
                    "midi": median(cur["vals"]),
                    "rms": median(cur["rms"]),
                })
            cur = None

    for t, v, rms in track:
        if v is None:
            gap += 1
            if cur is not None and gap >= MIN_GAP_FRAMES:
                close(cur["last"] + hop)
            continue
        gap = 0
        if cur is not None and abs(v - median(cur["vals"])) > SPLIT_SEMITONES:
            close(cur["last"] + hop)
        if cur is None:
            cur = {"start": t, "vals": [], "rms": [], "last": t}
        cur["vals"].append(v)
        cur["rms"].append(rms)
        cur["last"] = t
    if cur is not None:
        close(cur["last"] + hop)
    return notes


def notes_to_ticks(notes, ppq, bpm, quantize_grid):
    """Seconds -> ticks at (ppq, bpm); semitone quantize; optional light grid
    snap (onsets only, offsets stay put). Returns MidiNote wire dicts."""
    tps = bpm * ppq / 60.0  # ticks per second
    max_rms = max((n["rms"] for n in notes), default=0.0)
    grid = ppq * 4 // quantize_grid if quantize_grid else 0
    out = []
    for n in notes:
        on = int(round(n["startS"] * tps))
        off = int(round(n["endS"] * tps))
        if grid:
            on = int(round(on / grid)) * grid
        length = max(1, off - on)
        key = int(round(n["midi"]))
        if not (0 <= key <= 127):
            continue
        vel = 96
        if max_rms > 0:
            vel = int(round(50 + 65 * (n["rms"] / max_rms)))
        out.append({
            "tick": on,
            "lengthTicks": length,
            "key": key,
            "velocity": max(1, min(127, vel)),
            "channel": 0,
            "pitchHz": round(440.0 * 2 ** ((n["midi"] - 69.0) / 12.0), 2),
        })
    out.sort(key=lambda n: (n["tick"], n["key"]))
    return out


def clip_length_ticks(notes, ppq):
    """Total span rounded up to a whole bar (4/4)."""
    bar = 4 * ppq
    end = max((n["tick"] + n["lengthTicks"] for n in notes), default=0)
    return max(bar, ((end + bar - 1) // bar) * bar)


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------

def parse_params(raw):
    ppq = int(raw.get("ppq", 960))
    bpm = float(raw.get("bpm", 120.0))
    if ppq < 24:
        raise ValueError("ppq %d out of range" % ppq)
    if not (bpm > 0):
        raise ValueError("bpm %r out of range" % bpm)
    grid = int(raw.get("quantizeGrid") or 0)
    if grid < 0 or (grid and ppq * 4 % grid != 0):
        raise ValueError("quantizeGrid %d does not divide a bar at ppq %d"
                         % (grid, ppq))
    min_note_ms = float(raw.get("minNoteMs", 80.0))
    return raw.get("inputPath"), ppq, bpm, grid, min_note_ms


def run_simulate(ppq, bpm):
    """Deterministic, plausible 2-bar hummed melody (A minor pentatonic)."""
    progress(0.3, "trackingPitch")
    time.sleep(0.02)
    progress(0.7, "segmenting")
    eighth = ppq // 2
    riff = [(69, 2), (72, 1), (74, 1), (76, 2), (74, 1), (72, 1),
            (69, 2), (67, 2), (69, 4)]
    notes, t = [], 0
    for key, eighths in riff:
        length = eighths * eighth
        notes.append({
            "tick": t,
            "lengthTicks": max(1, length * 9 // 10),
            "key": key,
            "velocity": 96 if t % ppq == 0 else 82,
            "channel": 0,
            "pitchHz": round(440.0 * 2 ** ((key - 69) / 12.0), 2),
        })
        t += length
    emit({"type": "done", "result": {
        "kind": "humToMidi", "ppq": ppq, "bpm": bpm, "notes": notes,
        "lengthTicks": clip_length_ticks(notes, ppq),
        "stats": {"frames": 0, "voicedFraction": 0.0},
        "simulated": True,
    }})
    return 0


def analyze(input_path, ppq, bpm, grid, min_note_ms):
    progress(0.05, "loadingAudio")
    sr, samples = read_wav(input_path)
    if not samples:
        raise ValueError("WAV contains no samples")
    samples = resample_linear(samples, sr, TARGET_SR)
    log("hum_to_midi: %s (%.2fs @ %d Hz, %s backend)"
        % (os.path.basename(input_path), len(samples) / TARGET_SR, sr,
           "numpy" if _np is not None else "pure-python"))

    progress(0.1, "trackingPitch")
    track = pitch_track(
        samples, TARGET_SR,
        on_progress=lambda f: progress(0.1 + 0.6 * f, "trackingPitch"))
    if not track:
        raise ValueError("recording too short to analyze")
    # Hz -> fractional MIDI note values, then median-smooth (octave blips).
    track = [(t, hz_to_midi(f0) if f0 is not None else None, rms)
             for (t, f0, rms) in track]
    track = median_filter_track(track)

    progress(0.75, "segmenting")
    segs = segment_notes(track, min_note_ms / 1000.0)

    progress(0.9, "quantizing")
    notes = notes_to_ticks(segs, ppq, bpm, grid)
    voiced = sum(1 for _, v, _ in track if v is not None)
    stats = {
        "frames": len(track),
        "voicedFraction": round(voiced / len(track), 3),
        "detectedNotes": len(notes),
    }
    if notes:
        stats["medianF0"] = round(median(
            [n["pitchHz"] for n in notes]), 2)
    return notes, stats


def run_real(input_path, ppq, bpm, grid, min_note_ms):
    if not input_path:
        return fail("params.inputPath is required")
    if not os.path.isfile(input_path):
        return fail("input file not found: %s" % input_path)
    try:
        notes, stats = analyze(input_path, ppq, bpm, grid, min_note_ms)
    except ValueError as e:
        return fail(str(e))
    except Exception as e:  # clean NDJSON error, never a stack-trace crash
        import traceback
        traceback.print_exc(file=sys.stderr)
        return fail("hum analysis failed: %s" % e)
    if not notes:
        return fail("no melody detected in the recording — hum louder/closer "
                    "to the microphone, or check the right clip was recorded")
    emit({"type": "done", "result": {
        "kind": "humToMidi", "ppq": ppq, "bpm": bpm, "notes": notes,
        "lengthTicks": clip_length_ticks(notes, ppq),
        "stats": stats, "simulated": False,
    }})
    return 0


# ---------------------------------------------------------------------------
# Self-test: synthesized hummed-like input with known ground truth
# ---------------------------------------------------------------------------

def _synth_hum(spec, sr=16000, noise_db=-30.0, vibrato_hz=5.0,
               vibrato_cents=30.0, seed=42):
    """spec: [(freq_hz_or_None, seconds), ...] -> float samples with vibrato,
    attack/release envelopes and low-level noise (deterministic)."""
    import random
    rng = random.Random(seed)
    noise_amp = 10 ** (noise_db / 20.0)
    out = []
    for freq, dur in spec:
        n = int(dur * sr)
        if freq is None:
            out.extend(rng.gauss(0.0, noise_amp) for _ in range(n))
            continue
        phase = 0.0
        for i in range(n):
            vib = 2 ** (vibrato_cents / 1200.0
                        * math.sin(2 * math.pi * vibrato_hz * i / sr))
            phase += 2 * math.pi * freq * vib / sr
            env = min(1.0, i / (0.02 * sr), (n - i) / (0.03 * sr))
            out.append(0.5 * env * math.sin(phase)
                       + rng.gauss(0.0, noise_amp))
    return sr, out


def _write_wav16(path, sr, samples):
    import wave, array
    a = array.array("h", (int(max(-1.0, min(1.0, v)) * 32767)
                          for v in samples))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(a.tobytes())


def run_self_test():
    import tempfile
    failures = []

    def check(name, cond, detail=""):
        status = "ok" if cond else "FAIL"
        print("self-test %-42s %s %s" % (name, status, detail),
              file=sys.stderr)
        if not cond:
            failures.append(name)

    tmp = tempfile.mkdtemp(prefix="aura-hum-selftest-")

    # 1. A4 with vibrato + noise + surrounding silence -> one note, key 69.
    sr, s = _synth_hum([(None, 0.3), (440.0, 1.0), (None, 0.3)])
    p1 = os.path.join(tmp, "a4.wav")
    _write_wav16(p1, sr, s)
    notes, stats = analyze(p1, 960, 120.0, 0, 80.0)
    check("a4: exactly one note", len(notes) == 1, "got %d" % len(notes))
    if notes:
        check("a4: key == 69", notes[0]["key"] == 69,
              "key %d" % notes[0]["key"])
        cents = 1200.0 * math.log2(notes[0]["pitchHz"] / 440.0)
        check("a4: median f0 within 25 cents", abs(cents) < 25.0,
              "%.1f cents (f0 %.2f Hz)" % (cents, notes[0]["pitchHz"]))
        onset_s = notes[0]["tick"] / (960 * 2.0)  # ticks -> s at 120 bpm
        check("a4: onset within 60 ms", abs(onset_s - 0.3) < 0.06,
              "onset %.3fs (true 0.300s)" % onset_s)
        dur_s = notes[0]["lengthTicks"] / (960 * 2.0)
        check("a4: duration within 120 ms", abs(dur_s - 1.0) < 0.12,
              "duration %.3fs (true 1.000s)" % dur_s)

    # 2. Three-note melody A4-C5-E5 with breathy gaps -> keys 69, 72, 76.
    sr, s = _synth_hum([(None, 0.2), (440.0, 0.4), (None, 0.15),
                        (523.25, 0.4), (None, 0.15), (659.26, 0.4),
                        (None, 0.2)])
    p2 = os.path.join(tmp, "melody.wav")
    _write_wav16(p2, sr, s)
    notes, _ = analyze(p2, 960, 120.0, 0, 80.0)
    keys = [n["key"] for n in notes]
    check("melody: three notes", len(notes) == 3, "got %r" % (keys,))
    check("melody: keys 69,72,76", keys == [69, 72, 76], "got %r" % (keys,))
    if len(notes) == 3:
        errs = ["%.1f" % (1200.0 * math.log2(n["pitchHz"] / t))
                for n, t in zip(notes, [440.0, 523.25, 659.26])]
        check("melody: all within 30 cents",
              all(abs(float(e)) < 30.0 for e in errs),
              "cents errors: %s" % ", ".join(errs))

    # 3. Noise-only input -> no notes at all.
    sr, s = _synth_hum([(None, 1.0)])
    p3 = os.path.join(tmp, "noise.wav")
    _write_wav16(p3, sr, s)
    notes, _ = analyze(p3, 960, 120.0, 0, 80.0)
    check("noise: zero notes", len(notes) == 0, "got %d" % len(notes))

    # 4. Grid snap: onsets land on the requested grid.
    notes, _ = analyze(p2, 960, 120.0, 16, 80.0)
    grid = 960 * 4 // 16
    check("quantize: onsets on the 16th grid",
          bool(notes) and all(n["tick"] % grid == 0 for n in notes),
          "ticks %r" % [n["tick"] for n in notes])

    # 5. Simulate mode is deterministic and valid.
    eighth = 960 // 2
    sim = run_simulate(960, 120.0)
    check("simulate: exit 0", sim == 0)

    import shutil
    shutil.rmtree(tmp, ignore_errors=True)
    print("self-test: %d failure(s)" % len(failures), file=sys.stderr)
    return 1 if failures else 0


def run_pitch_track(input_path):
    """Print the per-frame pitch track as NDJSON, one object per 10 ms frame:

        {"t": <window start, seconds>, "hz": <float|null>,
         "midi": <float|null>, "rms": <float>}

    `hz` is raw out of `pitch_track`; `midi` is that value after
    `median_filter_track`, i.e. the stage `analyze` actually segments. Both
    are emitted because the Rust comparison needs the smoothed stage, but the
    raw one is what shows whether a disagreement came from detection or from
    smoothing.

    A diagnostic surface, not part of the job protocol — nothing in the app
    calls it. It exists so `src-tauri/tests/pitch_sidecar_parity.rs` can hold
    the Rust live detector to THIS file's behaviour: the live trainer and
    hum-to-MIDI must never disagree about what the singer sang, which is the
    whole reason the Rust port copies these constants verbatim.

    Raw means before `analyze`'s median filter and segmentation — the frames
    exactly as `pitch_track` produced them.
    """
    if not os.path.isfile(input_path):
        return fail("input file not found: %s" % input_path)
    try:
        sr, samples = read_wav(input_path)
    except ValueError as e:
        return fail("bad WAV: %s" % e)
    if not samples:
        return fail("WAV contains no samples")
    raw = pitch_track(resample_linear(samples, sr, TARGET_SR), TARGET_SR)
    # Exactly `analyze`'s next two steps, so `midi` is the stage that gets
    # segmented into notes.
    smoothed = median_filter_track(
        [(t, hz_to_midi(f0) if f0 is not None else None, rms)
         for (t, f0, rms) in raw])
    for (t, f0, rms), (_, midi, _) in zip(raw, smoothed):
        _NDJSON.write(json.dumps({
            "t": round(t, 6),
            "hz": (round(f0, 4) if f0 is not None else None),
            "midi": (round(midi, 6) if midi is not None else None),
            "rms": round(rms, 6),
        }) + "\n")
    _NDJSON.flush()
    return 0


def main():
    p = argparse.ArgumentParser(description="AURA hum-to-MIDI sidecar")
    p.add_argument("--params-json", default=None)
    p.add_argument("--simulate", action="store_true")
    p.add_argument("--self-test", action="store_true")
    p.add_argument("--pitch-track", default=None, metavar="WAV",
                   help="print the raw per-frame pitch track as NDJSON and "
                        "exit (diagnostic; see run_pitch_track)")
    args = p.parse_args()
    if args.self_test:
        return run_self_test()
    if args.pitch_track:
        return run_pitch_track(args.pitch_track)
    if args.params_json is None:
        return fail("--params-json is required")
    try:
        raw = json.loads(args.params_json)
        if not isinstance(raw, dict):
            raise ValueError("params must be a JSON object")
    except ValueError as e:
        return fail("bad --params-json: %s" % e)
    try:
        input_path, ppq, bpm, grid, min_note_ms = parse_params(raw)
    except (ValueError, TypeError) as e:
        return fail("bad humToMidi params: %s" % e)
    if args.simulate:
        return run_simulate(ppq, bpm)
    return run_real(input_path, ppq, bpm, grid, min_note_ms)


if __name__ == "__main__":
    sys.exit(main())
