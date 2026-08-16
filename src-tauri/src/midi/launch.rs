//! MIDI launch map: note → region or clip, plus the echo / self-trigger
//! guards so a clip AURA itself is playing cannot start itself via
//! MIDI-out loopback.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const ECHO_WINDOW_MS: u64 = 80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoNote {
    pub note: u8,
    pub channel: u8,
    pub at_ms: u64,
}

pub fn resolve<'a>(bindings: &'a [LaunchBinding], note: u8, channel: u8) -> Option<&'a LaunchBinding> {
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

/// A clip that contains its own trigger note would retrigger if those notes
/// were fed back into the mapper. The mapper never listens to clip playback
/// internally — this is the *document* half of the guard, used to suppress
/// the trigger key while that clip is the last thing we launched.
pub fn clip_would_self_trigger(
    notes: &[(u8, u8)],
    trigger_note: u8,
    trigger_channel: Option<u8>,
) -> bool {
    notes.iter().any(|(key, ch)| {
        *key == trigger_note && trigger_channel.map(|c| c == *ch).unwrap_or(true)
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingDecision {
    Pass,
    Learn { note: u8, channel: u8 },
    Echo,
    Suppressed,
    Fire(LaunchBinding),
}

/// Process-wide launch runtime: binding snapshot, echo ring, learn arm,
/// and a held-pad debounce. MIDI-out loopback is filtered by the echo
/// window — a clip cannot start itself that way. The fire callback runs
/// on a worker thread so the midir callback never waits on transport.
pub struct LaunchRuntime {
    bindings: Mutex<Vec<LaunchBinding>>,
    echo: Mutex<Vec<EchoNote>>,
    learning: Mutex<Option<String>>,
    armed: Mutex<Option<(u8, u8)>>,
    origin: Instant,
    fire_tx: Mutex<Option<std::sync::mpsc::SyncSender<LaunchBinding>>>,
    last_learn: Mutex<Option<(u8, u8)>>,
    gen: AtomicU64,
    drive_clips: Mutex<Vec<String>>,
    drive_started: AtomicBool,
}

impl Default for LaunchRuntime {
    fn default() -> Self {
        Self {
            bindings: Mutex::new(Vec::new()),
            echo: Mutex::new(Vec::new()),
            learning: Mutex::new(None),
            armed: Mutex::new(None),
            origin: Instant::now(),
            fire_tx: Mutex::new(None),
            last_learn: Mutex::new(None),
            gen: AtomicU64::new(0),
            drive_clips: Mutex::new(Vec::new()),
            drive_started: AtomicBool::new(false),
        }
    }
}

impl LaunchRuntime {
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn set_bindings(&self, bindings: Vec<LaunchBinding>) {
        *self.bindings.lock().unwrap() = bindings;
        self.gen.fetch_add(1, Relaxed);
    }

    pub fn bindings(&self) -> Vec<LaunchBinding> {
        self.bindings.lock().unwrap().clone()
    }

    pub fn set_learning(&self, id: Option<String>) {
        *self.learning.lock().unwrap() = id;
        *self.last_learn.lock().unwrap() = None;
    }

    pub fn take_learn(&self) -> Option<(u8, u8)> {
        self.last_learn.lock().unwrap().take()
    }

    pub fn install_fire(&self, f: Arc<dyn Fn(LaunchBinding) + Send + Sync>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let _ = std::thread::Builder::new()
            .name("aura-launch-fire".into())
            .spawn(move || {
                while let Ok(b) = rx.recv() {
                    f(b);
                }
            });
        *self.fire_tx.lock().unwrap() = Some(tx);
    }

    pub fn record_out(&self, note: u8, channel: u8) {
        let at = self.now_ms();
        let mut echo = self.echo.lock().unwrap();
        echo.push(EchoNote { note, channel, at_ms: at });
        let cutoff = at.saturating_sub(ECHO_WINDOW_MS * 4);
        echo.retain(|e| e.at_ms >= cutoff);
    }

    pub fn clear_armed(&self) {
        *self.armed.lock().unwrap() = None;
    }

    pub fn set_drive_clips(&self, ids: Vec<String>) {
        *self.drive_clips.lock().unwrap() = ids;
    }

    /// Watch the transport and fire launch bindings from clips marked as
    /// launch-map instruments. Note-on then immediate note-off so a
    /// sequencer pulse does not latch `armed`.
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
                loop {
                    std::thread::sleep(Duration::from_millis(8));
                    let playing = shared.playing.load(Relaxed);
                    let pos = shared.position.load(Relaxed);
                    if !playing || pos < last {
                        last = pos;
                        continue;
                    }
                    let drive = runtime().drive_clips.lock().unwrap().clone();
                    if drive.is_empty() {
                        last = pos;
                        continue;
                    }
                    let (clips, ppq, tempo) = {
                        let s = session.lock();
                        let clips: Vec<_> = s
                            .midi
                            .clips
                            .iter()
                            .filter(|c| drive.iter().any(|id| id == c.id.as_str()))
                            .cloned()
                            .collect();
                        (clips, s.midi.ppq, s.midi.tempo_events.clone())
                    };
                    let Ok(map) = crate::midi::TempoMap::new(ppq, tempo, shared.sample_rate.load(Relaxed).max(1)) else {
                        last = pos;
                        continue;
                    };
                    let bindings = runtime().bindings();
                    for clip in &clips {
                        for ev in crate::midi::schedule::clip_events(clip, &map) {
                            if ev.velocity == 0 || ev.sample <= last || ev.sample > pos {
                                continue;
                            }
                            if let Some(b) = resolve(&bindings, ev.key, 0) {
                                if let LaunchTarget::Clip { clip_id } = &b.target {
                                    if clip_id == clip.id.as_str() {
                                        continue;
                                    }
                                }
                            }
                            runtime().on_incoming(true, ev.key, 0);
                            runtime().on_incoming(false, ev.key, 0);
                        }
                    }
                    last = pos;
                }
            });
    }

    pub fn decide(&self, on: bool, note: u8, channel: u8) -> IncomingDecision {
        if !on {
            let mut armed = self.armed.lock().unwrap();
            if *armed == Some((note, channel)) {
                *armed = None;
            }
            return IncomingDecision::Pass;
        }
        if self.learning.lock().unwrap().is_some() {
            *self.last_learn.lock().unwrap() = Some((note, channel));
            return IncomingDecision::Learn { note, channel };
        }
        let now = self.now_ms();
        let echo = self.echo.lock().unwrap();
        if incoming_is_echo(
            &echo,
            EchoNote { note, channel, at_ms: now },
            ECHO_WINDOW_MS,
        ) {
            return IncomingDecision::Echo;
        }
        drop(echo);
        if *self.armed.lock().unwrap() == Some((note, channel)) {
            return IncomingDecision::Suppressed;
        }
        let bindings = self.bindings.lock().unwrap();
        match resolve(&bindings, note, channel).cloned() {
            Some(b) => IncomingDecision::Fire(b),
            None => IncomingDecision::Pass,
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
                *self.armed.lock().unwrap() = Some((note, channel));
                if let Some(tx) = self.fire_tx.lock().unwrap().as_ref() {
                    let _ = tx.try_send(b);
                }
                true
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSnapshot {
    pub bindings: Vec<LaunchBinding>,
    pub drive_clip_ids: Vec<String>,
}

impl crate::control::ControlPlane {
    pub fn launch_snapshot(&self) -> LaunchSnapshot {
        let s = self.session().lock();
        LaunchSnapshot {
            bindings: s.midi.launch_bindings.clone(),
            drive_clip_ids: s.midi.launch_drive_clip_ids.clone(),
        }
    }

    pub fn launch_bindings(&self) -> Vec<LaunchBinding> {
        self.session().lock().midi.launch_bindings.clone()
    }

    pub fn set_launch_binding(
        &self,
        id: String,
        binding: Option<LaunchBinding>,
        meta: crate::control::op::TxMeta,
    ) -> Result<LaunchSnapshot, String> {
        self.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::LaunchBindingSet { id, binding })?;
            Ok(())
        })?;
        self.emit_launch_changed();
        Ok(self.launch_snapshot())
    }

    pub fn set_launch_drive(
        &self,
        clip_id: String,
        on: bool,
        meta: crate::control::op::TxMeta,
    ) -> Result<LaunchSnapshot, String> {
        self.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::LaunchDriveSet { clip_id, on })?;
            Ok(())
        })?;
        self.emit_launch_changed();
        Ok(self.launch_snapshot())
    }

    pub fn launch_fire(&self, id: &str) -> Result<(), String> {
        let rate = self.transport_state().sample_rate;
        let (start_ticks, length_ticks, ppq, events) = {
            let s = self.session().lock();
            let b = s
                .midi
                .launch_bindings
                .iter()
                .find(|b| b.id == id)
                .ok_or_else(|| format!("unknown launch binding: {id}"))?;
            let (start_ticks, length_ticks) = match &b.target {
                LaunchTarget::Region { start_ticks, length_ticks, .. } => {
                    (start_ticks.clone(), length_ticks.clone())
                }
                LaunchTarget::Clip { clip_id } => {
                    let c = s
                        .midi
                        .clips
                        .iter()
                        .find(|c| c.id.as_str() == clip_id)
                        .ok_or_else(|| format!("launch clip is gone: {clip_id}"))?;
                    (c.timeline_start_ticks, c.length_ticks)
                }
            };
            (
                start_ticks,
                length_ticks,
                s.midi.ppq,
                s.midi.tempo_events.clone(),
            )
        };
        let map = crate::midi::TempoMap::new(ppq, events, rate.max(1))?;
        let start = map.tick_to_samples(start_ticks);
        let end = map.tick_to_samples(start_ticks.saturating_add(length_ticks)).max(start + 1);
        self.transport(crate::control::TransportAction::SetLoop {
            enabled: true,
            start_samples: start,
            end_samples: end,
        })?;
        self.transport(crate::control::TransportAction::Seek { position_samples: start })?;
        self.transport(crate::control::TransportAction::Play)?;
        Ok(())
    }
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
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<LaunchSnapshot, String> {
    match (binding, id) {
        (None, Some(id)) => control.set_launch_binding(
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
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<LaunchSnapshot, String> {
    control.set_launch_drive(clip_id, on, crate::control::op::TxMeta::user("set launch drive"))
}

#[tauri::command]
pub fn launch_fire(
    id: String,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.launch_fire(&id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnNote {
    pub note: u8,
    pub channel: u8,
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
            target: LaunchTarget::Clip { clip_id: clip_id.into() },
        }
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
        assert_eq!(clip, LaunchTarget::Clip { clip_id: "c-1".into() });
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
        let sent = [EchoNote { note: 60, channel: 0, at_ms: 1000 }];
        assert!(incoming_is_echo(
            &sent,
            EchoNote { note: 60, channel: 0, at_ms: 1040 },
            80
        ));
        assert!(!incoming_is_echo(
            &sent,
            EchoNote { note: 60, channel: 0, at_ms: 1090 },
            80
        ));
        assert!(!incoming_is_echo(
            &sent,
            EchoNote { note: 61, channel: 0, at_ms: 1010 },
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
        assert_eq!(rt.decide(true, 72, 3), IncomingDecision::Learn { note: 72, channel: 3 });
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
    fn fire_runs_on_a_worker_not_the_caller() {
        let rt = LaunchRuntime::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let caller = std::thread::current().id();
        rt.install_fire(std::sync::Arc::new(move |b| {
            let _ = tx.send((std::thread::current().id(), b.id.clone()));
        }));
        rt.set_bindings(vec![region("a", 60, None)]);
        rt.on_incoming(true, 60, 0);
        let (tid, id) = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("worker should fire");
        assert_eq!(id, "a");
        assert_ne!(tid, caller, "launch_fire must not run on the midir/caller thread");
    }
}
