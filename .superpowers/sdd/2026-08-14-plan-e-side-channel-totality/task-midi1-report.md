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
