#!/usr/bin/env bash
#
# One-command dependency bootstrap for AURA.
#
# The prerequisites live in four different places (apt, rustup, the Node
# package manager, and a Python venv for the AI sidecars), and getting one of
# them wrong fails deep inside a Rust build with a pkg-config error that says
# nothing about which apt package is missing. This script checks all four and
# installs only what is absent.
#
# Everything here is idempotent: re-running it on a complete machine installs
# nothing and exits 0.
#
#   scripts/setup.sh                 # enough to build and run the app
#   scripts/setup.sh --check         # report only, change nothing
#   scripts/setup.sh --all           # + plugins, + Python sidecar stack
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# --- Package groups -----------------------------------------------------
#
# Kept in sync by hand with README.md#quickstart and .github/workflows/tests.yml.
# libjavascriptcoregtk-4.1-dev and libsoup-3.0-dev arrive as dependencies of
# libwebkit2gtk-4.1-dev on Ubuntu, but are listed explicitly because the build
# needs their .pc files and not every derivative pulls them in.
APT_BUILD=(
  libasound2-dev libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev
  libsoup-3.0-dev build-essential curl wget file libxdo-dev libssl-dev
  libayatana-appindicator3-dev librsvg2-dev
  liblilv-dev   # the LV2 host: `livi` links system lilv
  ffmpeg        # compressed import/export, and a real test dependency
)

# Optional instruments. Each un-skips a gated test suite; without them those
# tests skip cleanly rather than fail.
APT_PLUGINS=(
  zynaddsubfx-lv2         # LV2 acceptance + state round-trip
  dpf-plugins-clap        # CLAP lifecycle
  dpf-plugins-lv2
  mda-lv2                 # LV2 params
)

MIN_NODE_MAJOR=18
# Node 25 ships an always-on built-in `localStorage` global that shadows
# jsdom's inside vitest's jsdom environment, leaving a stub without `.clear()`.
# The browser-folds DOM tests fail on 25 and pass on 18-24 (measured). Keep in
# sync with the `engines` field in package.json.
MAX_NODE_MAJOR=24
VENV=".venv-sidecars"
VENV_PYTHON="3.12"

# --- Options ------------------------------------------------------------
WANT_PLUGINS=0
WANT_PYTHON=0
WANT_MODELS=0
CHECK_ONLY=0
ASSUME_YES=0

usage() {
  cat <<'EOF'
Usage: scripts/setup.sh [OPTIONS]

With no options: installs the system packages, Node dependencies and Rust
toolchain needed to build and run AURA.

  --plugins   also install the LV2/CLAP instruments the gated plugin tests need
  --python    also create .venv-sidecars with the CPU-only AI sidecar stack
  --models    also install the heavy GPU model stack (torch/Demucs/ACE-Step);
              implies --python
  --all       everything above
  --check     report what is missing and install nothing (exit 1 if incomplete)
  --yes       never prompt; assume yes (for CI and unattended runs)
  -h, --help  this message

Without any model stack, run the app with AURA_SIDECAR_SIMULATE=1 and every AI
worker produces deterministic placeholder output.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --plugins) WANT_PLUGINS=1 ;;
    --python)  WANT_PYTHON=1 ;;
    --models)  WANT_PYTHON=1; WANT_MODELS=1 ;;
    --all)     WANT_PLUGINS=1; WANT_PYTHON=1; WANT_MODELS=1 ;;
    --check)   CHECK_ONLY=1 ;;
    --yes|-y)  ASSUME_YES=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

# --- Output -------------------------------------------------------------
if [ -t 1 ]; then
  B=$'\033[1m'; DIM=$'\033[2m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'
  RED=$'\033[31m'; R=$'\033[0m'
else
  B=''; DIM=''; GREEN=''; YELLOW=''; RED=''; R=''
fi

MISSING=0   # set by check-only mode so we can exit non-zero at the end

section() { printf '\n%s==> %s%s\n' "$B" "$1" "$R"; }
ok()      { printf '  %s✓%s %s\n' "$GREEN" "$R" "$1"; }
warn()    { printf '  %s!%s %s\n' "$YELLOW" "$R" "$1"; }
fail()    { printf '  %s✗%s %s\n' "$RED" "$R" "$1"; }
note()    { printf '    %s%s%s\n' "$DIM" "$1" "$R"; }

# Records something as absent. In --check mode this is what makes us exit 1.
absent() { fail "$1"; MISSING=1; }

confirm() {
  [ "$ASSUME_YES" = 1 ] && return 0
  [ -t 0 ] || return 1          # non-interactive without --yes: decline
  local reply
  read -r -p "    $1 [y/N] " reply
  [[ "$reply" =~ ^[Yy] ]]
}

# --- System packages ----------------------------------------------------
# Prints one missing package per line, and nothing at all when the whole group
# is installed — mapfile on a printf of an empty array would yield one empty
# element and read as "1 missing".
apt_missing() {
  local pkg
  for pkg in "$@"; do
    if [ "$(dpkg-query -W -f='${db:Status-Status}' "$pkg" 2>/dev/null)" != installed ]; then
      printf '%s\n' "$pkg"
    fi
  done
}

install_apt_group() {
  local label="$1"; shift
  local missing
  mapfile -t missing < <(apt_missing "$@")

  if [ ${#missing[@]} -eq 0 ]; then
    ok "$label: all $# packages installed"
    return 0
  fi

  absent "$label: ${#missing[@]} missing — ${missing[*]}"
  [ "$CHECK_ONLY" = 1 ] && return 0

  # --no-install-recommends: the recommends of this list are enormous and
  # unrelated (ffmpeg alone drags in a speech model). Nothing here needs them.
  local cmd=(sudo apt-get install -y --no-install-recommends "${missing[@]}")
  note "${cmd[*]}"
  if confirm "Install with apt?"; then
    sudo apt-get update
    "${cmd[@]}"
    ok "$label installed"
  else
    warn "skipped — run the command above when you're ready"
  fi
}

section "System packages (apt)"
if ! command -v dpkg-query >/dev/null 2>&1; then
  warn "not a dpkg-based system; install these yourself: ${APT_BUILD[*]}"
else
  install_apt_group "Tauri/audio build deps" "${APT_BUILD[@]}"
  if [ "$WANT_PLUGINS" = 1 ]; then
    install_apt_group "Plugin test instruments" "${APT_PLUGINS[@]}"
  else
    note "plugin instruments not requested (--plugins); gated plugin tests will skip"
  fi
fi

# --- Rust ---------------------------------------------------------------
section "Rust toolchain"
if command -v cargo >/dev/null 2>&1; then
  ok "$(cargo --version)"
else
  absent "cargo not found"
  if [ "$CHECK_ONLY" != 1 ]; then
    note "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    if confirm "Install the stable toolchain via rustup?"; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      # shellcheck disable=SC1090,SC1091
      . "$HOME/.cargo/env"
      ok "$(cargo --version)"
    else
      warn "skipped — the Rust build will not work until cargo is on PATH"
    fi
  fi
fi

# --- Node ---------------------------------------------------------------
section "Node dependencies"
if ! command -v node >/dev/null 2>&1; then
  absent "node not found — install Node ${MIN_NODE_MAJOR}+ (nvm, fnm, or your distro)"
else
  node_major="$(node -v | sed 's/^v\([0-9]*\).*/\1/')"
  if [ "$node_major" -lt "$MIN_NODE_MAJOR" ]; then
    absent "node $(node -v) is older than the required v${MIN_NODE_MAJOR}"
  elif [ "$node_major" -gt "$MAX_NODE_MAJOR" ]; then
    warn "node $(node -v) is newer than the supported v${MAX_NODE_MAJOR}"
    note "the app builds and runs, but the jsdom tests fail: Node 25's built-in"
    note "localStorage shadows jsdom's. Use \`nvm use\` (.nvmrc pins ${MAX_NODE_MAJOR}) to run npm test."
  else
    ok "node $(node -v)"
  fi

  # Both lockfiles are committed. npm is what CI runs, so it wins unless the
  # tree was already installed by pnpm — switching managers mid-tree means a
  # full reinstall for no gain.
  if [ -d node_modules/.pnpm ] && command -v pnpm >/dev/null 2>&1; then
    PM=pnpm; PM_INSTALL=(pnpm install)
  else
    PM=npm;  PM_INSTALL=(npm install)
  fi

  if [ "$CHECK_ONLY" = 1 ]; then
    if [ -d node_modules ]; then
      ok "node_modules present (${PM})"
    else
      absent "node_modules missing — run: ${PM_INSTALL[*]}"
    fi
  else
    note "${PM_INSTALL[*]}"
    "${PM_INSTALL[@]}"
    ok "JavaScript dependencies installed (${PM})"
  fi
fi

# --- Python sidecars ----------------------------------------------------
section "Python AI sidecars"
if [ "$WANT_PYTHON" != 1 ]; then
  if [ -d "$VENV" ]; then
    ok "$VENV exists"
  else
    note "not requested (--python); run with AURA_SIDECAR_SIMULATE=1 for placeholder AI output"
  fi
elif ! command -v uv >/dev/null 2>&1; then
  absent "uv not found — see https://docs.astral.sh/uv/getting-started/installation/"
elif [ "$CHECK_ONLY" = 1 ]; then
  if [ -x "$VENV/bin/python" ]; then ok "$VENV ready"; else absent "$VENV missing"; fi
else
  # The venv must live at the repo root under exactly this name: the
  # hum-to-song sidecar looks for <repo>/.venv-sidecars/bin/python by path,
  # PATH or not (src-tauri/src/control/hum.rs).
  [ -x "$VENV/bin/python" ] || uv venv "$VENV" --python "$VENV_PYTHON"

  VIRTUAL_ENV="$REPO_ROOT/$VENV" uv pip install -r sidecars/requirements.txt
  ok "CPU sidecar stack installed into $VENV"

  if [ "$WANT_MODELS" = 1 ]; then
    warn "installing the GPU model stack — this downloads several GB"
    # torch must come from the cu126 index before anything else pulls a
    # CPU-only wheel in as a transitive dependency.
    VIRTUAL_ENV="$REPO_ROOT/$VENV" uv pip install \
      --index-url https://download.pytorch.org/whl/cu126 torch torchaudio
    VIRTUAL_ENV="$REPO_ROOT/$VENV" uv pip install -r sidecars/requirements-models.txt
    ok "model stack installed"
  else
    note "heavy model stack skipped (--models installs torch/Demucs/ACE-Step)"
  fi
fi

# --- Summary ------------------------------------------------------------
section "Next steps"
if [ "$CHECK_ONLY" = 1 ] && [ "$MISSING" = 1 ]; then
  fail "some dependencies are missing — re-run without --check to install them"
  exit 1
fi

cat <<EOF
  ${B}npx vite${R}                  browser-only demo, no Rust build
  ${B}npm run tauri dev${R}         the full app (vite on port 1420, strictPort)

  With the Python stack, prepend the venv so sidecar jobs find it:
  ${B}PATH="\$PWD/$VENV/bin:\$PATH" npm run tauri dev${R}

  Without it: ${B}AURA_SIDECAR_SIMULATE=1${R} makes every AI worker emit
  deterministic placeholder output.
EOF
