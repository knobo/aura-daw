# Next: plugin manager Step 7 (unified audition) from `origin/main`

Reply to the user in Norwegian — they write Norwegian; the repo
documentation is English.

`origin/main` is the baseline (`e1ec61f`, PR #93 squash). Cut a fresh
branch in its **own worktree** from it — `git worktree add
.worktrees/<name> -b <branch> origin/main` — never reusing an existing
worktree and never branching from whatever is checked out. See
[`CLAUDE.md`](CLAUDE.md) for why both halves matter. Do **not** reopen
`feat/plugin-manager` — it was squash-merged.

Read these before touching plugin UI:

- Winner spec (layout that shipped): [`docs/superpowers/specs/2026-08-20-plugin-admin-winner-design.md`](docs/superpowers/specs/2026-08-20-plugin-admin-winner-design.md)
- Original design (catalog, rack, chips, frozen IPC): [`docs/superpowers/specs/2026-08-20-plugin-manager-design.md`](docs/superpowers/specs/2026-08-20-plugin-manager-design.md)
- Plan (status table + Step 6 as shipped; Step 7 is one line there — its
  detail is design §8.2, the "Original design" link above): [`docs/superpowers/plans/2026-08-20-plugin-manager.md`](docs/superpowers/plans/2026-08-20-plugin-manager.md)

## The owner's standing steer (2026-08-20)

> "Ikke kjenn på begrensninger i UI design i forhold til hva som ligger der
> fra før. Hvis vi skal vinne må vi lage en løsning som er bedre enn hva
> andre har."

**Do not let the existing UI set the ceiling.** This overrides the usual
"follow existing patterns" instinct for UI work on this track — it does
NOT override the frozen IPC surface, the theme-token rule, or the test
gates.

## Landed — PR #93 (do not redo)

Squash `e1ec61f`. CI green (frontend + rust). `tauri dev` booted: MCP on
`:41717`, catalog seeded, persisted instances restored.

| What | Where |
|---|---|
| Timeline follow-on-seek | `view.revealSamples` |
| Shared `browser/` layer | instruments / samples / presets migrated |
| Lane multi-select + bulk M/S/A | `lane-bulk.ts`, ARIA grid |
| Plugin catalog | `plugin-catalog.json`, incremental scan, `plugin_catalog_*` |
| Plugin Manager | browse / **split** / rack. Split is default when the project has instances. Facets: ALL/INST/FX, format + category chips, ★/⏱. Dock widens to 480 on the plugins tab. |
| Ctrl+P quick-pick | frecency (`aura.plugin.frecency`). Click/Enter follow the descriptor (effects insert; Shift+Enter forces insert). |
| Native floating GUI | CLAP `clap.gui`, LV2 `ui:showInterface`, Zyn via `zynaddsubfx-ext-gui` (no `--embed`). Additive IPC: `plugin_show_gui`. GUI button next to PARAMS, hidden if no editor. |
| On-top pref | INTERFACE `pluginGuiOnTop` (default on). Live toggle: EWMH ClientMessage + `WM_TRANSIENT_FOR`. Additive IPC: `plugin_set_gui_on_top`. |
| Review follow-up in the squash | LV2 Drop moves DSP onto plugin-main and closes the editor before free; `make_node` re-attaches ext-gui; CLAP `gui_created` after `create`; `plugin_list` drops the registry lock before `gui_flags`. |
| Param cache + shared lane reveal (Step 6) | `plugins.svelte.ts` gained a lazy per-instance `paramCache` over the frozen `plugin_get_params` (no plugin needs to be open to read its param names/values any more); `utils/lane-reveal.ts`'s `revealParamLane` (unfold the lane, `modulation.pickTarget`, scroll `[data-track-id]` on `TrackHeader`'s root) is the one jump every chip below calls. Branch `feat/plugin-automation-inventory`, PR #98 (squash SHA pending — PR still open). |
| Automation matrix (Step 6) | `AutomationMatrix.svelte` + `utils/automation-matrix.ts` — the rack projection grouped by parameter instead of instance. Lives as a 4th `ManagerMode` (`"matrix"`) alongside BROWSE/SPLIT/RACK, not a second dock tab. |
| Pinned params (Step 6) | `PluginParamPanel.svelte`: every param row is a `ParamChip`; pinned ones (`catalog.pinnedParams[uid]`, max 8) sort into a `Pinned` section up top. A chip creates or jumps to the param's automation lane. |
| Lane plugin strip (Step 6) | `LanePluginStrip.svelte` + `utils/lane-strip.ts`, wired into `TrackHeader.svelte`'s `.metadata-row` (both the unfolded and folded/collapsed branches). The FX chip survived rather than being replaced — the strip sits between the instrument chip and FX, and its `+N` overflow button opens `InsertChain`. Folded lane renders dots only, no name button. |

**Scope calls already made:** no Zyn patches section in browse (virtualised
~1318-row list stays on `ZynPatchBrowser`); no second dock tab; no
`OP_FORMAT_VERSION` bump; favourite star talks only to catalog commands.

## Do this next

1. **Step 7 — unified audition** (design §8.2). Double-click any browser
   row to hear it, gated behind a new `browserAudition` pref defaulting
   **off**. Last on this track: it touches every browser plus sampler and
   plugin hosts.
2. **Owner ear-check still owed** on the live *Keep plugin GUI on top*
   toggle (open editor → Preferences off/on → window follows, no restart)
   and on SPLIT vs BROWSE/RACK. DPF may print `Parent Window Id missing`
   (expected: v1 has no XEmbed parent). Zyn may log `Sending key 'state'
   to UI failed` until the editor is actually open. **New from Step 6,
   also owed:** the lane strip at `--track-height: 132px` on a narrow
   window (does a long plugin/instrument name push the FX chip out of
   the row?); the folded-lane dots (status + name legible enough without
   a name button — this was a judgment call, not pixel-verified); and
   MATRIX mode (does grouping by parameter read as useful, does the
   fourth mode chip fit the row at 480 px).

**Native GUI leftovers (do not start unless asked):** X11 embed /
Wayland embed; out-of-process isolation (SCALABILITY step 4); LV2
`save_state` still snapshots the shadow instance, so OSC tweaks on the
live DSP are not what project save sees. `docs/ARCHITECTURE.md` §15.4
and `docs/SCALABILITY.md` step 3 do not yet list `plugin_show_gui` /
`plugin_set_gui_on_top` or mark step 3 paid — a docs-only follow-up.

**Do not:** rebuild PluginManager / Ctrl+P / catalog / floating GUI;
migrate `ZynPatchBrowser` onto `BrowserRow`/`BrowserSection`; bump
`OP_FORMAT_VERSION`; break a frozen command name; start Composer H2+
unless asked.

## Traps

- **`no-literals` lives at `src/lib/theme/no-literals.test.ts`**, not under
  `components/`. Raw colour literals fail CI; use theme tokens.
- **Global focus ring and `prefers-reduced-motion` are already in
  `src/app.css`.** Do not re-implement them per component.
- **`display: contents` is not safe here** (WebKitGTK). Use a real flex
  container.
- **Parallel `cargo test` intermittently SIGSEGVs** (Cardinal CLAP
  teardown). Use `-- --test-threads=1`. Never `cargo test` and `tauri
  dev` on the same `src-tauri/target/`.
- **If the dev log is 99% `[carla] lv2ui_extension_data(...)`, it is not
  this branch's fault and it is fixed** — `OpenLv2Gui` re-queried the LV2
  UI's show/idle interfaces on every 30 ms tick, ~66 Carla log lines a
  second per open editor. Now resolved once at open. If the pattern comes
  back, look for a new per-tick `extension_data` caller before assuming
  Carla is just noisy.
- **Before any new `*.dom.test.ts`**, read
  [`2026-08-18-dom-test-environment.md`](docs/superpowers/plans/2026-08-18-dom-test-environment.md).
  jsdom has no Pointer Capture, `getBoundingClientRect()` is zeros,
  Svelte 5 `$state` proxies throw on `structuredClone`.
- **A Svelte 5 `$effect` tracks reactive reads made anywhere inside its
  callback, including deep inside a function it merely calls.** Splitting
  a `$derived` so the effect only reads a narrower value is not enough by
  itself if the effect's body then calls something (e.g. a store method)
  that synchronously reads other reactive state internally — that read
  still gets attributed to the effect, so it re-fires on unrelated churn.
  Wrap the actual side-effecting call in `svelte`'s `untrack(...)`, after
  reading the narrowed dependency outside it. (`AutomationMatrix.svelte`
  and `LanePluginStrip.svelte` both hit this with `plugins.ensureParams`
  reading `paramCache`; both now follow the same
  read-then-`untrack`-the-call shape.)
- **`CSS` is not a global under the `unit` (node) vitest project.**
  `CSS.escape(...)` throws a `ReferenceError` before a stubbed
  `querySelector` is ever called; if that throw lands inside a `try/catch`
  the test can pass for the wrong reason (the "missing element" branch
  never actually runs). Stub `CSS.escape` explicitly alongside `document`
  in any test that exercises a `[data-*]` selector built with it.
- **`formatParamValue` special-cases any param name containing "cutoff"
  or "freq"** as a frequency (rounds to an integer + `"hz"` unit). A test
  fixture named e.g. `"Filter / Cutoff"` silently gets frequency
  formatting instead of the plain `toFixed(2)` you were expecting — pick a
  fixture name without those substrings unless you're testing the
  frequency path on purpose.

## Older context

The pre-existing briefing (Track D automation leftovers, Plan F undo
carry-forwards, the MIDI-output handoff, the Composer deprioritisation)
still applies. It is preserved below.

## Landed (do not restart)

| What | Pointer |
|---|---|
| **Plugin manager + native floating GUI** — catalog, browse/split/rack, Ctrl+P frecency, CLAP/LV2/Zyn editors, live on-top pref | PR #93 `e1ec61f`. Winner spec: [`2026-08-20-plugin-admin-winner-design.md`](docs/superpowers/specs/2026-08-20-plugin-admin-winner-design.md). Plan: [`2026-08-20-plugin-manager.md`](docs/superpowers/plans/2026-08-20-plugin-manager.md). Next is Step 6 (lane strip + matrix + pinned params), not a redo of the manager. |
| **Extract melody to MIDI** — segmenting audio clip pitch frames to editable MIDI clip, auto-creating/targeting MIDI tracks with undo, and selecting as Pitch Coach reference track | PR #91. Product doc: [`pitch-track.md`](docs/pitch-track.md) |
| **Pitch analysis action on clip view** — analyse clip button on selected audio clips, persisted APTF cache rebuild, teardown/generation guards, and PitchCoach report cache invalidation | PR #87 `25af6ae`. |
| **Write / Touch / Latch automation modes** — Off / Read / Write / Touch / Latch per track, real-time control-thread point recorder, single-op commit on stop/release with undo | PR #85 `d496903`. Design spec: [`2026-08-18-automation-write-touch-latch-design.md`](docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md). Plan: [`2026-08-18-automation-write-touch-latch.md`](docs/superpowers/plans/2026-08-18-automation-write-touch-latch.md) |
| **MIDI output — per-track and per-clip patchbay routing** | PR #84 `cbbc240`. Handoff: [`midi-output.md`](docs/midi-output.md) |
| **Undo / Version graph read-only browser** | PR #82 `b741251`. |
| **DOM test environment (jsdom)** — mounted component tests for `AutomationTrackRow` | PR #80 `ca67b7d`. |
| G1 Task 6 — PDC (`DelayLine` + `compile_pdc`) | PR #71 `9e0b884`. `RtTrack::pdc` is still `None` everywhere in production — wiring `compile_pdc`'s output into a graph rebuild is Task 7, not started. Code review before merge caught and fixed a real bug: the formula didn't handle a bypassed insert's latency correctly (would have jumped the mix on bypass toggle once Task 7 wired it in). Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| Clip Delete/Backspace after a plain pointer-click | PR #70 `7011e81`. Pointer-select never focused the clip element, so its own keydown handler never fired; a window-level fallback now reads `clipSelection` directly. Single-clip only — batch-deleting a multi-selection still needs a `clips_remove` transaction (Track C leftover, unchanged) |
| G1 Task 5 — mixer strip: source-sum → inserts REPLACE → shared fader (`InsertNode`/`compile_inserts`) | PR #66 `c99293b`. `compile_inserts` is not yet wired into `engine::rebuild` (Task 7) — an insert still isn't audible end-to-end. Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| **The Composer, Plan H1** — merged to `main` (PR #65, `63cb7fa`, 2026-08-18): a pure music-theory library, the harmony document in the core, five generators (progression, voice-led chords, bass, melody, groove), the COMPOSER panel, and a piano roll that tints its keys by what each note does over the chord. Owner ear-checked the same day: works; MIDI-out-to-Hydrogen bug found, needs its own unscoped PR. **Composer is now deprioritized — do not start H2+ unless asked.** | PR [#65](https://github.com/knobo/aura-daw/pull/65) (merged). Product doc: [`composer-assistant.md`](docs/backlog/composer-assistant.md). Plan + rulings H-1…H-12: [`2026-08-17-plan-h-composer.md`](docs/superpowers/plans/2026-08-17-plan-h-composer.md). Handoff: [`composer-h1.md`](docs/handoff/composer-h1.md). ARCHITECTURE §16 |
| **MIDI output — eight fixes** (re-cue storm, per-note channel, routed track no longer doubles its internal synth, undo-after-delete silence, forced-channel persistence, piano-roll note channel, ALSA re-address, live port list). An owner ear-check on a real drum machine is still owed. | PR #77 `55257e8`. Handoff: [`midi-out-note-channel.md`](docs/handoff/midi-out-note-channel.md) |
| **CI: apt no longer hangs the Rust job, and the Rust cache now exists.** The job was ~27 min of which the 1231 tests were 20 s; `apt-get update` had stalled 23.5 min and been cancelled. apt is now 3 min (`--no-install-recommends` + bounded retries), and `push: branches: [main]` finally lets `Swatinem/rust-cache` save a cache a PR can restore — without it every PR compiled ~600 crates from scratch. | PR #78 `2b00e91` |
| Lanes UX — rename, fold (lane + group), group, drag-reorder, and the timeline scroll/alignment fix | PR #60 `5f891cb`. Rebased onto phase 3 + the theme system; 10 code-review findings fixed before merge. Handoff: [`lanes-ux.md`](docs/handoff/lanes-ux.md) |
| Pitch Coach **phase 3** — per-note scoring, stored pitch curve, take report | PR #61 `c14916d`. [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Theme system — token contract, eight built-in themes, user themes from JSON | PR #63 `46df20d`. User docs: [`docs/themes.md`](docs/themes.md). `no-literals.test.ts` now guards every component's `<style>` block — a new component with a raw colour literal fails CI; use a token or a `theme-exempt:` comment. |
| G1 Tasks 2–4 — insert ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23`. Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| G1 Task 1 — `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Pitch Coach **phase 2** — panel, frame bus, lane geometry, prefs | PR #58 `7c3cb87`. Adds `pitch_unsubscribe` (additive). Owner ear-check done; the fixes it produced have NOT been re-checked by ear. **Next: phase 3, Task 12** — but read [`pitch-as-data.md`](docs/backlog/pitch-as-data.md) before Task 14 |
| Pitch Coach phase 1 | PR #49 `84b0313` + PR #54 `f451a5a` (detection off the RT callback, listen mid-take, `pitch_check`). R3 answered with numbers. [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Gesture tokens | PR #47 |
| External audio editor | PR #48 |
| MIDI launch v0.1 | PR #42 + #50. Hardware GATE / sustain still open: [`midi-launch.md`](docs/backlog/midi-launch.md) |
| Tracks A–F / Plan F | log: [`docs/handoff/landed-tracks.md`](docs/handoff/landed-tracks.md). Rulings: [`docs/PHASE4-PLAN.md`](docs/PHASE4-PLAN.md) |

## Open leftovers (open the pointer if you are on that item)

- **G1 Tasks 7–10** (the rest of the plan, HOLD until an automation/undo leftover above is done — owner steer, 2026-08-18): rebuild/offline wiring, IPC+UI, handoff. Do not jump to Task 9 UI before Task 7 wires the mixer into rebuild — an insert is still silent end-to-end until then (G-11 note in the handoff). Task 6's own known gap for Task 7 to pick up: PDC is applied after inserts but before the fader, so once wired, a track's automation ramps will read the wrong playhead position by its own PDC delay — documented in `pdc.rs`'s module doc.
- **G1 deferred minors**: listed in [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md).
- **MIDI-out to Hydrogen bug** — found during the Composer H1 ear-check
  (2026-08-18), not yet filed or scoped. Needs its own small PR; do not fold
  it into G-series or Composer work.
- **Sing-along from any song (Melody extraction landed)** — 4-step flow (import → split stems → extract melody to MIDI → sing in Pitch Coach) is now complete. Remaining pitch work: Arrangement pitch child lane, offline pitch correction, vocal calibration.
  [`pitch-track.md`](docs/pitch-track.md).
- **Pitch as data / Arrangement pitch lane** — see [`pitch-track.md`](docs/pitch-track.md) for the persistent child lane on audio tracks.
- **Pitch correction / auto-tune** — new owner ask (2026-08-17), NOT part of
  the Pitch Coach plan. Detection is done; a formant-preserving shifter and
  a correction policy are not. Offline-first staged path and the four
  rulings it needs before Stage A:
  [`pitch-correction-autotune.md`](docs/backlog/pitch-correction-autotune.md).
  Do not start Stage C (live insert) before G1 inserts + PDC.
- **Pitch Coach phase 3 landed** (PR #61, see the Landed table). Whatever
  is next for Pitch Coach lives in
  [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md)
  — read that file rather than this bullet, which is not being kept
  current for that track.
- **Uncommitted Windows-build work in the main checkout** — status
  UNCLEAR as of this update (2026-08-17): the main checkout has since
  moved from branch `docs/pitch-coach-handoff` to `main` and is now
  clean, with none of the files below present or tracked. It is not
  known from this session whether that work was committed elsewhere,
  intentionally dropped, or is sitting in a stash/another checkout —
  **check before assuming it is gone**. What follows is what a review
  found while the tree was still dirty on branch `docs/pitch-coach-handoff`
  (2026-08-17), recorded here because it exists nowhere else: a `lv2`
  Cargo feature with an `lv2_host_stub`, Windows CLAP search paths,
  platform-aware ffmpeg hints, a `release-windows.yml`, and
  `bundle.active: true`. Not the current session's work, and not in a PR.
  Five things the owner had not seen yet:
  1. **`src-tauri/icons/icon.ico` is untracked** while `tauri.conf.json`
     (a tracked modification) references it. `git commit -am` lands the
     reference without the asset and the first `v*` tag fails at the bundle
     step. `.github/workflows/release-windows.yml` is untracked too.
  2. **`bundle.active: true` with `targets: "all"`** enables bundling on
     every platform, so a local Linux `tauri build` now runs the AppImage
     bundler (which downloads `linuxdeploy` mid-build) and emits deb/rpm
     with no `depends` and no sidecar resources. Scope it via
     `tauri.windows.conf.json` or `"targets": ["nsis", "msi"]`.
  3. **`control/export.rs:291`** still hard-codes `apt install ffmpeg` in
     `capabilities()`, one function from the new platform-aware `HOW`.
  4. **`control/export.rs:880`/`:894`** assert on `"apt install ffmpeg"`, so
     `cargo test` is red on Windows — the platform being added. Assert on
     `"requires ffmpeg"` instead.
  5. **`.github/workflows/tests.yml:67`** only exercises the `lv2` axis on
     ubuntu; nothing in CI compiles the new `#[cfg(target_os = "windows")]`
     branches, which is exactly the rot the step's own comment warns about.
  Unverified by the current session beyond reading the diff — treat as
  leads, not verdicts.
- **Owner ear-checks** (no suite substitutes): automation fade during play (Track D); MIDI note-out / Hydrogen / keyboard record (Track B). Insert FX is not ear-checkable until Task 7 wires `compile_inserts` into `engine::rebuild` (Task 5 landed the mixer walk itself but nothing calls it yet). **Pitch Coach R3 is done** — `cargo run --example pitch_check` reproduces it; a sustained vowel gave no octave errors in 1312 frames. **Pitch Coach phase 2 was ear-checked 2026-08-17** and it found a real bug (the lane was not drawn while stopped, so the reference notes seemed to appear only on record). Fixed in the same PR — but the fix itself has not been heard yet, so one more pass with a microphone is owed.
- **Plan F carry-forwards:** live-document B-tree, I-1 option (a), no journal auto-apply. The read-only version-graph browser was implemented by PR #82 on `codex/undo-version-graph-ui`; the ordered follow-up is the guarded linear `Undo to here` contract documented in the Plan F handoff. Read the Plan F handoff in `docs/PHASE4-PLAN.md` before touching snapshots, the journal reader, the version graph, or `engine::rebuild`. Branch `plan-f-history` is kept so cited SHAs resolve.
- **Track D leftovers:** plugin-param bounce, write/touch/latch, no DOM test env. (Non-blocking CLAP param path closed 2026-08-18, PR #75, branch `clap-nonblocking-params` — see the Track D handoff. A post-merge `/code-review` found two accepted-not-fixed trade-offs from that PR, recorded in the Track D handoff and in both new tests' doc comments — not new work items unless someone picks them up: `post_params` posts into `plugin_main()`'s **unbounded** channel with no back-pressure, so a slow concurrent `run()` (any instantiate/save_state/remove, CLAP or LV2) can let automation writes pile up and drain as an audible catch-up burst — parity with an LV2 risk that already existed, not a new one, and a real fix is a `plugin_main()`-level design question out of scope for a param-write PR; and the two new timing tests wedge that same process-wide singleton, safe only under this repo's `--test-threads=1` CI convention. Follow-up doc+test commit: PR #76, branch `clap-nonblocking-params-followup` — **merged 2026-08-18.**) Details: `docs/PHASE4-PLAN.md` "Track D handoff" and [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md).
- **Modulation design §8** — the ordered path to the finished system (ports, modulators, macros, curve shapes, recording, sample-accurate plugin params, lazy expansion, per-voice modulation):
  [`docs/superpowers/specs/2026-08-15-modulation-system-design.md` §8](docs/superpowers/specs/2026-08-15-modulation-system-design.md#8-the-path-to-the-finished-system).
  ADR 0008. Handoff: `docs/PHASE4-PLAN.md` "Track F handoff". Do not restate §8 here (R2).
- **Plan E review leftovers:** M-8 (unowned). Closed items: [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md) — do not re-open them.
- **Track B / C leftovers** that are not ear-checks: recording under an active loop; multi-clip delete (batch `clips_remove` — single-clip Delete-after-click is now fixed, PR #70). See the matching PHASE4-PLAN handoff.

## Standing constraints (all work)

- **`src-tauri/src/theory/` is PURE and stays pure** (Plan H1, ruling H-5):
  no `tauri`, `parking_lot`, `std::fs`, `crate::control`, `crate::audio`, and
  no `thread_rng` — every generator takes an explicit `seed`. The only
  sanctioned crate dependency is `crate::midi::MidiNote`. A generator that
  needs project data is passed it. Same class of rule as the RT contract: it
  is what keeps that suite fast and that library reusable.
- **The harmony document is a map pair, not a track** (H-3/H-5): one op
  (`Op::HarmonySet`), no `rebuild` effect, `OP_FORMAT_VERSION` still 2,
  `schemaVersion` unmoved, and the `project.json` key written only when the
  document is non-empty. Do not add a `TrackKind` or a second harmony op.

These are load-bearing as of Gate E closing. Historical plans called
this `next-prompt.md` §2.

- **The op log is ON.** `journal.ndjson` is a **persisted format** from
  now on: `OP_FORMAT_VERSION` (**2** since the post-merge follow-up PR —
  base64 state blobs, I-5) is load-bearing the moment any project has a
  journal file. Additive `#[serde(default)]` fields on an op or on
  `TxMeta` stay non-breaking; anything else (renaming a field, changing a
  variant's shape, removing a path) needs a version bump AND a reader that
  understands both shapes. Plan F's journal reader (`control/replay.rs`)
  requires `v == 2` on batch lines (ruling F-5); v1 lines are skipped
  with a warn. A change of this kind now costs a migration.
- **Thin renderer** (ADR 0006) still holds: no new authoritative state,
  business logic, or time math lands frontend-side. Every frontend change
  is op emission, gesture emission, or UI/chrome.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive (the same rule that shaped every Plan E task).
- **`transact` closures must not panic.** Plan F landed panic
  *containment* (`catch_unwind` + restore from the pre-tx snapshot;
  ruling F-3) — that is a crash-consistency net, not a license.
  Validate before mutating, every time, exactly as every landed op arm
  does.
- **The M-3 redo invariant**: a transient write must never touch a
  document field an entry's `ops` can address, or a pending redo silently
  lands on a different state than the entry recorded. If you add a new
  transient transaction (transport-like, engine-thread, or gesture
  mid-flight), this is the check to run before shipping it. As of the
  post-merge follow-up PR a `debug_assert!` in the commit path enforces it
  in debug builds (transient ops may address only `ObjectRef::Transport`,
  unless the batch is a mid-gesture fold) — so a violation now fails the
  test suite instead of waiting to be noticed.
- **Undo is bounded**: 200 entries, in-memory, bottom-eviction, cleared at
  epochs (project open/create/save-as). The journal is unbounded and
  append-only; Plan F added a reader (detection + primitive, no
  auto-apply — ruling F-8).
- **Foreground timeout-guarded test runs** and the **dated-count
  convention**: any task that changes test counts updates README.md +
  CONTRIBUTING.md in the same commit, with the date. Current counts live
  there — measure, do not copy a number from a briefing.
- **Gesture lock order is gesture-before-session, everywhere** (Task 14's
  fix) — if a track adds a new gesture-shaped commit path, follow this
  order or reintroduce the TOCTOU that fix closed.

## Branch and worktree rules (all work)

- **New branches start from `origin/main`.** Never branch from a stale
  local main or from another track's branch.
- **Continuing branches merge `origin/main` in** whenever it has advanced,
  at a task boundary — don't let a long-running branch drift.
- **Never use bare `git stash`.** If you need to shelve work, commit it or
  use a named stash; bare `stash` has bitten prior sessions when a worktree
  switch lost track of it.
- **Use a dedicated git worktree per track** (`git worktree add <path> -b
  <branch> origin/main`, one path per track) so parallel tracks do not
  share a working tree.
- **Foreground test runs only, `timeout`-guarded.** No backgrounding a test
  run and moving on — every gate in this project has been foreground since
  Plan A.

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

## Index

- [`docs/backlog/00-ROADMAP-real-alternative.md`](docs/backlog/00-ROADMAP-real-alternative.md) — master product index.
- [`docs/handoff/`](docs/handoff/) — landed-track log and Plan E review log.
- [`docs/PHASE4-PLAN.md`](docs/PHASE4-PLAN.md) — orchestration + per-track handoffs (rulings, carry-forwards).
- [`docs/SIDE-CHANNEL-INVENTORY.md`](docs/SIDE-CHANNEL-INVENTORY.md) — landed inventory. L-4/L-5/R-3 CLOSED (Plan F); L-2 documented benign (F-9).
- [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md) — Plan G product cut.
- [`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`](docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md) — G1 plan. Implement that. Do not start G2.
- [`docs/backlog/hardware-midi-io.md`](docs/backlog/hardware-midi-io.md), [`multi-clip-selection-and-paste.md`](docs/backlog/multi-clip-selection-and-paste.md), [`automation-audible-and-ui.md`](docs/backlog/automation-audible-and-ui.md), [`library-and-browser.md`](docs/backlog/library-and-browser.md) — Tracks B/C/D/E product docs.
- [`docs/backlog/external-instrument-return.md`](docs/backlog/external-instrument-return.md) — MIDI-out's missing half. Read before adding "hidden tracks" or a PipeWire graph orchestrator.
- [`docs/CORE-REDESIGN-ROUND-2.md`](docs/CORE-REDESIGN-ROUND-2.md) §6 / ADR 0005 — Plan F spec.

When a leftover lands, update this file's current-job line, the landed
table, and the relevant handoff — not a new essay in this file.
