//! AMT infilling glue (job kind `amtInfill`, zone C).
//!
//! The Python worker (`/sidecars/amt_infill_worker.py`) is driven through the
//! generic sidecar path (`sidecar_run_job` / `ControlPlane::run_sidecar_job`)
//! with `amtInfillParams`; this module owns the Rust side of the contract:
//!
//! * [`infill_params`] builds the params JSON from a clip + region
//!   (context notes = everything OUTSIDE the region; `bpm` is an additive
//!   field the worker uses for tick<->seconds conversion — schemas are
//!   emitter contracts, unknown fields are fine per D-06).
//! * [`parse_result`] validates an `amtInfillResult` payload.
//! * [`merge_infill`] applies the result with `midi_set_notes` semantics:
//!   the caller replaces the WHOLE clip note list with the merge of existing
//!   notes outside the region + generated notes inside it. Merge logic lives
//!   here, Rust-side — never in the worker.

use serde_json::{json, Value};

use super::types::{MidiClip, MidiNote};

/// Build `amtInfillParams` for a clip region (`region` in clip-relative
/// ticks, half-open `[start, end)`).
pub fn infill_params(
    ppq: u32,
    bpm: f64,
    clip: &MidiClip,
    region_start: u32,
    region_end: u32,
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> Result<Value, String> {
    if region_end <= region_start {
        return Err("regionEndTicks must be > regionStartTicks".into());
    }
    if region_end as u64 > clip.length_ticks {
        return Err(format!(
            "region end {region_end} exceeds clip length {}",
            clip.length_ticks
        ));
    }
    let context: Vec<&MidiNote> = clip
        .notes
        .iter()
        .filter(|n| n.tick < region_start || n.tick >= region_end)
        .collect();
    let mut params = json!({
        "ppq": ppq,
        "bpm": bpm,
        "contextNotes": context,
        "regionStartTicks": region_start,
        "regionEndTicks": region_end,
    });
    if let Some(t) = temperature {
        params["temperature"] = json!(t);
    }
    if let Some(p) = top_p {
        params["topP"] = json!(p);
    }
    Ok(params)
}

/// Validate + decode an `amtInfillResult` (the `result` of the job's `done`
/// event). Returns the generated notes rescaled to `target_ppq`.
pub fn parse_result(result: &Value, target_ppq: u32) -> Result<Vec<MidiNote>, String> {
    if result.get("kind").and_then(Value::as_str) != Some("amtInfill") {
        return Err(format!(
            "unexpected result kind {:?} (want \"amtInfill\")",
            result.get("kind")
        ));
    }
    let ppq = result
        .get("ppq")
        .and_then(Value::as_u64)
        .map(|p| p as u32)
        .unwrap_or(target_ppq);
    let notes_v = result
        .get("notes")
        .ok_or_else(|| "amtInfill result has no notes".to_string())?;
    let mut notes: Vec<MidiNote> =
        serde_json::from_value(notes_v.clone()).map_err(|e| format!("notes: {e}"))?;
    for n in &notes {
        n.validate()?;
    }
    if ppq != target_ppq && ppq > 0 {
        for n in notes.iter_mut() {
            n.tick = rescale(n.tick, ppq, target_ppq);
            n.length_ticks = rescale(n.length_ticks, ppq, target_ppq).max(1);
        }
    }
    Ok(notes)
}

#[inline]
fn rescale(t: u32, from_ppq: u32, to_ppq: u32) -> u32 {
    ((t as u64 * to_ppq as u64 + from_ppq as u64 / 2) / from_ppq as u64) as u32
}

/// Merge generated notes into a clip's note list (whole-clip replace value
/// for `midi_set_notes`): existing notes with onset INSIDE `[region_start,
/// region_end)` are replaced by the generated ones; generated notes outside
/// the region are discarded; lengths are clamped so nothing extends past the
/// region end. Result is sorted by (tick, key).
pub fn merge_infill(
    existing: &[MidiNote],
    region_start: u32,
    region_end: u32,
    generated: &[MidiNote],
) -> Vec<MidiNote> {
    let mut out: Vec<MidiNote> = existing
        .iter()
        .filter(|n| n.tick < region_start || n.tick >= region_end)
        .copied()
        .collect();
    for n in generated {
        if n.tick < region_start || n.tick >= region_end || n.validate().is_err() {
            continue;
        }
        let mut n = *n;
        let max_len = region_end - n.tick;
        n.length_ticks = n.length_ticks.min(max_len).max(1);
        // M-8: generated notes are copies, never identity carriers — force
        // to the "unassigned" sentinel so a sidecar echoing a live note id
        // can never masquerade as (and re-mint) an existing note. The
        // caller applies this list through `midi_set_notes`, whose own
        // keep-rule mints a fresh id for every zero.
        n.note_id = crate::ids::NoteId(0);
        out.push(n);
    }
    out.sort_by_key(|n| (n.tick, n.key));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};

    fn note(tick: u32, len: u32, key: u8, vel: u8) -> MidiNote {
        MidiNote { tick, length_ticks: len, key, velocity: vel, channel: 0, note_id: crate::ids::NoteId(0) }
    }

    fn note_id(tick: u32, len: u32, key: u8, vel: u8, id: u32) -> MidiNote {
        MidiNote { tick, length_ticks: len, key, velocity: vel, channel: 0, note_id: crate::ids::NoteId(id) }
    }

    fn clip(notes: Vec<MidiNote>) -> MidiClip {
        MidiClip {
            id: "c".into(),
            track_id: "t".into(),
            name: "n".into(),
            timeline_start_ticks: 0,
            length_ticks: 3840,
            notes,
            next_note_id: 1,
        }
    }

    #[test]
    fn params_take_context_outside_region_only() {
        let c = clip(vec![note(0, 100, 60, 100), note(1000, 100, 62, 100), note(2000, 100, 64, 100)]);
        let p = infill_params(960, 120.0, &c, 960, 1920, None, Some(0.9)).unwrap();
        let ctx = p["contextNotes"].as_array().unwrap();
        assert_eq!(ctx.len(), 2, "note at 1000 is inside the region");
        assert_eq!(p["regionStartTicks"], 960);
        assert_eq!(p["regionEndTicks"], 1920);
        assert_eq!(p["topP"], 0.9);
        assert_eq!(p["bpm"], 120.0);
        assert_eq!(ctx[0]["lengthTicks"], 100, "camelCase wire shape");

        assert!(infill_params(960, 120.0, &c, 1920, 960, None, None).is_err());
        assert!(infill_params(960, 120.0, &c, 0, 10_000, None, None).is_err());
    }

    #[test]
    fn parse_result_validates_and_rescales() {
        let r = json!({
            "kind": "amtInfill",
            "ppq": 480,
            "notes": [{"tick": 240, "lengthTicks": 120, "key": 60, "velocity": 90}],
        });
        let notes = parse_result(&r, 960).unwrap();
        assert_eq!(notes, vec![note(480, 240, 60, 90)]);

        assert!(parse_result(&json!({"kind": "other", "notes": []}), 960).is_err());
        let bad = json!({
            "kind": "amtInfill",
            "notes": [{"tick": 0, "lengthTicks": 10, "key": 200, "velocity": 90}],
        });
        assert!(parse_result(&bad, 960).is_err(), "range-validated");
    }

    #[test]
    fn merge_replaces_region_clamps_and_sorts() {
        let existing = vec![note(0, 100, 60, 100), note(1000, 500, 62, 100), note(2500, 100, 64, 100)];
        let generated = vec![
            note(1900, 500, 70, 90), // clamped to region end 2000
            note(1200, 100, 71, 90),
            note(2500, 100, 72, 90), // outside region -> dropped
        ];
        let merged = merge_infill(&existing, 1000, 2000, &generated);
        assert_eq!(
            merged,
            vec![
                note(0, 100, 60, 100),
                note(1200, 100, 71, 90),
                note(1900, 100, 70, 90), // length clamped 500 -> 100
                note(2500, 100, 64, 100),
            ]
        );
    }

    /// M-8: surviving existing notes keep their real ids; generated notes
    /// are ALWAYS forced to the unassigned sentinel, even one that happens
    /// to echo a live id from the context payload — otherwise a sidecar
    /// echoing a live id back could get an innocent existing note re-minted
    /// when the merged list is later applied through `midi_set_notes`.
    #[test]
    fn merge_keeps_existing_ids_and_forces_generated_to_unassigned() {
        let existing = vec![note_id(0, 100, 60, 100, 7), note_id(2500, 100, 64, 100, 9)];
        // A generated note that (adversarially) echoes the surviving id 9.
        let generated = vec![note_id(1200, 100, 71, 90, 9)];
        let merged = merge_infill(&existing, 1000, 2000, &generated);
        let survivor = merged.iter().find(|n| n.tick == 0).unwrap();
        assert_eq!(survivor.note_id.0, 7, "surviving note keeps its real id");
        let generated_out = merged.iter().find(|n| n.tick == 1200).unwrap();
        assert_eq!(generated_out.note_id.0, 0, "generated note forced to unassigned, never 9");
        let other_survivor = merged.iter().find(|n| n.tick == 2500).unwrap();
        assert_eq!(other_survivor.note_id.0, 9, "id 9 still belongs to the ORIGINAL survivor");
    }

    /// End-to-end simulate-mode run of the actual worker script, exercising
    /// params -> NDJSON -> result -> merge. Skipped when python3 is missing.
    #[test]
    fn simulate_worker_end_to_end() {
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sidecars/amt_infill_worker.py");
        assert!(script.is_file(), "worker script present at {script:?}");

        let c = clip(vec![note(0, 480, 60, 100), note(2880, 480, 72, 100)]);
        let params = infill_params(960, 120.0, &c, 960, 2880, Some(1.0), None).unwrap();
        let mut child = Command::new("python3")
            .arg(&script)
            .arg("--params-json")
            .arg(params.to_string())
            .arg("--simulate")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn worker");
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "worker exit ok; output: {out}");

        let mut result = None;
        for line in out.lines() {
            let v: Value = serde_json::from_str(line).expect("NDJSON line");
            match v["type"].as_str() {
                Some("done") => result = Some(v["result"].clone()),
                Some("error") => panic!("worker error: {v}"),
                _ => {}
            }
        }
        let result = result.expect("done event");
        assert_eq!(result["simulated"], true);
        let generated = parse_result(&result, 960).unwrap();
        assert!(!generated.is_empty());
        let merged = merge_infill(&c.notes, 960, 2880, &generated);
        // Context notes survive; all generated notes are inside the region.
        assert!(merged.contains(&note(0, 480, 60, 100)));
        assert!(merged.contains(&note(2880, 480, 72, 100)));
        for n in merged.iter().filter(|n| n.tick >= 960 && n.tick < 2880) {
            assert!(n.tick + n.length_ticks <= 2880, "clamped into region");
        }
        assert!(merged.len() > 2, "region actually infilled");
        // Determinism: a second run yields the identical result.
        let out2 = Command::new("python3")
            .arg(&script)
            .arg("--params-json")
            .arg(params.to_string())
            .arg("--simulate")
            .output()
            .unwrap();
        let stdout2 = String::from_utf8_lossy(&out2.stdout);
        let done2 = stdout2
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["type"] == "done")
            .unwrap();
        assert_eq!(parse_result(&done2["result"], 960).unwrap(), generated);
    }
}
