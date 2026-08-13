#!/usr/bin/env python3
"""Capture the hardware/software environment alongside the measurements."""
import json, subprocess, platform, os

os.chdir(os.path.join(os.path.dirname(__file__), ".."))

def sh(c):
    try:
        return subprocess.run(c, shell=True, capture_output=True, text=True, timeout=15).stdout.strip()
    except Exception as e:
        return f"err: {e}"

env = {
    "date": sh("date -Is"),
    "os": sh("grep PRETTY_NAME /etc/os-release | cut -d'\"' -f2"),
    "kernel": platform.release(),
    "cpu": sh("lscpu | grep 'Model name' | sed 's/Model name: *//'"),
    "ramGiB": sh("awk '/MemTotal/{printf \"%.1f\", $2/1048576}' /proc/meminfo"),
    "gpus_lspci": sh("lspci | grep -iE 'vga|3d'").split("\n"),
    "glx_renderer": sh("glxinfo | grep 'OpenGL renderer string' | cut -d: -f2").strip(),
    "mesa": sh("glxinfo | grep 'OpenGL core profile version string' | cut -d: -f2").strip(),
    "xrandr_providers": sh("xrandr --listproviders").split("\n"),
    "displays_active": sh("xrandr | grep ' connected'").split("\n"),
    "session": {"type": "X11", "wm": sh("wmctrl -m 2>/dev/null | head -2")},
    "webkit2gtk": sh("pkg-config --modversion webkit2gtk-4.1"),
    "gtk3": sh("pkg-config --modversion gtk+-3.0"),
    "wry": "0.55.1 (pinned == src-tauri/Cargo.lock)",
    "tao": "0.35.3 (pinned == src-tauri/Cargo.lock)",
    "rust": sh("rustc --version"),
    "nvidia": sh("nvidia-smi -L"),
}
with open("results/env.json", "w") as f:
    json.dump(env, f, indent=2)
print(json.dumps(env, indent=2))
