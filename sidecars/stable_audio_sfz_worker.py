#!/usr/bin/env python3
"""AURA sidecar: Stable Audio Open -> SFZ instrument builder (phase 2, zone D).

Job kind: stableAudioSfz.

    stable_audio_sfz_worker.py --params-json '{...}' [--simulate]

Params: docs/ipc-schemas/sidecar-job-v2.schema.json $defs stableAudioSfzParams
(prompt, name, outputDir, keys, velocityLayers, seed). Result:
$defs sfzInstrumentResult. The generated .sfz uses ONLY the AURA SFZ SUBSET v1
documented in src-tauri/src/audio/sampler.rs — the frozen contract between
this generator and the sampler loader.

Modes
-----
* real: text prompt -> one one-shot per (key, velocity layer) via Stable
  Audio Open **small** (stable-audio-tools), then pitch-verify (autocorrelation
  f0 estimate) + retune-by-resampling to the exact target pitch, trim,
  normalize, and write <outputDir>/<name>.sfz + samples/*.wav. GPU is used
  when available but NOT required (CPU generation is slow but works).
* --simulate: zero-ML fallback — a small additive/FM-flavored synthesizer
  renders correctly PITCHED notes per (key, layer), so the whole
  prompt -> job -> playable instrument -> preview flow works end to end
  with no model stack installed. Pure stdlib (math/array/wave).

NDJSON protocol (stdout): {"type":"progress","progress":0..1,"stage":...},
{"type":"error","message":...}, {"type":"done","result":{...}}. All other
prints are redirected to stderr.
"""

import argparse
import json
import math
import os
import re
import sys
import wave
from array import array

_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr

DEFAULT_KEYS = [36, 39, 42, 45, 48, 51, 54, 57, 60, 63, 66, 69, 72]
SAMPLE_RATE = 44_100
NOTE_NAMES = ["c", "c#", "d", "d#", "e", "f", "f#", "g", "g#", "a", "a#", "b"]

INSTALL_HINT = (
    "Stable Audio Open is not installed. Install the model stack with:\n"
    "  pip install stable-audio-tools torch torchaudio einops\n"
    "then accept the model license and log in via `huggingface-cli login`\n"
    "(model: stabilityai/stable-audio-open-small; GPU optional, CPU works).\n"
    "Or run with --simulate / AURA_SIDECAR_SIMULATE=1 for a synthesized "
    "instrument with no ML dependencies."
)


def emit(obj):
    _NDJSON.write(json.dumps(obj) + "\n")
    _NDJSON.flush()


def progress(frac, stage):
    emit({"type": "progress", "progress": round(max(0.0, min(1.0, frac)), 4),
          "stage": stage})


def key_to_freq(key):
    return 440.0 * 2.0 ** ((key - 69) / 12.0)


def key_to_name(key):
    return f"{NOTE_NAMES[key % 12]}{key // 12 - 1}"


def safe_name(name):
    s = re.sub(r"[^A-Za-z0-9._-]+", "_", name).strip("_")
    return s or "instrument"


# ---------------------------------------------------------------------------
# WAV + DSP helpers (stdlib only; real mode also uses numpy when present)
# ---------------------------------------------------------------------------

def write_wav_mono16(path, samples, rate=SAMPLE_RATE):
    """samples: iterable of floats in [-1, 1]."""
    pcm = array("h", (int(max(-1.0, min(1.0, s)) * 32767) for s in samples))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())


def normalize(samples, peak=0.891):  # -1 dBFS
    m = max((abs(s) for s in samples), default=0.0)
    if m <= 1e-9:
        return samples
    g = peak / m
    return [s * g for s in samples]


def trim_leading_silence(samples, rate, threshold=0.004):
    for i, s in enumerate(samples):
        if abs(s) >= threshold:
            start = max(0, i - rate // 200)  # keep 5 ms pre-attack
            return samples[start:]
    return samples


def fade_edges(samples, rate, fade_in_s=0.002, fade_out_s=0.02):
    n = len(samples)
    fi = min(n, int(rate * fade_in_s))
    fo = min(n, int(rate * fade_out_s))
    out = list(samples)
    for i in range(fi):
        out[i] *= i / max(1, fi)
    for i in range(fo):
        out[n - 1 - i] *= i / max(1, fo)
    return out


def estimate_f0(samples, rate, lo=30.0, hi=2000.0):
    """Normalized-autocorrelation f0 estimate; None when unpitched."""
    n = len(samples)
    seg = samples[: min(n, rate)]  # analyze up to 1 s
    n = len(seg)
    min_lag = max(2, int(rate / hi))
    max_lag = min(n // 2 - 1, int(rate / lo))
    if max_lag <= min_lag:
        return None
    energy = sum(s * s for s in seg)
    if energy < 1e-6:
        return None
    best_score, best_lag = 0.0, None
    step = max(1, (max_lag - min_lag) // 400)  # bound the O(n*lags) cost
    for lag in range(min_lag, max_lag + 1, step):
        num = d0 = d1 = 0.0
        m = n - lag
        for i in range(0, m, 2):  # stride 2: cheap, plenty for verification
            a, b = seg[i], seg[i + lag]
            num += a * b
            d0 += a * a
            d1 += b * b
        denom = math.sqrt(d0 * d1)
        if denom > 1e-9:
            score = num / denom
            if score > best_score:
                best_score, best_lag = score, lag
    if best_lag is None or best_score < 0.5:
        return None
    return rate / best_lag


def resample_ratio(samples, ratio):
    """Linear-interp resample by `ratio` (>1 => shorter/higher)."""
    if abs(ratio - 1.0) < 1e-6 or not samples:
        return list(samples)
    n_out = max(1, int(len(samples) / ratio))
    out = []
    for i in range(n_out):
        pos = i * ratio
        i0 = int(pos)
        i1 = min(i0 + 1, len(samples) - 1)
        t = pos - i0
        out.append(samples[i0] * (1.0 - t) + samples[i1] * t)
    return out


def retune(samples, rate, target_freq, stage_report=None):
    """Pitch-verify and retune-by-resampling toward target_freq."""
    f0 = estimate_f0(samples, rate)
    if f0 is None:
        if stage_report:
            stage_report("unpitched sample, kept as-is")
        return samples
    ratio = target_freq / f0
    # Only correct plausible detunes (within +/- one octave); wilder
    # estimates are more likely octave errors of the detector.
    if 0.5 < ratio < 2.0 and abs(1200.0 * math.log2(ratio)) > 5.0:
        return resample_ratio(samples, 1.0 / ratio)
    return samples


# ---------------------------------------------------------------------------
# Simulate-mode synthesizer (correctly pitched additive/FM one-shots)
# ---------------------------------------------------------------------------

def synth_note(key, layer_frac, seed, rate=SAMPLE_RATE, dur=1.5):
    """Additive synth with light FM shimmer: fundamental-dominant so the
    pitch is unambiguous, brightness scaled by the velocity layer."""
    f0 = key_to_freq(key)
    n = int(rate * dur)
    brightness = 0.35 + 0.65 * layer_frac  # louder layers are brighter
    harmonics = []
    for h in range(1, 9):
        if f0 * h > rate * 0.45:
            break
        amp = (1.0 / h ** 1.7) * (brightness if h > 1 else 1.0)
        harmonics.append((h, amp))
    det = 1.0 + 0.0007 * math.sin(seed + key)  # deterministic micro-detune
    out = []
    two_pi = 2.0 * math.pi
    fm_ratio, fm_depth = 2.0, 0.15 * brightness
    for i in range(n):
        t = i / rate
        env = math.exp(-3.2 * t) * (1.0 - math.exp(-t * 700.0))
        fm = fm_depth * math.sin(two_pi * f0 * fm_ratio * t) * math.exp(-6.0 * t)
        s = 0.0
        for h, amp in harmonics:
            s += amp * math.sin(two_pi * f0 * det * h * t + fm * h)
        out.append(s * env * (0.55 + 0.45 * layer_frac))
    return fade_edges(normalize(out), rate)


# ---------------------------------------------------------------------------
# Real mode: Stable Audio Open (small)
# ---------------------------------------------------------------------------

def generate_real(params, plan, samples_dir):
    try:
        import numpy as np  # noqa: F401
        import torch
        from stable_audio_tools import get_pretrained_model
        from stable_audio_tools.inference.generation import generate_diffusion_cond
    except ImportError as e:
        emit({"type": "error", "message": f"{INSTALL_HINT}\n(import failed: {e})"})
        return None

    import numpy as np

    progress(0.0, "loadingModel")
    device = "cuda" if torch.cuda.is_available() else "cpu"
    model, cfg = get_pretrained_model("stabilityai/stable-audio-open-small")
    model = model.to(device)
    model_rate = cfg["sample_rate"]
    sample_size = min(cfg["sample_size"], int(model_rate * 3.0))
    seed = int(params.get("seed", 0)) or None
    prompt = params["prompt"]

    written = []
    total = len(plan)
    for i, (key, layer, lovel, hivel, fname) in enumerate(plan):
        progress(0.05 + 0.75 * i / total, "generatingSamples")
        note = key_to_name(key)
        vel_word = ["soft", "medium", "hard", "very hard"][min(3, int(3 * (hivel / 127)))]
        cond = [{
            "prompt": f"{prompt}, single sustained note, pitch {note}, "
                      f"{vel_word} dynamics, monophonic, clean, no reverb tail",
            "seconds_start": 0,
            "seconds_total": sample_size / model_rate,
        }]
        audio = generate_diffusion_cond(
            model,
            steps=8,
            cfg_scale=1.0,
            conditioning=cond,
            sample_size=sample_size,
            device=device,
            seed=(seed + i) if seed is not None else -1,
        )
        mono = audio.squeeze(0).mean(dim=0).to(torch.float32).cpu().numpy()
        peak = float(np.abs(mono).max()) or 1.0
        mono = (mono / peak).tolist()

        progress(0.05 + 0.75 * (i + 0.5) / total, "pitchVerify")
        mono = trim_leading_silence(mono, model_rate)
        mono = retune(mono, model_rate, key_to_freq(key))
        mono = fade_edges(normalize(mono), model_rate)
        if model_rate != SAMPLE_RATE:
            mono = resample_ratio(mono, model_rate / SAMPLE_RATE)
        write_wav_mono16(os.path.join(samples_dir, fname), mono)
        written.append(fname)
    return written


# ---------------------------------------------------------------------------
# SFZ writing (AURA SFZ SUBSET v1 ONLY)
# ---------------------------------------------------------------------------

def key_spans(keys):
    """Map sorted sampled keys to (lokey, hikey) spans splitting at midpoints;
    first span reaches down to 0, last up to 127."""
    spans = {}
    for i, k in enumerate(keys):
        lo = 0 if i == 0 else (keys[i - 1] + k) // 2 + 1
        hi = 127 if i == len(keys) - 1 else (k + keys[i + 1]) // 2
        spans[k] = (lo, hi)
    return spans


def velocity_ranges(layers):
    """Split 1..127 into `layers` contiguous ranges."""
    edges = [1 + round(i * 126 / layers) for i in range(layers)] + [128]
    return [(edges[i], edges[i + 1] - 1) for i in range(layers)]


def write_sfz(sfz_path, name, plan_by_layer, simulated):
    lines = [
        f"// {name} — generated by AURA stable_audio_sfz_worker"
        f"{' (simulate mode)' if simulated else ''}",
        "// AURA SFZ SUBSET v1",
        "<control> default_path=samples/",
        "<global> ampeg_attack=0.003 ampeg_release=0.35",
    ]
    for (lovel, hivel), regions in plan_by_layer:
        lines.append(f"<group> lovel={lovel} hivel={hivel}")
        for key, (lokey, hikey), fname in regions:
            lines.append(
                f"<region> sample={fname} lokey={lokey} hikey={hikey} "
                f"pitch_keycenter={key}"
            )
    with open(sfz_path, "w") as f:
        f.write("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--params-json", required=True)
    p.add_argument("--simulate", action="store_true")
    args = p.parse_args()
    try:
        params = json.loads(args.params_json)
    except ValueError as e:
        emit({"type": "error", "message": f"bad --params-json: {e}"})
        return 1

    out_dir = params.get("outputDir")
    if not out_dir or not os.path.isabs(out_dir):
        emit({"type": "error", "message": "outputDir (absolute path) is required"})
        return 1
    prompt = params.get("prompt")
    if not prompt and not args.simulate:
        emit({"type": "error", "message": "prompt is required"})
        return 1

    name = params.get("name") or (prompt[:40] if prompt else "Generated Instrument")
    keys = sorted(set(int(k) for k in params.get("keys", DEFAULT_KEYS)
                      if 0 <= int(k) <= 127))
    if not keys:
        emit({"type": "error", "message": "keys must contain at least one MIDI key"})
        return 1
    layers = max(1, min(4, int(params.get("velocityLayers", 1))))
    seed = int(params.get("seed", 1234))

    samples_dir = os.path.join(out_dir, "samples")
    os.makedirs(samples_dir, exist_ok=True)

    spans = key_spans(keys)
    vranges = velocity_ranges(layers)
    # Flat generation plan: (key, layer_index, lovel, hivel, filename).
    plan = [
        (key, li, lovel, hivel, f"k{key:03d}_v{li + 1}.wav")
        for li, (lovel, hivel) in enumerate(vranges)
        for key in keys
    ]

    if args.simulate:
        total = len(plan)
        for i, (key, li, _lo, _hi, fname) in enumerate(plan):
            progress(0.05 + 0.8 * i / total, "generatingSamples")
            layer_frac = 1.0 if layers == 1 else li / (layers - 1)
            write_wav_mono16(os.path.join(samples_dir, fname),
                             synth_note(key, layer_frac, seed))
        progress(0.88, "pitchVerify")  # synth is pitch-exact by construction
    else:
        written = generate_real(params, plan, samples_dir)
        if written is None:
            return 1

    progress(0.95, "writingSfz")
    sfz_path = os.path.join(out_dir, f"{safe_name(name)}.sfz")
    plan_by_layer = [
        (
            vranges[li],
            [(key, spans[key], f"k{key:03d}_v{li + 1}.wav") for key in keys],
        )
        for li in range(layers)
    ]
    write_sfz(sfz_path, name, plan_by_layer, args.simulate)

    emit({"type": "done", "result": {
        "kind": "stableAudioSfz",
        "name": name,
        "sfzPath": sfz_path,
        "sampleCount": len(plan),
        "simulated": bool(args.simulate),
    }})
    return 0


if __name__ == "__main__":
    sys.exit(main())
