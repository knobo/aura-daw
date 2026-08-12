#!/usr/bin/env python3
"""AURA sidecar: ACE-Step music generation (phase 2, zone B).

Job kinds: aceStepGenerate | aceStepRepaint | aceStepAudio2Audio.
Spoken to by the Rust job runner over the NDJSON stdout protocol
(docs/ARCHITECTURE.md §6). Invocation (frozen):

    ace_step_worker.py --task generate|repaint|audio2audio \
                       --params-json '{...}' [--simulate]

`--params-json` schemas: docs/ipc-schemas/sidecar-job-v2.schema.json
($defs aceStepGenerateParams / aceStepRepaintParams / aceStepA2AParams);
result shape: $defs audioResult ({"kind": "<jobKind>", "outputPath": ...,
"durationSeconds": ..., "sampleRate": ..., "simulated": ...}).

Real runs need the ACE-Step package + checkpoints:

    pip install git+https://github.com/ace-step/ACE-Step.git

Checkpoints download on first use (or point ACESTEP_CHECKPOINT_DIR at an
existing set). A CUDA GPU with >= 8 GB VRAM is strongly recommended; CPU
works but is very slow. Dtype override: ACESTEP_DTYPE (default bfloat16).
On smaller/shared GPUs set ACESTEP_CPU_OFFLOAD=1 (sequential offload) and
ACESTEP_OVERLAPPED_DECODE=1 — verified to fit the 3.5B pipeline in ~7 GB
VRAM on a 12 GB RTX 4070 SUPER shared with a desktop. ACESTEP_INFER_STEPS
overrides the step count (default 60).

`--simulate` needs nothing: it synthesizes deterministic placeholder audio
(seeded chord arpeggio) to `outputPath` with staged progress, so the whole
generate -> import -> hear-it loop is demoable without any model stack.

`--selftest` runs the simulate paths end-to-end via subprocesses and checks
the NDJSON protocol + produced WAVs (used by CI/dev, no model needed).
"""

import argparse
import json
import math
import os
import random
import struct
import sys
import time
import traceback
import wave

# Private handle on the real stdout for protocol lines; stray library prints
# (torch, transformers, ...) go to stderr so they cannot corrupt the NDJSON.
_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr

SIM_SAMPLE_RATE = 48000
KIND_BY_TASK = {"generate": "aceStepGenerate", "repaint": "aceStepRepaint",
                "audio2audio": "aceStepAudio2Audio"}


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


# ---------------------------------------------------------------------------
# Params
# ---------------------------------------------------------------------------

REQUIRED = {
    "generate": ["prompt", "outputPath"],
    "repaint": ["inputPath", "startSeconds", "endSeconds", "outputPath"],
    "audio2audio": ["inputPath", "outputPath"],
}


def validate_params(task, params):
    missing = [k for k in REQUIRED[task]
               if params.get(k) in (None, "") and params.get(k) != 0]
    if missing:
        fail("missing required param(s) for %s: %s"
             % (KIND_BY_TASK[task], ", ".join(missing)))
    out = params["outputPath"]
    if not os.path.isabs(out):
        fail("outputPath must be an absolute path: %s" % out)
    if task in ("repaint", "audio2audio") and not os.path.isfile(params["inputPath"]):
        fail("input file not found: %s" % params["inputPath"])
    if task == "repaint":
        try:
            s, e = float(params["startSeconds"]), float(params["endSeconds"])
        except (TypeError, ValueError):
            fail("startSeconds/endSeconds must be numbers")
        if not (0 <= s < e):
            fail("invalid repaint region: startSeconds=%r endSeconds=%r"
                 % (params["startSeconds"], params["endSeconds"]))


def ensure_out_dir(path):
    d = os.path.dirname(path)
    if d:
        os.makedirs(d, exist_ok=True)


# ---------------------------------------------------------------------------
# Simulate mode: deterministic synthesized placeholder audio
# ---------------------------------------------------------------------------

def _synth_frames(seconds, seed, sample_rate=SIM_SAMPLE_RATE):
    """Deterministic stereo chord arpeggio, yields (l, r) floats in ±0.8."""
    rng = random.Random(seed)
    root = rng.choice([48, 50, 52, 53, 55, 57])  # MIDI note
    scale = [0, 3, 7, 10, 12] if rng.random() < 0.5 else [0, 4, 7, 11, 12]
    notes = [440.0 * 2 ** ((root + s - 69) / 12.0) for s in scale]
    total = int(seconds * sample_rate)
    step = max(1, sample_rate // 4)  # 16th-ish notes at 120 bpm
    for i in range(total):
        n = notes[(i // step) % len(notes)]
        t = i / sample_rate
        env = min(1.0, (i % step) / (0.01 * sample_rate) if step > 1 else 1.0)
        env *= max(0.0, 1.0 - ((i % step) / step) ** 2)
        v = 0.5 * env * (math.sin(2 * math.pi * n * t)
                         + 0.3 * math.sin(4 * math.pi * n * t))
        pad = 0.15 * math.sin(2 * math.pi * notes[0] / 2 * t)
        yield (max(-0.8, min(0.8, v + pad)), max(-0.8, min(0.8, 0.9 * v + pad)))


def write_synth_wav(path, seconds, seed, sample_rate=SIM_SAMPLE_RATE):
    ensure_out_dir(path)
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        buf, flush_at = [], 65536
        for (l, r) in _synth_frames(seconds, seed, sample_rate):
            buf.append(struct.pack("<hh", int(l * 32767), int(r * 32767)))
            if len(buf) >= flush_at:
                w.writeframes(b"".join(buf))
                buf = []
        if buf:
            w.writeframes(b"".join(buf))


def read_pcm_wav(path):
    """Read a PCM 16-bit WAV as [(l, r)] floats; None if unreadable here."""
    try:
        with wave.open(path, "rb") as r:
            if r.getsampwidth() != 2:
                return None
            ch, rate, n = r.getnchannels(), r.getframerate(), r.getnframes()
            raw = r.readframes(n)
        vals = struct.unpack("<%dh" % (len(raw) // 2), raw)
        if ch == 1:
            frames = [(v / 32768.0, v / 32768.0) for v in vals]
        else:
            frames = [(vals[i] / 32768.0, vals[i + 1] / 32768.0)
                      for i in range(0, len(vals) - ch + 1, ch)]
        return rate, frames
    except (wave.Error, EOFError, OSError):
        return None


def write_pcm_wav(path, rate, frames):
    ensure_out_dir(path)
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(b"".join(
            struct.pack("<hh",
                        int(max(-1.0, min(1.0, l)) * 32767),
                        int(max(-1.0, min(1.0, r)) * 32767))
            for (l, r) in frames))


def wav_info(path):
    """(durationSeconds, sampleRate) of a wav, or (None, None).

    stdlib `wave` only reads integer-PCM; real ACE-Step writes IEEE-float
    WAVs, so fall back to soundfile (a model-stack dependency) when present.
    """
    try:
        with wave.open(path, "rb") as r:
            rate = r.getframerate()
            return round(r.getnframes() / float(rate), 3), rate
    except (wave.Error, EOFError, OSError):
        pass
    try:
        import soundfile
        info = soundfile.info(path)
        return round(info.frames / float(info.samplerate), 3), info.samplerate
    except Exception:
        return None, None


def sim_stages(stages):
    for i, stage in enumerate(stages):
        progress(0.05 + 0.85 * i / len(stages), stage)
        time.sleep(0.05)


def run_simulate(task, params):
    seed = int(params.get("seed") or 0) or 42
    out = params["outputPath"]
    sim_stages(["loadingModel", "encodingPrompt", "diffusing", "decoding"])
    if task == "generate":
        seconds = float(params.get("durationSeconds") or 30.0)
        seconds = max(0.1, min(seconds, 600.0))
        write_synth_wav(out, seconds, seed)
    else:
        src = read_pcm_wav(params["inputPath"])
        if task == "repaint":
            if src is None:
                seconds = float(params["endSeconds"]) + 2.0
                log("input not PCM-readable in simulate mode; synthesizing "
                    "%.1fs placeholder" % seconds)
                write_synth_wav(out, seconds, seed)
            else:
                rate, frames = src
                s = int(float(params["startSeconds"]) * rate)
                e = min(len(frames), int(float(params["endSeconds"]) * rate))
                patch = list(_synth_frames((e - s) / rate, seed, rate))
                frames[s:e] = patch[: e - s]
                write_pcm_wav(out, rate, frames)
        else:  # audio2audio
            strength = float(params.get("strength", 0.7))
            strength = max(0.0, min(1.0, strength))
            if src is None:
                log("input not PCM-readable in simulate mode; synthesizing "
                    "placeholder")
                write_synth_wav(out, 30.0, seed)
            else:
                rate, frames = src
                synth = _synth_frames(len(frames) / rate, seed, rate)
                mixed = [((1 - strength) * l + strength * sl,
                          (1 - strength) * r + strength * sr)
                         for (l, r), (sl, sr) in zip(frames, synth)]
                write_pcm_wav(out, rate, mixed)
    progress(0.95, "writing")
    duration, rate = wav_info(out)
    emit({"type": "done", "result": {
        "kind": KIND_BY_TASK[task],
        "outputPath": os.path.abspath(out),
        "durationSeconds": duration,
        "sampleRate": rate,
        "simulated": True,
    }})


# ---------------------------------------------------------------------------
# Real mode: ACE-Step pipeline
# ---------------------------------------------------------------------------

INSTALL_HELP = (
    "ACE-Step is not installed. Install it with:\n"
    "    pip install git+https://github.com/ace-step/ACE-Step.git\n"
    "Checkpoints download on first run (or set ACESTEP_CHECKPOINT_DIR to an "
    "existing checkpoint directory). A CUDA GPU with >= 8 GB VRAM is "
    "recommended. For a model-free demo run with --simulate "
    "(AURA_SIDECAR_SIMULATE=1)."
)


def run_real(task, params):
    progress(0.01, "loadingModel")
    try:
        from acestep.pipeline_ace_step import ACEStepPipeline  # noqa
    except ImportError:
        fail(INSTALL_HELP)
    except Exception as e:  # torch/CUDA import-time failures
        traceback.print_exc(file=sys.stderr)
        fail("ACE-Step import failed: %s" % e)

    ckpt = os.environ.get("ACESTEP_CHECKPOINT_DIR") or None

    def env_flag(name):
        return os.environ.get(name, "").strip().lower() in ("1", "true", "yes")

    try:
        pipeline = ACEStepPipeline(
            checkpoint_dir=ckpt,
            dtype=os.environ.get("ACESTEP_DTYPE", "bfloat16"),
            torch_compile=False,
            # Fit-on-smaller-GPUs overrides (e.g. 12 GB cards shared with a
            # desktop): sequential CPU offload + overlapped VAE decode.
            cpu_offload=env_flag("ACESTEP_CPU_OFFLOAD"),
            overlapped_decode=env_flag("ACESTEP_OVERLAPPED_DECODE"),
        )
    except Exception as e:
        traceback.print_exc(file=sys.stderr)
        fail("failed to initialize the ACE-Step pipeline (checkpoints "
             "missing/corrupt, or no usable device?): %s" % e)

    out = os.path.abspath(params["outputPath"])
    ensure_out_dir(out)
    seed = params.get("seed")
    duration = float(params.get("durationSeconds") or 30.0)

    wanted = {
        "format": "wav",
        "audio_duration": duration,
        "prompt": params.get("prompt") or "",
        "lyrics": params.get("lyrics") or "",
        "infer_step": int(os.environ.get("ACESTEP_INFER_STEPS", "60")),
        "guidance_scale": 15.0,
        "scheduler_type": "euler",
        "cfg_type": "apg",
        "omega_scale": 10.0,
        "manual_seeds": str(seed) if seed is not None else None,
        "save_path": out,
    }
    if task == "repaint":
        wanted.update({
            "task": "repaint",
            "src_audio_path": params["inputPath"],
            "repaint_start": float(params["startSeconds"]),
            "repaint_end": float(params["endSeconds"]),
            "retake_seeds": str(seed) if seed is not None else None,
        })
    elif task == "audio2audio":
        wanted.update({
            "task": "audio2audio",
            "audio2audio_enable": True,
            "ref_audio_input": params["inputPath"],
            "ref_audio_strength": float(params.get("strength", 0.7)),
        })

    # Tolerate API drift between ACE-Step versions: only pass kwargs the
    # installed pipeline actually accepts.
    import inspect
    try:
        accepted = set(inspect.signature(pipeline.__call__).parameters)
        kwargs = {k: v for k, v in wanted.items()
                  if k in accepted and v is not None}
        dropped = sorted(k for k, v in wanted.items()
                         if k not in accepted and v is not None)
        if dropped:
            log("ACE-Step pipeline ignores unsupported kwargs: %s"
                % ", ".join(dropped))
    except (TypeError, ValueError):
        kwargs = {k: v for k, v in wanted.items() if v is not None}

    progress(0.10, "diffusing")
    try:
        result = pipeline(**kwargs)
    except SystemExit:
        raise
    except Exception as e:
        traceback.print_exc(file=sys.stderr)
        fail("ACE-Step %s failed: %s" % (task, e))

    progress(0.92, "writing")
    if not os.path.isfile(out):
        # Some versions return the written path(s) instead of honoring
        # save_path; move the first produced file into place.
        produced = None
        if isinstance(result, str):
            produced = result
        elif isinstance(result, (list, tuple)):
            produced = next((p for p in result
                             if isinstance(p, str) and os.path.isfile(p)), None)
        if produced and os.path.isfile(produced):
            import shutil
            shutil.move(produced, out)
        else:
            fail("ACE-Step finished but no output file was produced at %s" % out)

    duration_s, rate = wav_info(out)
    emit({"type": "done", "result": {
        "kind": KIND_BY_TASK[task],
        "outputPath": out,
        "durationSeconds": duration_s,
        "sampleRate": rate,
        "simulated": False,
    }})


# ---------------------------------------------------------------------------
# Self-test (simulate paths E2E through the real CLI)
# ---------------------------------------------------------------------------

def run_selftest():
    import subprocess
    import tempfile

    def run_case(name, argv, expect):
        proc = subprocess.run([sys.executable, os.path.abspath(__file__)] + argv,
                              capture_output=True, text=True, timeout=120)
        lines = [json.loads(l) for l in proc.stdout.splitlines() if l.strip()]
        terminal = [l for l in lines if l.get("type") in ("done", "error")]
        assert len(terminal) == 1, "%s: want exactly 1 terminal line, got %r" \
            % (name, terminal)
        assert terminal[0]["type"] == expect, "%s: %r" % (name, terminal[0])
        fracs = [l["progress"] for l in lines if l.get("type") == "progress"]
        assert all(0 <= f <= 1 for f in fracs), name
        return terminal[0]

    ok = 0
    with tempfile.TemporaryDirectory(prefix="ace-selftest-") as td:
        gen = os.path.join(td, "gen.wav")
        t = run_case("generate", ["--task", "generate", "--simulate",
                                  "--params-json", json.dumps({
                                      "prompt": "warm synthwave",
                                      "durationSeconds": 2.0, "seed": 7,
                                      "outputPath": gen})], "done")
        assert t["result"]["kind"] == "aceStepGenerate" and t["result"]["simulated"]
        d, r = wav_info(gen)
        assert r == SIM_SAMPLE_RATE and abs(d - 2.0) < 0.05, (d, r)
        ok += 1

        rep = os.path.join(td, "rep.wav")
        t = run_case("repaint", ["--task", "repaint", "--simulate",
                                 "--params-json", json.dumps({
                                     "inputPath": gen, "startSeconds": 0.5,
                                     "endSeconds": 1.0, "outputPath": rep})],
                     "done")
        assert t["result"]["kind"] == "aceStepRepaint"
        d, _ = wav_info(rep)
        assert abs(d - 2.0) < 0.05, d  # repaint preserves input length
        ok += 1

        a2a = os.path.join(td, "a2a.wav")
        t = run_case("audio2audio", ["--task", "audio2audio", "--simulate",
                                     "--params-json", json.dumps({
                                         "inputPath": gen, "strength": 0.5,
                                         "outputPath": a2a})], "done")
        assert t["result"]["kind"] == "aceStepAudio2Audio"
        ok += 1

        t = run_case("missing-prompt", ["--task", "generate", "--simulate",
                                        "--params-json", json.dumps({
                                            "outputPath": gen})], "error")
        assert "prompt" in t["message"], t
        ok += 1

        t = run_case("bad-json", ["--task", "generate", "--simulate",
                                  "--params-json", "{nope"], "error")
        ok += 1

        t = run_case("bad-region", ["--task", "repaint", "--simulate",
                                    "--params-json", json.dumps({
                                        "inputPath": gen, "startSeconds": 2.0,
                                        "endSeconds": 1.0, "outputPath": rep})],
                     "error")
        ok += 1

    print("ace_step_worker selftest: %d/%d cases passed" % (ok, ok),
          file=sys.stderr)
    return 0


# ---------------------------------------------------------------------------

def main():
    p = argparse.ArgumentParser(description="AURA ACE-Step generation sidecar")
    p.add_argument("--task", choices=["generate", "repaint", "audio2audio"])
    p.add_argument("--params-json")
    p.add_argument("--simulate", action="store_true")
    p.add_argument("--selftest", action="store_true")
    args = p.parse_args()

    if args.selftest:
        return run_selftest()
    if not args.task or args.params_json is None:
        fail("--task and --params-json are required")
    try:
        params = json.loads(args.params_json)
        if not isinstance(params, dict):
            raise ValueError("params must be a JSON object")
    except ValueError as e:
        fail("bad --params-json: %s" % e)

    validate_params(args.task, params)
    if args.simulate:
        run_simulate(args.task, params)
    else:
        run_real(args.task, params)
    return 0


if __name__ == "__main__":
    sys.exit(main())
