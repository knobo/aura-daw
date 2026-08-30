//! MIDI launch map: note → region or clip, plus the echo / self-trigger
//! guards so a clip AURA itself is playing cannot start itself via
//! MIDI-out loopback.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use serde::{Deserialize, Serialize};

pub const ECHO_WINDOW_MS: u64 = 80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LaunchTarget {
    Region {
        #[serde(alias = "start_ticks")]
        start_ticks: u64,
        #[serde(alias = "length_ticks")]
        length_ticks: u64,
        #[serde(alias = "track_ids")]
        track_ids: Vec<String>,
    },
    /// Kept so a project saved before Task 12's migration still
    /// deserializes; nothing in this crate constructs one any more.
    Clip {
        #[serde(alias = "clip_id")]
        clip_id: String,
    },
    /// Plan V: the binding fires a player. What a `Clip` target becomes on
    /// open — see [`migrate_clip_targets_to_players`].
    Player {
        #[serde(alias = "player_id")]
        player_id: crate::ids::PlayerId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchBinding {
    pub id: String,
    pub name: String,
    pub note: u8,
    /// `None` = any channel.
    pub channel: Option<u8>,
    pub target: LaunchTarget,
}

impl LaunchBinding {
    pub fn matches(&self, note: u8, channel: u8) -> bool {
        self.note == note && self.channel.map(|c| c == channel).unwrap_or(true)
    }
}

pub const DEFAULT_MAP_ID: &str = "default";

/// How a drive-clip note plays a scene.
///
/// * `Gate` — sound while the MIDI note is held; note-off cuts the scene.
/// * `OneShot` — the note is a trigger; the scene plays to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum LaunchPlayMode {
    #[default]
    Gate,
    OneShot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchMap {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<LaunchBinding>,
    #[serde(default)]
    pub drive_clip_ids: Vec<String>,
    #[serde(default)]
    pub play_mode: LaunchPlayMode,
}

impl LaunchMap {
    pub fn named(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            bindings: Vec::new(),
            drive_clip_ids: Vec::new(),
            play_mode: LaunchPlayMode::Gate,
        }
    }

    pub fn default_map() -> Self {
        Self::named(DEFAULT_MAP_ID, "Launcher 1")
    }
}

pub fn migrate_legacy_maps(
    maps: Option<Vec<LaunchMap>>,
    bindings: Vec<LaunchBinding>,
    drive_clip_ids: Vec<String>,
) -> Vec<LaunchMap> {
    if let Some(maps) = maps {
        if !maps.is_empty() {
            return maps;
        }
    }
    if bindings.is_empty() && drive_clip_ids.is_empty() {
        return vec![LaunchMap::default_map()];
    }
    let mut m = LaunchMap::default_map();
    m.bindings = bindings;
    m.drive_clip_ids = drive_clip_ids;
    vec![m]
}

/// The instrument the clip's source track rendered through, in
/// `TrackState::instrument_id`'s own vocabulary — what a migrated player
/// must keep so the pad sounds the same as it did through the track.
fn instrument_of_track(
    tracks: &[crate::audio::types::TrackState],
    track_id: &crate::ids::TrackId,
) -> Option<String> {
    tracks.iter().find(|t| &t.id == track_id).and_then(|t| t.instrument_id.clone())
}

/// Turn every resolvable `LaunchTarget::Clip` into a player (Plan V, V2's
/// migration gate — Task 12). Idempotent: a binding already pointing at a
/// player is left alone, so running this twice over the same maps/players
/// (an unsaved re-open, or a stray double call) mints no second player —
/// the second pass finds nothing left to convert.
///
/// Bindings that name the SAME clip share ONE player — a drum kit's two
/// pads on one clip is one instrument, and a player that already exists for
/// that clip (loaded from a previously-saved migration) is reused rather
/// than re-minted, which is what keeps identity stable across a save/reload
/// instead of just the count.
///
/// A binding whose clip is gone is left EXACTLY as it was — still a `Clip`
/// target, still dangling — rather than dropped. Before this migration
/// existed, pressing such a pad did nothing but `log::warn!` (see
/// `launch_fire_from`'s `Clip` arm); dropping the binding here would be
/// LOUDER at open time (this same `log::warn!`, moved earlier) but far
/// LESS recoverable, since the user's note-to-pad mapping would be gone
/// rather than sitting there, still editable, waiting for a rebind. Failing
/// the whole open over one dangling reference is worse still — that loses
/// every OTHER binding in the project too.
pub fn migrate_clip_targets_to_players(
    maps: &mut [LaunchMap],
    midi_clips: &[crate::midi::MidiClip],
    tracks: &[crate::audio::types::TrackState],
    players: &mut Vec<crate::audio::player::Player>,
) -> usize {
    use crate::audio::player::{Player, PlayerSource};

    let mut by_clip: std::collections::HashMap<String, crate::ids::PlayerId> = players
        .iter()
        .filter_map(|p| match &p.source {
            PlayerSource::MidiClip { clip_id, .. } => Some((clip_id.to_string(), p.id.clone())),
            _ => None,
        })
        .collect();
    let mut migrated = 0usize;

    for map in maps.iter_mut() {
        for b in map.bindings.iter_mut() {
            let LaunchTarget::Clip { clip_id } = &b.target else {
                continue;
            };
            let Some(clip) = midi_clips.iter().find(|c| c.id.as_str() == clip_id.as_str()) else {
                log::warn!(
                    "launch: binding {} ({}) names a clip that is gone ({clip_id}); leaving it unbound",
                    b.id,
                    b.name
                );
                continue;
            };
            let clip_id = clip_id.clone();
            let player_id = by_clip
                .entry(clip_id)
                .or_insert_with(|| {
                    let mut p = Player::new(
                        crate::ids::PlayerId::for_migrated_clip(clip.id.as_str()),
                        b.name.clone(),
                    );
                    p.source = PlayerSource::MidiClip {
                        clip_id: clip.id.clone(),
                        instrument_id: instrument_of_track(tracks, &clip.track_id),
                    };
                    let id = p.id.clone();
                    players.push(p);
                    id
                })
                .clone();
            b.target = LaunchTarget::Player { player_id };
            migrated += 1;
        }
    }
    migrated
}

pub fn all_bindings(maps: &[LaunchMap]) -> Vec<LaunchBinding> {
    maps.iter()
        .flat_map(|m| m.bindings.iter().cloned())
        .collect()
}

pub fn all_drive_clip_ids(maps: &[LaunchMap]) -> Vec<String> {
    maps.iter()
        .flat_map(|m| m.drive_clip_ids.iter().cloned())
        .collect()
}

pub fn ensure_maps(maps: &mut Vec<LaunchMap>) {
    if maps.is_empty() {
        maps.push(LaunchMap::default_map());
    }
}

pub fn map_index(maps: &[LaunchMap], map_id: &str) -> Option<usize> {
    if map_id.is_empty() {
        return if maps.is_empty() { None } else { Some(0) };
    }
    maps.iter().position(|m| m.id == map_id)
}

/// A drive clip only sounds when it still sits on a live track, and when
/// a clip-editor "play this clip" focus is set, only that clip.
pub fn drive_clip_eligible(
    clip_id: &str,
    track_id: &str,
    drive_ids: &[String],
    live_tracks: &std::collections::HashSet<String>,
    focus: Option<&str>,
) -> bool {
    if !drive_ids.iter().any(|id| id == clip_id) {
        return false;
    }
    if !live_tracks.contains(track_id) {
        return false;
    }
    match focus {
        Some(id) => clip_id == id,
        None => true,
    }
}

/// Drive poller window: half-open `(last, pos]` when time moves forward,
/// or the two wrap halves `(last, +inf) ∪ (0, pos]` after a loop wrap.
pub fn drive_event_in_window(sample: u64, last: u64, pos: u64) -> bool {
    if pos >= last {
        sample > last && sample <= pos
    } else {
        sample > last || sample <= pos
    }
}

/// Play-edge window: `[last, pos]` so a note at sample 0 fires.
/// `last = pos.saturating_sub(1)` plus the half-open window is empty at 0.
pub fn drive_in_window(sample: u64, last: u64, pos: u64, play_edge: bool) -> bool {
    if play_edge {
        sample >= last && sample <= pos
    } else {
        drive_event_in_window(sample, last, pos)
    }
}

/// Which endings this poll owes the worker a `Release`, and the memory that
/// keeps the edge an EDGE. `sounding` is the runtime's ledger; `still_on`
/// answers whether that binding's clock is running.
///
/// The drive loop calls this and its test calls this — the same three lines,
/// not a copy. The first version of the edge memory lived inline in the loop
/// and its test re-implemented it; the copy had the identical defect, so the
/// test passed while production was broken (fix round 2, finding 2). A test
/// that cannot fail for the reason it exists is worse than no test.
///
/// TWO rules, and only one of them is about correctness:
///
/// * `sent.remove` on the ON EDGE. A binding that is sounding again has spent
///   whatever we sent for its previous ending. Without this, an id that is
///   announced and re-fired inside one 8 ms poll gap is still in the ledger
///   when the next poll runs, so the memory keeps it, and every LATER ending
///   of that binding is swallowed: no `LaunchFired { playing: false }` ever
///   reaches the frontend, the pad stays lit, and the id sticks in the ledger
///   until a transport stop clears it. Retriggering a pad as its clip ends is
///   ordinary launcher use, and a drive clip with a repeating note does it by
///   itself.
/// * `retain` against the ledger is hygiene, not correctness: it bounds the
///   set to what is actually sounding rather than to every binding ever
///   fired. It is NOT what makes the edge work — it cannot be, because it
///   sees the re-fired id as still present.
///
/// One `Release` per ending matters because `enqueue_release` is a `try_send`
/// into an 8-slot channel SHARED with `FireCmd::Start`: duplicates of one
/// ending fill it and silently drop the user's next pad press.
pub fn releases_to_enqueue(
    sent: &mut std::collections::HashSet<String>,
    sounding: &[String],
    mut still_on: impl FnMut(&str) -> bool,
) -> Vec<String> {
    sent.retain(|id| sounding.iter().any(|s| s == id));
    let mut out = Vec::new();
    for id in sounding {
        if still_on(id) {
            sent.remove(id);
            continue;
        }
        if sent.insert(id.clone()) {
            out.push(id.clone());
        }
    }
    out
}

/// Clip-as-instrument: match the written key only. Binding `channel` is a
/// hardware MIDI-in filter; a drive clip must still fire `channel: Some(n)`.
pub fn resolve_drive<'a>(bindings: &'a [LaunchBinding], key: u8) -> Option<&'a LaunchBinding> {
    bindings.iter().find(|b| b.note == key)
}

pub fn find_binding<'a>(
    maps: &'a [LaunchMap],
    id: &str,
) -> Option<(&'a LaunchMap, &'a LaunchBinding)> {
    maps.iter()
        .find_map(|m| m.bindings.iter().find(|b| b.id == id).map(|b| (m, b)))
}

/// Map that owns `binding_id`, else the named map, else the first map.
pub fn map_index_for_binding(maps: &[LaunchMap], map_id: &str, binding_id: &str) -> Option<usize> {
    if !map_id.is_empty() {
        return map_index(maps, map_id);
    }
    maps.iter()
        .position(|m| m.bindings.iter().any(|b| b.id == binding_id))
        .or_else(|| if maps.is_empty() { None } else { Some(0) })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoNote {
    pub note: u8,
    pub channel: u8,
    pub at_ms: u64,
}

pub fn resolve<'a>(
    bindings: &'a [LaunchBinding],
    note: u8,
    channel: u8,
) -> Option<&'a LaunchBinding> {
    bindings.iter().find(|b| b.matches(note, channel))
}

pub fn incoming_is_echo(recent: &[EchoNote], incoming: EchoNote, window_ms: u64) -> bool {
    recent.iter().any(|s| {
        s.note == incoming.note
            && s.channel == incoming.channel
            && incoming.at_ms >= s.at_ms
            && incoming.at_ms - s.at_ms <= window_ms
    })
}

/// True when the clip contains its own trigger note. Used for a UI warning;
/// the live guards are the same-clip target skip, the echo window, and
/// hardware `armed`.
pub fn clip_would_self_trigger(
    notes: &[(u8, u8)],
    trigger_note: u8,
    trigger_channel: Option<u8>,
) -> bool {
    notes
        .iter()
        .any(|(key, ch)| *key == trigger_note && trigger_channel.map(|c| c == *ch).unwrap_or(true))
}

/// The live drive-loop guard: does firing `target` fire the clip that is
/// driving it right now? A same-clip `Clip` target names the clip
/// directly; a `Player` target (what `Clip` becomes on migration) has to
/// be resolved through `players` to its `PlayerSource::MidiClip`'s
/// `clip_id` instead. `Region` never self-triggers — a scene names
/// tracks, never the driving clip.
///
/// Extracted so the drive loop and its test call the SAME match, not a
/// copy (this file's own rule, see `releases_to_enqueue`'s doc): fix
/// round 1, Important 3 found this guard as an inline `if let` matching
/// only `Clip`, so a migrated `Player` target never hit it and re-fired
/// its own player on every drive note-on for the clip it plays.
pub fn binding_self_triggers(
    target: &LaunchTarget,
    players: &[crate::audio::player::Player],
    driving_clip_id: &str,
) -> bool {
    match target {
        LaunchTarget::Clip { clip_id } => clip_id.as_str() == driving_clip_id,
        LaunchTarget::Player { player_id } => players.iter().any(|p| {
            &p.id == player_id
                && matches!(
                    &p.source,
                    crate::audio::player::PlayerSource::MidiClip { clip_id, .. }
                        if clip_id.as_str() == driving_clip_id
                )
        }),
        LaunchTarget::Region { .. } => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FireOrigin {
    Hardware,
    Drive,
    /// UI audition: same as Drive (no seek/loop) but always restarts.
    Preview,
}

pub enum FireCmd {
    Start(LaunchBinding, FireOrigin),
    Release(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingDecision {
    Pass,
    Learn { note: u8, channel: u8 },
    Echo,
    Suppressed,
    Fire(LaunchBinding),
}

pub fn launch_trace_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("AURA_LAUNCH_TRACE").is_some())
}

pub fn launch_trace(msg: impl std::fmt::Display) {
    if launch_trace_enabled() {
        log::info!("launch: {msg}");
    }
}

/// Process-wide launch runtime: binding snapshot, echo ring, learn arm,
/// and a held-pad debounce. MIDI-out loopback is filtered by the echo
/// window — a clip cannot start itself that way. The fire callback runs
/// on a worker thread so the midir callback never waits on transport.
pub struct LaunchRuntime {
    maps: Mutex<Vec<LaunchMap>>,
    echo: Mutex<Vec<EchoNote>>,
    learning: Mutex<Option<String>>,
    armed: Mutex<Option<(u8, u8)>>,
    origin: Instant,
    fire_tx: Mutex<Option<std::sync::mpsc::SyncSender<FireCmd>>>,
    last_learn: Mutex<Option<(u8, u8)>>,
    gen: AtomicU64,
    drive_started: AtomicBool,
    /// Every binding whose scene is currently sounding, as far as the
    /// FRONTEND has been told. Plural since Task 8: scenes each own a clock
    /// now, so any number of them can sound at once — this was
    /// `overlay_id: Option<String>` for exactly as long as there was one
    /// shadow playhead to be in.
    ///
    /// It is not the truth about what is playing (the clock table is); it is
    /// the drive thread's ledger of which endings it still owes a
    /// `LaunchFired { playing: false }`, so an ending is announced once.
    sounding: Mutex<std::collections::HashSet<String>>,
    drive_focus: Mutex<Option<String>>,
}

impl Default for LaunchRuntime {
    fn default() -> Self {
        Self {
            maps: Mutex::new(Vec::new()),
            echo: Mutex::new(Vec::new()),
            learning: Mutex::new(None),
            armed: Mutex::new(None),
            origin: Instant::now(),
            fire_tx: Mutex::new(None),
            last_learn: Mutex::new(None),
            gen: AtomicU64::new(0),
            drive_started: AtomicBool::new(false),
            sounding: Mutex::new(std::collections::HashSet::new()),
            drive_focus: Mutex::new(None),
        }
    }
}

impl LaunchRuntime {
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn set_maps(&self, maps: Vec<LaunchMap>) {
        *self.maps.lock() = maps;
        self.gen.fetch_add(1, Relaxed);
    }

    pub fn maps(&self) -> Vec<LaunchMap> {
        self.maps.lock().clone()
    }

    /// Test/helper: write bindings onto the first launcher (created if needed).
    pub fn set_bindings(&self, bindings: Vec<LaunchBinding>) {
        let mut maps = self.maps.lock();
        if maps.is_empty() {
            maps.push(LaunchMap::default_map());
        }
        maps[0].bindings = bindings;
        self.gen.fetch_add(1, Relaxed);
    }

    pub fn bindings(&self) -> Vec<LaunchBinding> {
        all_bindings(&self.maps.lock())
    }

    pub fn set_learning(&self, id: Option<String>) {
        *self.learning.lock() = id;
        *self.last_learn.lock() = None;
    }

    pub fn take_learn(&self) -> Option<(u8, u8)> {
        self.last_learn.lock().take()
    }

    pub fn install_fire(&self, f: Arc<dyn Fn(FireCmd) + Send + Sync>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let _ = std::thread::Builder::new()
            .name("aura-launch-fire".into())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    f(cmd);
                }
            });
        *self.fire_tx.lock() = Some(tx);
    }

    /// Record that this binding's scene has been fired. Called AFTER the
    /// clock is actually running, so the drive thread's release edge — "in
    /// the ledger, but its clock is off" — cannot fire on the gap between
    /// the two and announce an ending that never happened.
    pub fn mark_sounding(&self, id: &str) {
        self.sounding.lock().insert(id.to_string());
    }

    /// Take this binding out of the ledger, reporting whether it was in it.
    /// False means its ending has already been announced, which is what
    /// makes `stop_drive_launch` idempotent — the guard
    /// `overlay_id == Some(id)` used to be.
    pub fn take_sounding(&self, id: &str) -> bool {
        self.sounding.lock().remove(id)
    }

    pub fn sounding_ids(&self) -> Vec<String> {
        self.sounding.lock().iter().cloned().collect()
    }

    pub fn clear_sounding(&self) {
        self.sounding.lock().clear();
    }

    pub fn set_drive_focus(&self, id: Option<String>) {
        *self.drive_focus.lock() = id;
    }

    pub fn drive_focus(&self) -> Option<String> {
        self.drive_focus.lock().clone()
    }

    pub fn record_out(&self, note: u8, channel: u8) {
        let at = self.now_ms();
        let mut echo = self.echo.lock();
        echo.push(EchoNote {
            note,
            channel,
            at_ms: at,
        });
        let cutoff = at.saturating_sub(ECHO_WINDOW_MS * 4);
        echo.retain(|e| e.at_ms >= cutoff);
    }

    pub fn clear_armed(&self) {
        *self.armed.lock() = None;
    }

    pub fn set_drive_clips(&self, ids: Vec<String>) {
        let mut maps = self.maps.lock();
        if maps.is_empty() {
            maps.push(LaunchMap::default_map());
        }
        maps[0].drive_clip_ids = ids;
    }

    /// Watch the transport and fire launch bindings from clips marked as
    /// launch-map instruments. Hardware `armed` is not involved.
    /// `tables` is here for the release edge below: whether a scene is still
    /// sounding is a property of the CURRENT graph's clock table now
    /// (Plan V — V2), not of a `SharedRt` atomic, so it has to be read where
    /// the truth lives.
    pub fn attach_drive(
        &self,
        shared: Arc<crate::audio::rt::SharedRt>,
        session: Arc<parking_lot::Mutex<crate::control::Session>>,
        tables: crate::audio::rt::SharedGraphTables,
    ) {
        if self.drive_started.swap(true, Relaxed) {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("aura-launch-drive".into())
            .spawn(move || {
                let mut last = 0u64;
                let mut was_playing = false;
                let mut play_started = Instant::now();
                let mut last_onset: std::collections::HashMap<String, u64> =
                    std::collections::HashMap::new();
                // Ids already put on the fire channel as a Release — see
                // `releases_to_enqueue`, which owns the edge logic so that
                // this loop and its test run the same code.
                let mut release_sent: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                loop {
                    std::thread::sleep(Duration::from_millis(8));
                    // The release edge, once per sounding scene. Task 8 made
                    // this a set rather than one `overlay_was_on` bool: N
                    // scenes each own a clock, so N of them can reach their
                    // end independently, and each owes the frontend its own
                    // `LaunchFired { playing: false }`.
                    let sounding = runtime().sounding_ids();
                    for id in releases_to_enqueue(&mut release_sent, &sounding, |id| {
                        let t = tables.lock();
                        t.scene_clocks.get(id).is_some_and(|&c| t.clocks.is_on(c))
                    }) {
                        runtime().enqueue_release(id);
                    }
                    // The other half of every ending: hand the tracks back,
                    // but only once a rendered block has delivered the
                    // `all_notes_off` the cut left behind. See
                    // `GraphTables::release_finished_scenes` — this is the
                    // only caller, and the only place a scene's slots are
                    // released.
                    tables.lock().release_finished_scenes();
                    let playing = shared.playing.load(Relaxed);
                    let pos = shared.position.load(Relaxed);
                    if !playing {
                        last = pos;
                        was_playing = false;
                        last_onset.clear();
                        continue;
                    }
                    if pos < last {
                        let ms = play_started.elapsed().as_millis();
                        launch_trace(format!(
                            "drive wrap/seek pos={pos} last={last} since_play_ms={ms}"
                        ));
                        // Clip-edit seek right after Play must not
                        // retrigger. A later loop wrap may.
                        if ms <= 80 {
                            last = pos;
                            continue;
                        }
                        last_onset.clear();
                        // Fall through and scan the wrap halves.
                    } else {
                        let seek_gap = u64::from(shared.sample_rate.load(Relaxed).max(1))
                            .saturating_mul(80)
                            / 1000;
                        if pos.saturating_sub(last) > seek_gap.max(1) {
                            launch_trace(format!(
                                "drive forward-seek pos={pos} last={last} gap={}",
                                pos - last
                            ));
                            last = pos.saturating_sub(1);
                            continue;
                        }
                    }
                    let mut just_started = false;
                    if !was_playing {
                        // Inclusive of the stop/seek sample. Do not
                        // saturating_sub(1): at pos 0 that empties the window.
                        was_playing = true;
                        just_started = true;
                        play_started = Instant::now();
                        launch_trace(format!("drive play-edge pos={pos} last={last}"));
                    }
                    let launchers = runtime().maps();
                    if launchers.iter().all(|m| m.drive_clip_ids.is_empty()) {
                        last = pos;
                        continue;
                    }
                    let (clips, ppq, tempo, live_tracks, players) = {
                        let s = session.lock();
                        let live_tracks: std::collections::HashSet<String> = s
                            .store
                            .tracks
                            .iter()
                            .map(|t| t.id.to_string())
                            .collect();
                        (
                            s.midi.clips.clone(),
                            s.midi.ppq,
                            s.midi.tempo_events.clone(),
                            live_tracks,
                            s.store.players.clone(),
                        )
                    };
                    let Ok(tempo_map) =
                        crate::midi::TempoMap::new(ppq, tempo, shared.sample_rate.load(Relaxed).max(1))
                    else {
                        last = pos;
                        continue;
                    };
                    let focus = runtime().drive_focus();
                    let mut fired = std::collections::HashSet::new();
                    let mut released = std::collections::HashSet::new();
                    for launcher in &launchers {
                        if launcher.drive_clip_ids.is_empty() {
                            continue;
                        }
                        for clip in clips.iter().filter(|c| {
                            drive_clip_eligible(
                                c.id.as_str(),
                                c.track_id.as_str(),
                                &launcher.drive_clip_ids,
                                &live_tracks,
                                focus.as_deref(),
                            )
                        }) {
                            let evs = crate::midi::schedule::clip_drive_events(clip, &tempo_map);
                            if just_started && launch_trace_enabled() {
                                let ons: Vec<_> = evs
                                    .iter()
                                    .filter(|e| e.velocity > 0)
                                    .map(|e| format!("{}@{}", e.key, e.sample))
                                    .collect();
                                launch_trace(format!(
                                    "drive clip={} notes={} ons=[{}]",
                                    clip.id,
                                    clip.notes.len(),
                                    ons.join(",")
                                ));
                            }
                            for ev in evs {
                                if !drive_in_window(ev.sample, last, pos, just_started) {
                                    continue;
                                }
                                let Some(b) = resolve_drive(&launcher.bindings, ev.key) else {
                                    continue;
                                };
                                if binding_self_triggers(&b.target, &players, clip.id.as_str()) {
                                    continue;
                                }
                                if ev.velocity == 0 {
                                    if launcher.play_mode == LaunchPlayMode::Gate
                                        && released.insert(b.id.clone())
                                    {
                                        launch_trace(format!(
                                            "drive release clip={} note={} sample={} -> {}",
                                            clip.id, ev.key, ev.sample, b.id
                                        ));
                                        runtime().enqueue_release(b.id.clone());
                                    }
                                    continue;
                                }
                                if !fired.insert(b.id.clone()) {
                                    launch_trace(format!(
                                        "drive skip tick-dup clip={} note={} sample={} last={} pos={} -> {}",
                                        clip.id, ev.key, ev.sample, last, pos, b.id
                                    ));
                                    continue;
                                }
                                if last_onset.get(&b.id) == Some(&ev.sample) {
                                    launch_trace(format!(
                                        "drive skip same-onset clip={} note={} sample={} last={} pos={} -> {}",
                                        clip.id, ev.key, ev.sample, last, pos, b.id
                                    ));
                                    continue;
                                }
                                last_onset.insert(b.id.clone(), ev.sample);
                                log::info!(
                                    "launch: drive clip={} note={} sample={} last={} pos={} -> {}",
                                    clip.id,
                                    ev.key,
                                    ev.sample,
                                    last,
                                    pos,
                                    b.id
                                );
                                runtime().enqueue_fire(b.clone(), FireOrigin::Drive);
                            }
                        }
                    }
                    last = pos;
                }
            });
    }

    pub fn decide(&self, on: bool, note: u8, channel: u8) -> IncomingDecision {
        if !on {
            let mut armed = self.armed.lock();
            if *armed == Some((note, channel)) {
                *armed = None;
            }
            return IncomingDecision::Pass;
        }
        if self.learning.lock().is_some() {
            *self.last_learn.lock() = Some((note, channel));
            return IncomingDecision::Learn { note, channel };
        }
        let now = self.now_ms();
        let echo = self.echo.lock();
        if incoming_is_echo(
            &echo,
            EchoNote {
                note,
                channel,
                at_ms: now,
            },
            ECHO_WINDOW_MS,
        ) {
            return IncomingDecision::Echo;
        }
        drop(echo);
        if *self.armed.lock() == Some((note, channel)) {
            return IncomingDecision::Suppressed;
        }
        let maps = self.maps.lock();
        let bindings = all_bindings(&maps);
        match resolve(&bindings, note, channel).cloned() {
            Some(b) => IncomingDecision::Fire(b),
            None => IncomingDecision::Pass,
        }
    }

    pub fn enqueue_fire(&self, b: LaunchBinding, origin: FireOrigin) {
        if let Some(tx) = self.fire_tx.lock().as_ref() {
            let _ = tx.try_send(FireCmd::Start(b, origin));
        }
    }

    pub fn enqueue_release(&self, id: String) {
        if let Some(tx) = self.fire_tx.lock().as_ref() {
            let _ = tx.try_send(FireCmd::Release(id));
        }
    }

    /// MIDI-in callback: decide, and if it's a Fire, enqueue the binding
    /// for the worker (never run transport on this thread). Returns true
    /// when the note is consumed as a launch / learn / echo / debounce.
    pub fn on_incoming(&self, on: bool, note: u8, channel: u8) -> bool {
        match self.decide(on, note, channel) {
            IncomingDecision::Pass => false,
            IncomingDecision::Learn { .. } => true,
            IncomingDecision::Echo | IncomingDecision::Suppressed => true,
            IncomingDecision::Fire(b) => {
                *self.armed.lock() = Some((note, channel));
                launch_trace(format!("hardware note={} ch={} -> {}", note, channel, b.id));
                self.enqueue_fire(b, FireOrigin::Hardware);
                true
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSnapshot {
    pub maps: Vec<LaunchMap>,
}

impl crate::control::ControlPlane {
    pub fn launch_snapshot(&self) -> LaunchSnapshot {
        let s = self.session().lock();
        let mut maps = s.midi.launch_maps.clone();
        ensure_maps(&mut maps);
        LaunchSnapshot { maps }
    }

    pub fn set_launch_binding(
        &self,
        map_id: String,
        id: String,
        binding: Option<LaunchBinding>,
        meta: crate::control::op::TxMeta,
    ) -> Result<LaunchSnapshot, String> {
        self.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::LaunchBindingSet {
                map_id,
                id,
                binding,
            })?;
            Ok(())
        })?;
        self.emit_launch_changed();
        Ok(self.launch_snapshot())
    }

    pub fn set_launch_drive(
        &self,
        map_id: String,
        clip_id: String,
        on: bool,
        meta: crate::control::op::TxMeta,
    ) -> Result<LaunchSnapshot, String> {
        self.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::LaunchDriveSet {
                map_id,
                clip_id,
                on,
            })?;
            Ok(())
        })?;
        self.emit_launch_changed();
        Ok(self.launch_snapshot())
    }

    pub fn set_launch_map(
        &self,
        id: String,
        map: Option<LaunchMap>,
        meta: crate::control::op::TxMeta,
    ) -> Result<LaunchSnapshot, String> {
        self.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::LaunchMapSet { id, map })?;
            Ok(())
        })?;
        self.emit_launch_changed();
        Ok(self.launch_snapshot())
    }

    pub fn launch_fire(&self, id: &str) -> Result<(), String> {
        self.launch_fire_from(id, FireOrigin::Hardware)
    }

    pub fn launch_fire_from(&self, id: &str, origin: FireOrigin) -> Result<(), String> {
        let player_target = {
            let s = self.session().lock();
            let (_, b) = find_binding(&s.midi.launch_maps, id)
                .ok_or_else(|| format!("unknown launch binding: {id}"))?;
            match &b.target {
                LaunchTarget::Player { player_id } => Some(player_id.clone()),
                _ => None,
            }
        };
        if let Some(player_id) = player_target {
            // A player owns its own clock and playhead (V-1) and is fired
            // through `player_fire`, not `fire_scene` — none of the
            // scene bookkeeping below (tick resolution, track hijack,
            // `LaunchFired`) applies to it.
            return self.player_fire(player_id.as_str());
        }
        let rate = self.transport_state().sample_rate;
        let (start_ticks, length_ticks, ppq, events, track_ids, name) = {
            let s = self.session().lock();
            let (_, b) = find_binding(&s.midi.launch_maps, id)
                .ok_or_else(|| format!("unknown launch binding: {id}"))?;
            let name = b.name.clone();
            let (start_ticks, length_ticks, track_ids) = match &b.target {
                LaunchTarget::Region {
                    start_ticks,
                    length_ticks,
                    track_ids,
                } => (start_ticks.clone(), length_ticks.clone(), track_ids.clone()),
                LaunchTarget::Clip { clip_id } => {
                    let c = s
                        .midi
                        .clips
                        .iter()
                        .find(|c| c.id.as_str() == clip_id)
                        .ok_or_else(|| format!("launch clip is gone: {clip_id}"))?;
                    (
                        c.timeline_start_ticks,
                        c.length_ticks,
                        vec![c.track_id.to_string()],
                    )
                }
                LaunchTarget::Player { .. } => unreachable!("handled by the early return above"),
            };
            (
                start_ticks,
                length_ticks,
                s.midi.ppq,
                s.midi.tempo_events.clone(),
                track_ids,
                name,
            )
        };
        let map = crate::midi::TempoMap::new(ppq, events, rate.max(1))?;
        let start = map.tick_to_samples(start_ticks);
        let end = map
            .tick_to_samples(start_ticks.saturating_add(length_ticks))
            .max(start + 1);
        log::info!(
            "launch: fire id={id} name={name} origin={origin:?} start={start} end={end} tracks={track_ids:?}"
        );
        // Every origin now does the same thing, because the reason they
        // differed is gone: `Hardware` used to SetLoop + Seek + Play on the
        // arrangement transport (design §2.2), which moved the user's
        // playhead every time they touched a pad. It only ever did that
        // because there was ONE shadow playhead to share and the transport's
        // was it. A scene has its own clock now, so firing one is firing one,
        // whoever pressed it.
        if !self.fire_scene(id, &track_ids, start, end) {
            // Dropped, because the graph has not been rebuilt since this
            // binding was added. Say nothing to the frontend either: a
            // `LaunchFired { playing: true }` for a scene that is not
            // sounding lights the launcher up with no ending to switch it
            // off, since nothing releases a scene that never entered the
            // ledger. The warn in `fire_scene` is the whole report.
            return Ok(());
        }
        // AFTER the clock is running: the drive thread's release edge is
        // "in the ledger, clock off", and marking first would let a poll
        // landing in between announce an ending that never happened.
        runtime().mark_sounding(id);
        self.emit_launch_fired(LaunchFired {
            id: id.to_string(),
            name,
            origin,
            follow_view: matches!(origin, FireOrigin::Hardware),
            track_ids,
            start_samples: start,
            end_samples: end,
            playing: true,
        });
        Ok(())
    }

    /// One scene has ended (its clip ran out, a gate note lifted, or a
    /// stop-all cut it): hand its tracks back to the arrangement and tell the
    /// frontend. The ledger take is the idempotence guard — the drive thread
    /// may enqueue the same release twice, and the frontend must be told once.
    pub fn stop_drive_launch(&self, id: &str) {
        if !runtime().take_sounding(id) {
            return;
        }
        launch_trace(format!("drive stop {id}"));
        // CUT, not released: the Gate path reaches here with the clock still
        // RUNNING (a note-off lifting mid-clip), so `stop` latches a
        // discontinuity the nodes have not read yet. The drive poll's
        // `release_finished_scenes` hands the tracks back once a rendered
        // block has delivered it.
        self.cut_scene(id);
        self.emit_launch_fired(LaunchFired {
            id: id.to_string(),
            name: String::new(),
            origin: FireOrigin::Drive,
            follow_view: false,
            track_ids: Vec::new(),
            start_samples: 0,
            end_samples: 0,
            playing: false,
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFired {
    pub id: String,
    pub name: String,
    pub origin: FireOrigin,
    pub follow_view: bool,
    pub track_ids: Vec<String>,
    pub start_samples: u64,
    pub end_samples: u64,
    #[serde(default = "launch_playing_default")]
    pub playing: bool,
}

fn launch_playing_default() -> bool {
    true
}

#[tauri::command]
pub fn launch_get(
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<LaunchSnapshot, String> {
    Ok(control.launch_snapshot())
}

#[tauri::command]
pub fn launch_set(
    binding: Option<LaunchBinding>,
    id: Option<String>,
    map_id: Option<String>,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<LaunchSnapshot, String> {
    let map_id = map_id.unwrap_or_default();
    match (binding, id) {
        (None, Some(id)) => control.set_launch_binding(
            map_id,
            id,
            None,
            crate::control::op::TxMeta::user("remove launch"),
        ),
        (Some(mut b), _) => {
            if b.id.is_empty() {
                b.id = uuid::Uuid::new_v4().to_string();
            }
            let key = b.id.clone();
            control.set_launch_binding(
                map_id,
                key,
                Some(b),
                crate::control::op::TxMeta::user("set launch"),
            )
        }
        (None, None) => Err("launch_set needs a binding or an id to delete".into()),
    }
}

#[tauri::command]
pub fn launch_set_drive(
    clip_id: String,
    on: bool,
    map_id: Option<String>,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<LaunchSnapshot, String> {
    control.set_launch_drive(
        map_id.unwrap_or_default(),
        clip_id,
        on,
        crate::control::op::TxMeta::user("set launch drive"),
    )
}

#[tauri::command]
pub fn launch_set_drive_focus(clip_id: Option<String>) {
    runtime().set_drive_focus(clip_id);
}

#[tauri::command]
pub fn launch_set_map(
    id: String,
    map: Option<LaunchMap>,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<LaunchSnapshot, String> {
    let label = if map.is_some() {
        "set launcher"
    } else {
        "remove launcher"
    };
    control.set_launch_map(id, map, crate::control::op::TxMeta::user(label))
}

#[tauri::command]
pub fn launch_fire(
    id: String,
    bypass: Option<bool>,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    if bypass.unwrap_or(false) {
        control.launch_fire_from(&id, FireOrigin::Preview)
    } else {
        control.launch_fire(&id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnNote {
    pub note: u8,
    pub channel: u8,
}

/// Stop the launch overlay. Additive and idempotent: pressing it with
/// nothing launched is a no-op, so the frontend's stop-all can call it
/// unconditionally.
#[tauri::command]
pub fn launch_stop(
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.stop_launch_overlay();
    Ok(())
}

#[tauri::command]
pub fn launch_learn_arm(id: Option<String>) {
    runtime().set_learning(id);
}

#[tauri::command]
pub fn launch_learn_take() -> Option<LearnNote> {
    runtime()
        .take_learn()
        .map(|(note, channel)| LearnNote { note, channel })
}

static RUNTIME: OnceLock<LaunchRuntime> = OnceLock::new();

pub fn runtime() -> &'static LaunchRuntime {
    RUNTIME.get_or_init(LaunchRuntime::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(id: &str, note: u8, channel: Option<u8>) -> LaunchBinding {
        LaunchBinding {
            id: id.into(),
            name: id.into(),
            note,
            channel,
            target: LaunchTarget::Region {
                start_ticks: 0,
                length_ticks: 960,
                track_ids: vec!["t1".into()],
            },
        }
    }

    fn clip(id: &str, note: u8, clip_id: &str) -> LaunchBinding {
        LaunchBinding {
            id: id.into(),
            name: id.into(),
            note,
            channel: None,
            target: LaunchTarget::Clip {
                clip_id: clip_id.into(),
            },
        }
    }

    #[test]
    fn drive_window_is_half_open_forward_and_wraps() {
        assert!(drive_event_in_window(10, 0, 10));
        assert!(!drive_event_in_window(0, 0, 10));
        assert!(!drive_event_in_window(11, 0, 10));
        assert!(drive_event_in_window(990, 980, 10), "wrap tail");
        assert!(drive_event_in_window(5, 980, 10), "wrap start");
        assert!(!drive_event_in_window(11, 980, 10));
        assert!(!drive_event_in_window(500, 980, 10));
    }

    #[test]
    fn target_round_trips_the_frontend_camel_case_shape() {
        let region: LaunchTarget = serde_json::from_str(
            r#"{"kind":"region","startTicks":480,"lengthTicks":960,"trackIds":["t1"]}"#,
        )
        .unwrap();
        assert_eq!(
            region,
            LaunchTarget::Region {
                start_ticks: 480,
                length_ticks: 960,
                track_ids: vec!["t1".into()],
            }
        );
        let clip: LaunchTarget = serde_json::from_str(r#"{"kind":"clip","clipId":"c-1"}"#).unwrap();
        assert_eq!(
            clip,
            LaunchTarget::Clip {
                clip_id: "c-1".into()
            }
        );
        let wire = serde_json::to_value(&region).unwrap();
        assert_eq!(wire["startTicks"], 480);
        assert!(wire.get("start_ticks").is_none());
        let from_disk: LaunchTarget = serde_json::from_str(
            r#"{"kind":"region","start_ticks":1,"length_ticks":2,"track_ids":["t"]}"#,
        )
        .unwrap();
        assert_eq!(
            from_disk,
            LaunchTarget::Region {
                start_ticks: 1,
                length_ticks: 2,
                track_ids: vec!["t".into()],
            }
        );

        // Player crosses to TypeScript (Task 12): the wire form is pinned
        // explicitly, not just proven symmetric under Rust's own
        // serialize/deserialize — a round-trip through this crate alone
        // would pass even if the frontend expected a different key.
        let player: LaunchTarget =
            serde_json::from_str(r#"{"kind":"player","playerId":"p-1"}"#).unwrap();
        assert_eq!(
            player,
            LaunchTarget::Player {
                player_id: crate::ids::PlayerId::from("p-1")
            }
        );
        let player_wire = serde_json::to_value(&player).unwrap();
        assert_eq!(
            player_wire,
            serde_json::json!({"kind": "player", "playerId": "p-1"})
        );
    }

    #[test]
    fn resolve_matches_any_channel_when_unscoped() {
        let b = [region("a", 60, None)];
        assert_eq!(resolve(&b, 60, 0).unwrap().id, "a");
        assert_eq!(resolve(&b, 60, 15).unwrap().id, "a");
        assert!(resolve(&b, 61, 0).is_none());
    }

    #[test]
    fn resolve_honours_channel() {
        let b = [region("a", 60, Some(2))];
        assert!(resolve(&b, 60, 2).is_some());
        assert!(resolve(&b, 60, 3).is_none());
    }

    #[test]
    fn first_binding_wins() {
        let b = [region("first", 60, None), region("second", 60, None)];
        assert_eq!(resolve(&b, 60, 0).unwrap().id, "first");
    }

    #[test]
    fn clip_self_trigger_detects_matching_note() {
        assert!(clip_would_self_trigger(&[(60, 0), (64, 0)], 60, None));
        assert!(clip_would_self_trigger(&[(60, 2)], 60, Some(2)));
        assert!(!clip_would_self_trigger(&[(64, 0)], 60, None));
        assert!(!clip_would_self_trigger(&[(60, 1)], 60, Some(2)));
    }

    #[test]
    fn echo_window_filters_loopback() {
        let sent = [EchoNote {
            note: 60,
            channel: 0,
            at_ms: 1000,
        }];
        assert!(incoming_is_echo(
            &sent,
            EchoNote {
                note: 60,
                channel: 0,
                at_ms: 1040
            },
            80
        ));
        assert!(!incoming_is_echo(
            &sent,
            EchoNote {
                note: 60,
                channel: 0,
                at_ms: 1090
            },
            80
        ));
        assert!(!incoming_is_echo(
            &sent,
            EchoNote {
                note: 61,
                channel: 0,
                at_ms: 1010
            },
            80
        ));
    }

    #[test]
    fn runtime_fires_once_until_note_off() {
        let rt = LaunchRuntime::default();
        rt.set_bindings(vec![region("a", 60, None)]);
        assert!(matches!(rt.decide(true, 60, 0), IncomingDecision::Fire(_)));
        // simulate armed after fire
        rt.on_incoming(true, 60, 0);
        assert_eq!(rt.decide(true, 60, 0), IncomingDecision::Suppressed);
        rt.on_incoming(false, 60, 0);
        assert!(matches!(rt.decide(true, 60, 0), IncomingDecision::Fire(_)));
    }

    #[test]
    fn runtime_learn_consumes_the_next_note() {
        let rt = LaunchRuntime::default();
        rt.set_bindings(vec![region("a", 60, None)]);
        rt.set_learning(Some("a".into()));
        assert_eq!(
            rt.decide(true, 72, 3),
            IncomingDecision::Learn {
                note: 72,
                channel: 3
            }
        );
        assert_eq!(rt.take_learn(), Some((72, 3)));
    }

    #[test]
    fn runtime_echo_blocks_reentry() {
        let rt = LaunchRuntime::default();
        rt.set_bindings(vec![clip("c", 60, "clip-1")]);
        rt.record_out(60, 0);
        assert_eq!(rt.decide(true, 60, 0), IncomingDecision::Echo);
    }

    #[test]
    fn clip_retrigger_after_note_off() {
        let rt = LaunchRuntime::default();
        rt.set_bindings(vec![clip("c", 60, "clip-1")]);
        assert!(rt.on_incoming(true, 60, 0));
        assert_eq!(rt.decide(true, 60, 0), IncomingDecision::Suppressed);
        rt.on_incoming(false, 60, 0);
        assert!(
            matches!(rt.decide(true, 60, 0), IncomingDecision::Fire(_)),
            "the same pad must be able to fire again after note-off"
        );
    }

    #[test]
    fn migrate_legacy_bindings_become_the_default_launcher() {
        let maps = migrate_legacy_maps(None, vec![region("a", 60, None)], vec!["c1".into()]);
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].id, DEFAULT_MAP_ID);
        assert_eq!(maps[0].name, "Launcher 1");
        assert_eq!(maps[0].bindings[0].id, "a");
        assert_eq!(maps[0].drive_clip_ids, vec!["c1".to_string()]);
    }

    #[test]
    fn migrate_empty_legacy_still_gives_a_default_launcher() {
        assert_eq!(
            migrate_legacy_maps(None, vec![], vec![]),
            vec![LaunchMap::default_map()]
        );
    }

    #[test]
    fn orphaned_drive_clips_are_not_eligible() {
        let drive = vec!["c1".into(), "c2".into()];
        let live: std::collections::HashSet<String> = ["t-live".into()].into_iter().collect();
        assert!(
            !drive_clip_eligible("c1", "t-gone", &drive, &live, None),
            "clip whose track is gone must not fire"
        );
        assert!(drive_clip_eligible("c1", "t-live", &drive, &live, None));
        assert!(
            !drive_clip_eligible("c2", "t-live", &drive, &live, Some("c1")),
            "clip-editor focus plays only that clip"
        );
        assert!(drive_clip_eligible(
            "c1",
            "t-live",
            &drive,
            &live,
            Some("c1")
        ));
        assert!(!drive_clip_eligible("other", "t-live", &drive, &live, None));
    }

    #[test]
    fn launch_map_missing_play_mode_defaults_to_gate() {
        let m: LaunchMap = serde_json::from_str(r#"{"id":"x","name":"Drums"}"#).unwrap();
        assert_eq!(m.play_mode, LaunchPlayMode::Gate);
        let one: LaunchMap =
            serde_json::from_str(r#"{"id":"x","name":"Drums","playMode":"oneShot"}"#).unwrap();
        assert_eq!(one.play_mode, LaunchPlayMode::OneShot);
    }

    #[test]
    fn migrate_prefers_named_maps() {
        let named = vec![LaunchMap::named("drums", "Drums")];
        let maps = migrate_legacy_maps(Some(named.clone()), vec![region("a", 60, None)], vec![]);
        assert_eq!(maps, named);
    }

    #[test]
    fn fire_runs_on_a_worker_not_the_caller() {
        let rt = LaunchRuntime::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let caller = std::thread::current().id();
        rt.install_fire(std::sync::Arc::new(move |cmd| {
            if let FireCmd::Start(b, _) = cmd {
                let _ = tx.send((std::thread::current().id(), b.id.clone()));
            }
        }));
        rt.set_bindings(vec![region("a", 60, None)]);
        rt.on_incoming(true, 60, 0);
        let (tid, id) = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("worker should fire");
        assert_eq!(id, "a");
        assert_ne!(
            tid, caller,
            "launch_fire must not run on the midir/caller thread"
        );
    }

    #[test]
    fn play_edge_includes_sample_zero() {
        assert!(
            drive_in_window(0, 0, 0, true),
            "play from 0 must include the downbeat"
        );
        assert!(
            !drive_event_in_window(0, 0, 0),
            "half-open (last, pos] is empty at 0 — that is why play-edge is inclusive"
        );
    }

    #[test]
    fn resolve_drive_ignores_binding_channel() {
        let b = [region("kick", 60, Some(2))];
        assert!(
            resolve(&b, 60, 0).is_none(),
            "hardware resolve still honours channel"
        );
        assert_eq!(
            resolve_drive(&b, 60).unwrap().id,
            "kick",
            "clip-as-instrument matches the written key only"
        );
    }

    fn clip_note_at_tick_zero() -> crate::midi::types::MidiClip {
        use crate::ids::NoteId;
        use crate::midi::types::{MidiClip, MidiNote};
        MidiClip {
            id: "drive-1".into(),
            track_id: "t1".into(),
            name: "Drive".into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: vec![MidiNote {
                tick: 0,
                length_ticks: 120,
                key: 60,
                velocity: 100,
                channel: 2,
                note_id: NoteId(1),
            }],
            next_note_id: 2,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        }
    }

    fn drive_tick(
        rt: &LaunchRuntime,
        last: &mut u64,
        was_playing: &mut bool,
        last_onset: &mut std::collections::HashMap<String, u64>,
        playing: bool,
        pos: u64,
        events: &[crate::midi::schedule::AbsNoteEvent],
        bindings: &[LaunchBinding],
    ) {
        if !playing {
            *last = pos;
            *was_playing = false;
            last_onset.clear();
            return;
        }
        let play_edge = !*was_playing;
        *was_playing = true;
        for ev in events.iter().filter(|e| e.velocity > 0) {
            if !drive_in_window(ev.sample, *last, pos, play_edge) {
                continue;
            }
            let Some(b) = resolve_drive(bindings, ev.key) else {
                continue;
            };
            if last_onset.get(&b.id) == Some(&ev.sample) {
                continue;
            }
            last_onset.insert(b.id.clone(), ev.sample);
            rt.enqueue_fire(b.clone(), FireOrigin::Drive);
        }
        *last = pos;
    }

    #[test]
    fn play_from_zero_enqueues_again_after_stop() {
        let rt = LaunchRuntime::default();
        let (tx, rx) = std::sync::mpsc::channel();
        rt.install_fire(std::sync::Arc::new(move |cmd| {
            if let FireCmd::Start(b, FireOrigin::Drive) = cmd {
                let _ = tx.send(b.id);
            }
        }));
        let map = crate::midi::TempoMap::from_v1(120.0, 48_000).unwrap();
        let evs = crate::midi::schedule::clip_drive_events(&clip_note_at_tick_zero(), &map);
        assert_eq!(
            evs.iter().find(|e| e.velocity > 0).map(|e| e.sample),
            Some(0),
            "tick 0 is sample 0 at 120 bpm / 48 kHz"
        );
        // channel: Some(2) must still match a drive note (key only).
        let bindings = vec![region("kick", 60, Some(2))];
        let mut last = 0u64;
        let mut was_playing = false;
        let mut last_onset = std::collections::HashMap::new();

        drive_tick(
            &rt,
            &mut last,
            &mut was_playing,
            &mut last_onset,
            true,
            0,
            &evs,
            &bindings,
        );
        drive_tick(
            &rt,
            &mut last,
            &mut was_playing,
            &mut last_onset,
            false,
            0,
            &evs,
            &bindings,
        );
        drive_tick(
            &rt,
            &mut last,
            &mut was_playing,
            &mut last_onset,
            true,
            0,
            &evs,
            &bindings,
        );

        let first = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("play from 0 must enqueue a fire");
        let second = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("stop/play must enqueue the same downbeat again");
        assert_eq!(first, "kick");
        assert_eq!(second, "kick");
        assert!(
            rx.try_recv().is_err(),
            "exactly two fires — one per play edge"
        );
    }
    // -----------------------------------------------------------------
    // Plan V — V2, Task 8: a scene owns its clock. These drive a real
    // `ControlPlane` (no engine thread) with the `GraphTables` a rebuild
    // would have published for the same document — see `plane`.
    // -----------------------------------------------------------------

    fn region_on(id: &str, note: u8, tracks: &[&str]) -> LaunchBinding {
        LaunchBinding {
            id: id.into(),
            name: id.into(),
            note,
            channel: None,
            target: LaunchTarget::Region {
                start_ticks: 0,
                length_ticks: 960,
                track_ids: tracks.iter().map(|t| (*t).to_string()).collect(),
            },
        }
    }

    /// A `ControlPlane` over `tracks` and `bindings`, plus the `GraphTables`
    /// `engine::rebuild` would have published for that document: params and
    /// clocks sized to the mixer slots, the slot map, and one clock per
    /// Region binding numbered exactly as `rebuild` numbers them.
    ///
    /// Building the tables by hand is not a shortcut — there is no engine
    /// thread in a unit test, so nothing would ever publish one, and every
    /// clock write would silently drop (`fire_scene` warns and returns false
    /// on a binding with no clock). Same reasoning as
    /// `control::mod::tests::test_plane_with_tracks`, which cannot be reused
    /// here: it is private to that module's `#[cfg(test)] mod tests`.
    ///
    /// The `EngineHandle` receiver is returned, not dropped: `transport`
    /// commits send on that channel, and a disconnected one silently eats
    /// them. The emitted-events sink is returned for the same reason — a
    /// fire's whole visible output to the frontend is `launch://fired`.
    ///
    /// `LaunchRuntime` is a PROCESS-WIDE singleton, so its sounding ledger
    /// outlives any one test; the fixture clears it, which is what keeps
    /// these independent of each other's leftovers.
    /// What [`plane`] hands back: the plane itself, the engine channel's
    /// receiver (kept alive so `transport` commits are not sent into a
    /// disconnected channel) and the emitted-events sink.
    type Plane = (
        crate::control::ControlPlane,
        crossbeam_channel::Receiver<crate::audio::engine::ControlMsg>,
        Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    );

    fn plane(tracks: &[&str], bindings: Vec<LaunchBinding>) -> Plane {
        use crate::audio::engine::EngineHandle;
        use crate::audio::rt::{GraphTables, SharedRt};
        use crate::audio::types::{derive_slots, mixer_slot_count, Store};
        use crate::control::Session;

        let mut store = Store::default();
        for &id in tracks {
            store.tracks.push(crate::audio::types::testutil::test_track(id));
        }
        let mut session = Session::new(store, crate::midi::MidiStore::default());
        let mut map = LaunchMap::default_map();
        map.bindings = bindings;
        session.midi.launch_maps = vec![map];

        let n_slots = mixer_slot_count(&session.store.tracks);
        let slots = derive_slots(&session.store.tracks);
        let scene_ids: Vec<String> = crate::audio::engine::scene_binding_ids(&session.midi.launch_maps);
        let first = 1 + session.store.players.len() as u32;
        let scene_clocks: std::collections::HashMap<String, u32> = scene_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), first + i as u32))
            .collect();
        let clocks = crate::audio::clock::ClockTable::with_slots_and_clocks(
            n_slots,
            1 + session.store.players.len() + scene_ids.len(),
        );
        let shared = Arc::new(SharedRt::default());
        shared.sample_rate.store(48_000, Relaxed);
        let tables = Arc::new(Mutex::new(GraphTables {
            generation: 1,
            params: Arc::new(crate::audio::rt::ParamTable::with_slots_and_sends(n_slots, 0)),
            clocks: Arc::new(clocks),
            scene_clocks,
            player_clocks: Default::default(),
            orphan_clock: None,
            slots,
            send_slots: Default::default(),
        }));
        let (engine, engine_rx) = EngineHandle::for_tests();
        let events: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let cp = crate::control::ControlPlane::new(
            Arc::new(Mutex::new(session)),
            shared,
            tables,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(move |e, p| sink.lock().push((e.to_string(), p))),
            Arc::new(crate::control::HistoryLog::new()),
            Arc::new(crate::control::GestureState::new()),
        );
        runtime().clear_sounding();
        (cp, engine_rx, events)
    }

    /// Design §2.2's defect, killed: pressing a pad must not move the user's
    /// arrangement. This is the whole reason `FireOrigin::Hardware` existed
    /// as a separate arm — it did SetLoop + Seek + Play on the transport,
    /// because the transport's playhead was the only one there was.
    #[test]
    fn firing_from_hardware_does_not_move_the_transport() {
        let (cp, _rx, _ev) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.transport(crate::control::TransportAction::Seek { position_samples: 96_000 })
            .unwrap();
        cp.transport(crate::control::TransportAction::Play).unwrap();
        let before = cp.transport_state();

        cp.launch_fire_from("b1", FireOrigin::Hardware).unwrap();

        let after = cp.transport_state();
        assert_eq!(after.position_samples, before.position_samples);
        assert_eq!(after.loop_enabled, before.loop_enabled);
        assert!(!after.loop_enabled, "and no loop was invented over the region");
        assert_eq!(after.state, "playing");
        let tables = cp.tables_for_tests();
        let clock = tables.scene_clocks["b1"];
        assert!(tables.clocks.is_on(clock), "the scene sounds on its own clock");
    }

    /// A hardware press still tells the VIEW to follow — that distinction is
    /// all `FireOrigin` carries now, and it is why the type survives the
    /// collapse of the three fire arms into one.
    #[test]
    fn follow_view_still_separates_a_hardware_press_from_a_drive_fire() {
        let (cp, _rx, events) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.launch_fire_from("b1", FireOrigin::Hardware).unwrap();
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();

        let fired: Vec<bool> = events
            .lock()
            .iter()
            .filter(|(name, _)| name == "launch://fired")
            .map(|(_, p)| p["followView"].as_bool().unwrap_or(false))
            .collect();
        assert_eq!(fired, vec![true, false], "the hardware press moves the view; the drive fire does not");
    }

    /// Two scenes sounding at once is what the single overlay could never do
    /// — and it is the reason a scene needs its OWN clock rather than a
    /// shared one.
    #[test]
    fn two_scenes_sound_at_once_on_different_clocks() {
        let (cp, _rx, _ev) = plane(
            &["t1", "t2"],
            vec![region_on("b1", 60, &["t1"]), region_on("b2", 61, &["t2"])],
        );
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        cp.launch_fire_from("b2", FireOrigin::Drive).unwrap();

        let c1 = cp.scene_clock_for("b1").expect("b1 has a clock");
        let c2 = cp.scene_clock_for("b2").expect("b2 has a clock");
        assert_ne!(c1, c2);
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(c1));
        assert!(tables.clocks.is_on(c2), "firing b2 must not have stopped b1");
        assert_eq!(tables.clocks.clock_of(tables.slots[&crate::ids::TrackId::from("t1")]), c1);
        assert_eq!(tables.clocks.clock_of(tables.slots[&crate::ids::TrackId::from("t2")]), c2);
    }

    /// V-14. Two scenes naming the same track is newly expressible; ending
    /// the first must not take the track away from the second.
    #[test]
    fn stopping_one_scene_does_not_steal_a_track_the_other_now_owns() {
        let (cp, _rx, _ev) = plane(
            &["shared"],
            vec![
                region_on("b1", 60, &["shared"]),
                region_on("b2", 61, &["shared"]),
            ],
        );
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        cp.launch_fire_from("b2", FireOrigin::Drive).unwrap();
        let c2 = cp.scene_clock_for("b2").unwrap();

        cp.stop_drive_launch("b1");

        let tables = cp.tables_for_tests();
        let slot = tables.slots[&crate::ids::TrackId::from("shared")];
        assert_eq!(tables.clocks.clock_of(slot), c2, "b2 still owns it");
        assert!(tables.clocks.is_on(c2), "and is still sounding");
    }

    /// A binding the graph has never seen (added since the last rebuild) has
    /// no clock, and a fire naming it must drop rather than land on whichever
    /// index happens to be free — the same rule `ParamTable`'s setters follow
    /// for an unknown slot.
    #[test]
    fn firing_a_binding_with_no_clock_yet_drops_instead_of_firing_another() {
        let (cp, _rx, ev) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.session().lock().midi.launch_maps[0]
            .bindings
            .push(region_on("b-new", 62, &["t1"]));

        assert_eq!(cp.scene_clock_for("b-new"), None);
        cp.launch_fire_from("b-new", FireOrigin::Drive).unwrap();

        assert!(
            !runtime().take_sounding("b-new"),
            "a dropped fire must not enter the ledger — nothing would ever release it"
        );
        assert!(
            !ev.lock().iter().any(|(name, _)| name == "launch://fired"),
            "and the frontend is not told a scene is playing when none is"
        );
        let tables = cp.tables_for_tests();
        assert!(
            !tables.clocks.is_on(tables.scene_clocks["b1"]),
            "b1's clock is not b-new's to fire"
        );
    }

    /// Stop-all cuts every sounding scene, not just the last one fired —
    /// `launch_stop` is one frozen command over what is now N clocks.
    #[test]
    fn stop_all_cuts_every_sounding_scene() {
        let (cp, _rx, _ev) = plane(
            &["t1", "t2"],
            vec![region_on("b1", 60, &["t1"]), region_on("b2", 61, &["t2"])],
        );
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        cp.launch_fire_from("b2", FireOrigin::Drive).unwrap();

        assert!(cp.stop_launch_overlay(), "something was sounding");

        let c1 = cp.scene_clock_for("b1").unwrap();
        let c2 = cp.scene_clock_for("b2").unwrap();
        let tables = cp.tables_for_tests();
        assert!(!tables.clocks.is_on(c1));
        assert!(!tables.clocks.is_on(c2));
        assert!(
            tables.clocks.flush_pending(),
            "both cuts still owe their live nodes an all-notes-off"
        );
        assert_eq!(
            tables.clocks.clock_of(tables.slots[&crate::ids::TrackId::from("t1")]),
            c1,
            "and the slots stay bound so they can read it (the drive thread releases)"
        );
    }

    /// The frontend must be told a scene ended exactly once, however many
    /// times the drive poller enqueues the release.
    #[test]
    fn a_scene_ending_is_announced_once() {
        let (cp, _rx, _ev) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        assert!(runtime().take_sounding("b1"), "fired, and in the ledger");
        runtime().mark_sounding("b1");

        cp.stop_drive_launch("b1");
        cp.stop_drive_launch("b1");
        assert!(!runtime().take_sounding("b1"), "taken once, by the first call");
    }

    /// Fix round 1, finding 1. The Gate path cuts a clock that is STILL
    /// RUNNING (a note-off lifting mid-clip), so the flush `ClockTable::stop`
    /// latches has not been read by anyone yet. Releasing the slot in the
    /// same breath — which is what `stop_scene` did — means the live node
    /// never sees the `all_notes_off` and keeps the note.
    #[test]
    fn cutting_a_running_scene_keeps_its_tracks_bound_until_the_flush_is_read() {
        let (cp, _rx, _ev) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        let clock = cp.scene_clock_for("b1").unwrap();

        cp.stop_drive_launch("b1"); // the Gate note-off path

        {
            let t = cp.tables_for_tests();
            let slot = t.slots[&crate::ids::TrackId::from("t1")];
            assert!(!t.clocks.is_on(clock), "cut");
            assert!(t.clocks.flush_pending_for(clock), "and owing one jump");
            assert_eq!(
                t.clocks.clock_of(slot),
                clock,
                "the track stays bound, or nothing ever reads that jump"
            );
            // The release pass must refuse while the flush is unread.
            t.release_finished_scenes();
            assert_eq!(t.clocks.clock_of(slot), clock, "still owed");
            // A rendered block latches it, and the node has had its
            // all-notes-off.
            t.clocks.begin_block();
            t.release_finished_scenes();
            assert_eq!(
                t.clocks.clock_of(slot),
                crate::audio::clock::TRANSPORT_CLOCK,
                "now the track goes back to the arrangement"
            );
        }
    }

    /// V-14 at the release pass, which is where the release lives now: two
    /// scenes may name the same track, and handing back a track the other one
    /// is still playing would silence it mid-clip.
    #[test]
    fn the_release_pass_leaves_a_track_a_second_scene_is_still_playing() {
        let (cp, _rx, _ev) = plane(
            &["shared"],
            vec![
                region_on("b1", 60, &["shared"]),
                region_on("b2", 61, &["shared"]),
            ],
        );
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        cp.launch_fire_from("b2", FireOrigin::Drive).unwrap();
        let c2 = cp.scene_clock_for("b2").unwrap();
        cp.stop_drive_launch("b1");

        let t = cp.tables_for_tests();
        t.clocks.begin_block();
        t.release_finished_scenes();
        let slot = t.slots[&crate::ids::TrackId::from("shared")];
        assert_eq!(t.clocks.clock_of(slot), c2, "b2 is still sounding on it");
    }

    /// Fix round 1, finding 1 again, at the transport-stop path: pressing
    /// Stop with a scene sounding used to release the slots in the same
    /// breath as the cut, so a held note stayed held in the live node.
    #[test]
    fn stopping_the_transport_cuts_the_scenes_without_dropping_their_flush() {
        let (cp, _rx, _ev) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.transport(crate::control::TransportAction::Play).unwrap();
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        let clock = cp.scene_clock_for("b1").unwrap();

        cp.transport(crate::control::TransportAction::Stop).unwrap();

        let t = cp.tables_for_tests();
        let slot = t.slots[&crate::ids::TrackId::from("t1")];
        assert!(!t.clocks.is_on(clock), "the scene ends with the song");
        assert!(t.clocks.flush_pending_for(clock), "owing its node one jump");
        assert_eq!(t.clocks.clock_of(slot), clock, "still bound to read it");
    }

    /// Fix round 1, finding 4. The ledger test is level-triggered — an id
    /// sits there with its clock off until the worker drains it — so the
    /// drive loop has to remember what it already sent. `enqueue_release` is
    /// a `try_send` into an 8-slot channel SHARED with `FireCmd::Start`:
    /// duplicates of one ending fill it and drop the user's next pad press.
    ///
    /// Drives `releases_to_enqueue`, which is what the drive loop calls.
    /// Round 2 finding 2: the first version of this test re-implemented those
    /// three lines inline and the copy carried the same defect production
    /// had, so it passed while the loop was broken.
    #[test]
    fn the_release_edge_enqueues_once_per_ending_not_once_per_poll() {
        let mut sent = std::collections::HashSet::new();
        let sounding = vec!["b1".to_string()];

        let first = releases_to_enqueue(&mut sent, &sounding, |_| false);
        assert_eq!(first, vec!["b1".to_string()], "the ending, announced");
        for _ in 0..4 {
            assert!(
                releases_to_enqueue(&mut sent, &sounding, |_| false).is_empty(),
                "one Release per ending — the channel is shared with pad presses"
            );
        }
    }

    /// Round 2, finding 1. `take_sounding` (worker) and `mark_sounding`
    /// (re-fire) both land between two polls, so the memory cannot be cleared
    /// by watching the ledger: the id is still in it. Clearing on the ON EDGE
    /// is what makes a re-fired pad announce its NEXT ending — without it the
    /// binding is silently mute to the frontend forever after, the pad stays
    /// lit, and the id never leaves the ledger.
    ///
    /// Retriggering a pad as its clip ends is ordinary launcher use.
    #[test]
    fn a_binding_refired_inside_one_poll_gap_still_announces_its_next_ending() {
        let mut sent = std::collections::HashSet::new();
        let sounding = vec!["b1".to_string()];

        // Poll N: b1 has ended. One Release goes out.
        assert_eq!(
            releases_to_enqueue(&mut sent, &sounding, |_| false),
            vec!["b1".to_string()]
        );
        // Between the polls: the worker announces it and empties the ledger,
        // then the user re-fires the same pad and it goes back in. The ledger
        // looks identical from here — which is the whole trap.
        // Poll N+1: sounding again.
        assert!(releases_to_enqueue(&mut sent, &sounding, |_| true).is_empty());
        // Poll N+2: the SECOND ending must be announced too.
        assert_eq!(
            releases_to_enqueue(&mut sent, &sounding, |_| false),
            vec!["b1".to_string()],
            "an ending after a re-fire is still an ending"
        );
    }

    /// The other half: an id the worker took out of the ledger and that never
    /// came back must not stay in the memory. Correctness comes from the ON
    /// edge above; this is what bounds the set to what is sounding rather than
    /// to every binding ever fired.
    #[test]
    fn the_release_memory_does_not_grow_with_every_binding_ever_fired() {
        let mut sent = std::collections::HashSet::new();
        releases_to_enqueue(&mut sent, &["b1".to_string()], |_| false);
        assert_eq!(sent.len(), 1);
        releases_to_enqueue(&mut sent, &[], |_| false);
        assert!(sent.is_empty(), "gone with the ledger entry");
    }

    /// The loop and the runtime meet here: what the drive thread actually
    /// feeds `releases_to_enqueue` is the ledger and a clock-table lookup.
    #[test]
    fn the_drive_loops_release_edge_reads_the_ledger_and_the_clock_table() {
        let (cp, _rx, _ev) = plane(&["t1"], vec![region_on("b1", 60, &["t1"])]);
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        let mut sent = std::collections::HashSet::new();
        let still_on = |id: &str| {
            let t = cp.tables_for_tests();
            t.scene_clocks.get(id).is_some_and(|&c| t.clocks.is_on(c))
        };

        let sounding = runtime().sounding_ids();
        assert!(
            releases_to_enqueue(&mut sent, &sounding, still_on).is_empty(),
            "still sounding"
        );
        cp.stop_launch_overlay(); // Escape
        assert_eq!(
            releases_to_enqueue(&mut sent, &sounding, still_on),
            vec!["b1".to_string()]
        );
    }

    // ---- Plan V — V2, Task 12: migrating clip bindings to players ----

    fn test_midi_clip(id: &str, track_id: &str) -> crate::midi::types::MidiClip {
        crate::midi::types::MidiClip {
            id: id.into(),
            track_id: track_id.into(),
            name: id.into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track_id),
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        }
    }

    fn one_binding_map(binding: LaunchBinding) -> LaunchMap {
        LaunchMap {
            bindings: vec![binding],
            ..LaunchMap::default_map()
        }
    }

    #[test]
    fn migrate_turns_a_clip_target_into_a_player_naming_the_track_instrument() {
        let mut track = crate::audio::types::testutil::test_track("t1");
        track.instrument_id = Some("plugin:i1".into());
        let tracks = vec![track];
        let clips = vec![test_midi_clip("mc1", "t1")];
        let mut players = Vec::new();
        let mut maps = vec![one_binding_map(clip("b1", 36, "mc1"))];

        let n = migrate_clip_targets_to_players(&mut maps, &clips, &tracks, &mut players);

        assert_eq!(n, 1);
        assert_eq!(players.len(), 1, "the binding became one player");
        assert_eq!(
            players[0].source,
            crate::audio::player::PlayerSource::MidiClip {
                clip_id: "mc1".into(),
                instrument_id: Some("plugin:i1".into()),
            },
            "the player plays what the binding played, through the clip's own instrument"
        );
        assert_eq!(
            maps[0].bindings[0].target,
            LaunchTarget::Player { player_id: players[0].id.clone() }
        );
        assert_eq!(maps[0].bindings[0].note, 36, "the note it was learned on is untouched");
    }

    #[test]
    fn migrate_shares_one_player_across_two_bindings_on_the_same_clip() {
        let clips = vec![test_midi_clip("mc1", "t1")];
        let mut players = Vec::new();
        let mut maps = vec![LaunchMap {
            bindings: vec![clip("b1", 36, "mc1"), clip("b2", 37, "mc1")],
            ..LaunchMap::default_map()
        }];

        let n = migrate_clip_targets_to_players(&mut maps, &clips, &[], &mut players);

        assert_eq!(n, 2);
        assert_eq!(players.len(), 1, "one clip, one player");
        assert_eq!(maps[0].bindings[0].target, maps[0].bindings[1].target);
    }

    /// Fix round 1, Important 4: the reviewer replaced the assignment with
    /// `players[0].id.clone()` — always the first player — and every
    /// existing test still passed, because no fixture in this file had two
    /// DISTINCT clips. `players[0]` cannot be wrong when there is only ever
    /// one player to index. This is the test that can only pass if each
    /// binding resolves to ITS OWN clip's player.
    #[test]
    fn migrate_assigns_each_binding_the_player_for_its_own_clip() {
        let mut t1 = crate::audio::types::testutil::test_track("t1");
        t1.instrument_id = Some("plugin:i1".into());
        let mut t2 = crate::audio::types::testutil::test_track("t2");
        t2.instrument_id = Some("plugin:i2".into());
        let tracks = vec![t1, t2];
        let clips = vec![test_midi_clip("mc1", "t1"), test_midi_clip("mc2", "t2")];
        let mut players = Vec::new();
        let mut maps = vec![LaunchMap {
            bindings: vec![clip("b1", 36, "mc1"), clip("b2", 37, "mc2")],
            ..LaunchMap::default_map()
        }];

        let n = migrate_clip_targets_to_players(&mut maps, &clips, &tracks, &mut players);

        assert_eq!(n, 2);
        assert_eq!(players.len(), 2, "two distinct clips, two distinct players");

        let player_for = |binding_idx: usize| {
            let LaunchTarget::Player { player_id } = &maps[0].bindings[binding_idx].target else {
                panic!("expected a Player target");
            };
            players
                .iter()
                .find(|p| &p.id == player_id)
                .expect("the referenced player exists")
        };

        assert_eq!(
            player_for(0).source,
            crate::audio::player::PlayerSource::MidiClip {
                clip_id: "mc1".into(),
                instrument_id: Some("plugin:i1".into()),
            },
            "b1 must resolve to mc1's player, not merely SOME player"
        );
        assert_eq!(
            player_for(1).source,
            crate::audio::player::PlayerSource::MidiClip {
                clip_id: "mc2".into(),
                instrument_id: Some("plugin:i2".into()),
            },
            "b2 must resolve to mc2's player, not merely SOME player"
        );
        assert_ne!(
            player_for(0).id,
            player_for(1).id,
            "distinct clips must not share a player"
        );
    }

    /// The exact shape task 12's brief names as the risky one: run the
    /// migration twice over the SAME maps/players (an unsaved reopen looks
    /// like this at the function level) and check identity, not just a
    /// flag — a bug that re-mints on the second pass would still leave
    /// `players.len()` looking plausible if the assertion stopped there.
    #[test]
    fn migrate_is_idempotent_across_two_runs_over_the_same_maps_and_players() {
        let clips = vec![test_midi_clip("mc1", "t1")];
        let mut players = Vec::new();
        let mut maps = vec![one_binding_map(clip("b1", 36, "mc1"))];

        let first = migrate_clip_targets_to_players(&mut maps, &clips, &[], &mut players);
        assert_eq!(first, 1);
        let player_id_after_first = match &maps[0].bindings[0].target {
            LaunchTarget::Player { player_id } => player_id.clone(),
            other => panic!("expected a Player target, got {other:?}"),
        };

        let second = migrate_clip_targets_to_players(&mut maps, &clips, &[], &mut players);

        assert_eq!(second, 0, "nothing left to migrate — the binding already names a player");
        assert_eq!(players.len(), 1, "a second run must not mint a second player");
        assert_eq!(
            maps[0].bindings[0].target,
            LaunchTarget::Player { player_id: player_id_after_first },
            "and it is still the SAME player, not a fresh one"
        );
    }

    /// The close-without-saving case, which the two-runs test above cannot
    /// see: the migration is IN-MEMORY ONLY, so an unsaved project arrives
    /// at the next open with its `Clip` targets intact and no players — a
    /// fresh run over fresh maps, not a second run over migrated ones.
    ///
    /// A control-surface pad bound to a migrated player stores
    /// `player:<id>` in localStorage, which outlives the session. If the id
    /// were random the pad would point at nothing after that reopen, and
    /// would be silently dead.
    #[test]
    fn migrating_the_same_unsaved_project_twice_mints_the_same_player_id() {
        let clips = vec![test_midi_clip("mc1", "t1")];

        let run = || {
            let mut players = Vec::new();
            let mut maps = vec![one_binding_map(clip("b1", 36, "mc1"))];
            migrate_clip_targets_to_players(&mut maps, &clips, &[], &mut players);
            match &maps[0].bindings[0].target {
                LaunchTarget::Player { player_id } => player_id.clone(),
                other => panic!("expected a Player target, got {other:?}"),
            }
        };

        assert_eq!(
            run(),
            run(),
            "an unsaved project reopened must land on the SAME player id, or every \
             surface pad bound to it dies"
        );
    }

    #[test]
    fn migrate_reuses_an_existing_player_already_on_the_same_clip() {
        // A project reopened after an earlier migration was saved: the
        // player already exists, but a second binding on the same clip
        // (added since) is still a bare `Clip` target. A DECOY player for
        // a different clip sits first in the vec — fix round 1, Important
        // 4 — so a `players[0]` shortcut resolves to the wrong player
        // instead of trivially passing.
        let clips = vec![test_midi_clip("mc1", "t1"), test_midi_clip("mc-decoy", "t1")];
        let mut decoy =
            crate::audio::player::Player::new(crate::ids::PlayerId::from("decoy-p"), "DECOY");
        decoy.source = crate::audio::player::PlayerSource::MidiClip {
            clip_id: "mc-decoy".into(),
            instrument_id: None,
        };
        let mut existing =
            crate::audio::player::Player::new(crate::ids::PlayerId::from("existing-p"), "PAD");
        existing.source = crate::audio::player::PlayerSource::MidiClip {
            clip_id: "mc1".into(),
            instrument_id: None,
        };
        let mut players = vec![decoy, existing];
        let mut maps = vec![one_binding_map(clip("b1", 36, "mc1"))];

        let n = migrate_clip_targets_to_players(&mut maps, &clips, &[], &mut players);

        assert_eq!(n, 1);
        assert_eq!(players.len(), 2, "reused, not duplicated — the decoy is untouched");
        assert_eq!(
            maps[0].bindings[0].target,
            LaunchTarget::Player { player_id: crate::ids::PlayerId::from("existing-p") },
            "b1 must resolve to the EXISTING mc1 player, not players[0] (the decoy)"
        );
    }

    #[test]
    fn migrate_leaves_a_dangling_clip_binding_unbound_rather_than_dropping_it() {
        let mut players = Vec::new();
        let mut maps = vec![one_binding_map(clip("b1", 36, "gone"))];

        let n = migrate_clip_targets_to_players(&mut maps, &[], &[], &mut players);

        assert_eq!(n, 0);
        assert!(players.is_empty());
        assert_eq!(maps[0].bindings.len(), 1, "the binding stays — the pad mapping is not lost");
        assert_eq!(
            maps[0].bindings[0].target,
            LaunchTarget::Clip { clip_id: "gone".into() }
        );
    }

    #[test]
    fn migrate_leaves_a_region_binding_untouched() {
        let mut players = Vec::new();
        let mut maps = vec![one_binding_map(region("b1", 60, None))];

        let n = migrate_clip_targets_to_players(&mut maps, &[], &[], &mut players);

        assert_eq!(n, 0);
        assert!(players.is_empty());
        assert!(matches!(maps[0].bindings[0].target, LaunchTarget::Region { .. }));
    }

    /// Fix round 1, Important 3: the guard that used to be an inline
    /// `if let LaunchTarget::Clip` in the drive loop, non-exhaustive by
    /// construction. A `Player` target whose source names the driving
    /// clip must self-trigger exactly like a `Clip` target naming it
    /// directly did before migration.
    #[test]
    fn binding_self_triggers_resolves_a_player_target_through_its_source_clip() {
        let mut sounding =
            crate::audio::player::Player::new(crate::ids::PlayerId::from("p1"), "PAD");
        sounding.source = crate::audio::player::PlayerSource::MidiClip {
            clip_id: "mc1".into(),
            instrument_id: None,
        };
        let mut other =
            crate::audio::player::Player::new(crate::ids::PlayerId::from("p2"), "OTHER");
        other.source = crate::audio::player::PlayerSource::MidiClip {
            clip_id: "mc2".into(),
            instrument_id: None,
        };
        let players = vec![sounding, other];

        assert!(
            binding_self_triggers(
                &LaunchTarget::Player { player_id: crate::ids::PlayerId::from("p1") },
                &players,
                "mc1",
            ),
            "the player's own source clip is the one driving it"
        );
        assert!(
            !binding_self_triggers(
                &LaunchTarget::Player { player_id: crate::ids::PlayerId::from("p2") },
                &players,
                "mc1",
            ),
            "a DIFFERENT clip driving must not suppress a player it does not play"
        );
        assert!(
            !binding_self_triggers(
                &LaunchTarget::Region {
                    start_ticks: 0,
                    length_ticks: 960,
                    track_ids: vec!["t1".into()],
                },
                &players,
                "mc1",
            ),
            "a scene never self-triggers — it names tracks, not the driving clip"
        );
        assert!(
            binding_self_triggers(&LaunchTarget::Clip { clip_id: "mc1".into() }, &[], "mc1"),
            "the pre-migration shape still works too"
        );
    }

    /// Point 4 of the migration's own brief: a `Player` target must reach
    /// `player_fire`, not fall through to the scene path (which would
    /// panic on the `unreachable!()` guarding the old Region/Clip match, or
    /// silently do nothing at all). The clock actually turning on is the
    /// only proof that is not just "did it return Ok".
    #[test]
    fn launch_fire_from_a_player_target_routes_to_player_fire() {
        use crate::audio::engine::EngineHandle;
        use crate::audio::player::{Player, PlayerSource};
        use crate::audio::rt::{GraphTables, SharedRt};
        use crate::audio::types::{derive_slots, mixer_slot_count, Store};
        use crate::control::Session;

        let mut store = Store::default();
        store.tracks.push(crate::audio::types::testutil::test_track("t1"));
        let player_id = crate::ids::PlayerId::from("p1");
        let mut player = Player::new(player_id.clone(), "PAD");
        player.source = PlayerSource::MidiClip { clip_id: "mc1".into(), instrument_id: None };
        store.players.push(player);

        let mut session = Session::new(store, crate::midi::MidiStore::default());
        session.midi.clips.push(crate::midi::types::MidiClip {
            id: "mc1".into(),
            track_id: "t1".into(),
            name: "pad".into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        });
        session.midi.launch_maps = vec![one_binding_map(LaunchBinding {
            id: "b1".into(),
            name: "b1".into(),
            note: 36,
            channel: None,
            target: LaunchTarget::Player { player_id: player_id.clone() },
        })];

        let n_slots = mixer_slot_count(&session.store.tracks);
        let slots = derive_slots(&session.store.tracks);
        let clocks = crate::audio::clock::ClockTable::with_slots_and_clocks(n_slots, 2);
        let shared = Arc::new(SharedRt::default());
        shared.sample_rate.store(48_000, Relaxed);
        let mut player_clocks = std::collections::HashMap::new();
        player_clocks.insert(player_id, 1u32);
        let tables = Arc::new(Mutex::new(GraphTables {
            generation: 1,
            params: Arc::new(crate::audio::rt::ParamTable::with_slots_and_sends(n_slots, 0)),
            clocks: Arc::new(clocks),
            scene_clocks: Default::default(),
            player_clocks,
            orphan_clock: None,
            slots,
            send_slots: Default::default(),
        }));
        let (engine, _engine_rx) = EngineHandle::for_tests();
        let cp = crate::control::ControlPlane::new(
            Arc::new(Mutex::new(session)),
            shared,
            tables,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(|_e, _p| {}),
            Arc::new(crate::control::HistoryLog::new()),
            Arc::new(crate::control::GestureState::new()),
        );

        cp.launch_fire_from("b1", FireOrigin::Hardware).unwrap();

        let t = cp.tables_for_tests();
        assert!(
            t.clocks.is_on(1),
            "the player's own clock is running — launch_fire_from routed the Player target to player_fire"
        );
    }
}
