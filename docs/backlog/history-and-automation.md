# Backlog — history, undo, automation, modulation

Rulings and per-track handoffs live in
[`../PHASE4-PLAN.md`](../PHASE4-PLAN.md). Read the Plan F handoff there
**before touching snapshots, the journal reader, the version graph, or
`engine::rebuild`.** Plan F spec:
[`../CORE-REDESIGN-ROUND-2.md`](../CORE-REDESIGN-ROUND-2.md) §6 / ADR
0005. Landed inventory:
[`../SIDE-CHANNEL-INVENTORY.md`](../SIDE-CHANNEL-INVENTORY.md) — L-4/L-5/R-3
CLOSED (Plan F); L-2 documented benign (F-9).
Product doc: [`automation-audible-and-ui.md`](automation-audible-and-ui.md).

## Open

- **Plan F carry-forwards:** live-document B-tree, I-1 option (a), no
  journal auto-apply. The read-only version-graph browser landed (PR #82,
  branch `codex/undo-version-graph-ui`); the guarded linear `Undo to here`
  contract documented in the Plan F handoff landed too (PR #107,
  2026-08-23). Branch `plan-f-history` is kept so cited SHAs resolve.
- **`Undo to here` leftovers** (PR #107's own reviews recorded these as
  accepted, not fixed — **not work items unless someone picks them up**).
  `history_gate` serialises undo, redo and a walk against each other but
  NOT against ordinary commits, so a walk *notices* an interleaving
  rather than preventing it: `pop_undo_if` refuses the step whose
  revision is no longer on top, and the steps already applied stay
  applied. All-or-nothing would need a different mechanism — a snapshot
  restore, or gating commits on `history_gate` — and that is a design
  question, not a follow-up patch. Two smaller ones: the per-step
  epoch-mismatch branch has no test (it is reachable with the same
  event-emitter device `undo_to_stops_when_a_commit_lands_between_two_steps_of_the_walk`
  already builds), and `History::push_undo_unchanged`'s doc still claims
  "nothing can have been recorded in between", which the same
  ungated-commit premise weakens — a racer landing between a pop and a
  failed step's push-back leaves `undo` momentarily non-ascending. That
  window predates PR #107 and exists identically in `undo`'s and
  `redo`'s failure arms.
- **Track D leftovers:** plugin-param bounce; write/touch/latch for PLUGIN
  PARAMS (the track-gain half landed, PR #85). The panel-follow leftover is
  closed — the engine publishes what its driver wrote
  (`MeterFrame.drivenParams`) and the param panel paints it, PR #108. The
  non-blocking CLAP param path closed 2026-08-18 (PR #75, branch
  `clap-nonblocking-params`; doc+test follow-up PR #76 on branch
  `clap-nonblocking-params-followup` merged the same day). A post-merge review recorded two accepted-not-fixed trade-offs —
  **not new work items unless someone picks them up**: `post_params`
  posts into `plugin_main()`'s unbounded channel with no back-pressure,
  so a slow concurrent `run()` (any instantiate/save_state/remove, CLAP
  or LV2) can let automation writes pile up and drain as an audible
  catch-up burst — parity with an existing LV2 risk, and a real fix is a
  `plugin_main()`-level design question; and the two new timing tests
  wedge that same process-wide singleton, safe only under this repo's
  `--test-threads=1` convention. Details: `PHASE4-PLAN.md` "Track D
  handoff" and [`../handoff/plan-e-review.md`](../handoff/plan-e-review.md).
- **Modulation design §8** — the ordered path to the finished system
  (ports, modulators, macros, curve shapes, recording, sample-accurate
  plugin params, lazy expansion, per-voice modulation):
  [`2026-08-15-modulation-system-design.md` §8](../superpowers/specs/2026-08-15-modulation-system-design.md#8-the-path-to-the-finished-system).
  ADR 0008. Handoff: `PHASE4-PLAN.md` "Track F handoff". Do not restate
  §8 elsewhere (R2).
- **Plan E review leftovers:** M-8 (unowned). Closed items are in
  [`../handoff/plan-e-review.md`](../handoff/plan-e-review.md) — do not
  re-open them.
- **Track B / C leftovers** that are not ear-checks: recording under an
  active loop; multi-clip delete (batch `clips_remove` — single-clip
  Delete-after-click is fixed, PR #70). See the matching PHASE4-PLAN
  handoff and [`multi-clip-selection-and-paste.md`](multi-clip-selection-and-paste.md).

## Ear-check

Automation fade during play (Track D).

Eye-check owed with it, on the same playthrough (PR #108): draw a curve on
a hosted plugin's param, open its param panel, press play. The fader should
follow the lane in magenta with an AUTO flag beside the chip, and drop back
to the stored value the moment the transport stops. The suite proves the
engine publishes the values and the panel paints what it is handed; nobody
has yet watched it move.
