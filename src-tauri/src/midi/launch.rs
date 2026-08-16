//! MIDI launch map: note → region or clip, plus the echo / self-trigger
//! guards so a clip AURA itself is playing cannot start itself via
//! MIDI-out loopback.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub const ECHO_WINDOW_MS: u64 = 80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LaunchTarget {
    Region {
        start_ticks: u64,
        length_ticks: u64,
        track_ids: Vec<String>,
    },
    Clip {
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
/// last-fired suppress set. The MIDI-in callback consults this; the
/// control plane owns the document copy and pushes snapshots here.
pub struct LaunchRuntime {
    bindings: Mutex<Vec<LaunchBinding>>,
    echo: Mutex<Vec<EchoNote>>,
    /// Trigger (note, channel-or-0xff-for-any) of the last fired clip that
    /// would self-trigger, held until a different fire or an explicit clear.
    suppressed: Mutex<Option<(u8, u8)>>,
    learning: Mutex<Option<String>>,
    armed: Mutex<Option<(u8, u8)>>,
    origin: Instant,
    fire: Mutex<Option<Arc<dyn Fn(LaunchBinding) + Send + Sync>>>,
    last_learn: Mutex<Option<(u8, u8)>>,
    gen: AtomicU64,
}

impl Default for LaunchRuntime {
    fn default() -> Self {
        Self {
            bindings: Mutex::new(Vec::new()),
            echo: Mutex::new(Vec::new()),
            suppressed: Mutex::new(None),
            learning: Mutex::new(None),
            armed: Mutex::new(None),
            origin: Instant::now(),
            fire: Mutex::new(None),
            last_learn: Mutex::new(None),
            gen: AtomicU64::new(0),
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
        *self.fire.lock().unwrap() = Some(f);
    }

    pub fn record_out(&self, note: u8, channel: u8) {
        let at = self.now_ms();
        let mut echo = self.echo.lock().unwrap();
        echo.push(EchoNote { note, channel, at_ms: at });
        let cutoff = at.saturating_sub(ECHO_WINDOW_MS * 4);
        echo.retain(|e| e.at_ms >= cutoff);
    }

    pub fn clear_suppress(&self) {
        *self.suppressed.lock().unwrap() = None;
        *self.armed.lock().unwrap() = None;
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
        if let Some((n, ch)) = *self.suppressed.lock().unwrap() {
            if n == note && (ch == 0xff || ch == channel) {
                return IncomingDecision::Suppressed;
            }
        }
        if *self.armed.lock().unwrap() == Some((note, channel)) {
            return IncomingDecision::Suppressed;
        }
        let bindings = self.bindings.lock().unwrap();
        match resolve(&bindings, note, channel).cloned() {
            Some(b) => IncomingDecision::Fire(b),
            None => IncomingDecision::Pass,
        }
    }

    /// MIDI-in callback: decide, and if it's a Fire, invoke the installed
    /// callback (off-RT; the midir thread). Returns true when the note is
    /// consumed as a launch / learn / echo / suppress.
    pub fn on_incoming(&self, on: bool, note: u8, channel: u8) -> bool {
        match self.decide(on, note, channel) {
            IncomingDecision::Pass => false,
            IncomingDecision::Learn { .. } => true,
            IncomingDecision::Echo | IncomingDecision::Suppressed => true,
            IncomingDecision::Fire(b) => {
                *self.armed.lock().unwrap() = Some((note, channel));
                if let LaunchTarget::Clip { .. } = &b.target {
                    let ch = b.channel.unwrap_or(0xff);
                    *self.suppressed.lock().unwrap() = Some((b.note, ch));
                }
                if let Some(f) = self.fire.lock().unwrap().clone() {
                    f(b);
                }
                true
            }
        }
    }
}

impl crate::control::ControlPlane {
    pub fn launch_bindings(&self) -> Vec<LaunchBinding> {
        self.session().lock().midi.launch_bindings.clone()
    }

    pub fn set_launch_binding(
        &self,
        id: String,
        binding: Option<LaunchBinding>,
        meta: crate::control::op::TxMeta,
    ) -> Result<Vec<LaunchBinding>, String> {
        self.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::LaunchBindingSet { id, binding })?;
            Ok(())
        })?;
        Ok(self.launch_bindings())
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
                LaunchTarget::Region { start_ticks, length_ticks, .. } => (*start_ticks, *length_ticks),
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
) -> Result<Vec<LaunchBinding>, String> {
    Ok(control.launch_bindings())
}

#[tauri::command]
pub fn launch_set(
    binding: Option<LaunchBinding>,
    id: Option<String>,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<Vec<LaunchBinding>, String> {
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
    fn firing_a_clip_suppresses_its_trigger() {
        let rt = LaunchRuntime::default();
        rt.set_bindings(vec![clip("c", 60, "clip-1")]);
        assert!(rt.on_incoming(true, 60, 0));
        assert_eq!(rt.decide(true, 60, 1), IncomingDecision::Suppressed);
    }
}
