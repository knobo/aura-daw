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
    Clip {
        #[serde(alias = "clip_id")]
        clip_id: String,
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
    audible_tracks: Mutex<Vec<String>>,
    overlay_id: Mutex<Option<String>>,
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
            audible_tracks: Mutex::new(Vec::new()),
            overlay_id: Mutex::new(None),
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

    pub fn set_audible_tracks(&self, ids: Vec<String>) {
        *self.audible_tracks.lock() = ids;
    }

    pub fn audible_tracks(&self) -> Vec<String> {
        self.audible_tracks.lock().clone()
    }

    pub fn clear_audible_tracks(&self) {
        self.audible_tracks.lock().clear();
        *self.overlay_id.lock() = None;
    }

    pub fn set_overlay_id(&self, id: Option<String>) {
        *self.overlay_id.lock() = id;
    }

    pub fn overlay_id(&self) -> Option<String> {
        self.overlay_id.lock().clone()
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
    pub fn attach_drive(
        &self,
        shared: Arc<crate::audio::rt::SharedRt>,
        session: Arc<parking_lot::Mutex<crate::control::Session>>,
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
                let mut overlay_was_on = false;
                loop {
                    std::thread::sleep(Duration::from_millis(8));
                    let overlay_on = shared.launch_on.load(Relaxed);
                    if overlay_was_on && !overlay_on {
                        if let Some(id) = runtime().overlay_id() {
                            runtime().enqueue_release(id);
                        }
                    }
                    overlay_was_on = overlay_on;
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
                    let (clips, ppq, tempo, live_tracks) = {
                        let s = session.lock();
                        let live_tracks: std::collections::HashSet<String> = s
                            .store
                            .tracks
                            .iter()
                            .map(|t| t.id.to_string())
                            .collect();
                        (s.midi.clips.clone(), s.midi.ppq, s.midi.tempo_events.clone(), live_tracks)
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
                                if let LaunchTarget::Clip { clip_id } = &b.target {
                                    if clip_id == clip.id.as_str() {
                                        continue;
                                    }
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
        runtime().set_audible_tracks(track_ids.clone());
        log::info!(
            "launch: fire id={id} name={name} origin={origin:?} start={start} end={end} tracks={track_ids:?}"
        );
        match origin {
            FireOrigin::Drive => {
                // Keep the arrangement loop and playhead on the clip.
                // Retrigger rewinds the single overlay (v0.1 has no overlap).
                runtime().set_overlay_id(Some(id.to_string()));
                self.arm_drive_launch(&track_ids, start, end);
            }
            FireOrigin::Preview => {
                runtime().set_overlay_id(Some(id.to_string()));
                self.arm_drive_launch(&track_ids, start, end);
            }
            FireOrigin::Hardware => {
                self.clear_drive_overlay();
                self.apply_launch_audible(&track_ids);
                self.transport(crate::control::TransportAction::SetLoop {
                    enabled: true,
                    start_samples: start,
                    end_samples: end,
                })?;
                self.transport(crate::control::TransportAction::Seek {
                    position_samples: start,
                })?;
                self.transport(crate::control::TransportAction::Play)?;
            }
        }
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

    pub fn stop_drive_launch(&self, id: &str) {
        if runtime().overlay_id().as_deref() != Some(id) {
            return;
        }
        launch_trace(format!("drive stop {id}"));
        self.clear_launch_audible();
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
}
