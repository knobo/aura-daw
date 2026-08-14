# Backlog: multi-clip selection, group drag, and cross-track copy/paste

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
