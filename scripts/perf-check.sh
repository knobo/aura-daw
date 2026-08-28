#!/usr/bin/env bash
#
# Run AURA's engine performance gate, with exit codes `git bisect run`
# understands.
#
#   0    under budget          -> bisect: this commit is GOOD
#   1    over budget           -> bisect: this commit is BAD
#   125  cannot judge          -> bisect: SKIP this commit
#
# 125 is the load-bearing one. A commit that does not compile, or that
# predates the harness, or that lacks the plugins a run needs, is not a slow
# commit — and marking it BAD hands the bisect a confidently wrong answer.
#
# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------
#
#   scripts/perf-check.sh --budget 520
#   scripts/perf-check.sh --budget 1400 --run full --tracks 32
#
# Find the budget first, on the machine you will be checking on:
#
#   scripts/perf-check.sh --measure
#
# then add headroom. 1.3x is a reasonable start; see "Noise" below, because
# on a laptop it may need to be considerably more.
#
# ---------------------------------------------------------------------------
# Bisecting
# ---------------------------------------------------------------------------
#
# The harness is a file in the tree, so at commits older than it the test
# target does not exist and `cargo test --test plugin_load_profile` fails —
# which, without the 125 protocol, marks every one of those commits BAD.
# `--harness-from <ref>` copies the current harness in at each step and
# removes it afterwards, so the SAME measurement runs at every commit:
#
#   cp scripts/perf-check.sh /tmp/perf-check.sh      # survives the checkouts
#   git bisect start <bad> <good>
#   git bisect run /tmp/perf-check.sh --budget 520 --harness-from main
#   git bisect reset
#
# Copy the script out of the tree first: bisect rewrites the working tree at
# every step, and a script that deletes itself mid-run is not a good time.
#
# ---------------------------------------------------------------------------
# Noise, and why the budget needs headroom
# ---------------------------------------------------------------------------
#
# The gate takes the FASTEST of N runs, because benchmark noise is one-sided
# — contention makes a run slower, never faster. That absorbs most of it.
#
# What it does not do is make the number absolute. On the i9-14900 this was
# developed on, ten invocations over unchanged code read 388-408 us, and one
# batch of four read 260-275. Cold and hot runs were indistinguishable, so
# it was not thermal; the cause was never found. A 1.5x unexplained step is
# larger than most regressions worth hunting.
#
# Three consequences, all practical:
#
#   * Measure the budget with THIS SCRIPT, not from the table in
#     `docs/GAP_ANALYSIS.md` §9 — that table was measured in a different
#     process shape and its numbers do not transfer.
#   * Measure it in the same sitting as the run you compare it against.
#   * Give the budget real headroom, and when a bisect fingers a commit,
#     confirm it by hand at that commit and its parent before believing it.
#
set -uo pipefail

BUDGET=""
RUN="bare"
TRACKS=32
RUNS=3
HARNESS_REF=""
MEASURE=0
HARNESS="src-tauri/tests/plugin_load_profile.rs"

die() { echo "perf-check: $*" >&2; exit 125; }

while [ $# -gt 0 ]; do
  case "$1" in
    --budget)        BUDGET="${2:-}"; shift 2 ;;
    --run)           RUN="${2:-}"; shift 2 ;;
    --tracks)        TRACKS="${2:-}"; shift 2 ;;
    --runs)          RUNS="${2:-}"; shift 2 ;;
    --harness-from)  HARNESS_REF="${2:-}"; shift 2 ;;
    --measure)       MEASURE=1; shift ;;
    -h|--help)       sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)               die "unknown argument: $1" ;;
  esac
done

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || die "not in a git repo"
cd "$ROOT" || die "cannot cd to $ROOT"

# `--measure` is "tell me the number", so it needs no budget. Any other
# invocation without one would silently pass everything.
if [ "$MEASURE" -eq 0 ] && [ -z "$BUDGET" ]; then
  die "no --budget given (and no --measure). Refusing to pass everything."
fi

# Put the harness in place if asked.
#
# The tree MUST come back exactly as found, in both directions. Bisect
# checks out the next commit itself, and it refuses to move while the tree
# is dirty — so a stray file (or a modified one we never put back) does not
# just leave mess, it halts the bisect several steps in, with the earlier
# results already discarded.
WROTE=0            # we created a file that was not there
MODIFIED=0         # we overwrote a file the commit owns
cleanup() {
  [ "$WROTE" -eq 1 ] && rm -f "$HARNESS"
  [ "$MODIFIED" -eq 1 ] && git checkout HEAD -- "$HARNESS" 2>/dev/null
  return 0
}
trap cleanup EXIT

if [ -n "$HARNESS_REF" ]; then
  git cat-file -e "$HARNESS_REF:$HARNESS" 2>/dev/null \
    || die "$HARNESS does not exist at $HARNESS_REF"

  # Refuse to eat uncommitted work. The restore below is `git checkout HEAD
  # -- <file>`, which would silently throw away edits in progress — and the
  # obvious way to reach this line is a developer testing the bisect recipe
  # while still working on the harness itself.
  if ! git diff --quiet -- "$HARNESS" 2>/dev/null; then
    die "$HARNESS has uncommitted changes; --harness-from would overwrite \
and then restore over them. Commit or stash first."
  fi

  if git cat-file -e "HEAD:$HARNESS" 2>/dev/null; then
    MODIFIED=1     # this commit has its own; ours replaces it for one run
  else
    WROTE=1        # this commit predates the harness; ours is a visitor
  fi
  mkdir -p "$(dirname "$HARNESS")"
  git show "$HARNESS_REF:$HARNESS" > "$HARNESS" || die "cannot write $HARNESS"
fi

# Build FIRST and separately. `cargo test` reports a compile error and a
# failing test with the same exit code, and telling them apart is the whole
# point of 125 — a commit that does not build is unjudgeable, not slow.
if ! cargo test --manifest-path src-tauri/Cargo.toml --release \
      --test plugin_load_profile --no-run >/dev/null 2>&1; then
  die "this commit does not build the harness (unjudgeable, not slow)"
fi

export AURA_PROFILE_RUN="$RUN"
export AURA_PROFILE_TRACKS="$TRACKS"
export AURA_PROFILE_RUNS="$RUNS"
# --measure wants the number, so nothing may fail on budget: an absurd
# ceiling keeps the assertion quiet while still printing the verdict.
export AURA_PROFILE_MAX_US="${BUDGET:-100000000}"

OUT=$(cargo test --manifest-path src-tauri/Cargo.toml --release \
        --test plugin_load_profile perf_budget_gate -- --nocapture 2>&1)
VERDICT=$(printf '%s\n' "$OUT" | grep -m1 "PERF-VERDICT:")

if [ -z "$VERDICT" ]; then
  printf '%s\n' "$OUT" | tail -20 >&2
  die "the gate printed no verdict (see above)"
fi
echo "$VERDICT"

case "$VERDICT" in
  *"PERF-VERDICT: SKIP"*) exit 125 ;;
  *"PERF-VERDICT: OK"*)   exit 0 ;;
  *"PERF-VERDICT: OVER"*) [ "$MEASURE" -eq 1 ] && exit 0 || exit 1 ;;
  *)                      die "unparseable verdict: $VERDICT" ;;
esac
