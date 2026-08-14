# Backlog: library & browser panel (Track E)

**Captured 2026-08-14 from the owner ("biblioteksfunksjon for clips og
lyder og sånt").** Nothing like this exists at HEAD — imports are
one-shot (drop zone / dialog), and there is no place to keep, browse, or
audition material.

## What it is

A side panel with three roots:

1. **Samples** — user-configured folders (plus a default library dir),
   showing audio files with **audition preview** (click = hear it, without
   touching the project; reuse the sampler-preview voice path the MIDI
   monitor already uses — it is exactly an "audition" engine).
2. **Project clips** — the current project's audio/MIDI clips (and later
   patterns/`ContentId` content), draggable back onto tracks.
3. **Presets/instruments** — plugin/instrument presets (Zyn patches
   already enumerate via `zyn_list_patches`; sampler instruments via
   `sampler_list_instruments`).

Drag out of the panel onto a track/position = the existing import/clip-add
commands (all channel-routed post-Plan-E, so undoable for free).

## Design notes

- Frontend: a panel component + a small store; browsing/scanning backend
  commands (`library_scan(dir)`, cached listing with file metadata;
  waveform thumbnails can reuse the pyramid cache keyed by source).
- Audition: `sampler_preview`-style one-shot playback of a decoded file
  head (bounded seconds, cancel on next click) — no document coupling,
  Gate-safe category.
- Config: library folder list is app config (prefs-side or backend config
  file — NOT project state).
- Later: tagging/favorites, search, drag MIDI files in (SMF import
  already exists), drag clips OUT to the OS (ties into track C's
  cross-instance work).

## Suggested cut

1. Panel + samples root + audition + drag-to-track (wires to
   import_audio_clip at drop position).
2. Project-clips root (drag = clip copy via the channel).
3. Presets root (Zyn/sampler lists; apply = existing commands).
4. Folders config UI + persistence; thumbnails.
