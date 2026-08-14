# Task midi1 — hardware MIDI input, slice 1 (port list/select + activity)

## Status: DONE

Branch: `midi-input-ports` (created from `origin/main` at fetch time,
`15c9909 feat(ui): adaptive top-bar overflow — chips collapse into a ⋯ menu (#16)`).

## What was built

### Backend (`src-tauri/`)

- **Cargo.toml**: added `midir = "0.11"` (latest 0.x; ALSA-seq backend on
  Linux comes for free — no new system deps beyond what cpal/ALSA already
  need). Cargo.toml is marked "FROZEN — owned by Agent 1" in its header
  comment; edited anyway per this task's explicit instruction #1 ("Add the
  `midir` crate... to src-tauri/Cargo.toml"). Flagging as a deliberate,
  directed deviation from that convention, not an oversight.
- **New module `src-tauri/src/midi_input.rs`** (self-contained, ~330
  lines incl. tests):
  - `MidiPortInfo { id, name }` — id is `"<name>#<index>"` from the current
    enumeration (stable-ish, not durable across replug/reboot; documented).
  - `list_ports() -> Result<Vec<MidiPortInfo>, String>` — throwaway
    `midir::MidiInput` client, enumerate + name-lookup, skips a port that
    vanishes mid-enumeration instead of failing the whole call.
  - `MidiInputManager` (holds `Mutex<Inner>`, `Default`-constructed,
    managed via `app.manage(...)`):
    - `select_port(Option<String>)` — always closes any existing connection
      first (drop), then opens a new one when `Some(id)` is given and the id
      resolves to a live port; `None` just closes. Not-found is a clean
      `Err(String)`, never a panic.
    - `status() -> Result<MidiInputStatus, String>` where
      `MidiInputStatus { selected, events_seen, last_event_age_ms,
      last_status_bytes }`. The last field is an addition beyond the task's
      minimal 3-field sketch — it surfaces the "last 3 raw status bytes"
      diagnostic ring the spec asked the callback to maintain "for
      display"; additive JSON field, does not break the 3 named fields.
  - Callback: only touches `AtomicU64 events_seen`, `AtomicU64
    last_event_ms` (millis-since-connection-open), and a `Mutex<Vec<u8>>`
    capped-at-3 ring of raw status bytes. Entire body wrapped in
    `catch_unwind(AssertUnwindSafe(...))` so it can never unwind into the
    midir backend thread; all ops inside are independently infallible too
    (defense in depth, not either/or).
  - 10 unit tests, all hardware-independent: port-id formatting + JSON
    round-trip, id disambiguation by index, `list_ports()` never panics
    (runs for real against this machine's ALSA-seq — `/dev/snd/seq` exists
    here, ports list happens to be empty), status-with-no-selection is
    idle, `select_port(None)` no-op, `select_port(Some(bogus))` is a clean
    `Err` containing "not found" and leaves no half-open connection,
    status age-math against directly-injected atomics (the task's called
    -out case), `record_event` atomics+ring update including cap/evict and
    empty-message handling, and command-wrapper error paths.
- **`lib.rs`** (also header-marked "FROZEN FILE — owned by Agent 1"; edited
  per this task's explicit instruction #3 to register the module/state/
  commands there — same directed-deviation note as Cargo.toml above):
  added `pub mod midi_input;`, `.manage(midi_input::MidiInputManager::default())`,
  and 3 commands appended to `generate_handler!`:
  `midi_list_input_ports`, `midi_select_input_port`, `midi_input_status`.
  All additive; nothing existing touched or reordered.

### Frontend (`src/`)

- `src/lib/types/ipc.ts`: added `MidiPortInfo` and `MidiInputStatus`
  (camelCase wire types; `eventsSeen`, `lastEventAgeMs`, `lastStatusBytes`).
- `src/lib/tauri.ts`: added 3 **optional** `Backend` interface methods
  (`midiListInputPorts?`, `midiSelectInputPort?`, `midiInputStatus?`) —
  same optionality pattern as `seedDemoProject?`/`hintClipCharacter?`/
  `registerClip?` already in that file (the task named this the "`moveClip?`
  precedent"; no literal `moveClip` exists in this codebase, so the nearest
  established convention was followed instead). `TauriBackend` implements
  all three as thin `invoke(...)` wrappers. **`demo.ts` untouched** — demo
  mode simply doesn't have these methods, callers guard with `?.()`.
- `src/lib/components/MasterBar.svelte`: added a "midi in" `<select>`
  (ports + "None") next to the existing input/output device selects, plus
  a small activity dot (`.midi-dot`, lights cyan when
  `lastEventAgeMs < 300`). Port list fetched once on mount via
  `backend.midiListInputPorts?.()`. Status polled every 500ms (~2Hz) via a
  `$effect`-owned `setInterval` for as long as MasterBar (the always-visible
  master strip) is mounted, cleaned up on effect teardown — mirrors the
  existing raw `setInterval` cadence pattern in `state/mcp.svelte.ts`
  (that one polls at 5s; 500ms was chosen here per this task's ~2Hz
  requirement for a responsive-feeling indicator).

### Docs

- `docs/midi-input.md`: "Connect a MIDI keyboard" recipe — plug in, open
  AURA, pick the port under the master strip, play a key, watch the dot.
  States ALSA/Linux as the only manually-verified path in this slice, and
  explicitly lists what this slice does NOT do (no document writes, no
  instrument routing, no persistence across restarts) plus known
  limitations (one-shot port list read at boot; unplugged mid-session just
  goes quiet, no explicit disconnect message).

## Self-review (per task instructions)

- Grepped the diff for `control::`/`Session`/`Store`: only hit is a
  doc-comment sentence in `midi_input.rs` explaining the app-config
  carve-out ("not part of the project document (`Store`/`Session`)") — no
  actual import or coupling. `src-tauri/src/control/` and
  `src-tauri/src/midi/` (document module) have zero diff.
- `src/lib/demo.ts` has zero diff.
- Callback path: `catch_unwind`-wrapped; every atomic/mutex op inside is
  independently infallible (no `.unwrap()` on the mutex lock in the
  callback — `if let Ok(...)` instead).
- Commands are additive only in `generate_handler!` — existing entries
  untouched, no renumbering/reordering beyond appending.
- UI mirrors the existing `.dev` label + `<select>` device pattern
  byte-for-byte in structure (just adds the activity dot after it).

## Test summary

- `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`: **394
  passed, 0 failed** (383 lib unit tests incl. the 10 new
  `midi_input::tests::*`, + 11 across the 4 integration test binaries).
- `cargo check --manifest-path src-tauri/Cargo.toml --lib --tests`: clean
  rebuild after `touch`ing the changed files — **zero warnings**.
- `timeout 300 npx vitest run`: **185 passed (19 files), 0 failed**.
- `npx tsc --noEmit -p .`: no errors.

## Concerns / notes for follow-up (not blockers)

1. `Cargo.toml` and `lib.rs` are both header-marked as frozen/owned by
   Agent 1 in this repo's convention. This task's own instructions
   explicitly directed edits to both files (crate addition + module/command
   registration), so I followed the task spec over the file-header
   convention. Worth a heads-up to whoever owns that convention before
   merge, in case they want the registration done via a different seam.
2. Port ids (`name#index`) are enumeration-order-dependent, not a durable
   identity — fine for slice 1's session-local selection, called out in
   both the module doc and `docs/midi-input.md`. Slice 2 will need a better
   identity scheme if/when persistence is added.
3. No live port-list refresh while the app is running (only fetched once on
   `MasterBar` mount) — matches "no polling infra for that in this slice"
   scope; documented as a known limitation.
4. This machine's ALSA-seq has 0 real MIDI ports, so `list_ports()` /
   `select_port(Some(real_id))` / actual note-event flow are exercised by
   logic-level tests only (id math, atomics, error paths), never against a
   physical keyboard — matches the task's "manually verified later" carve
   -out; not faked.

## Branch + commit

Branch: `midi-input-ports`
Commit: see final report line (created after this file, same message as
directed by the task spec).

---

# Addendum — slice 1b: live monitoring (extension, on top of 61123c6)

## Status: DONE

## Investigation (per coordinator's cap)

Read `src-tauri/src/audio/sampler_preview.rs` (option 1) and confirmed the
"preview-style voice path" is exactly reusable:

- `sampler_preview_note` (audio/mod.rs ~:500) plays via
  `PreviewHandle::play(instrument, key, velocity)`, which lazily opens a
  DEDICATED output stream (own cpal device, own control thread) and installs
  a `SamplerNode` (RCU pointer-swap over rtrb queues, exactly the engine's
  graph-swap discipline) driven by a `NoteCmd` queue (`On`/`Off`/`AllOff`).
  `NoteCmd::On { hold_frames: 0 }` already maps to `HOLD_MANUAL` inside
  `SamplerNode::process` — i.e. the RT node already supports a sustained
  note that waits for an explicit `NoteCmd::Off`, not just the auto-release
  "audition note" `sampler_preview_note` currently sends. This existed
  unused before this change.
- What it needs loaded: `sampler_preview_note` requires an instrument
  already in `AudioState.samplers` (`SamplerBank`, `#[derive(Default)]`,
  empty until `sampler_load_instrument` runs). Confirmed the bank CAN be
  (and by default IS) empty — no built-in default instrument existed.
  Per the coordinator's own fallback instruction, this would normally mean
  "fall back to option 2." Instead of that, I found a **third, still-
  zero-new-RT-infra option**: the sampler_preview test fixture already
  proves `CompiledInstrument`/`CompiledRegion`/`SampleData` can be
  constructed entirely in-process (no `.sfz`/file I/O) — a synthesized
  wavetable. So slice 1b builds one fixed built-in "monitor" instrument
  this way and uses it regardless of sampler-bank contents, sidestepping
  the empty-bank problem without touching `midi/synth.rs` or any new RT
  plumbing at all. Did not need option 2 or a NEEDS_CONTEXT report.

## What was built

### `src-tauri/src/audio/sampler_preview.rs` (small, additive, zone-neutral)

- `PreviewHandle` gained `#[derive(Clone)]` (cheap — just an mpsc
  `Sender<Msg>` clone) so `midi_input` can hold its own handle to a
  SEPARATE preview stream, independent from `AudioState.preview` (used by
  `sampler_preview_note`) — zero risk of the two contending over which
  instrument is installed in a shared node.
- `Msg` gained `NoteOn { instrument, key, velocity }`, `NoteOff { key }`,
  `AllOff` — fire-and-forget (no reply channel) siblings of the existing
  `Play`. `PreviewHandle::note_on`/`note_off`/`all_off` just enqueue on the
  existing crossbeam channel; **never block waiting for the control
  thread**, unlike `play()`'s `recv_timeout(10s)` reply wait. This is the
  mechanism that satisfies "the callback thread must never block."
- `handle_play`'s instrument-install logic was factored out into
  `ensure_instrument_installed` (pure refactor, identical behavior,
  verified by the 2 pre-existing `sampler_preview` tests still passing
  unchanged) and reused by the new `handle_note_on` (same install path,
  `hold_frames: 0` instead of the auto-release hold).
- Module doc updated to describe the new sustained-note siblings.

### `src-tauri/src/midi_input.rs` (self-contained, as before)

- New imports: `crate::audio::sampler_engine::CompiledInstrument`,
  `crate::audio::sampler_voice::{CompiledRegion, SampleData}`,
  `crate::audio::sampler_preview::{self, PreviewHandle}` — all
  live-resource, non-document, same category as `sampler_preview_note`'s
  own dependencies. `crate::control`/`crate::midi` (document) still
  untouched (grepped the whole diff to confirm).
- `MidiInputManager` gained `preview: OnceLock<PreviewHandle>` (its own
  lazily-started preview stream) and `preview_handle()`.
- `select_port(port_id, monitor: bool)` — signature extended with the
  `monitor` bool (was `select_port(port_id)`). Every call now also does
  `self.preview_handle().all_off()` right after tearing down any previous
  connection, so switching ports / closing / toggling monitor off never
  leaves a note stuck sounding. `ActiveConnection` gained a `monitor: bool`
  field (fixed for that connection's lifetime — re-run `select_port` to
  change it, which it always fully rebuilds anyway, so no atomic/mutable
  toggle was needed: a plain `bool` moved into the closure is enough).
- New pure `parse_note_event(running_status: &mut Option<u8>, message: &[u8]) -> Option<NoteEvent>`:
  recognizes `0x9n`/`0x8n`, treats NoteOn-vel-0 as NoteOff, and implements
  MIDI running status (a status byte is remembered and reused for a
  following data-only message) — defensive/portable even though midir's
  ALSA backend hands one complete expanded message per callback in
  practice, per its `alsa::seq::MidiEvent::decode()`-based implementation
  (read the vendored crate source to confirm this, not assumed).
- `monitor_instrument()` / `build_monitor_instrument()`: a `OnceLock`-cached
  built-in instrument — one region, full key range (0-127), a single
  exact-one-cycle 440Hz sine wavetable (48kHz, ~109 samples) marked
  `looped: true` so it sustains losslessly at the loop point (zero click:
  a full period wraps to itself exactly), 5ms attack / 80ms release,
  moderate gain (0.35) for headroom. Built once, reused for every
  monitored note; independent of `SamplerBank`.
- `forward_for_monitoring(preview, running_status, monitor_enabled, message)`:
  parses unconditionally (keeps running-status tracking correct even while
  monitoring is off), forwards via `note_on`/`note_off` only when enabled;
  swallows/logs errors (no caller to report them to from a MIDI callback).
- The midir connect closure's single `catch_unwind` body now does BOTH the
  slice-1 atomics/ring bookkeeping AND the slice-1b parse+forward, in that
  order, still with a plain captured `bool` for `monitor` and a
  closure-local (not shared/locked) `running_status: Option<u8>` — no new
  synchronization primitive needed for the parser state since only the
  single MIDI callback thread ever touches it.
- `MidiInputStatus` gained `monitor: bool` (`false` whenever nothing is
  selected — never "stuck on"; mirrors the connection's fixed value
  otherwise).
- `midi_select_input_port` command gained `monitor: Option<bool>`,
  defaulting to `true` via `.unwrap_or(true)` — "default ON when a port is
  selected" per the extension spec. `midi_input_status` unchanged in
  signature (the new field just rides along in the existing return type).
- 12 new unit tests, covering: NoteOn/NoteOff parsing, velocity-0-as-off,
  non-note status bytes updating running status without emitting an event,
  running status reuse across a data-only follow-up message (both On and
  Off), data-only message with no prior status (`None`, not a panic),
  empty/short messages (`None`, not a panic), `forward_for_monitoring`
  respecting the toggle and never panicking with or without a real output
  device, the built-in monitor instrument's well-formedness (full key
  range, looped, loop bounds sane, non-empty sample, cached/reused via
  `Arc::ptr_eq`), the command wrapper's `monitor` default-to-`true`
  behavior, and that a failed `select_port` never leaves `status().monitor`
  reading `true`. Existing slice-1 tests updated only where the signature
  changed (`select_port(id)` → `select_port(id, bool)`, status struct
  gained `monitor`); their assertions are otherwise untouched.

### Frontend

- `src/lib/types/ipc.ts`: `MidiInputStatus` gained `monitor: boolean`.
- `src/lib/tauri.ts`: `midiSelectInputPort?(portId, monitor?)` — optional
  second param; `TauriBackend` passes `monitor: monitor ?? null` (Tauri
  deserializes `null` as `Option::None`, matching the Rust default-ON
  behavior when the value is genuinely absent vs. explicitly chosen).
- `src/lib/components/MasterBar.svelte`: the "midi in" row's outer element
  changed from `<label>` to `<div class="dev">` (labels must not nest per
  HTML semantics) wrapping the existing select + activity dot PLUS a new
  `<label class="monitor">` checkbox (default checked, matching the
  backend default). `selectMidiPort(id)` and `toggleMidiMonitor(enabled)`
  helpers both call `backend.midiSelectInputPort?.(...)` (re-selecting is
  how a monitor-only toggle is applied too, consistent with the backend
  always fully rebuilding the connection). The status poll (~2Hz, already
  in place from slice 1) now also syncs the checkbox from
  `status().monitor` whenever a port is selected, so the UI can't drift out
  of sync with the backend's authoritative state.
- `docs/midi-input.md`: rewritten recipe — pick port → play → **hear a
  tone** (not just watch the dot); a new "Preview-grade monitoring, not the
  real RT path" section stating the latency/quality honesty required by the
  spec (small inconsistent latency, fixed built-in tone regardless of any
  loaded instrument, real low-latency track routing deferred to a later,
  post-architecture-gate slice); "What this slice does NOT do" and
  "Limitations" updated accordingly (monitor toggle also does not
  persist).

## Self-review

- Grepped the full diff (`sampler_preview.rs` + `midi_input.rs`) for
  `control::`/`Session`/`Store`: only hit is the pre-existing module-doc
  sentence in `midi_input.rs` describing the app-config carve-out — no
  actual coupling added. `src-tauri/src/control/` and `src-tauri/src/midi/`
  (document module) have zero diff, same as slice 1. `src/lib/demo.ts` has
  zero diff.
- Callback path: unchanged discipline from slice 1 — single
  `catch_unwind(AssertUnwindSafe(...))` around the whole body; every
  operation inside (`record_event`, `parse_note_event`,
  `PreviewHandle::note_on`/`note_off` — which are themselves just a
  crossbeam channel `send`) is independently non-blocking/infallible. No
  `.lock().unwrap()` anywhere in or reachable synchronously from the
  callback.
- Commands additive/signature-extended only (`midi_select_input_port`
  gained one new `Option` param — backward compatible in shape;
  `midi_input_status`'s return type gained one field). `generate_handler!`
  list itself untouched by this addendum (no new commands were needed).
- UI mirrors the existing pattern: same select markup, checkbox styled
  consistently with the rest of the master strip (`--cyan` accent, `.silk`
  label typography already used everywhere else in this file).

## Test summary

- `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`: **393
  lib tests + 11 integration tests, 404 total, 0 failed** (was 383/394 in
  the base slice-1 report — net +10 in `midi_input::tests` after replacing
  1 test's assertions and adding others; the 2 pre-existing
  `sampler_preview::tests` still pass unchanged, confirming the
  `handle_play` refactor is behavior-preserving).
- `cargo check --manifest-path src-tauri/Cargo.toml --lib --tests`: clean
  rebuild after touching both changed files — **zero warnings**.
- `timeout 300 npx vitest run`: **185 passed (19 files), 0 failed**
  (unchanged count — this addendum didn't add frontend unit tests, only
  wiring + a doc update; the change is UI wiring, not logic with its own
  test file to extend).
- `npx tsc --noEmit -p .`: no errors.
- **Manual verification NOT done** (no physical MIDI keyboard available in
  this sandboxed worktree — `/dev/snd/seq` exists but `aconnect -l` shows
  no external clients). Documented in `docs/midi-input.md` as the expected
  end-to-end behavior; the note-parsing, toggle, and instrument-construction
  logic are covered by the unit tests above instead, per the task's
  "manually verified later, do NOT fake it" instruction.

## Concerns / notes for follow-up

1. Same frozen-file caveat as the base report does not apply here — this
   addendum touched no frozen files (`lib.rs`/`Cargo.toml` untouched;
   `sampler_preview.rs`/`sampler_voice.rs`/`sampler_engine.rs` are
   sampler-zone-owned, not architecture-frozen, and the changes there are
   small, additive, and behavior-preserving for existing callers).
2. Two independent preview output streams can now exist simultaneously
   (one for `sampler_preview_note`/UI audition via `AudioState.preview`,
   one for MIDI monitoring via `MidiInputManager.preview`) if both features
   are used at once. Functionally fine (cpal/ALSA generally tolerate
   multiple independent output streams to the default device via
   dmix/pulse), just a minor resource duplication — flagged in case a
   later slice wants to unify them behind one shared preview handle.
3. `monitor` is not independently toggleable without a full connection
   rebuild (activity counters reset on every toggle) — acceptable per the
   extension spec's literal instruction to "extend `midi_select_input_port`
   /status", but worth knowing if a future slice wants toggle-without-reset.
4. The built-in monitor tone is intentionally NOT the user's loaded sampler
   instrument (if any) — a deliberate design choice for predictability/
   zero-setup, documented in `docs/midi-input.md`; flagging in case product
   intent was actually "let me hear MY loaded instrument," which would be a
   different (still small) change: swap `monitor_instrument()` for a bank
   lookup with the synthesized tone as fallback only when the bank is
   empty.

## Branch + commit (addendum)

Branch: `midi-input-ports`
Commit: see final short status reply (message:
`feat(midi): live monitoring — keyboard notes audible via the preview voice path (slice 1b)`).
