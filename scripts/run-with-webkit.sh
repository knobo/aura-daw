#!/usr/bin/env bash
# Run AURA against a locally built WebKitGTK instead of the distro's.
#
# Ubuntu 24.04 ships WebKitGTK 2.52.3 and will not ship the 2.53 development
# series at all, so testing a newer webview means building one. This script
# does not build it — see docs/backlog/webkitgtk-2.53.md — it only points a
# single run at a prefix that already holds one. Nothing is installed
# system-wide and the distro copy is never replaced: the selection is three
# environment variables and it lasts exactly as long as this process.
set -euo pipefail

PREFIX="${WEBKIT_PREFIX:-/opt/webkitgtk-2.53.91}"
LIBDIR="$PREFIX/lib/x86_64-linux-gnu"
[ -d "$LIBDIR" ] || LIBDIR="$PREFIX/lib"

if [ ! -e "$LIBDIR/libwebkit2gtk-4.1.so.0" ]; then
  echo "no libwebkit2gtk-4.1.so.0 under $LIBDIR — is $PREFIX built and installed?" >&2
  exit 1
fi

export PKG_CONFIG_PATH="$LIBDIR/pkgconfig:${PKG_CONFIG_PATH:-}"
export LD_LIBRARY_PATH="$LIBDIR:${LD_LIBRARY_PATH:-}"

# The web and network process binaries are found through a path compiled into
# the library at build time (LIBEXEC_INSTALL_DIR), so a prefix built with
# -DCMAKE_INSTALL_PREFIX="$PREFIX" already resolves its own helpers. Set the
# override anyway: if the prefix moved after it was built, the compiled-in
# path is stale and the web process fails to spawn with a message that does
# not mention paths at all.
export WEBKIT_EXEC_PATH="$PREFIX/libexec/webkit2gtk-4.1"

echo "webkit: $(pkg-config --modversion webkit2gtk-4.1) from $LIBDIR" >&2

cd "$(dirname "$0")/.."

# Never share CARGO_TARGET_DIR with the main checkout or another worktree.
# This build links against a different webview than every other worktree does,
# so a shared target dir means the two configurations overwrite each other's
# target/debug/aura and force a rebuild of the gtk/webkit sys crates on every
# switch. Worse, it is invisible: the SONAME is identical either way, so
# nothing fails loudly - the other worktree just silently rebuilds. That
# happened on 2026-08-30 while another agent was running the app from
# plan-v2-players.
#
# This assignment is unconditional on purpose. ~/.bashrc on this machine
# exports CARGO_TARGET_DIR=/home/knobo/prog/dav/target for every shell, so a
# ":-" default never fires and every worktree quietly shares one target dir
# no matter what CLAUDE.md says about isolation. Set AURA_SHARE_TARGET_DIR=1
# if you actually want the shared one.
if [ "${AURA_SHARE_TARGET_DIR:-0}" != "1" ]; then
  export CARGO_TARGET_DIR="$PWD/target"
fi

# Same PATH rule as scripts everywhere else in this repo: .venv-sidecars/bin
# must come first or resolve_python() silently picks system python3.
PATH="$PWD/.venv-sidecars/bin:$PATH" exec npm run tauri dev -- "$@"
