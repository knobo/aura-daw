# AURA — working agreements

## Start every branch in its own worktree, cut from `origin/main`

```sh
git fetch origin
git worktree add .worktrees/<short-name> -b <branch-name> origin/main
```

Both halves of that command matter, and each one has already cost this
project a session:

- **From `origin/main`, not from whatever is checked out.** A branch cut
  from a local branch inherits everything that branch carries. When the
  parent was squash-merged, `git log origin/main..HEAD` reports commits
  that are "ahead" but whose content is already in `main` — the diff is
  empty and there is nothing to push, which is confusing to discover
  halfway through a review.

- **Its own worktree, not a reused one.** Two tasks sharing a worktree
  edit the same files. The second one to touch a file silently lands on
  top of the first, and `git status` cannot tell you which change belonged
  to which task. Bulk operations make it worse: a rebase rewrites every
  file's mtime at once, so timestamps stop being evidence of who changed
  what.

A worktree whose branch has merged is **spent**. Do not reopen it and do
not branch from it — cut a fresh one from `origin/main`. Stale worktrees
for merged branches are harmless to leave lying around; `git worktree
list` shows them and `git worktree remove` cleans them up.

Name the worktree after the work (`.worktrees/automation-matrix`), not
after the ticket or the date.

## Claim the job in `next-prompt.md` before you start it

Read [`next-prompt.md`](next-prompt.md) first. It is the dispatcher: what
is claimed, what is free, and which backlog file holds the detail.

Add your row to its *Active claims* table, commit that **as the first
commit on your branch**, push, and open the PR — draft is fine. Only then
start implementing. Delete the row again in the last commit before you
merge: merging does not clean the table up for you, and a row pointing at
a branch that no longer exists is indistinguishable from live work.

This is not bookkeeping. On 2026-08-21 two agents worked the same task in
the same worktree at the same time; one finished, gates green, and
discovered its files being reverted underneath it by the other. Nothing
in git could say which change belonged to which agent, and one session's
work was thrown away. A claim is what makes that visible before the work
starts rather than after.

An unpushed claim is not a claim. And because a claim only reaches
`main` when its PR merges, checking for in-flight work means checking the
open PRs and remote branches too:

```sh
gh pr list --state open
git ls-remote --heads origin
git worktree list
```

## If you touch the render path, measure it

`src-tauri/src/audio/` — `mixer.rs`, `rt.rs`, `insert.rs`, `bus.rs`,
`pdc.rs`, `offline.rs` — and `midi/playback.rs` are the per-block path.
Adding work there, or to what the graph compiler emits, means running
the gate on `origin/main` and on your branch **in the same sitting**,
and quoting both numbers in the PR:

```sh
scripts/perf-check.sh --measure                 # on origin/main
scripts/perf-check.sh --budget <that x 1.3>     # on your branch
```

It needs no plugins installed and takes about ten seconds. The pair is
the evidence; a single number is not, because the same unchanged code has
measured 260–408 µs on one machine across a day.
[`docs/STANDING-CONSTRAINTS.md`](docs/STANDING-CONSTRAINTS.md)
§Performance has the bisect recipe and the reasoning;
[`docs/GAP_ANALYSIS.md`](docs/GAP_ANALYSIS.md) §9 is what normal looks
like.

## Keep `next-prompt.md` small

Detail belongs in [`docs/backlog/`](docs/backlog/), one file per track;
rules that bind all work belong in
[`docs/STANDING-CONSTRAINTS.md`](docs/STANDING-CONSTRAINTS.md); things
that cost you time belong in [`docs/TRAPS.md`](docs/TRAPS.md); what
merged belongs in [`docs/LANDED.md`](docs/LANDED.md). If you are writing
a paragraph into `next-prompt.md`, it belongs somewhere else.
