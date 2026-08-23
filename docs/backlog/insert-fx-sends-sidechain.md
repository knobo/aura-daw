# Backlog: insert FX, sends, sidechain (Plan G)

**Captured 2026-08-15 from the owner** ("hvordan legger andre DAWer på
reverb/echo og sånne fancy ting — bassvolum som ducker andre tracks — hva
bør vi støtte?"). This is the product decision for the Tier-1 mixer-graph
item that `00-ROADMAP-real-alternative.md` still listed as
`[inline→plan]`. It is **not** an implementation plan: the graph work
still needs its own research → plan → gates round (round-2 §8, PDC-before-
sends). This doc freezes *what* we host and in which order, so the next
agent does not invent a stock FX suite or ship sends without PDC.

Status: **G1 landed; G2 landed (PR #109). Next is G3 (sidechain edges).**
G1 plan: `docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`.
Handoff: `docs/handoff/g1-insert-fx.md`. What G2 actually shipped, and
what it deliberately did not, is at the bottom of this file.

## What AURA has today

The mixer is still a **linear track list**: clips + one live instrument
node → gain/pan/mute → master (`audio::mixer`). That is a degenerate
graph, which is the intended prototype shape (`SCALABILITY.md` §1).

Plugin hosting is live for instruments **and** (since PR #55) insert
effects on the document/host path. The mixer still does not walk a
chain (Task 5):

- CLAP (clack) and LV2 (livi) instantiate instruments on MIDI tracks
  behind `LiveInstrument`. `insert_add` instantiates effects onto
  `TrackState.inserts` (`HostRole::Effect`).
- `plugin_scan` already discovers effects (Calf, LSP, Zam, etc.).
- The instrument load path still **rejects** `!isInstrument`.
- `dsp::Effect` exists; the mixer does not yet walk inserts.
- CLAP/LV2 `latency_samples()` exist on the host. No compensating
  delay lines yet (Task 6).
- `track.kind: "bus"` is reserved in the schema and rejected at
  `add_track` (`control::ops`).

So the gap is not "we have no plugins". The gap is **the graph those
plugins sit in**. Reverb, echo, EQ, compressors, saturators — the
hundreds of effects the owner is looking at — already exist as LV2/CLAP.
We host them. We do not write them.

## How other DAWs actually do this

They do **not** ship 200 first-party effects as the architecture. They
ship three routing primitives. Almost every "fancy" mixer trick is a
combination of those three plus a third-party plugin.

### 1. Insert chain (serial, per track)

Audio on *this* track walks an ordered list of processors before the
fader:

```
source (clips / instrument)
  → insert 0 (EQ)
  → insert 1 (compressor)
  → insert 2 (sat)
  → fader / pan
  → mix bus
```

This is where you put things that **shape this source**: EQ, dynamics,
distortion, a character delay that is part of the sound. Ableton's
device chain, Bitwig's hybrid device chain, FL's 10 mixer-insert slots,
Reaper's FX chain, Ardour's processor box — same object.

AURA already has the plugin *instance* half (CLAP/LV2 nodes, param UI,
state blobs). Missing: a per-track ordered list, an audio-input path
into those nodes, and a mixer walk that is not "one live instrument".

### 2. Sends / return buses (parallel, shared)

A track peels off a copy of its signal (pre- or post-fader) and routes
it to a **bus track** that has its own insert chain:

```
vocal  ──inserts──fader──► master
   \__send 0.3──────────► [Reverb bus] ──inserts──► master
guitar ──inserts──fader──► master
   \__send 0.2──────────►     ↑
```

This is how every DAW does hall reverb and slap echo. One Dragonfly /
Calf / Valhalla instance, many tracks send into it, one shared space.
Doing the same thing as inserts would mean N reverb instances, N
different rooms, and N times the CPU.

FL: any insert can route to any other insert; "send" is just another
edge, post-fader by default. Ableton: dedicated Return tracks A/B.
Logic / Cubase: aux sends. Bitwig: the same device chain on a group /
FX track. The data model is always **an edge with an amount**, not a
special effect type.

AURA's reserved `kind: "bus"` is exactly this node.

### 3. Sidechain / extra ports (a wire, not an effect)

The "bass volume ducks the pads" clip the owner saw is **not** a
reverb. It is an extra audio (or control) edge:

```
bass ──inserts──fader──► master
  \__listen tap (not heard at dest)──► compressor.sidechain on [pads]
pads ──[compressor, SC from bass]──fader──► master
```

Three common implementations of the same idea:

| What you saw | What it actually is | Where |
|---|---|---|
| Pads duck on every bass hit | Compressor (or gate) insert whose **sidechain input** is fed by the bass | Ableton "Sidechain" on Glue/Compressor; FL Fruity Limiter + "sidechain" send ("audio without being heard at the destination"); Logic / Cubase SC input |
| A moving line that looks like automation | **Envelope follower** on the bass, written onto another track's gain (or a plugin param) | Bitwig audio-rate modulators; FL Peak Controller; Reaper parameter modulation |
| One track's fader slaves others | Control-rate link / VCA / group | Bitwig / Live racks + macros; Reaper VCA; analog-console VCAs |

AURA already decided the renderer must not special-case this:
`SCALABILITY.md` §1 — "a compressor's sidechain input is just another
audio input port. Sends and sidechains are ordinary edges."

The envelope-follower flavour is **modulation**, not mixer topology. It
belongs after Track D/F's param addressing is real, as a modulator
source — not as a fake automation lane the engine draws into.

## Decision: what we support

**Host the graph. Do not write a stock FX suite.**

Calf, LSP, Zam, Dragonfly, TAL, x42, and whatever CLAP the user
installs already cover reverb, delay, EQ, compression, saturation,
chorus, limiter. First-party DSP would be worse, later, and would
compete with the host path we have to build anyway. The `dsp::Effect`
trait stays as the *internal* slot contract; its first implementations
are CLAP/LV2 adapters with audio inputs, not `aura.reverb`.

### We ship, in this order

**G1 — Insert FX chains + PDC.** The first thing that makes "add reverb
to this vocal" work at all.

- Per-track ordered insert list. Stable slot UUIDs (not indices —
  `SCALABILITY.md` §2: undo and automation must survive reorder).
- Lift the `isInstrument` rejection for insert slots. Instruments stay
  on the instrument slot; effects go on inserts. Same plugin host, same
  param UI, same state blobs.
- Mixer walk: source sum → inserts in order → existing gain/pan/mute.
- Bypass per slot. Reorder is a structural graph rebuild.
- **PDC in the same slice.** Every node reports `latency_samples()`
  (CLAP latency ext already read; LV2 `port-props:latency` / the
  analogue). Compiler inserts compensating delay on the shorter paths
  so a 1024-sample lookahead limiter on drums does not make the dry
  vocal late. A plugin reporting new latency = structural recompile,
  never an RT computation. Round-2's standing rule is binding: **PDC
  before sends ship**, and G1 is where PDC is born so G2 cannot
  "forget".
- Offline bounce walks the same compiled schedule.

**G2 — Bus tracks + sends.** The first thing that makes *shared* reverb
/ echo cheap and musical.

- Honour `track.kind: "bus"` (no clips, no instrument; inserts +
  sends only).
- Per-track send list: `{id, destBus, amount, prePost}`. Amount is a
  parameter (automatable, same ring as gain). Pre/post-fader is a
  structural flag.
- Default recipe the UI can stamp: "New return: Reverb" / "New return:
  Delay" creates a bus, loads nothing (user picks the plugin — we do
  not bundle), and adds a send from the selected tracks.
- Feedback (bus A → bus B → bus A) is **out of G2**. Cycles are only
  legal through an explicit one-block delay node
  (`SCALABILITY.md` §1); that is G2.1 if anyone needs a feedback
  send.

**G3 — Sidechain edges.** The bass-ducks-pads trick.

- A listen tap: `{id, fromTrack, fromPoint, toNode, toPort}`.
  `fromPoint` is post-insert / pre-fader for v1 (the usual "key" tap).
  The tap is **not** audible at the destination — FL's wording, and
  the right default.
- Only exposed on plugins that actually declare a sidechain / extra
  audio input. No fake "AURA sidechain" device.
- The compressor that does the ducking is a hosted plugin (Calf /
  LSP / whatever). We draw the wire; we do not write the detector.

**G4 — Envelope-follower modulator (later, not Plan G).** The "volume
into automation" flavour. A modulator source that reads a track's
envelope and writes any addressable param (gain, a plugin knob, a
send amount). This is Track D/F's parameter namespace plus a new
source, not a mixer edge. Do not fake it by writing automation points
on the RT thread.

### We do not ship in Plan G

- First-party reverb / delay / chorus / EQ / compressor DSP.
- Bitwig Grid, FL Patcher, Ableton Racks, nested device chains,
  macros. Encapsulation is retrofittable once the device abstraction
  is uniform (Live shipped Racks in 6.0). Do not invent a mini-engine
  beside the host — that is how Patcher's PDC diverged from FL's
  (`research/03-fl-bitwig-ableton.md` §1.3).
- VST3. CLAP + LV2 already scan the Linux effect catalogue we care
  about.
- Per-voice / note FX (Bitwig green→blue). Needs voice identity past
  the instrument, which we do not have.
- External hardware insert (send to an interface output, return from
  an input). Different clock domain; later.
- "Reduced latency when monitoring" (Live's PDC bypass on the armed
  track). Real, but a deliberate hole in compensation — add when
  someone is trying to track through a lookahead limiter.
- Stock "sidechain preset" that hides the wire. The wire *is* the
  feature; hiding it is how FL ended up needing the user to *name a
  mixer track* so PDC could find the route (`research/03` §1.7).

## Why this order, and why not "just add a reverb"

A send without PDC is how you get a mix that *moves* when the user
adds a limiter on the drums: the dry vocal is suddenly early, the
reverb return is late, and every project from that point on has the
wrong compensation burned into fader habits. FL still carries manual
PDC offsets and "name this track" hints for that reason. AURA's
docs have been saying "PDC before sends" since `SCALABILITY.md`; G1
is the slice that makes the rule true.

A first-party reverb would not save that work. The moment the user
loads Calf Vintage Delay next to it, we need the graph anyway. Spend
the weeks on the graph.

## Suggested cut (when the plan round opens)

1. **G1a.** Schema + channel ops: insert list on a track, add / remove
   / reorder / bypass. Reject instruments in insert slots and effects
   in the instrument slot. Undo is free via the channel.
2. **G1b.** Mixer walk through the chain. CLAP/LV2 nodes grow an
   audio-input path (`AudioProcessor::process` is already in-place).
   Headless render test: a known LV2 effect (Zam / Calf if present,
   otherwise a test double) changes a fixture WAV in a measurable way.
3. **G1c.** PDC compiler: `latency_samples()` on every node, delay
   lines on short paths, a test that a 256-sample dummy latency on
   track A lines up with dry track B in an offline bounce. Plugin
   latency change → rebuild.
4. **G2.** `kind: bus` + send edges + a two-track-plus-return bounce
   test (dry + send-to-identity-bus == 2× dry, within float error).
5. **G3.** Listen-tap edges into a plugin extra input. Bounce test:
   a dummy "gain-from-sidechain" node on track B, keyed by track A,
   produces the expected envelope. Then point the same wire at a real
   compressor if one is installed.
6. **UI.** Track inspector: insert list (reuse the plugin browser,
   filter `!isInstrument`), send row, sidechain picker on plugins
   that declare the port. No new mixer view required for G1; G2
   needs a bus row in the existing track list.

Gates live in the plan-round doc, not here. The binding constraints
this doc must not lose:

- Compiled schedule + RCU swap; no in-place graph mutation
  (`SCALABILITY.md` §1).
- PDC before any user-visible send or sidechain.
- Stable node UUIDs, never "insert index 3".
- Channel-routed mutations (one op = one undo).
- Do not grow the frozen MCP roster to drive this.
- Channel-counted buffers, not a stereo-only buffer type.

## Pointers

- `docs/SCALABILITY.md` §1 — target node/port model, PDC, migration
  path (this doc is the product cut of that path).
- `docs/research/03-fl-bitwig-ableton.md` §1.2 (FL mixer), §1.7 (PDC
  cautionary tale), §2.4 (Bitwig hybrid chains), §3.4–3.6 (Live racks
  + delay compensation).
- `docs/PHASE3-PLAN.md` §6 — effects-on-audio-tracks explicitly
  deferred; that deferral is what G1 lifts.
- `docs/PHASE4-PLAN.md` — standing "PDC before sends ship" note.
- `docs/backlog/00-ROADMAP-real-alternative.md` — Tier-1 row that
  pointed here.
- `src-tauri/src/audio/dsp.rs` — `Effect` / `AudioProcessor` /
  `LiveInstrument` seams.
- `src-tauri/src/audio/mixer.rs` — today's linear walk, the thing G1
  replaces with a schedule executor.

## What G2 shipped (2026-08-23, PR #109)

`kind: "bus"` is a real track kind, `TrackState.sends` is a list of
`{id, dest, amountDb, preFader}` edges, and both the live engine and the
offline bounce render them through one shared compile step,
`audio::bus::compile_routing`.

The parts worth knowing before touching this:

- **Two compensating delays, and they are not interchangeable.**
  `RtTrack::pdc` (G1) aligns the SOURCES and sits BEFORE the send taps,
  so every sender reaches a bus at the same latency — that is what lets
  one delay line per bus be enough instead of one per edge.
  `RtTrack::master_pdc` sits AFTER the taps and makes the dry path wait
  for the slowest return. Growing `pdc` instead would delay the sends by
  the same amount and dry and wet would never converge.
  `audio::bus`'s module doc has the diagram.
- **The render loops inverted.** A bus cannot run until every track that
  feeds it has contributed, so `mixer::render_impl` is now windows of
  `MAX_LIVE_BLOCK` outside, tracks inside, then the bus pass. The window
  is what bounds `bus_buf` — the RT thread still never sizes a buffer.
  A block of `MAX_LIVE_BLOCK` frames or fewer (every real one: cpal
  quanta here are 128–2048) runs that loop exactly once, so the strip is
  the pre-G2 strip plus the taps.
- **A return uses the balance pan law**, not constant-power. See
  `docs/TRAPS.md`.
- **A return answers to its own mute only.** Soloing a vocal must not
  take its reverb with it; that is Live's default and the only one that
  makes solo usable on a project with returns.
- **The amount is a param lane, not a rebuild.** `ParamTable::send_amount`
  + `GraphTables::send_slots`, resolved by `SendSlot::id`, and
  `SendSetAmount` folds into an open gesture the way a fader does.
- **The offline bounce now walks insert chains too**, which is G1 Task
  8's offline half. Without it an export would have dropped exactly the
  reverb the user mixed with. It shares the caveat this module already
  had for instrument nodes: a bounce sees whatever param values the live
  host instance currently holds (`audio::offline`'s header).

Deliberately NOT in G2, in the order they are likely to be wanted:

1. **Bus-to-bus edges.** A reverb into a delay is a legal DAG, but the
   compiler assumes two levels: `master_delay` is a single number, and
   per-edge alignment would be needed the moment a bus can feed a bus.
   Cycles stay illegal without an explicit one-block delay node
   (`SCALABILITY` §1). `Op::SendAdd` rejects a bus source today.
2. **Send-amount automation.** The lane exists and is automatable in
   shape; nothing addresses it from `AutomationDoc` yet.
3. **Reordering sends.** The list is document-ordered and there is no
   `SendReorder` — order does not affect the sum, so this is cosmetic.
4. **G3 sidechain edges** and **G4 envelope followers**, unchanged from
   the plan above.

Owner ear-check owed: create a bus, load a convolution reverb on it,
send two or three tracks in, and confirm it sounds like one room — then
export and confirm the WAV has it.
