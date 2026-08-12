#!/usr/bin/env python3
"""AURA sidecar: ElevenLabs Music API (cloud), phase 2, zone B.

Job kind: elevenLabsMusic. Cloud jobs are ordinary workers: this script does
the HTTPS calls so the Rust job manager sees the exact same NDJSON lifecycle
(and cancel = SIGTERM) as local model jobs.

    elevenlabs_music_worker.py --params-json '{...}' [--simulate]

Params: docs/ipc-schemas/sidecar-job-v2.schema.json $defs elevenLabsParams
(prompt, durationSeconds, outputPath). Result: $defs audioResult.

The API key comes from the ELEVENLABS_API_KEY environment variable ONLY —
never from params, and it is never written to stdout/stderr. Endpoint base
is overridable via ELEVENLABS_BASE_URL (default https://api.elevenlabs.io),
which is how tests point the worker at a local mock server.

Output format follows outputPath's extension: `.wav` requests raw PCM
(pcm_44100, mono 16-bit; override channel count with ELEVENLABS_PCM_CHANNELS)
and wraps it in a WAV header; anything else saves the API's MP3 bytes as-is.

`--simulate` never touches the network: it writes a synthesized placeholder
WAV with staged progress. `--selftest` runs simulate + a local mock-server
exercise of the real HTTP path (success, missing key, bad key, quota).
"""

import argparse
import json
import math
import os
import struct
import sys
import time
import wave

# Private handle on the real stdout for protocol lines; stray prints and
# tracebacks go to stderr so they cannot corrupt the NDJSON.
_NDJSON = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
sys.stdout = sys.stderr

DEFAULT_BASE_URL = "https://api.elevenlabs.io"
SIM_SAMPLE_RATE = 44100
PCM_RATE = 44100
MIN_MS, MAX_MS = 10_000, 300_000


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


def ensure_out_dir(path):
    d = os.path.dirname(path)
    if d:
        os.makedirs(d, exist_ok=True)


def validate_params(params):
    if not params.get("prompt"):
        fail("missing required param: prompt")
    out = params.get("outputPath")
    if not out:
        fail("missing required param: outputPath")
    if not os.path.isabs(out):
        fail("outputPath must be an absolute path: %s" % out)


def wav_info(path):
    try:
        with wave.open(path, "rb") as r:
            rate = r.getframerate()
            return round(r.getnframes() / float(rate), 3), rate
    except (wave.Error, EOFError, OSError):
        return None, None


# ---------------------------------------------------------------------------
# Simulate mode (no network, ever)
# ---------------------------------------------------------------------------

def write_synth_wav(path, seconds, sample_rate=SIM_SAMPLE_RATE):
    """Deterministic placeholder: soft two-chord pad, stereo 16-bit PCM."""
    ensure_out_dir(path)
    freqs_a = [220.0, 277.18, 329.63]   # A major-ish
    freqs_b = [196.0, 246.94, 293.66]   # G major-ish
    total = int(seconds * sample_rate)
    bar = sample_rate * 2
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        buf = []
        for i in range(total):
            t = i / sample_rate
            freqs = freqs_a if (i // bar) % 2 == 0 else freqs_b
            v = sum(math.sin(2 * math.pi * f * t) for f in freqs) / len(freqs)
            v *= 0.4 * (0.8 + 0.2 * math.sin(2 * math.pi * 0.25 * t))
            s = int(max(-1.0, min(1.0, v)) * 32767)
            buf.append(struct.pack("<hh", s, s))
            if len(buf) >= 65536:
                w.writeframes(b"".join(buf))
                buf = []
        if buf:
            w.writeframes(b"".join(buf))


def run_simulate(params):
    seconds = float(params.get("durationSeconds") or 30.0)
    seconds = max(0.1, min(seconds, 300.0))
    for i, stage in enumerate(["requesting", "generating", "downloading"]):
        progress(0.05 + 0.8 * i / 3, stage)
        time.sleep(0.05)
    out = params["outputPath"]
    write_synth_wav(out, seconds)
    progress(0.95, "writing")
    duration, rate = wav_info(out)
    emit({"type": "done", "result": {
        "kind": "elevenLabsMusic",
        "outputPath": os.path.abspath(out),
        "durationSeconds": duration,
        "sampleRate": rate,
        "simulated": True,
    }})


# ---------------------------------------------------------------------------
# Real mode: HTTPS to the ElevenLabs Music API
# ---------------------------------------------------------------------------

def _error_detail(body):
    """Best-effort human detail from an API error body (never echoes keys)."""
    try:
        payload = json.loads(body)
        detail = payload.get("detail", payload)
        if isinstance(detail, dict):
            detail = detail.get("message") or detail.get("status") or detail
        return str(detail)[:500]
    except (ValueError, AttributeError):
        return body.decode("utf-8", "replace")[:200] if isinstance(body, bytes) \
            else str(body)[:200]


def run_real(params):
    import urllib.error
    import urllib.request

    api_key = os.environ.get("ELEVENLABS_API_KEY")
    if not api_key:
        fail("ELEVENLABS_API_KEY is not set — export your ElevenLabs API key "
             "in the app's environment (it is never passed in job params). "
             "For a network-free demo run with --simulate "
             "(AURA_SIDECAR_SIMULATE=1).")

    out = params["outputPath"]
    want_wav = out.lower().endswith(".wav")
    output_format = "pcm_44100" if want_wav else "mp3_44100_128"
    base = os.environ.get("ELEVENLABS_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    url = "%s/v1/music?output_format=%s" % (base, output_format)

    body = {"prompt": params["prompt"], "model_id": "music_v1"}
    if params.get("durationSeconds"):
        ms = int(float(params["durationSeconds"]) * 1000)
        body["music_length_ms"] = max(MIN_MS, min(MAX_MS, ms))

    req = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={
            "xi-api-key": api_key,
            "Content-Type": "application/json",
            "Accept": "*/*",
        },
        method="POST",
    )

    progress(0.05, "requesting")
    audio = bytearray()
    try:
        with urllib.request.urlopen(req, timeout=600) as resp:
            total = resp.headers.get("Content-Length")
            total = int(total) if total and total.isdigit() else None
            progress(0.2, "generating")
            last = 0.0
            while True:
                chunk = resp.read(65536)
                if not chunk:
                    break
                audio.extend(chunk)
                if total:
                    frac = 0.2 + 0.65 * min(1.0, len(audio) / total)
                else:
                    frac = min(0.85, 0.2 + len(audio) / 5e6)
                if frac - last >= 0.02:
                    last = frac
                    progress(frac, "downloading")
    except urllib.error.HTTPError as e:
        detail = _error_detail(e.read())
        if e.code == 401:
            fail("ElevenLabs rejected the API key (HTTP 401): %s — check "
                 "ELEVENLABS_API_KEY" % detail)
        if e.code in (402, 429):
            fail("ElevenLabs quota/rate limit (HTTP %d): %s — check your plan "
                 "or retry later" % (e.code, detail))
        fail("ElevenLabs API error (HTTP %d): %s" % (e.code, detail))
    except urllib.error.URLError as e:
        fail("network error contacting ElevenLabs (%s): %s"
             % (base if base != DEFAULT_BASE_URL else "api.elevenlabs.io",
                getattr(e, "reason", e)))
    except TimeoutError:
        fail("ElevenLabs request timed out")

    if not audio:
        fail("ElevenLabs returned an empty audio response")

    progress(0.9, "writing")
    ensure_out_dir(out)
    if want_wav:
        channels = int(os.environ.get("ELEVENLABS_PCM_CHANNELS", "1") or 1)
        with wave.open(out, "wb") as w:
            w.setnchannels(max(1, channels))
            w.setsampwidth(2)
            w.setframerate(PCM_RATE)
            w.writeframes(bytes(audio))
        duration, rate = wav_info(out)
    else:
        with open(out, "wb") as f:
            f.write(audio)
        duration, rate = params.get("durationSeconds"), None

    result = {
        "kind": "elevenLabsMusic",
        "outputPath": os.path.abspath(out),
        "simulated": False,
    }
    if duration is not None:
        result["durationSeconds"] = duration
    if rate is not None:
        result["sampleRate"] = rate
    emit({"type": "done", "result": result})


# ---------------------------------------------------------------------------
# Self-test: simulate E2E + real HTTP path against a local mock server
# ---------------------------------------------------------------------------

def run_selftest():
    import subprocess
    import tempfile
    import threading
    from http.server import BaseHTTPRequestHandler, HTTPServer

    fake_pcm = struct.pack("<%dh" % PCM_RATE, *([0] * PCM_RATE))  # 1 s silence
    seen = {}

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            seen["path"] = self.path
            seen["key"] = self.headers.get("xi-api-key")
            length = int(self.headers.get("Content-Length", "0"))
            seen["body"] = self.rfile.read(length)
            key = seen["key"]
            if key == "good-key":
                self.send_response(200)
                self.send_header("Content-Type", "audio/pcm")
                self.send_header("Content-Length", str(len(fake_pcm)))
                self.end_headers()
                self.wfile.write(fake_pcm)
            elif key == "quota-key":
                body = json.dumps({"detail": {"status": "quota_exceeded",
                                              "message": "out of credits"}}).encode()
                self.send_response(429)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            else:
                body = json.dumps({"detail": "invalid api key"}).encode()
                self.send_response(401)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        def log_message(self, *a):  # keep the selftest output clean
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = "http://127.0.0.1:%d" % server.server_address[1]

    def run_case(name, params, env_key, expect, simulate=False):
        env = dict(os.environ)
        env.pop("ELEVENLABS_API_KEY", None)
        env["ELEVENLABS_BASE_URL"] = base  # belt & braces: never the real API
        if env_key is not None:
            env["ELEVENLABS_API_KEY"] = env_key
        argv = [sys.executable, os.path.abspath(__file__),
                "--params-json", json.dumps(params)]
        if simulate:
            argv.append("--simulate")
        proc = subprocess.run(argv, capture_output=True, text=True,
                              timeout=120, env=env)
        lines = [json.loads(l) for l in proc.stdout.splitlines() if l.strip()]
        terminal = [l for l in lines if l.get("type") in ("done", "error")]
        assert len(terminal) == 1, "%s: %r" % (name, terminal)
        assert terminal[0]["type"] == expect, "%s: %r" % (name, terminal[0])
        # The key must never leak into protocol output or stderr.
        if env_key:
            assert env_key not in proc.stdout and env_key not in proc.stderr, \
                "%s: API key leaked into worker output" % name
        return terminal[0]

    ok = 0
    with tempfile.TemporaryDirectory(prefix="eleven-selftest-") as td:
        sim = os.path.join(td, "sim.wav")
        t = run_case("simulate", {"prompt": "lofi beat", "durationSeconds": 2,
                                  "outputPath": sim}, None, "done",
                     simulate=True)
        assert t["result"]["simulated"] and abs(
            wav_info(sim)[0] - 2.0) < 0.05
        ok += 1

        real = os.path.join(td, "real.wav")
        t = run_case("mock-success", {"prompt": "cinematic strings",
                                      "durationSeconds": 12,
                                      "outputPath": real}, "good-key", "done")
        assert t["result"]["simulated"] is False
        d, r = wav_info(real)
        assert r == PCM_RATE and abs(d - 1.0) < 0.05, (d, r)
        assert seen["path"].startswith("/v1/music"), seen["path"]
        req_body = json.loads(seen["body"])
        assert req_body["prompt"] == "cinematic strings"
        assert req_body["music_length_ms"] == 12_000
        ok += 1

        t = run_case("missing-key", {"prompt": "x", "outputPath": real},
                     None, "error")
        assert "ELEVENLABS_API_KEY" in t["message"], t
        ok += 1

        t = run_case("bad-key", {"prompt": "x", "outputPath": real},
                     "bad-key", "error")
        assert "401" in t["message"], t
        ok += 1

        t = run_case("quota", {"prompt": "x", "outputPath": real},
                     "quota-key", "error")
        assert "quota" in t["message"].lower(), t
        ok += 1

        t = run_case("missing-prompt", {"outputPath": real}, "good-key",
                     "error")
        assert "prompt" in t["message"], t
        ok += 1

    server.shutdown()
    print("elevenlabs_music_worker selftest: %d/%d cases passed" % (ok, ok),
          file=sys.stderr)
    return 0


# ---------------------------------------------------------------------------

def main():
    p = argparse.ArgumentParser(description="AURA ElevenLabs Music sidecar")
    p.add_argument("--params-json")
    p.add_argument("--simulate", action="store_true")
    p.add_argument("--selftest", action="store_true")
    args = p.parse_args()

    if args.selftest:
        return run_selftest()
    if args.params_json is None:
        fail("--params-json is required")
    try:
        params = json.loads(args.params_json)
        if not isinstance(params, dict):
            raise ValueError("params must be a JSON object")
    except ValueError as e:
        fail("bad --params-json: %s" % e)

    validate_params(params)
    if args.simulate:
        run_simulate(params)
    else:
        run_real(params)
    return 0


if __name__ == "__main__":
    sys.exit(main())
