//! Chord progressions: circle walks, named schemas, and a functional automaton.
//!
//! Three plans, deliberately different in kind (product doc §4.4):
//!
//! * **Circle walk** — the owner's literal ask, and a genuinely useful étude:
//!   counter-clockwise motion by fifths is the strongest harmonic drive there
//!   is, so a walk *is* a progression.
//! * **Schemas** — the progressions a listener already knows, written once as
//!   Roman numerals (`"I V vi IV"`) and transposed by `analysis::parse_roman`
//!   into any key, major or minor. One string, not twelve tables.
//! * **Functional automaton** — a weighted walk over T → PD → D → T with a
//!   cadence nailed to the end. This is what generates something the user has
//!   not heard before while still obeying the grammar.
//!
//! Everything here is seeded and pure (ruling H-4).

use super::analysis::{analyze, parse_roman, Function};
use super::chord::{diatonic_chord, Chord, ChordQuality};
use super::circle::{self, borrowed_chords};
use super::rng::Rng;
use super::scale::Key;

/// A generated progression: the chords, and the key they are actually IN.
///
/// The key is a result, not just an echo of the request: a minor-mode schema
/// asked for in a major key resolves against the RELATIVE minor (the
/// Andalusian cadence in C major is `Am G F E` — same seven notes, different
/// home), and the caller writes that key into the harmony document so the
/// analysis, the palette and the labels all agree with what was generated.
#[derive(Debug, Clone, PartialEq)]
pub struct Progression {
    pub key: Key,
    pub slots: Vec<Slot>,
    /// The plan-level explanation: what this progression IS.
    pub why: String,
}

impl Progression {
    pub fn symbols(&self) -> Vec<String> {
        self.slots.iter().map(|s| s.chord.symbol()).collect()
    }

    pub fn total_bars(&self) -> u32 {
        self.slots.iter().map(|s| s.bars).sum()
    }
}

/// One chord in a generated progression, with its length in BARS and the
/// sentence that says why it is there.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    pub chord: Chord,
    pub bars: u32,
    pub roman: String,
    pub why: String,
}

/// How to generate.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// `steps` chords walking the circle. `direction` `-1` is
    /// counter-clockwise (falling fifths — the direction harmony moves).
    CircleWalk { direction: i16, sevenths: bool },
    /// A named schema from [`SCHEMAS`].
    Schema(String),
    /// The functional automaton. `adventurousness` 0..100 unlocks sevenths,
    /// then secondary dominants, then borrowed chords.
    Functional { adventurousness: u8 },
}

impl Default for Plan {
    fn default() -> Self {
        Plan::Functional { adventurousness: 25 }
    }
}

impl Plan {
    /// Parse the wire form: `"circle"`, `"circle:cw"`, `"functional"`,
    /// `"functional:60"`, or a schema id.
    pub fn parse(s: &str) -> Result<Plan, String> {
        let s = s.trim();
        let (head, arg) = match s.split_once(':') {
            Some((h, a)) => (h, Some(a)),
            None => (s, None),
        };
        match head {
            "circle" | "circleOfFifths" => Ok(Plan::CircleWalk {
                direction: if arg == Some("cw") { 1 } else { -1 },
                sevenths: arg == Some("7"),
            }),
            "functional" => Ok(Plan::Functional {
                adventurousness: arg.and_then(|a| a.parse().ok()).unwrap_or(25),
            }),
            other => {
                if SCHEMAS.iter().any(|s| s.id == other) {
                    Ok(Plan::Schema(other.to_string()))
                } else {
                    Err(format!("unknown progression plan {s:?}"))
                }
            }
        }
    }
}

/// A named progression, written as Roman numerals so it transposes into any
/// key. `numerals` tokens are `NUMERAL` or `NUMERAL*bars`.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    pub id: &'static str,
    pub label: &'static str,
    pub numerals: &'static str,
    /// Written for a minor tonic (the numerals read against a minor key).
    pub minor: bool,
    pub why: &'static str,
}

/// The schemas. Short on purpose: each one is a progression a beginner will
/// recognise the moment they hear it, which is the point — recognition is the
/// fastest way to believe that theory describes music you already like.
pub const SCHEMAS: &[Schema] = &[
    Schema {
        id: "axis",
        label: "I–V–vi–IV (the axis)",
        numerals: "I V vi IV",
        minor: false,
        why: "The most-used four chords in popular music. It works because it leaves home, \
              takes the dominant early, drops to the relative minor, and comes back through \
              the subdominant — every function in four bars, and it loops seamlessly.",
    },
    Schema {
        id: "doo-wop",
        label: "I–vi–IV–V (doo-wop)",
        numerals: "I vi IV V",
        minor: false,
        why: "The fifties changes. Same four chords as the axis in a different order, and the \
              difference is everything: this one ENDS on the dominant, so it pulls back to the \
              top of the loop instead of resting.",
    },
    Schema {
        id: "royal-road",
        label: "IV–V–iii–vi (the royal road)",
        numerals: "IV V iii vi",
        minor: false,
        why: "The 王道進行 of Japanese pop and anime themes. The V does not go to I — it goes \
              to iii, which is the deceptive move that makes it sound wistful rather than \
              resolved.",
    },
    Schema {
        id: "pachelbel",
        label: "Pachelbel / canon",
        numerals: "I V vi iii IV I IV V",
        minor: false,
        why: "A descending bass line dressed as chords: every step falls, which is why it \
              feels inevitable. Three hundred years of pop songs have borrowed it.",
    },
    Schema {
        id: "circle-progression",
        label: "iii–vi–ii–V–I (the circle progression)",
        numerals: "iii vi ii V I",
        minor: false,
        why: "Four consecutive steps counter-clockwise on the circle of fifths. This is the \
              circle of fifths used as music rather than as a diagram — each chord is the \
              dominant of the next.",
    },
    Schema {
        id: "ii-v-i",
        label: "ii7–V7–Imaj7 (the jazz cadence)",
        numerals: "ii7 V7 Imaj7*2",
        minor: false,
        why: "The sentence jazz is made of. The 7th of ii falls to the 3rd of V, and the 3rd \
              and 7th of V resolve inward to the 7th and 3rd of I — two voices doing all the \
              work while the roots walk down the circle.",
    },
    Schema {
        id: "12-bar-blues",
        label: "12-bar blues",
        numerals: "I7*4 IV7*2 I7*2 V7 IV7 I7 V7",
        minor: false,
        why: "Twelve bars, three chords, all of them dominant sevenths — which is theoretically \
              impossible and practically the foundation of most of the twentieth century. The \
              last bar's V7 is the turnaround: it exists to send you round again.",
    },
    Schema {
        id: "andalusian",
        label: "i–♭VII–♭VI–V (Andalusian)",
        numerals: "i bVII bVI V",
        minor: true,
        why: "A stepwise descent to a major dominant — flamenco, surf, and every dramatic \
              minor-key vamp. The last chord is borrowed from harmonic minor, which is what \
              makes it sound like it means it.",
    },
    Schema {
        id: "lament",
        label: "i–♭VII–♭VI–♭VII (lament)",
        numerals: "i bVII bVI bVII",
        minor: true,
        why: "The Andalusian's softer sibling: it never takes the dominant, so it never \
              resolves — it just circles. Use it when you want unease without drama.",
    },
    Schema {
        id: "minor-cadence",
        label: "i–iv–V7–i (minor cadence)",
        numerals: "i iv V7 i",
        minor: true,
        why: "The minor key's own sentence. Its V is borrowed from harmonic minor: natural \
              minor's v is minor and has no leading tone, so it cannot cadence.",
    },
    Schema {
        id: "emotional",
        label: "vi–IV–I–V",
        numerals: "vi IV I V",
        minor: false,
        why: "The axis rotated to start on the relative minor. The same four chords sound sad \
              when the minor one comes first — which is a good demonstration that harmony is \
              about order, not ingredients.",
    },
];

/// Look a schema up by id.
pub fn schema(id: &str) -> Option<&'static Schema> {
    SCHEMAS.iter().find(|s| s.id == id)
}

/// Generate a progression of `bars` bars.
///
/// Schemas repeat to fill (and truncate at a chord boundary if the last
/// repetition does not fit); the automaton and the circle walk generate
/// exactly as many chords as there are bars.
pub fn generate(key: &Key, plan: &Plan, bars: usize, seed: u64) -> Result<Progression, String> {
    if bars == 0 {
        return Ok(Progression { key: *key, slots: Vec::new(), why: String::new() });
    }
    match plan {
        Plan::CircleWalk { direction, sevenths } => Ok(Progression {
            key: *key,
            slots: circle_walk(key, bars, *direction, *sevenths),
            why: if *direction < 0 {
                "A walk counter-clockwise around the circle of fifths. Every chord is a fifth \
                 above the next, so each one acts as the dominant of the one that follows — \
                 which is why falling fifths pull forward and rising fifths drift."
                    .to_string()
            } else {
                "A walk clockwise around the circle of fifths — rising fifths. Each step adds a \
                 sharp and brightens; it drifts away from home instead of pulling toward it."
                    .to_string()
            },
        }),
        Plan::Schema(id) => {
            let s = schema(id).ok_or_else(|| format!("unknown schema {id:?}"))?;
            // A schema is written in a mode. Asked for against the other mode,
            // it resolves in the RELATIVE key rather than being forced through
            // numerals that would mean something else there.
            let working = if s.minor == key.scale.is_minorish() {
                *key
            } else {
                circle::relative(key)
            };
            let mut why = format!("{} — {}", s.label, s.why);
            if working != *key {
                why.push_str(&format!(
                    " Written for a {} tonic, so it lands in {} — the relative {} of {}, which \
                     shares every note.",
                    if s.minor { "minor" } else { "major" },
                    working.label(),
                    if working.scale.is_minorish() { "minor" } else { "major" },
                    key.label(),
                ));
            }
            Ok(Progression { key: working, slots: schema_slots(&working, s, bars)?, why })
        }
        Plan::Functional { adventurousness } => Ok(Progression {
            key: *key,
            slots: functional(key, bars, *adventurousness, seed),
            why: format!(
                "Generated from the functional grammar of {}: tonic departs, predominant \
                 prepares, dominant resolves, and the last two bars are a cadence so the \
                 phrase ends rather than stops.",
                key.label()
            ),
        }),
    }
}

fn slot(key: &Key, chord: Chord, bars: u32, why: Option<String>) -> Slot {
    let a = analyze(&chord, key);
    Slot { chord, bars, roman: a.roman, why: why.unwrap_or(a.why) }
}

/// A walk around the circle of fifths, starting on the tonic. Each chord is
/// the dominant of the next when walking counter-clockwise, which is why this
/// sounds like a progression and not like a scale exercise.
fn circle_walk(key: &Key, bars: usize, direction: i16, sevenths: bool) -> Vec<Slot> {
    let roots = circle::walk(key.tonic, bars, direction);
    roots
        .into_iter()
        .enumerate()
        .map(|(i, root)| {
            // Diatonic where possible so the walk stays inside the key for as
            // long as it can; dominant sevenths once it has left, because that
            // is what keeps a walk moving.
            let degree = key.spelled().iter().position(|t| t.pitch_class() == root.pitch_class());
            let chord = match degree.and_then(|d| diatonic_chord(key, d, sevenths)) {
                Some(c) => c,
                None if sevenths || direction < 0 => Chord::new(root, ChordQuality::Dom7),
                None => Chord::new(root, ChordQuality::Maj),
            };
            let why = if direction < 0 {
                format!(
                    "Step {} counter-clockwise. {} is a fifth above the next chord, so it acts \
                     as its dominant — this is the pull that makes falling fifths the strongest \
                     progression in tonal music.",
                    i + 1,
                    chord.root.pretty(),
                )
            } else {
                format!(
                    "Step {} clockwise (rising fifths). Each chord adds a sharp; the harmony \
                     brightens and drifts away from home rather than pulling toward it.",
                    i + 1,
                )
            };
            slot(key, chord, 1, Some(why))
        })
        .collect()
}

/// Expand a schema's numerals into slots, repeating to fill `bars`.
fn schema_slots(key: &Key, s: &Schema, bars: usize) -> Result<Vec<Slot>, String> {
    let mut one_pass: Vec<Slot> = Vec::new();
    for token in s.numerals.split_whitespace() {
        let (numeral, n) = match token.split_once('*') {
            Some((n, count)) => (n, count.parse::<u32>().unwrap_or(1).max(1)),
            None => (token, 1),
        };
        let chord = parse_roman(key, numeral)?;
        one_pass.push(slot(key, chord, n, None));
    }
    if one_pass.is_empty() {
        return Err(format!("schema {:?} has no chords", s.id));
    }
    // Repeat whole passes, then take chords until the bar budget runs out.
    let mut out: Vec<Slot> = Vec::new();
    let mut used = 0u32;
    while used < bars as u32 {
        for sl in &one_pass {
            if used >= bars as u32 {
                break;
            }
            let mut sl = sl.clone();
            // A chord that would overrun the budget is shortened rather than
            // dropped: a truncated 12-bar blues should still start on I7.
            sl.bars = sl.bars.min(bars as u32 - used);
            used += sl.bars;
            out.push(sl);
        }
    }
    Ok(out)
}

/// Function-to-function transition weights: the grammar. Ordered
/// `[Tonic, Predominant, Dominant]`, rows = from, columns = to.
///
/// These are not measured from a corpus and do not pretend to be — they encode
/// the textbook rule (tonic departs, predominant prepares, dominant resolves)
/// with enough slack that the output is not four chords on a loop.
const TRANSITIONS: [[f32; 3]; 3] = [
    [0.15, 0.45, 0.40], // from tonic
    [0.15, 0.15, 0.70], // from predominant
    [0.80, 0.05, 0.15], // from dominant
];

fn function_index(f: Function) -> usize {
    match f {
        Function::Tonic => 0,
        Function::Predominant => 1,
        Function::Dominant | Function::Chromatic => 2,
    }
}

/// Candidate degrees per function, with a preference weight, indexed the same
/// way as [`TRANSITIONS`] (`0` tonic, `1` predominant, `2` dominant). `vi`
/// appears under both tonic and predominant because it genuinely is both.
const DEGREES: [&[(usize, f32)]; 3] = [
    &[(0, 0.6), (5, 0.3), (2, 0.1)],
    &[(3, 0.45), (1, 0.45), (5, 0.10)],
    &[(4, 0.8), (6, 0.2)],
];

/// How canonical a degree is within its function, normalised to 0..1 (`V` is
/// 1.0 among dominants, `vii` is 0.25). Without this the suggestion ranking
/// over-rewards shared notes and puts `Bm7♭5` above `G7` after a `IV` — which
/// is defensible arithmetic and bad advice.
fn degree_preference(degree: usize) -> f32 {
    DEGREES
        .iter()
        .filter_map(|group| {
            let max = group.iter().map(|(_, w)| *w).fold(0.0f32, f32::max).max(f32::EPSILON);
            group.iter().find(|(d, _)| *d == degree).map(|(_, w)| w / max)
        })
        .fold(0.0f32, f32::max)
}

/// The functional automaton with a cadence nailed to the end.
fn functional(key: &Key, bars: usize, adventurousness: u8, seed: u64) -> Vec<Slot> {
    let mut rng = Rng::stream(seed, "progression");
    let adv = adventurousness.min(100) as f32 / 100.0;
    let sevenths = adv > 0.3;
    let mut out: Vec<Slot> = Vec::new();
    let mut state = 0usize; // start at home
    let mut last_degree = usize::MAX;
    let mut repeats = 0usize;

    // The last two bars are the cadence; everything before them is the walk.
    let walk_bars = bars.saturating_sub(2);
    for _ in 0..walk_bars {
        let candidates = DEGREES[state];
        let weights: Vec<f32> = candidates
            .iter()
            .map(|(d, w)| {
                // Never three of the same chord in a row, and prefer to move.
                if *d == last_degree {
                    if repeats >= 1 {
                        0.0
                    } else {
                        *w * 0.25
                    }
                } else {
                    *w
                }
            })
            .collect();
        let (degree, _) = candidates[rng.weighted(&weights)];
        repeats = if degree == last_degree { repeats + 1 } else { 0 };
        last_degree = degree;
        let chord = diatonic_chord(key, degree, sevenths && rng.chance(adv))
            .or_else(|| diatonic_chord(key, degree, false))
            .unwrap_or_else(|| Chord::new(key.degree(degree), ChordQuality::Maj));
        out.push(slot(key, chord, 1, None));

        // A secondary dominant inserted BEFORE a chord is the cheapest way to
        // make a diatonic walk sound intentional; it costs the bar it takes.
        if adv > 0.5 && rng.chance((adv - 0.5) * 0.6) && out.len() + 2 < bars {
            let target = out.last().map(|s| s.chord.root).unwrap_or(key.tonic);
            let secondary = Chord::new(target.plus_fifths(1), ChordQuality::Dom7);
            let last = out.pop().expect("just pushed");
            out.push(slot(key, secondary, 1, None));
            out.push(last);
        }
        // Borrowed colour, only at the top of the range.
        if adv > 0.75 && rng.chance((adv - 0.75) * 0.5) && out.len() < bars.saturating_sub(2) {
            let options = borrowed_chords(key);
            if let Some((chord, why)) = rng.pick(&options).cloned() {
                out.push(slot(key, chord, 1, Some(why)));
            }
        }
        // Transition.
        state = rng.weighted(&TRANSITIONS[state]);
    }

    // Cadence: V → I (authentic), or IV → I (plagal) as the softer option.
    let plagal = rng.chance(0.2);
    if bars >= 2 {
        let pre_degree = if plagal { 3 } else { 4 };
        let pre = diatonic_chord(key, pre_degree, sevenths)
            .or_else(|| diatonic_chord(key, pre_degree, false))
            .unwrap_or_else(|| Chord::new(key.degree(pre_degree), ChordQuality::Maj));
        // A minor key's own v cannot cadence — it has no leading tone. Borrow
        // the major dominant, which is the entire reason harmonic minor exists.
        let pre = if !plagal && key.scale.is_minorish() {
            Chord::new(key.tonic.plus_fifths(1), if sevenths { ChordQuality::Dom7 } else { ChordQuality::Maj })
        } else {
            pre
        };
        let why = if plagal {
            "A plagal cadence (IV → I): it arrives without a leading tone, so it settles rather \
             than snaps shut. The \"amen\" ending."
                .to_string()
        } else {
            format!(
                "The cadence. {}",
                super::analysis::tritone_resolution(&pre).map(|t| format!("Its tritone resolves: {t}."))
                    .unwrap_or_else(|| "Its leading tone rises a semitone into the tonic.".to_string())
            )
        };
        out.push(slot(key, pre, 1, Some(why)));
    }
    let tonic = diatonic_chord(key, 0, false)
        .unwrap_or_else(|| Chord::new(key.tonic, ChordQuality::Maj));
    out.push(slot(
        key,
        tonic,
        1,
        Some(format!(
            "Home. Ending on {} on a strong bar is what makes the phrase sound finished rather \
             than interrupted.",
            tonic.pretty()
        )),
    ));

    // The insertions above can overshoot; trim from the middle so the cadence
    // always survives (a progression that loses its ending loses the point).
    while out.len() > bars {
        let victim = out.len().saturating_sub(3);
        out.remove(victim);
    }
    out
}

/// One ranked suggestion for "what comes next".
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub chord: Chord,
    pub roman: String,
    pub function: &'static str,
    /// 0..1, higher is a stronger recommendation. Composed of the grammar's
    /// transition weight, voice-leading smoothness and a novelty term — and
    /// the breakdown is in `why`, because a ranking a user cannot interrogate
    /// is a black box.
    pub score: f32,
    pub why: String,
}

/// Rank candidate next chords. This is the coaching surface (product doc §3's
/// "suggest" rung): it must be able to say WHY each option ranks where it does.
pub fn suggest_next(key: &Key, so_far: &[Chord], limit: usize) -> Vec<Suggestion> {
    let mut candidates: Vec<Chord> = Vec::new();
    for d in 0..key.degree_count() {
        for seventh in [false, true] {
            if let Some(c) = diatonic_chord(key, d, seventh) {
                if !candidates.contains(&c) {
                    candidates.push(c);
                }
            }
        }
    }
    for (c, _) in borrowed_chords(key) {
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    }

    let last = so_far.last();
    let from = last.map(|c| function_index(analyze(c, key).function)).unwrap_or(0);
    // Only the chord that is ALREADY sounding is penalised. A wider lookback
    // was tried and it demoted the single most expected move in tonal music —
    // returning home two chords after leaving it.
    let recent: Vec<u8> = so_far.last().map(|c| vec![c.root.pitch_class()]).unwrap_or_default();

    let mut out: Vec<Suggestion> = candidates
        .into_iter()
        .map(|chord| {
            let a = analyze(&chord, key);
            let to = function_index(a.function);
            let grammar = if last.is_some() { TRANSITIONS[from][to] } else { 0.6 };
            let smooth = last.map(|l| common_tone_fraction(l, &chord)).unwrap_or(0.5);
            let novelty = if recent.contains(&chord.root.pitch_class()) { 0.0 } else { 1.0 };
            let pref = a.degree.map(degree_preference).unwrap_or(0.4);
            let borrowed_penalty = if a.borrowed { 0.75 } else { 1.0 };
            let score = ((grammar * 0.40 + pref * 0.25 + smooth * 0.20 + novelty * 0.15)
                * borrowed_penalty)
                .clamp(0.0, 1.0);
            let why = match last {
                Some(l) => {
                    let common = common_count(l, &chord);
                    format!(
                        "{} → {}: {} → {} (the grammar weights that move {:.0}%), \
                         {common} common tone{}, {}. {}",
                        l.pretty(),
                        chord.pretty(),
                        analyze(l, key).function.id(),
                        a.function.id(),
                        grammar * 100.0,
                        if common == 1 { "" } else { "s" },
                        if novelty > 0.0 { "and it is new here" } else { "but you just played it" },
                        a.why,
                    )
                }
                None => a.why.clone(),
            };
            Suggestion { chord, roman: a.roman, function: a.function.id(), score, why }
        })
        .collect();
    // Total order, tie-broken by symbol so the ranking is deterministic
    // (ruling H-4 applies to suggestions too — the list must not shuffle
    // between two identical calls).
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chord.symbol().cmp(&b.chord.symbol()))
    });
    out.truncate(limit.max(1));
    out
}

fn common_count(a: &Chord, b: &Chord) -> usize {
    let pa = a.pitch_classes();
    b.pitch_classes().iter().filter(|pc| pa.contains(pc)).count()
}

/// Shared pitch classes as a fraction of the smaller chord — a cheap, honest
/// stand-in for voice-leading distance at the chord-choice stage (the real
/// distance is computed later, in `voicing`, where octaves exist).
fn common_tone_fraction(a: &Chord, b: &Chord) -> f32 {
    let n = a.pitch_classes().len().min(b.pitch_classes().len()).max(1);
    common_count(a, b) as f32 / n as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::scale::ScaleType;
    use crate::theory::tpc::Tpc;

    fn gen(key: &Key, plan: &Plan, bars: usize, seed: u64) -> Progression {
        generate(key, plan, bars, seed).expect("plan generates")
    }

    #[test]
    fn a_counter_clockwise_walk_falls_by_fifths_and_leaves_the_key_as_sevenths() {
        let p = gen(&Key::c_major(), &Plan::CircleWalk { direction: -1, sevenths: false }, 6, 1);
        assert_eq!(p.symbols(), ["C", "F", "Bb7", "Eb7", "Ab7", "Db7"]);
        assert!(p.slots[0].why.contains("counter-clockwise"));
        assert!(p.why.contains("dominant"));
        assert!(p.slots.iter().all(|s| s.bars == 1));
    }

    #[test]
    fn a_clockwise_walk_rises_and_says_so() {
        let p = gen(&Key::c_major(), &Plan::CircleWalk { direction: 1, sevenths: false }, 4, 1);
        assert_eq!(p.symbols(), ["C", "G", "Dm", "Am"], "still diatonic while it can be");
        assert!(p.slots[1].why.contains("clockwise"));
    }

    #[test]
    fn schemas_transpose_into_any_key() {
        let d = Key::new(Tpc::D, ScaleType::Ionian);
        let p = gen(&d, &Plan::Schema("axis".into()), 4, 0);
        assert_eq!(p.symbols(), ["D", "A", "Bm", "G"]);
        assert_eq!(p.slots[0].roman, "I");
        assert_eq!(p.slots[2].roman, "vi");
        assert!(p.why.contains("axis"), "the progression carries the schema's own why");
        // A minor-mode schema in a minor key: the accidentals are read against
        // the major scale, so ♭VII is G and not G♭.
        let am = Key::new(Tpc::A, ScaleType::Aeolian);
        let and = gen(&am, &Plan::Schema("andalusian".into()), 4, 0);
        assert_eq!(and.symbols(), ["Am", "G", "F", "E"]);
        assert_eq!(and.key, am);
    }

    #[test]
    fn the_twelve_bar_blues_is_twelve_bars_of_sevenths() {
        let p = gen(&Key::c_major(), &Plan::Schema("12-bar-blues".into()), 12, 0);
        assert_eq!(p.total_bars(), 12);
        assert_eq!(p.symbols(), ["C7", "F7", "C7", "G7", "F7", "C7", "G7"]);
        assert_eq!(p.slots[0].bars, 4);
        assert_eq!(p.slots[1].bars, 2);
        assert!(p.slots.iter().all(|s| s.chord.quality == ChordQuality::Dom7));
    }

    #[test]
    fn a_minor_mode_schema_in_a_major_key_lands_on_the_relative_minor() {
        // Same seven notes, different home — and the progression says so.
        let p = gen(&Key::c_major(), &Plan::Schema("andalusian".into()), 4, 0);
        assert_eq!(p.symbols(), ["Am", "G", "F", "E"]);
        assert_eq!(p.key.canonical(), "A aeolian");
        assert!(p.why.contains("relative minor"), "{}", p.why);
        // And the other way round: a major schema asked for in a minor key.
        let am = Key::new(Tpc::A, ScaleType::Aeolian);
        let axis = gen(&am, &Plan::Schema("axis".into()), 4, 0);
        assert_eq!(axis.symbols(), ["C", "G", "Am", "F"]);
        assert_eq!(axis.key.canonical(), "C ionian");
    }

    #[test]
    fn schemas_repeat_to_fill_and_truncate_at_a_bar_boundary() {
        let p = gen(&Key::c_major(), &Plan::Schema("axis".into()), 10, 0);
        assert_eq!(p.total_bars(), 10, "exactly the bars asked for");
        assert_eq!(p.symbols().len(), 10);
        assert_eq!(p.symbols()[4], "C", "the loop starts over");
        // A schema longer than the request is cut, not scaled.
        let short = gen(&Key::c_major(), &Plan::Schema("12-bar-blues".into()), 3, 0);
        assert_eq!(short.total_bars(), 3);
        assert_eq!(short.symbols(), ["C7"], "one chord, shortened to three bars");
    }

    #[test]
    fn the_automaton_always_ends_on_a_cadence() {
        for seed in 0..40u64 {
            for bars in [4usize, 8, 16] {
                let slots =
                    gen(&Key::c_major(), &Plan::Functional { adventurousness: 30 }, bars, seed).slots;
                assert_eq!(slots.len(), bars, "seed {seed}, {bars} bars");
                let last = slots.last().unwrap();
                assert_eq!(last.chord.root.pitch_class(), 0, "ends at home (seed {seed})");
                let pre = &slots[slots.len() - 2];
                let f = analyze(&pre.chord, &Key::c_major()).function;
                assert!(
                    matches!(f, Function::Dominant | Function::Predominant),
                    "the penultimate chord must set up the arrival, got {} (seed {seed})",
                    pre.roman
                );
            }
        }
    }

    #[test]
    fn the_automaton_never_repeats_a_chord_three_times() {
        for seed in 0..40u64 {
            let slots =
                gen(&Key::c_major(), &Plan::Functional { adventurousness: 50 }, 16, seed).slots;
            for w in slots.windows(3) {
                assert!(
                    !(w[0].chord == w[1].chord && w[1].chord == w[2].chord),
                    "seed {seed}: {} three times",
                    w[0].chord.symbol()
                );
            }
        }
    }

    #[test]
    fn a_minor_key_automaton_borrows_a_real_dominant() {
        let am = Key::new(Tpc::A, ScaleType::Aeolian);
        let slots = gen(&am, &Plan::Functional { adventurousness: 40 }, 8, 5).slots;
        let pre = &slots[slots.len() - 2];
        // Either the borrowed major dominant (E/E7) or the plagal iv — never
        // the powerless natural-minor v.
        assert!(
            pre.chord.symbol().starts_with('E') || pre.chord.symbol().starts_with("Dm"),
            "got {}",
            pre.chord.symbol()
        );
        if pre.chord.symbol().starts_with('E') && !pre.chord.symbol().starts_with("Em") {
            assert_ne!(pre.chord.quality, ChordQuality::Min, "a minor v cannot cadence");
        }
    }

    #[test]
    fn generation_is_deterministic_and_seed_sensitive() {
        let plan = Plan::Functional { adventurousness: 70 };
        let a = gen(&Key::c_major(), &plan, 8, 1234);
        let b = gen(&Key::c_major(), &plan, 8, 1234);
        assert_eq!(a, b, "same seed, same progression");
        let c = gen(&Key::c_major(), &plan, 8, 1235);
        assert_ne!(a.symbols(), c.symbols(), "a different seed is a different idea");
    }

    #[test]
    fn adventurousness_adds_colour_rather_than_noise() {
        let tame: Vec<String> = (0..12u64)
            .flat_map(|s| gen(&Key::c_major(), &Plan::Functional { adventurousness: 0 }, 8, s).symbols())
            .collect();
        assert!(
            tame.iter().all(|s| ["C", "Dm", "Em", "F", "G", "Am", "Bdim"].contains(&s.as_str())),
            "at zero, strictly diatonic triads: {tame:?}"
        );
        let bold: Vec<String> = (0..12u64)
            .flat_map(|s| gen(&Key::c_major(), &Plan::Functional { adventurousness: 100 }, 8, s).symbols())
            .collect();
        assert!(
            bold.iter().any(|s| !["C", "Dm", "Em", "F", "G", "Am", "Bdim"].contains(&s.as_str())),
            "at a hundred, something borrowed or applied shows up: {bold:?}"
        );
    }

    #[test]
    fn suggestions_rank_the_dominant_after_a_predominant() {
        let key = Key::c_major();
        let so_far = [Chord::parse("C").unwrap(), Chord::parse("F").unwrap()];
        let s = suggest_next(&key, &so_far, 5);
        assert!(!s.is_empty());
        // After IV, the grammar wants a dominant.
        assert!(
            matches!(s[0].function, "dominant"),
            "top suggestion after C F was {} ({})",
            s[0].chord.symbol(),
            s[0].function
        );
        assert_eq!(s[0].chord.symbol(), "G7", "the dominant seventh, not the vii chord");
        assert!(s[0].why.contains("common tone"), "the ranking explains itself: {}", s[0].why);
        assert!(s.windows(2).all(|w| w[0].score >= w[1].score), "sorted by score");
    }

    #[test]
    fn suggestions_do_not_recommend_what_you_just_played() {
        let key = Key::c_major();
        let g = Chord::parse("G").unwrap();
        let s = suggest_next(&key, &[Chord::parse("C").unwrap(), g], 4);
        let top = &s[0];
        assert_ne!(top.chord.root.pitch_class(), g.root.pitch_class(), "novelty term");
        // From the dominant, home is at the top — as a triad or a maj7, both
        // of which are "C".
        assert_eq!(top.chord.root.pitch_class(), 0, "got {}", top.chord.symbol());
    }

    #[test]
    fn suggestions_are_deterministic() {
        let key = Key::c_major();
        let a = suggest_next(&key, &[Chord::parse("Am").unwrap()], 6);
        let b = suggest_next(&key, &[Chord::parse("Am").unwrap()], 6);
        assert_eq!(a, b);
    }

    #[test]
    fn plans_parse_from_the_wire() {
        assert!(matches!(
            Plan::parse("circle").unwrap(),
            Plan::CircleWalk { direction: -1, .. }
        ));
        assert!(matches!(
            Plan::parse("circle:cw").unwrap(),
            Plan::CircleWalk { direction: 1, .. }
        ));
        assert!(matches!(
            Plan::parse("functional:80").unwrap(),
            Plan::Functional { adventurousness: 80 }
        ));
        assert_eq!(Plan::parse("axis").unwrap(), Plan::Schema("axis".into()));
        assert!(Plan::parse("nonsense").is_err());
    }

    #[test]
    fn every_schema_generates_in_a_major_and_a_minor_key() {
        for s in SCHEMAS {
            for key in [Key::c_major(), Key::new(Tpc::A, ScaleType::Aeolian)] {
                let p = generate(&key, &Plan::Schema(s.id.into()), 8, 0)
                    .unwrap_or_else(|e| panic!("{} in {}: {e}", s.id, key.label()));
                assert_eq!(p.total_bars(), 8, "{}", s.id);
                assert!(!p.slots.is_empty());
                assert!(p.slots.iter().all(|x| !x.roman.is_empty()));
                assert!(!p.why.is_empty(), "{} explains itself", s.id);
            }
        }
    }
}
