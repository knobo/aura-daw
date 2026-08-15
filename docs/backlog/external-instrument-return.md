# Backlog: external instrument = MIDI out + audio return

**Captured 2026-08-15 from the owner** ("kanskje vi skal få eksterne MIDI-
enheter inn på et spor — brukeren selv, eller AURA orkestrerer PipeWire /
i hvert fall hjelper — så sangen kan eksporteres til WAV").

This is the product cut for the roadmap's Tier-2 row *External instrument
tracks (MIDI out + audio return)* and for track B's still-open item 6
(the document-model target). MIDI **notes** already leave AURA
(`docs/midi-output.md`). MIDI carries no audio back. Export today is an
in-process offline bounce (`audio::offline`) and never hears Hydrogen
or a hardware synth.

Status: **decision recorded, not started.**

## Decision

The return is a **visible audio clip on a real track**. AURA may help
the user wire PipeWire. AURA does **not** grow hidden tracks, and does
**not** silently rewrite the session graph.

Two reasons hidden tracks are the wrong object:

1. A take you cannot see is a take you cannot undo, trim, or notice went
   silent because Hydrogen was not running. That is how you ship a WAV
   that is not the song you heard.
2. The recorder we already have (`audio::recorder`) finalizes to a `Clip`
   on an armed track — one transaction, one undo. A parallel "ghost take"
   would be a second recording path with none of that.

Export then has something to mix. The offline bounce stays the default
once returns exist as clips. Realtime-only export is a fallback, not
the architecture.

## What the user is actually pointing at

Three layers, do not collapse them:

| Layer | Who does it | What AURA owes |
|---|---|---|
| MIDI out | already shipped | notes + clock to a port |
| Audio return | missing | a per-track capture *source*, not the one global input device |
| Patch | user or helper | get Hydrogen / the interface into that source |

Today every armed track records the **same** cpal input
(`list_input_devices` / `select_input_device`). That is enough for one
mic. It is not enough for "this track is Hydrogen, that track is the
Juno on input 3–4". The engine work is **per-track return sources**.
PipeWire is how some of those sources get signal, not a second mixer.

## What we ship, in this order

**X1 — Return source + record onto the same track.** The slice that
makes export honest without any PipeWire code.

- A MIDI track that is routed out grows a **return** picker: an input
  device (the existing cpal list) or "none".
- Recording that track captures *that* source, not the global default.
  Finalize is the existing `ClipAdd` path. Mute the internal instrument
  (already the documented way to avoid doubling MIDI-out).
- Hardware synth: pick the interface inputs. Software (Hydrogen): pick
  whatever capture cpal already exposes (a monitor / loopback / a
  dedicated virtual source the user created). If cpal cannot see it
  yet, X1 still works for hardware; software waits for X2.
- Multiple distinct sources ⇒ multiple capture streams. Same source on
  two tracks can share one stream (channel map later).
- Persist the return source next to the MIDI route, same file, same
  rule: per-machine, matched by name, never in `project.json`.

After X1 a user can: route MIDI to Hydrogen, patch Hydrogen into an
AURA input *themselves* (Helvum / qpwgraph / a cable), record, export.
WAV contains the drums. That is the whole feature, manually wired.

**X2 — PipeWire helper, not an orchestrator.** The "i hvert fall
hjelpe litt" slice.

- Enumerate PipeWire playback nodes (name + ports). Suggest a match
  when the MIDI-out port name and a PW node name look like the same
  app (`Hydrogen`, `ZynAddSubFX`).
- One button: **Connect to this track's return.** Creates (or reuses) a
  capture stream for that track and links the suggested node into it.
  Persist the link by `node.name`, same as MIDI ports.
- Show the current link. Offer **Disconnect**. Never patch on launch
  without that persisted intent. Never touch links we did not create.
- Do not invent a hidden null-sink that swallows the default monitor
  (YouTube, notifications, every other app).
- If PipeWire is not the session (rare on this target, but possible):
  X2 is a no-op and X1's device picker is the whole UI.

**X3 — Freeze returns, then export.** The export story.

- A routed track with no audio covering the song, or with MIDI that
  changed since the last return clip, is **stale**. Badge it.
- **Freeze returns** = realtime play + record on every stale ext track
  (MIDI goes out, capture comes in), then stop. Clips land, undoable.
- **Export** on a project with stale returns: offer "Freeze and export"
  (default) or "Export AURA only" (today's bounce, honest about the
  hole). Do not silently write a WAV that is missing Hydrogen.
- A freeze is wall-clock length of the song. The offline bounce after
  that is fast and repeatable. Do not make every export realtime
  forever — that is how a limiter-tweak costs four minutes every time.
- Loop-record / seek-during-take are already broken for MIDI capture
  (`hardware-midi-io.md`). Freeze must refuse an active loop or inherit
  whatever fix that item lands, not invent a third behaviour.

**X4 — Monitor the return through AURA (later).** Hear Hydrogen via
AURA's track (latency-compensated), and mute Hydrogen's own output so
you do not double. This is a live-monitoring problem, not an export
problem. X1–X3 do not depend on it.

## What we do not ship

- Hidden / ghost tracks the engine records onto and the user never
  sees. Export-time scratch buffers that never become clips are fine
  *inside* a Freeze-and-export job; they must not survive in the
  project.
- Silent PipeWire graph edits (launch-time reconnect of everything
  that looks like a synth, hijacking the default sink, mixing the
  desktop).
- A second, realtime-only exporter as the steady state.
- JACK transport / Ableton Link (still the other track-B deferral).
- Treating the default PipeWire monitor as "the mix".

## Why not "just realtime-export the master + whatever is patched"

It works once, if Hydrogen is open, patched, and in sync. It fails
closed as silence, with no clip to inspect. It also cannot be faster
than the song, so every export is a performance. Freeze-to-clip is the
same realtime cost **once**, then the song is audio and the bounce we
already trust applies.

The clock caveat in `docs/midi-output.md` (AURA's card clock vs the
slave's, re-cue every few minutes) is another reason to *capture* the
return rather than hope two apps stay aligned for the length of an
export: the take is the timing.

## Suggested cut

1. Schema/UI: return source on a routed MIDI track, persisted with the
   routing file. No `OP_FORMAT_VERSION` bump (still app config).
2. Engine: per-source capture streams; `start_recording` on an ext
   track reads *its* source. Tests with a virtual input / fixture.
3. PipeWire enumerate + link + unlink, behind a feature that no-ops
   when PW is absent. Suggested-match by name. One integration test
   that can SKIP without PW.
4. Stale badge + Freeze returns command (realtime, refuses loop).
5. Export dialog grows the two honest choices when anything is stale.

Gates live in the plan-round doc. Binding constraints:

- Return clips are ordinary `Clip`s. Undo is the channel.
- Routing + return source stay per-machine app config.
- We only link PipeWire nodes the user asked us to link.
- Offline bounce never pretends it rendered an external device.
- Do not grow the frozen MCP roster for this.

## Pointers

- `docs/midi-output.md` — what already leaves AURA, and the timing
  caveat a freeze inherits.
- `docs/backlog/hardware-midi-io.md` item 6 — this doc is that item.
- `docs/backlog/00-ROADMAP-real-alternative.md` — Tier-2 row.
- `src-tauri/src/audio/recorder.rs` — the take path X1 reuses.
- `src-tauri/src/audio/offline.rs` — bounce that must keep ignoring
  live MIDI-out, and must mix the return *clips*.
- `docs/SCALABILITY.md` D-01 — device identity should become a
  backend-qualified id (`pipewire:node.name`) before we persist PW
  links for real; name-matching is the v1 we already use for MIDI
  ports.
