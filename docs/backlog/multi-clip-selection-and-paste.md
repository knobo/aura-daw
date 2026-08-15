# Backlog: multi-clip selection, group drag, and cross-track copy/paste

> **LANDED 2026-08-15** (Track C, branch `multiclip-clipboard`, PR #22).
> Plan: `docs/superpowers/plans/2026-08-14-multiclip-selection-paste.md` —
> read its scope rulings before changing any of this. Full handoff (rulings,
> mid-flight rulings, deferred minors, what is still owed):
> **`docs/PHASE4-PLAN.md`'s "Track C handoff"** section. Deviations from the
> design notes below, all recorded there rather than silently taken:
>
> - **Plain copy, not instancing** — a paste mints a fresh `ContentId` per
>   clip (the "decide at implementation time" question below, decided).
> - **No new op and no batch paste op** — paste composes the existing
>   `TrackAdd`/`ClipAdd`/`MidiClipAdd` in ONE transaction, so
>   `OP_FORMAT_VERSION` stays 2.
> - **SMF is export-only, to a file** (Ctrl+Shift+M). The OS clipboard slot
>   a Tauri v2 desktop app can own is plain text, so SMF cannot ride on the
>   clipboard and paste never parses it. The `application/x-aura-clips` MIME
>   name lives INSIDE the JSON envelope.
> - **Audio travels by reference**, with a three-step resolution on paste
>   (in-project → copy-in from an absolute path → skip-and-report).
> - **Group resize is MIDI-only**; audio clips have no shipped resize
>   gesture yet.
> - **Multi-clip delete/duplicate and arrow-key nudge stay single-clip** —
>   not in Track C's brief.
> - **Selection keys are `"<kind>:<id>"`**, not bare `ClipId` — audio and
>   MIDI clips are minted by two independent stores.
>
> ### The size ceiling on cross-instance copy (product constraint, read this)
>
> Cross-instance transfer is reliable to **~165 KB** and **TOTALLY LOST —
> not truncated — at ~200 KB** on X11 with a clipboard manager running:
> the write reports success, the cross-process read comes back EMPTY, and
> waits of up to 10 s do not help. Measured on real payloads, the marginal
> cost is **~80 bytes per MIDI note** (100 notes = 8 126 B, 1 000 = 79 788,
> 2 000 = 160 212), i.e. the cliff sits at roughly **2 000–2 500 notes**.
> That is not an exotic size: one three-minute recorded MIDI take at a
> modest 10 notes/second is 1 800 notes in a SINGLE clip, and a song section
> across 8 MIDI tracks at ~300 notes each is 2 400. **The operation most
> likely to exceed the ceiling is the operation this feature exists for.**
> Audio clips are irrelevant here — they travel as references, a few hundred
> bytes each. This is a MIDI-note ceiling.
>
> **The harm the user cannot undo:** the local write SUCCEEDS and AURA takes
> X11 selection ownership before the handoff to the clipboard manager fails.
> A failed large copy therefore **destroys whatever was on the system
> clipboard beforehand — from any application — and leaves it empty**. Data
> loss outside AURA, caused by an operation that reported success.
>
> **Deliberate ruling: the frontend WARNS at copy time, it does not refuse.**
> The cliff is a property of the ENVIRONMENT (X11 + which clipboard manager
> + its configuration; without a manager, megabyte INCR transfers are
> routine, and macOS/Windows differ), not of AURA. Baking one desktop's
> measurement into a cross-platform layer buys false refusals where it would
> have worked and false confidence where the limit is tighter. The store
> keeps the payload IN MEMORY as well, so same-instance paste is unaffected
> — that is a MITIGATION of the same-instance case, **not a fix**: it does
> nothing for cross-instance paste of a large selection, which is the
> feature's stated reason to exist. Details in `src-tauri/src/osclipboard.rs`'s
> module doc.

**Captured 2026-08-14 from the owner, mid-Plan-E. Deliberately deferred
until Gate E closes** — every gesture below maps directly onto the finished
channel's primitives (gesture batches, multi-op transactions, one undo
entry per gesture), and building it earlier would recreate the
side-channels Plan E removes. Owner's wording preserved; design notes map
it onto the landed architecture.

## The owner's requirements (2026-08-14, verbatim intent)

1. **Multi-select MIDI clips on the timeline**, and drag them TOGETHER —
   e.g. to adjust the length they loop at — with **individual offsets
   preserved** (each clip keeps its own relative position/offset; the drag
   applies the delta to all).
2. **Copy elements across tracks**: select multiple clips on multiple
   tracks, hit copy, move the **playhead** (the time indicator) to the
   target position, paste — ALL selected elements land at that position
   with their individual offsets intact, each on the SAME track it came
   from.
3. **Optionally paste onto NEW tracks** — needs some explicit indication
   in the UI (owner: "jeg vet ikke hva som blir beste måte" — design
   freedom; a modifier key or a paste-menu choice are the obvious
   candidates).

## Design notes (how this lands on the post-Plan-E architecture)

- **Selection model**: clip-level multi-select mirrors PR #11's
  note-level selection in the piano roll (`src/lib/utils/note-ops.ts`
  marquee/add/subtract modes) — a `Set<ClipId>` in the timeline's view
  state (frontend-only, viewer state, NOT document state).
- **Group drag** = one gesture: `gesture_begin("move clips")` →
  per-pointermove local preview (established preview/commit split from
  Tasks 4/PR #8) → on pointerup ONE transaction: `Set{Clip,
  TimelineStartSamples}` / `Set{MidiClip, TimelineStartTicks}` per
  selected clip (audio and MIDI clips mix freely in one tx — the channel
  is cross-store atomic) → `gesture_end()`. Result: one undo entry for
  the whole group move. Loop-length group-adjust uses the same shape over
  `LengthTicks`/`ContentLengthTicks` (bounds ops from Task 5).
- **Copy/paste** = clipboard of `(clip snapshot, source track id,
  offset-from-anchor)` where the anchor is the leftmost selected clip's
  start; paste at playhead = ONE transaction of `MidiClipAdd`/`ClipAdd`
  with `timeline_start = playhead + individual offset`, `track_id` = the
  original track (fresh clip ids minted; note ids re-minted by the
  backend keep-rule when target clips are new — see round-2 §2.1's copy
  rule: copy mints fresh). Paste-to-new-tracks variant: same tx prefixed
  by `add_track_tx` per distinct source track, clips retargeted to the
  minted rows.
- **Content-instancing question**: round-2 §5/ADR 0004 defines
  copy-vs-instance semantics (`ContentId` sharing). A paste COULD share
  content ids (true instancing — "arrives free for MIDI" per round-2) —
  but round-2 §2.1's remint rules bind the first content op. Decide at
  implementation time: plain copy (fresh ContentId, simplest, matches the
  piano-roll paste precedent) first; instancing as its own later feature.
- **Undo**: nothing extra to build — each gesture/paste is one committed
  batch, so undo/redo comes free from Task 17's history.

## Prerequisites (all land with Plan E)

- Gesture IPC (Task 14), audio `move_clip` + MIDI bounds commands (Tasks
  4/5/7), the op vocabulary for clip add/move (Tasks 3/5), history/undo
  (Task 17).

## Cross-instance / OS-clipboard copy (owner request 2026-08-14)

Today the clip/note clipboard is per-window frontend memory — a second
AURA instance (or any other app) never sees it. Extension: on copy,
ALSO write the OS clipboard with (a) an `application/x-aura-clips` JSON
payload (clips + notes + offsets + source-track hints, schema-versioned)
and (b) a standard MIDI file (SMF) fallback so other DAWs can paste too;
on paste, prefer the AURA payload, fall back to SMF. Audio clips ride as
project-relative source references within the same machine (absolute-path
fallback + copy-into-project on paste across projects). NOTE: true
two-instance workflows also need the fixed MCP port collision solved
(41717 hardcoded — dynamic port + token discovery), tracked in the
roadmap's Tier 2.

## Suggested cut when picked up

1. Timeline multi-select (marquee + shift-click) — frontend only.
2. Group drag → one gesture-wrapped tx (move only).
3. Copy/paste same-tracks at playhead.
4. Group loop-length adjust.
5. Paste-to-new-tracks.

*(All five landed; see the banner at the top.)*

## Still open after Track C

- **The owed manual cross-instance check** — see the handoff; the one link
  no test in this repo can make.
- **Multi-clip DELETE, split and DUPLICATE.** Ctrl+D is still
  `midi.duplicateSelected` (single, focused clip). Delete/Backspace and the
  `×` button (PR #26, merged in at close-out) are also still single-clip,
  and that is a RULING, not an oversight: `remove_clip` /
  `midi_remove_clip` are one transaction each, so deleting a four-clip
  selection through them would be FOUR undo entries and a partial Ctrl+Z.
  The fix is a `clips_remove` batch command composing `Op::ClipRemove` /
  `Op::MidiClipRemove` in ONE transaction, exactly as `clips_paste`
  composes the add ops — then wire Delete to it. Full reasoning in the
  handoff.
- **Arrow-key nudge over a selection.** `nudgeSelection` is note-level in
  the piano roll; the clip-level equivalent would reuse `move_clips`
  directly (it is already a batch command).
- **Audio-clip resize.** Group resize is MIDI-only because audio clips have
  no shipped edge-drag gesture at all; adding one means settling
  source-length clamping and fades first. `move_clips`' payload already
  leaves room (an audio entry simply never carries a length).
- **True content instancing** (shared `ContentId`) — round-2 §5 / ADR 0004's
  own later feature.
- **SMF paste/import**, and rebased/looping-expanded SMF export (see
  `exportSelectionSmf`'s doc comment for the two documented surprises).
- **The hardcoded MCP port 41717**, which every real two-instance workflow
  hits — the second instance logs a collision. Roadmap Tier 2, deliberately
  out of Track C's scope.
- **Clearing `contentLengthTicks` still goes through
  `midi_set_clip_bounds(…, null)`** — `Option<u64>` on `move_clips`' wire
  means "unchanged", never "clear" (ruling H).
