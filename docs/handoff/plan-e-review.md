# Plan E post-merge review — closed log and leftovers

Extracted from `next-prompt.md` on 2026-08-16 so the briefing stays
short. Historical plans said "read the review findings first"; they
now mean this file. **Do not re-open a closed item.**

The final whole-branch review of Plan E (`15c9909..27911d8`) is at
`.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/final-review-report.md`
(verdict: NEEDS FOLLOW-UP PR). Its **FIX NOW** triage list is done —
follow-up PR #18, `fix/plan-e-followup`.

## FIX NOW — all closed

- **C-1 (Critical)** — no epoch guard on `HistoryLog::record_commit`/
  `record_gesture`; a commit racing an epoch boundary journaled into the
  NEW project's file and pushed a live undo entry for the OLD document.
  → fixed in PR #18 (the urgent item; the other four were bundled
  behind it).
- **I-2** — LoopJam `watch_and_apply` busy-spun at 100 % CPU when a
  retryable `apply` kept failing with the transport stopped.
  → fixed in PR #18 (back-off + bounded retries + the mid-air-race
  test the Task 8 ledger asked for).
- **Task 13 deferral** — the deadlock audit's five stale `request`
  call-site line citations. → fixed in PR #18.
- **I-5 + L-1** — plugin state blobs serialized as JSON number arrays
  (~4x); `Op::PluginRemove.params` was captured but never read.
  → fixed in PR #18 (`OP_FORMAT_VERSION` 2, base64 blobs, apply
  seeds the mirror from the op on cold replay).
- **I-4's two caveats** — journal line order vs `rev` order under
  concurrency, and a panicking `transact` diverging log from document.
  → recorded as L-4/L-5 in `docs/SIDE-CHANNEL-INVENTORY.md` in PR
  #18; **both CLOSED by Plan F** (PR #23). L-4: reader sorts by
  `(epoch, rev)`, undo stack is rev-ordered. L-5: `catch_unwind` +
  snapshot restore.
- **M-3** — the transient/redo invariant was a comment checked by nothing.
  → fixed in PR #18 (a `debug_assert!` in the commit path).

## Held findings — closed unless named

Remaining unowned notes are **M-8** and the owner ear-checks. Read the
report first if you need the original wording.

- ~~**I-1** `save_project_as_epoch` writes only project.json + midi, so
  Save-As silently drops plugin `.state` blobs and automation chunks —
  and **I-7** a new/opened project inherits the previous project's plugin
  rows when `project.json` has no `plugins` key.~~ → **closed by Plan F
  Tasks 1–2** (I-1 option (b): Save-As writes plugin/automation/
  modulation into the new dir; I-7: adopt-on-open clears when the file
  has no `plugins` key). Residual: option (a) — flush outgoing persist
  before an epoch swap — is deferred (ruling F-6).
- ~~**I-3** `execute_host_forward` writes `status`/`params` with no op, no
  epoch guard, and no inventory residual (with **M-6**).~~ → **closed by
  Track D** (`061786b`): epoch guard + residual R-4 + the grep-gate
  enumeration corrected.
- ~~**I-6** `undo`/`redo` are sync Tauri commands and can block the UI
  thread on plugin re-instantiation + disk I/O.~~ → **closed by Plan F
  Task 4** (`async` + `spawn_blocking`, epoch-guarded, serialized by
  `history_gate`).
- ~~**C-1 residual** — `undo`/`redo` pop an entry, commit, then push it
  back; an epoch between pop and push resurrects it onto the new
  stack.~~ → **closed by Plan F Task 4** (`Committed.epoch` plumbed
  through undo/redo; mismatch drops the entry).
- ~~**I-8** inventory row 13 claims the per-knob `project.json` rewrite is
  closed; only its position moved off the lock, the frequency is
  unchanged.~~ → **closed by Track D** (`7ef1f70`/`feec7e9`/`bb20280`):
  gesture-scoped persist DEFERRAL (folding alone was not enough — a
  transient commit still runs its full `EngineEffect`), so a knob or lane
  drag is one undo entry and one `project.json` write; row 13's wording
  corrected.
- ~~**Minors M-1, M-2, M-4, M-5, M-7**~~ → **closed by Plan F**
  (Tasks 3 / 4 / 11 / 13). **M-8** (Figma oracle omitted derived
  fields) is still recorded, unowned. **M-9 is RESOLVED** by the
  review itself (`ClapNode::reset` verified to leave `steady_fallback`
  alone); close that ledger item.
- ~~**M-3 (frontend)** undo/redo re-pull misses automation and plugin
  panels.~~ → **closed by Track D** (`2a11ed0`).

## From Track D — still open

Details in `docs/PHASE4-PLAN.md`'s "Track D handoff".

- **The ear check is OWED.** Nobody has heard an automation lane change
  the volume during playback — the implementing agents had no audio
  device. It is the sole verification of `engine.rs:884`, the line the
  whole engine task exists for. Start the app, draw a fade on a track,
  press play, listen.
- **A bounce ignores PLUGIN-PARAM automation** and captures whatever the
  live host instance happens to hold. Track-gain lanes DO export
  correctly. The fix needs private per-render plugin instances — see the
  handoff and the note on `audio::offline::build_graph`.
- **The non-blocking CLAP param path.** `clap_host::set_params` is
  `plugin_main().run(…)`, a blocking round-trip; `lv2_host::set_params`
  already posts, so only CLAP blocks. Writes are batched per instance per
  tick, but an active ramp on two instances still costs ~1000 blocking
  round-trips/s onto the plugin-main thread that also serves the param
  panel, `instantiate` and `save_state`. Wanted: a fire-and-forget
  sibling to `set_params` plus a driver that uses it. **Owner: the
  plugin-host path.**
- **An automated plugin param is PINNED for the whole playthrough**, not
  just while a knob is held. Track F shipped the curve editor (lane
  picker + overlay), so a plugin param *can* have a drawn curve now —
  the remaining gap is write/touch/latch, not "no way to draw". Turn
  that param in the plugin's own GUI during playback and it snaps back
  within ~0.5 s and stays, while AURA's panel still shows the new
  value. Intended scope (automation overrides the knob), but say so
  plainly.
- ~~**`gesture_end` has no id — it closes whatever is open.**~~ →
  **closed by PR #47** (`a93cfa7`): `gesture_begin` returns the
  gesture's run id; `gesture_end(id)` no-ops on mismatch (omitting `id`
  keeps the old close-whatever contract). Async callers (plugin knobs,
  library stamp, lane/envelope delete+commit, clip-drag, faders, tempo)
  hold the token across the await.
- **No DOM test environment exists** (no jsdom/testing-library), so nothing
  inside a `.svelte` file is covered by any test. Both of Track D's real
  frontend bugs lived in event handlers and both were found by reading.
  Move async-ordering logic into a store where it can be tested.
- ~~**Two UI minors** — `movePoint` deleted a neighbour on a tick
  collision, and `.tog.auto.on` was byte-identical to `.tog.arm.on`.~~
  → **closed by PR #32** (`feat/lowhanging-fl-fruits`): a colliding
  drag keeps the neighbour's tick; automation-visible is magenta, ARM
  stays red. Piano-roll **Q / Shift+Q** (quantize) landed in the same
  PR.
