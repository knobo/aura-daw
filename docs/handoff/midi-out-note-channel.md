# MIDI output: what was broken, what is fixed, and what to do next

**Merged as PR #77 `55257e8`** (2026-08-18), with the CI fix that followed it as
PR #78 `2b00e91`. Read this before touching MIDI output again — it is
self-contained on purpose, nothing here needs the session that produced it.

One caveat on the merge: the owner merged #77 on local evidence (Rust 1250/0,
frontend green) before CI finished, so no full CI run covered that diff. The
first `main` run after #78 is the first real signal — and the ALSA-seq tests skip
on the runner regardless, so their coverage stays local-only.

Fixes the bug [`next-prompt.md`](../../next-prompt.md) recorded after the
Composer H1 ear-check as *"one bug (MIDI-out to Hydrogen) needs its own PR, not
yet filed/scoped"*. It turned out to be eight, one of which was the root cause of
everything the owner could actually hear.

## The report

> I patch a MIDI track to Hydrogen and I can't hear the drum machine playing it.
> Also there shouldn't be a local instrument on a routed MIDI track any more,
> should there? … Hydrogen starts up but doesn't play the drums.

and later, once notes were reaching a device at all:

> only three or four hits in a row, then a little pause, then three or four hits
> again.

## Root cause: a re-cue storm (`d0bb8fa`)

Not about notes at all. Snooping the ALSA-seq port the device listens on
(`aseqdump -p 14:0`) across a real session, before and after:

| | before | after |
|---|---|---|
| `Stop` -> `SongPosition` -> `Continue` triples | **31 676** | **1** |
| clock pulses | 22 429 | 1 224 |
| note-ons | 950 | 99 |

`midi_out`'s clock engine was re-cueing the device **about 1500 times a second**.
`drift_tolerance` was a flat `rate / 50` — 960 samples, 20 ms at 48 kHz — while
PipeWire's default quantum is 1024 frames (21.3 ms). `SharedRt::position` only
moves when an audio callback runs, so the divergence between it and this thread's
wall-clock estimate sawtooths up to one full block and drops back, every block: a
threshold *below* the block size is crossed forever.

No slave can start under that. Worse, the flood of realtime bytes overran the
receiving ALSA-seq client's event pool, so notes were dropped as collateral —
which is exactly what "three or four hits, a gap, three or four hits" was.

Fixed by publishing `SharedRt::block_frames` from the RT callback and sizing the
tolerance as `max(rate / 50, 2 * block)`, plus making a small FORWARD resync
advance through the gap instead of `reseek`ing past it (only a backward move, or
a jump beyond the catch-up limit, still repositions).

**Why the existing test missed it.**
`block_quantized_position_does_not_trigger_a_resync` passed throughout, because
it used a 480-frame block — 10 ms, which happens to fit under 20 ms. Block size
was the variable that mattered and nothing varied it: pinning the intended
*behaviour* at one arbitrary parameter value is not a test of the behaviour.
`a_pipewire_sized_block_does_not_re_cue_the_device` now drives 5 s of 1024-frame
callbacks at 500 us ticks, exactly as the port thread does.

## The other seven

| # | What | Commit |
|---|---|---|
| 2 | The per-note MIDI channel was dropped on the way out. `AbsNoteEvent` had no channel field, so the route's channel (a `u8` defaulting to 0) was stamped over every note. The Composer writes GM drums on channel 10 and says so in an annotation; MIDI *file* export honoured it; live routing sent them on channel 1, where no drum machine maps them. `RouteTarget::channel` is now `Option<u8>`: `None` ("from clip", the default) follows each note, `Some(ch)` forces the route. | `c45602e` |
| 3 | A routed track kept sounding AURA's internal instrument on top of the device — as pitches, for a drum part, loud enough to mask the machine it was driving. The old answer was "mute it yourself". Routing now REPLACES the internal voice, the way a freeze return already did. | `c45602e` |
| 4 | A deleted-then-undone track went **permanently silent**. The engine caches `RoutedOut` and the first cut published it from three of six mutation sites; `Op::TrackAdd` restores a row byte-identically, so undo brought the track back with no route AND no internal voice, with no rebuild able to converge it. Replaced with a `RoutesChanged` hook fired from every mutation, the port thread's self-heal included. | `d89628f` |
| 5 | A deliberately forced MIDI channel 1 did not survive a project reopen — it is stored as `0`, the same value legacy files used for "never picked". The routing file now carries a `version` and the migration is keyed on it, not on the value. | `d89628f` |
| 6 | A note drawn in the piano roll always got channel 1, whatever the clip was on. Found in the owner's own clip: 59 notes on channel 10 and exactly one on channel 1, at the spot they had drawn one in. Harmless until the channel started reaching the wire. `channelForNewNote` takes the clip's majority channel. | `092d277` |
| 7 | Persisted routing was lost when a device came back at a new ALSA sequencer address. `midir` spells an ALSA port `"<name> <client>:<port>"`, and that number is assigned in connection order — so restarting Hydrogen renamed the port and the route was silently skipped. The owner's file held both `...Midi-In 128:0` and `...Midi-In 129:0` for one device. Matching is now exact-name first, then `persist::port_name_key`. | `c45602e` |
| 8 | The port list was read once when the panel mounted, so a device started after AURA never appeared and the documented workaround was to restart the app. The backend already re-enumerated per call; the panel now asks again on a slower beat than the status poll. | `d89628f` |

Plus two `.gitignore` commits (`9f60109`, `86e042b`): `/node_modules/` matches a
*directory* only, so the symlink shape a worktree uses to avoid a second 1 GB
install stayed untracked-but-visible, one `git add -A` away from being committed
— the exact accident PR #73 had just cleaned up. Same for `src-tauri/target` and
`.venv-sidecars`.

## State of the gate

- Rust: **1250 passed, 0 failed** (`--test-threads=1`, ALSA sink pinned).
- Frontend: **825 vitest**, svelte-check **0 errors**, production build OK.
- **Never run in GitHub Actions.** The runner has no `/dev/snd/seq`, so every
  test in `midi_out::route_e2e_test` and the two pre-existing ALSA ones *skip*
  there — their coverage is local-only. The clock and persist tests are pure and
  do run, including the re-cue regression, which is the important one.

## Recommended route

**1. ~~Open the PR~~ — done, #77.** The two `.gitignore` commits rode along in
it; they are unrelated to MIDI and could have been split, which is worth doing
next time rather than unpicking now.

**2. An owner ear-check is still owed.** The wire is proven; the drum-kit end is
not. Nobody has yet heard a GM drum machine play a channel-10 part end to end.
Two caveats for whoever sets that up: the owner's clip has since been edited well
past the Composer's output (all one key, then keys 51-58, channels mixed), so a
freshly generated groove is the cleaner subject; and
`fluidsynth -o synth.midi-bank-select=gm` or `qsynth` is a far simpler target
than Hydrogen, which piles transport slaving, a MIDI action map and a kit mapping
on top of the thing under test.

**3. Worth filing: the piano roll can neither show nor set a note's channel.**
The sharpest gap this work exposed. A channel-mixed clip is *invisible* in the
UI, and under the default "from clip" routing its notes split across two devices
— the owner lost exactly one hit that way with no means to notice or repair it.
Two small independent pieces:
   - a **"set MIDI channel" action for the selected notes** — there is no way to
     repair a mixed clip today; `Op::MidiSetNotes` already carries whole note
     lists, so this is frontend work plus a menu entry;
   - some **indication** that a clip is mixed — a per-note tint, or a
     `channels: 1, 10` badge on the clip header.

   `channelForNewNote` only stops *new* notes from drifting; it repairs nothing.

**4. Decisions that are the owner's, not to be guessed at.**
   - **Mute does not stop MIDI out.** Documented as deliberate (mute is a mixer
     control and the external device is not on AURA's mixer), but it is arguable,
     and it interacts with what "keep this part out of the bounce" should mean.
   - **`export_song` ignores routing on purpose** — routing is per-machine app
     config, so honouring it would make one project bounce differently on the
     machine that has the synth plugged in. Recorded in
     `audio::offline::build_graph`'s comment and in `docs/midi-output.md`. Revisit
     only if the owner explicitly wants machine-dependent exports.

**5. Knobs to reach for if a new report arrives.**
   - `drift_tolerance` is `max(rate/50, 2 * block)`, and `block_frames` is 0
     until the first callback — so the 20 ms floor applies for a moment at
     startup. A burst of re-cues in the first instants of playback would point
     here.
   - The resync catch-up limit is `2 * drift_tolerance` (~85 ms at 1024/48 kHz).
     A genuine *seek* smaller than that now replays the notes inside it rather
     than skipping them. Deliberate and commented; if a flurry on tiny seeks is
     ever a complaint, that is the line to move.

**6. Not AURA bugs, but they cost hours — check these before believing a report.**
   - **`pipewire-pulse` can wedge.** `pactl info` HANGS while `aplay -l` answers
     fine, and the app logs `PulseAudio: Unable to connect: Timeout`. In that
     state a full `cargo test` fails **20** tests — every engine/transport/
     loopjam/meter test plus the gated plugin probes — which looks exactly like a
     regression in the audio path. Run `timeout 8 pactl info` first. Recovery:
     `systemctl --user restart wireplumber pipewire-pulse pipewire`, then
     re-check `pactl get-default-sink`, which may flip to Bluetooth (its own
     documented failure mode). Memory: `aura-build-box-constraints`.
   - **Two paths into one device.** `aconnect -l` showed AURA subscribed both
     directly to `Hydrogen:Hydrogen Midi-In` *and* to `Midi Through`, which
     Hydrogen also subscribes to — every clock pulse and note arriving twice, 48
     ppq instead of 24. AURA cannot detect this; from its side they are two
     unrelated ports. Now in the troubleshooting docs.
   - **Carla with a JACK engine publishes no ALSA-seq port at all**, so it is
     invisible to `midir`'s ALSA backend however long you wait. The way in is
     PipeWire's ALSA-seq/JACK-MIDI bridge (client `PipeWire-RT-Event`), patched
     from Carla's own patchbay.

**7. Housekeeping left behind.** `~/.config/aura/midi-routing.json` carries two
stale `open_ports` entries from this session's debugging — an `aseqdump:...` port
(litter from the wire snooping) and a long-dead `Carla:Midi Through:...` name.
Harmless: they resolve to nothing and are skipped, and they disappear on the next
re-patch. A tmux session named `aura-midi` may still be running the app.

## How it was measured, for the next person

Guessing at this cost far more than measuring did. What actually answered
questions, in the order it paid off:

1. **The owner's own files.** `~/.config/aura/midi-routing.json` (which track is
   routed where, on which channel); `project.json` plus the AMEV chunk under
   `events/` for the notes — header `<IHHII` = magic, version, columns, ppq,
   count, then count x 16 bytes of `<IIBBBBf` = tick, length, kind, key,
   velocity, **channel**, f32; `~/.hydrogen/hydrogen.conf` (`channel_filter: -1`
   ruled out a channel filter); `aconnect -l` for the real ALSA graph.
2. **The app's own log.** `set_midi_track_route: ... channel=Some(0)` twelve
   seconds after `channel=None` settled "is the forced channel a bug or a
   setting" in one line.
3. **`aseqdump -p <client>:<port>`** on the port the device listens on. This is
   what turned "it feels wrong" into 31 676 versus 1. Subscribe to the shared
   port (`Midi Through`) rather than to AURA's own client, and the capture
   survives AURA reopening its ports.
4. **An in-crate test over `ControlPlane` + a `midir` virtual port**
   (`midi_out::route_e2e_test`) for anything that must become a regression test.
   The MCP roster has no MIDI tools, so a routing repro cannot be built through
   it — and do NOT put a real audio engine in such a test: two of them in one
   process open the default output device in turn and the second gets no
   callbacks, which made these tests flaky 2 runs in 8 (`15d0c0c`).

Driving the running app from outside: memory
`aura-drive-live-app-diagnostics`.
