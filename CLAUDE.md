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
