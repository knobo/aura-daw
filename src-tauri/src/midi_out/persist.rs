//! Per-machine, per-project MIDI routing persistence.
//!
//! Routing (which track/clip goes to which port+channel, and whether a
//! port is slaved via clock) is app config — same "ruling 10" carve-out as
//! before: it is never written into `project.json`. This file lives at
//! `dirs::config_dir()/aura/midi-routing.json`, keyed by the project's
//! canonical directory path, so a project's routing survives an app
//! restart on the SAME machine but never travels with the project itself
//! (ports and port ids are machine-specific).
//!
//! Keyed by port **name**, not the volatile `"<name>#<index>"` id `midir`
//! hands out each session — an index can shift between restarts (same
//! reasoning `docs/midi-output.md` already gives for why routing used to
//! reset on every launch).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// Read [`PersistedRoute::channel`]: absent or `null` -> `None`, `0` ->
/// `None` (the legacy "never picked" value, see the field's doc), any other
/// channel -> `Some(ch)`. Out-of-range values are clamped into 0-15 by the
/// same rule, so a corrupt file cannot forge a bad status byte.
fn de_channel<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
    let raw = Option::<u8>::deserialize(d)?;
    Ok(match raw {
        None | Some(0) => None,
        Some(ch) => Some(ch & 0x0F),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingFile {
    #[serde(default)]
    pub ports: HashMap<String, PortPrefs>,
    #[serde(default)]
    pub projects: HashMap<String, ProjectRouting>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PortPrefs {
    #[serde(default)]
    pub clock_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectRouting {
    #[serde(default)]
    pub routes: Vec<PersistedRoute>,
    /// Port names that were open for this project, including clock-only
    /// ports that have no routes. Distinct from `routes` so Recipe 1
    /// (clock slave, no note routing) survives a restart. `#[serde(default)]`
    /// keeps files written before this field readable.
    #[serde(default)]
    pub open_ports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedRoute {
    /// `"track"` or `"clip"`.
    pub scope: String,
    pub id: String,
    pub port_name: String,
    /// Forced output channel, or `None` for "each note on its own channel"
    /// (the default — see [`super::RouteTarget::channel`]).
    ///
    /// **Legacy migration.** Before the channel became an override, this
    /// field was a plain `u8` that `midi_set_*_route` filled with
    /// `channel.unwrap_or(0)` — so a stored `0` cannot be told apart from
    /// "the user never picked a channel", which is what it almost always
    /// was. A stored `0` therefore loads as `None` (follow the clip),
    /// while any stored non-zero channel was a deliberate pick and stays
    /// forced. See [`de_channel`].
    #[serde(default, deserialize_with = "de_channel")]
    pub channel: Option<u8>,
    /// cpal input-device name this track records its audio return from.
    /// Track routes only; clip overrides never carry one. Missing in files
    /// written before X1 — `None` means "no return, MIDI-only".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_device: Option<String>,
}

fn default_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("aura"))
}

fn default_path() -> Option<PathBuf> {
    Some(default_dir()?.join("midi-routing.json"))
}

/// Read the per-machine file, or an empty default if it doesn't exist yet,
/// can't be read, or fails to parse (corruption degrades to "no persisted
/// routing", never a crash — same philosophy as `utils/prefs.ts` on the
/// frontend side).
pub fn load() -> RoutingFile {
    match default_path() {
        Some(p) => load_from_path(&p),
        None => RoutingFile::default(),
    }
}

/// Read from an explicit path — the seam both `load()` (production default)
/// and `MidiOut`'s test-only path override (`set_routing_path_for_test`)
/// funnel through, so a test never touches the real developer machine's
/// config file.
pub(crate) fn load_from_path(path: &Path) -> RoutingFile {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => RoutingFile::default(),
    }
}

/// Whole-file rewrite — this is not a hot path (called once per explicit
/// routing/port/clock change, a human-paced action). A write failure is
/// logged, not propagated: the live routing already applied in-memory, and
/// there is nothing a caller could usefully do about a failed local-config
/// save.
pub fn save(file: &RoutingFile) {
    if let Some(p) = default_path() {
        save_to_path(&p, file);
    }
}

pub(crate) fn save_to_path(path: &Path, file: &RoutingFile) {
    let Some(dir) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("midi routing: cannot create {}: {e}", dir.display());
        return;
    }
    let json = match serde_json::to_string_pretty(file) {
        Ok(j) => j,
        Err(e) => {
            log::warn!("midi routing: serialize failed: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(path, json) {
        log::warn!("midi routing: cannot write {}: {e}", path.display());
    }
}

/// The key a project's routing is filed under: its canonicalized directory
/// path, falling back to the given path's own (lossy) string form if
/// canonicalization fails (e.g. called before the directory exists on a
/// brand-new project's first save) — either way the SAME directory always
/// produces the SAME key within one run, which is all correctness here
/// needs.
pub fn project_key(dir: &Path) -> String {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// A port name with the volatile ALSA address stripped off the end.
///
/// `midir`'s ALSA-seq backend formats a port name as
/// `"<client>:<port> <clientNum>:<portNum>"` — e.g.
/// `"Hydrogen:Hydrogen Midi-In 128:0"`. The trailing `128:0` is the sequencer
/// address, which the kernel hands out in connection order: restart Hydrogen
/// (or plug a USB keyboard in first) and the SAME device comes back as
/// `"Hydrogen:Hydrogen Midi-In 129:0"`. Persisting routing by the raw name
/// therefore lost the route on the next restart — silently, because a route
/// naming a port that is not visible is skipped by design. A reporter's own
/// routing file showed both spellings of the same Hydrogen for exactly that
/// reason.
///
/// So matching is two-stage: exact name first (never weaken an exact hit),
/// this normalized form second. A name with no such suffix — every non-ALSA
/// backend — is returned unchanged, so nothing else changes behaviour.
pub fn port_name_key(name: &str) -> &str {
    let Some((head, tail)) = name.rsplit_once(' ') else { return name };
    let Some((client, port)) = tail.split_once(':') else { return name };
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if numeric(client) && numeric(port) && !head.is_empty() {
        head
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aura-midi-routing-persist-test-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_from_a_missing_file_is_an_empty_default() {
        let dir = tmp_dir("missing");
        let path = dir.join("does-not-exist.json");
        assert_eq!(load_from_path(&path), RoutingFile::default());
    }

    #[test]
    fn load_from_corrupt_json_degrades_to_default_instead_of_panicking() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("midi-routing.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_from_path(&path), RoutingFile::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tmp_dir("roundtrip");
        let path = dir.join("cfg").join("midi-routing.json");

        let mut file = RoutingFile::default();
        file.ports.insert("Hydrogen".into(), PortPrefs { clock_enabled: true });
        file.projects.insert(
            "/tmp/song-a".into(),
            ProjectRouting {
                routes: vec![PersistedRoute {
                    scope: "track".into(),
                    id: "t-1".into(),
                    port_name: "Hydrogen".into(),
                    channel: Some(9),
                    return_device: Some("Interface In 3-4".into()),
                }],
                open_ports: vec!["Hydrogen".into()],
            },
        );

        save_to_path(&path, &file);
        let back = load_from_path(&path);
        assert_eq!(back, file);
    }

    #[test]
    fn project_key_is_stable_for_the_same_directory() {
        let dir = tmp_dir("key-stability");
        assert_eq!(project_key(&dir), project_key(&dir));
    }

    /// Files written before `open_ports` existed must still load — a missing
    /// field is empty, never a hard parse failure that would wipe routing.
    #[test]
    fn project_routing_missing_open_ports_deserializes_as_empty() {
        let json = r#"{"routes":[{"scope":"track","id":"t-1","port_name":"Hydrogen","channel":9}]}"#;
        let proj: ProjectRouting = serde_json::from_str(json).unwrap();
        assert!(proj.open_ports.is_empty());
        assert_eq!(proj.routes.len(), 1);
        assert_eq!(proj.routes[0].port_name, "Hydrogen");
        assert_eq!(proj.routes[0].return_device, None);
    }

    /// The legacy `channel: 0` migration. Before the channel became an
    /// override there was no way to express "leave the notes alone": the
    /// command filled the field with `channel.unwrap_or(0)`, so every route a
    /// user patched without touching the channel box stored a 0. Loading that
    /// as a forced channel 1 would keep the reported bug alive across the
    /// upgrade for exactly the people who hit it.
    #[test]
    fn a_legacy_zero_channel_loads_as_follow_the_clip() {
        let json = r#"{"scope":"track","id":"t-1","port_name":"Hydrogen","channel":0}"#;
        let r: PersistedRoute = serde_json::from_str(json).unwrap();
        assert_eq!(r.channel, None);
    }

    /// A non-zero channel, on the other hand, could only have come from a
    /// deliberate pick — it stays forced.
    #[test]
    fn a_deliberately_picked_channel_stays_forced() {
        let json = r#"{"scope":"track","id":"t-1","port_name":"Hydrogen","channel":9}"#;
        let r: PersistedRoute = serde_json::from_str(json).unwrap();
        assert_eq!(r.channel, Some(9));
    }

    /// A missing channel is the same "follow the clip" default, so a file
    /// written by this version round-trips through an older reader shape.
    #[test]
    fn a_missing_channel_is_follow_the_clip() {
        let json = r#"{"scope":"track","id":"t-1","port_name":"Hydrogen"}"#;
        let r: PersistedRoute = serde_json::from_str(json).unwrap();
        assert_eq!(r.channel, None);
    }

    #[test]
    fn port_name_key_strips_only_a_trailing_alsa_address() {
        assert_eq!(
            port_name_key("Hydrogen:Hydrogen Midi-In 128:0"),
            "Hydrogen:Hydrogen Midi-In"
        );
        // Same device, later session — both spell to the same key, which is
        // the whole point.
        assert_eq!(
            port_name_key("Hydrogen:Hydrogen Midi-In 129:0"),
            port_name_key("Hydrogen:Hydrogen Midi-In 128:0")
        );
        // Nothing that is not an address gets eaten.
        assert_eq!(port_name_key("Hydrogen"), "Hydrogen");
        assert_eq!(port_name_key("IAC Driver Bus 1"), "IAC Driver Bus 1");
        assert_eq!(port_name_key("Weird:Port 12"), "Weird:Port 12");
        assert_eq!(port_name_key("Weird:Port a:b"), "Weird:Port a:b");
        assert_eq!(port_name_key("128:0"), "128:0");
        assert_eq!(port_name_key(""), "");
    }

    /// Files written before X1 must still load — a missing `return_device`
    /// is "no return", never a hard parse failure that would wipe routing.
    #[test]
    fn persisted_route_missing_return_device_deserializes_as_none() {
        let json = r#"{"scope":"track","id":"t-1","port_name":"Hydrogen","channel":9}"#;
        let r: PersistedRoute = serde_json::from_str(json).unwrap();
        assert_eq!(r.return_device, None);
        assert_eq!(r.port_name, "Hydrogen");
    }
}
