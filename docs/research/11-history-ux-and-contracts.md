# Browsable Edit History and A/B Comparison in a DAW
## Interaction design, and what it demands of the architecture

**Date:** 2026-08-13
**Provenance:** The underlying idea — a browsable edit history, extract-to-track, and A/B comparison of alternatives — originated with the project owner. This document is the *interaction design* for that idea: the UX research, the design judgments, and the concrete architectural contracts those judgments impose. It does not originate the feature; it specifies how it should behave and what the backend must provide.

### Why this document exists

A DAW that keeps an op log with materializable revisions has, latently, something no shipping DAW has quite built: a history you can *hear*. The substrate work (op log, revisions, per-track variants) is specified elsewhere in `docs/SCALABILITY.md` §4–§5 and `docs/ipc-schemas/op-envelope.schema.json`. What was missing was the other half — what the user actually sees and does, and therefore what the backend must expose for any of it to be possible.

This document closes that gap. It surveys how eight other applications solved (or failed to solve) history browsing, decides what a DAW's history entry should be, designs the audible-diff and A/B interactions that are unique to this medium, settles the "what happens when you edit after travelling back" question, handles the AI-agent case, and then — **§7, the operative section** — turns all of it into 26 numbered contracts that can be tested.

If you read one section, read §7. If you read two, read §7 and §2.

**Substrate assumed:** an op log with monotonic revisions, any revision materializable, a per-track *variant* concept. Grounded against this repo's actual planned substrate — `docs/SCALABILITY.md` §4 (undo/journal) and §5 (op-log protocol), `docs/ipc-schemas/op-envelope.schema.json`, `docs/ARCHITECTURE.md` §2.3/§2.6/§11/§12, and `src-tauri/src/audio/offline.rs`.

**Epistemic note.** Everything under "Observed" was verified by fetching the source. "Judgment" is mine. A few items remain explicitly marked **UNVERIFIED** — prior knowledge without a citation anyone confirmed. Treat those as claims to check, not as facts. Items originally marked UNVERIFIED that were subsequently confirmed by a parallel research thread are marked **[confirmed by parallel research]** with the quotes that settled them.

---

## 1. Prior art in history UI

### 1.1 The survey

| App | What is listed | Granularity | Navigation | Branching | Survives session | User-annotatable |
|---|---|---|---|---|---|---|
| Photoshop | image states | one per image-changing action | click a state | opt-in (*Allow Non-Linear History*) + snapshots | no | snapshots renameable |
| Blender | operator entries | one per operator | click; dot marks current | no — truncates | **no** | no |
| REAPER | undo points, named by action | one per undo point | double-click; right-click menu | **yes**, opt-in | **yes** (`.RPP-UNDO`) | no |
| Cubase/Nuendo | Action / Time / State / Details | one row per edit | **drag a separator** | no | not documented | **yes** (Details) |
| Ableton Live 12 | actions, newest first | one per action | click *or* arrow keys + Enter | no | no | no |
| Figma | checkpoints + named versions | 30-min autosave, or manual | click to preview | via restore (non-destructive) | yes | yes (name + description) |
| Google Docs | versions, grouped | grouped edits, expandable | click to preview | restore | yes | yes (named versions) |

### 1.2 What each one teaches

**Photoshop — snapshots exist because the linear model was not felt to be safe.** *(Observed, but from a secondary source — Adobe's own helpx pages were unreachable from this environment; verified against Martin Evening's [Photoshop for Photographers help guide](http://www.photoshopforphotographers.com/CC_2013/Help_guide/tp/History_brush.html).)* You can reverse through "up to 1000 image states", configurable in preferences. The scary part is one sentence: when you select an earlier state, later states are **dimmed**, and "when you make further edits to the image, the dimmed history states after the selected history state will become deleted." No confirmation. The escape hatch — *Allow Non-Linear History*, which lets you "branch off in different directions" — is buried in a panel flyout, and the same source calls it "not an easy concept to grasp."

Two ideas here are genuinely worth stealing. First, **the history brush**: click the gutter beside a state and paint that state back in *spatially*. That is regional selective undo with a direct-manipulation front end, and it has an obvious audio analogue (§3.4). Second, **tile-scoped cost**: history memorizes changes "in each tile only", so a local brush stroke is cheap and a global filter is expensive. The memory model matches the edit's actual extent. Our op log has the same property for free.

**Blender — the two failure modes, stated in the manual's own words.** *(Observed: [Undo & Redo](https://docs.blender.org/manual/en/latest/interface/undo_redo.html).)* You may "hop around on the Undo timeline as much as you want **as long as you do not make a new change**. Once you do make a new change, the Undo History is **truncated at that point**." And: "When you quit Blender, the complete list of user actions will be **lost, even if you save your file** before quitting." Truncation plus non-persistence. Blender also documents *Undo Memory Limit* ("0 is unlimited") and a *Global Undo* toggle separating object-mode from edit-mode stacks. (The widely-cited default of 32 undo steps is **UNVERIFIED** — the manual page does not state defaults.)

Blender's best idea is adjacent to history rather than in it: **Adjust Last Operation (F9)** re-parameterizes the step you just took, in place. That deletes an entire category of undo/redo round-trips, and it is the right model for "the fade was 40 ms, make it 60."

**REAPER — the only one that actually branches, and it's nearly invisible.** *(Observed: [Up and Running: A REAPER User Guide v7.78](https://www.reaper.fm/userguide.php), §2.28, §22.2.1, §22.13 — fetched and text-extracted.)* Ctrl+Alt+Z opens the window; double-click any event to load that state; right-click offers *Remove selected state(s) from undo history*. Preferences expose maximum undo memory, whether to keep newest states when memory fills, and — tellingly — **which categories even count as undoable**: "whether to include item, track, envelope point and/or time selection and/or cursor positions changes." That setting ships configurable precisely because "selection changed" undo points are contentious.

History persists to a `.RPP-UNDO` file beside the project: "Even at some later date, you will still be able to revert the project to an earlier state." And the headline: "**Store multiple undo/redo paths.** You can even store alternate sequences of commands and actions, then switch between them!" §22.13 spells out the semantics — going back and editing creates "an alternate set: REAPER will remember both paths independently of each other. Moreover, every time you return to that point, another new undo path will be created."

The UI for this is a `(*2)` suffix on the branch-point row and a right-click menu. **The data model is a DAG; the presentation is a footnote.** That is the single largest unexploited affordance in the entire survey. (Forum threads could not be read — cockos.com is bot-walled — but a Reddit thread titled *"i have loaded an old undo history and lost all of my work"* exists, which is at least consistent with "the power is real and the affordance is not discoverable.")

**Cubase — the separator is the best safety metaphor in the survey.** *(Observed: [Edit History Dialog](https://www.steinberg.help/r/cubase-pro/15.0/en/cubase_nuendo/topics/project_window/project_window_edit_history_dialog_r.html).)* Four columns: **Action**, **Time**, **State**, **Details**. Navigation is not "click a row" — it is "**Move the separator upwards to undo your actions. To redo an action, move the separator down.**" The undone entries stay on screen *above the line*. The interaction visually promises reversibility instead of merely permitting it. And **Details** "allows you to enter new text" — the only user-annotatable history row in this entire survey. A user can leave themselves "before the vocal comp."

**Ableton Live 12 — has a history panel now**, opened from View or Ctrl/Cmd+Alt+Z, newest at top, navigable "by either clicking it or navigating to it with the up and down arrow keys and pressing Enter", and explicitly "**not saved with a Set** once it is closed." *(Observed: [Live 12 manual §5.4.2](https://www.ableton.com/en/live-manual/12/managing-files-and-sets/).)* Arrow-key traversal is underrated: it makes scrubbing history cheap, which is exactly what you want when the differences are audible rather than visible.

**Figma — restore is additive, and that is the whole trick.** *(Observed: [View a file's version history](https://help.figma.com/hc/en-us/articles/360038006754-View-a-file-s-version-history).)* "Figma records a new checkpoint every 30 minutes." Restoring "is a **non-destructive action**, so you can still access the current version in the file's version history" — Figma "will add **two autosave checkpoints** to the file's version history", one capturing the pre-restore state and one marking the restore. The future is never discarded; it is *demoted*.

And the principle that should govern the whole feature, from [How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/): "**if you undo a lot, copy something, and redo back to the present (a common operation), the document should not change**." Their mechanism: an undo modifies the redo history at that moment, and a redo modifies the undo history at that moment. Undo is not a time machine over the document; it is a *pair of stacks that stay consistent with the present*.

**Google Docs — grouping and naming.** *(Observed: [See changes to your file](https://support.google.com/docs/answer/190843).)* Automatic versions are clustered and expandable; named versions exist so "your versions aren't merged" (cap: 40 named per document). The lesson: **automatic grouping is the default and manual naming is the override**, not the other way round. That is the correct division of labour for §2's labelling problem.

**Git GUIs — DAG legibility.** *(Partly UNVERIFIED — the git-GUI research thread did not return, and GitKraken's and Sublime Merge's docs could not be fetched.)* The one design source verified is Nikita Prokopov's [Reinventing Git interface](https://tonsky.me/blog/reinventing-git-interface/), and its two claims transfer directly: merge commits should get "a different, much subtler look, because they are **not an effort per se**, but a place where two other efforts join"; and commits should be colored "**not by branch color** … but by **author**", because in practice you are looking for a specific person's work.

Both map onto our problem. A DAW history has exactly one structural join type worth de-emphasizing (returning to the mainline after an experiment), and exactly one identity axis worth coloring by: **who or what made the change — you, or the agent.**

The general point about DAG legibility, which the git-GUI ecosystem has spent fifteen years failing to solve: **users think linearly and history is a DAG.** Wikipedia's [Undo](https://en.wikipedia.org/wiki/Undo) article catalogues the models — linear, script model ("as though it never occurred"), US&R (skipped commands stay marked, "creates a directed graph structure allowing branching paths, though this complexity becomes difficult to manage across multiple steps"), selective undo. The literature already concluded that exposing the DAG *as a DAG* is where these systems fail. **Design judgment: render the DAG as a mainline with side-rails, never as a general graph.** More in §5.3.

---

## 2. The granularity problem

### 2.1 The unit is the *gesture*, not the op and not the session

Raw ops are unreadable ("set param 0.42" × 200). Sessions are useless ("Tuesday"). The unit that works is the one the user would name if you asked them what they just did: **one gesture = one history entry = one revision**.

The good news is that this repo's op envelope already encodes exactly this. `applyBatch` is documented as "applied atomically: all ops commit as one revision (**one undo entry**, at most one graph swap)", and it already carries an optional `label` — "Optional human-readable gesture label for the undo history (e.g. `"Move 12 clips"`)". The design work is deciding *where the batch boundary falls* and *who writes the label*.

### 2.2 Time-based coalescing: what everyone actually ships

Verified defaults, all from primary sources:

| System | Mechanism | Value |
|---|---|---|
| Yjs `UndoManager` | merges edits within `captureTimeout` | **500 ms**; `stopCapturing()` forces a break ([docs](https://docs.yjs.dev/api/undo-manager)) |
| ProseMirror `prosemirror-history` | `newGroupDelay` | **500 ms**, `depth` 100 ([src](https://github.com/ProseMirror/prosemirror-history/blob/master/src/history.ts)) |
| CodeMirror `@codemirror/commands` | `newGroupDelay` | **500 ms**, `minDepth` 100 ([src](https://github.com/codemirror/commands/blob/main/src/history.ts)) |
| Tracktion Engine | `UndoTransactionTimer` | **350 ms** ([src](https://github.com/Tracktion/tracktion_engine/blob/master/modules/tracktion_engine/model/edit/tracktion_Edit.cpp)) |
| JUCE `UndoManager` | `beginNewTransaction()` | explicit; no timeout in the class ([docs](https://docs.juce.com/master/classUndoManager.html)) |

### 2.3 But the interesting part is what they do *in addition* to the timer

**Nobody who did this well used time alone.** This is the finding that should drive the design.

**ProseMirror** groups by time *and* locality, and says so in the option's own doc comment: "The delay between changes after which a new group should be started. Defaults to 500 (milliseconds). **Note that when changes aren't adjacent, a new group is always started.**" The predicate in the source is `history.prevTime < tr.time - options.newGroupDelay || !isAdjacentTo(tr, history.prevRanges)`. **CodeMirror** carries the same idea as a pluggable `joinToEvent`: "when close enough together in time, changes are joined into an existing undo event **if they touch any of the changed ranges** from that event."

**Tracktion is the most instructive, and it is a DAW.** Its 350 ms timer does not close the transaction on its own:

```cpp
void timerCallback() override
{
    if (edit.numUndoTransactionInhibitors > 0)
        return;

    if (! juce::Component::isMouseButtonDownAnywhere())
    {
        stopTimer();
        edit.getUndoManager().beginNewTransaction();
    }
}
```

The timer fires every 350 ms after a change, **and refuses to close a transaction while the mouse button is held anywhere**. It also has an explicit RAII escape hatch, `Edit::UndoTransactionInhibitor`, whose header carries the warning "long or you will ruin your undo chain." (`Edit::getDefaultNumUndoLevels()` returns 30.)

So Tracktion's real rule is: **gesture boundary primary, timer as the fallback for when no gesture boundary is observable.** The 350 ms number is not a granularity policy; it is a debounce on "the world went quiet."

### 2.4 The rule I'd adopt

> **A history entry closes on the earliest of: (a) an explicit gesture end, (b) a target-set change, or (c) 400 ms of silence. Explicit beats implicit, always.**

Concretely:

1. **Explicit boundaries are authoritative.** Pointer-up, key-up on a repeat-held key, dialog OK, drag-drop completion, MCP tool-call return. The UI knows when a gesture ended; it must *say so* rather than let a timer guess. This is `stopCapturing()` / `beginNewTransaction()` / `applyBatch` with a final non-`transient` op.
2. **A change of target set forces a break, even inside the timeout.** Dragging clip A then clip B is two entries even if they are 80 ms apart. This is ProseMirror's `isAdjacentTo` and CodeMirror's `joinToEvent`, transposed: for us, "adjacent" means *the same target entity set and the same op kind*. Without this, a fast user gets `Edit 6 things` and cannot undo one of them.
3. **The timer is the fallback only.** 400 ms sits between Tracktion's 350 and the web ecosystem's 500 — the differing constants suggest the exact value is not load-bearing, and it should be a tunable constant, not a magic number sprinkled through call sites.
4. **`transient: true` is the pressure valve.** The envelope already specifies it: intermediate values of a continuous gesture are "latest-wins, not journaled, not undoable on its own." A fader drag emits 200 transient ops so the engine and other windows track it live, then **one** committed op. History never sees the 200. This is the single most important existing decision and it should not be softened.
5. **Explicit inhibition scopes exist**, à la `UndoTransactionInhibitor`: a plugin-editor session, a batch import, a whole agent run (§6). One entry, opened and closed by code that knows the semantic boundary.

**The gesture types and their collapse:**

| Gesture | Op stream | Entry |
|---|---|---|
| Fader drag | ~200 × `param.set{transient}` + 1 committed | `Vocals: gain −3.2 dB` (from −1.0) |
| Drawing an automation curve | N transient point-inserts + 1 committed curve op | `Draw automation: Filter cutoff, bars 17–21 (34 points)` |
| Dragging 40 clips | 1 batch, 40 `clip.move` ops | `Move 40 clips +1 bar` |
| Typing a track name | keystrokes coalesced by the 400 ms timer | `Rename track: "Gtr" → "Gtr DI"` |
| Recording a take | one entry at stop | `Record: Vocals take 3 (0:32–1:04)` |
| Agent run | inhibitor spanning the run | `Agent: "make the chorus bigger" (14 changes)` |

### 2.5 Labelling: the system names, the user renames

**Design judgment: automatic labels are mandatory and must be good; user labels are an override, never a requirement.** Google Docs has it right — automatic grouping is the default, named versions are the exception. Requiring users to name things is how you get a history full of blank rows.

The auto-label is a pure function of the committed batch. Because ops are namespaced `<area>.<verb>` with a `target`, a small table generates: **verb + object + count + delta**.

```
Move 40 clips  +1 bar          ← verb, count, the *magnitude* of the change
Vocals: gain −3.2 dB           ← subject-first when a single track dominates
Delete 3 clips                 ← no delta to report
Draw automation: Cutoff        ← curve ops summarize to the parameter
Add track "Bass"
```

Three rules that make auto-labels not-annoying:

- **Report the delta, not the destination.** `gain −3.2 dB` beats `set gain to 0.68`. Users navigate history by remembering what they *did*.
- **Subject-first when one track dominates the batch**, count-first when many do. `Vocals: gain −3.2 dB` vs `Move 40 clips`.
- **Never say "Edit" or "Change".** If the label generator cannot do better than a generic verb for an op kind, that op kind's label is a bug to fix, not a fallback to ship.

The `label` field is capped at 128 chars in the schema — right call; it forces a summary rather than a manifest. The *manifest* is the hover card (§3).

User renaming follows Cubase's Details column: **inline-editable, optional, sparse.** In practice users will name maybe five rows in a session, and those five are the ones they navigate by. A renamed entry gets visual weight — a filled marker on the rail — because a user-named row is a declared landmark. This is also the natural place for the promotion in §5.3: naming an entry is how a transient history row becomes a durable one.

---

## 3. The "what changed here?" affordance

### 3.1 The three-tier disclosure

Hover, select, audition. Each tier costs more and is invoked more deliberately.

```
┌─ HISTORY ────────────────────────────────────┬─ hover card (≤50 ms, no state change) ─┐
│                                              │                                        │
│  ●  now                                      │  Move 40 clips  +1 bar                 │
│  │                                           │  14:32:07 · you · rev 1846             │
│  ├─ 14:33  Vocals: gain −3.2 dB              │                                        │
│  ├─ 14:32  Move 40 clips +1 bar        ◀hover│  Tracks   Drums, Bass, Gtr L, Gtr R     │
│  ├─ 14:31  Draw automation: Cutoff           │  Range    bar 33 → 65                  │
│  │    ⋮                                      │  ┌────────────────────────────────┐    │
│  ├─ 14:28  ★ "before the vocal comp"         │  │▓▓▓▓░░░░░░████████░░░░░░░░░░░░░░│    │
│  ├─ 14:22  ▣ Agent: "make chorus bigger"     │  │  was          now              │    │
│  │           14 changes · 6 tracks           │  └────────────────────────────────┘    │
│  ├─ 14:05  Record: Vocals take 3             │                                        │
│  └─ 13:58  Add track "Bass"                  │  [ ▶ hear before / after ]  [ show ]    │
└──────────────────────────────────────────────┴────────────────────────────────────────┘
```

**Tier 1 — hover (≤50 ms, nothing changes).** A card: label, timestamp, author, affected tracks, affected time range, and a thumbnail strip showing before/after extent. No state mutation, no playhead movement, no audio.

**Tier 2 — select (≤150 ms, non-destructive highlight).** The timeline itself shows the change *in place* without materializing the revision: affected clips outlined, the affected bar range shaded, ghost outlines at the old positions with a motion arrow to the new. Parameter changes surface on the track header as `−3.2 dB` with a small before/after bar. Selection **does not move the playhead and does not change the project.**

**Tier 3 — audition (§3.3).**

### 3.2 The visual diff: show extent, not a two-pane comparison

**Design judgment: a DAW does not want a side-by-side diff.** The arrangement is already a spatial view of the whole document; the right diff is an **overlay on the real timeline**, not a second timeline. Two-pane diffs work for text because text has no canonical spatial layout. A DAW arrangement does.

The overlay vocabulary:

- **Moved** — ghost outline at the old position, solid at the new, thin connector.
- **Added** — solid, brightened, with an "in" wedge on the left edge.
- **Removed** — outline only, hatched fill, at its old position.
- **Modified in place** (gain, fade, plugin param) — a badge on the clip or track header carrying the delta.
- **Unchanged** — dimmed to ~40%, so the eye lands on the change without the surrounding context vanishing.

The one place a genuine two-pane view earns its keep is **waveform-level change on a single clip** — a comped vocal, a rendered stem. There, the existing min/max LOD tile pyramid (`ARCHITECTURE.md` §2.5, `AWTF` wire format) lets you draw both versions stacked, plus a *difference* strip. That is cheap because tiles are already content-addressed per clip.

### 3.3 The audible diff — the part no other medium has

This is where the whole design either earns its keep or doesn't. **A DAW history entry is not legible until you have heard it.** Text diffs are read in a glance; a two-bar arrangement change takes four seconds of real time to perceive, and you must hear the *same* four seconds twice.

Three verified constraints shape the interaction:

1. **Echoic memory is 3–4 seconds.** ([Echoic memory](https://en.wikipedia.org/wiki/Echoic_memory): "retained for a short period of time, typically 3 to 4 seconds"; Baddeley's phonological store "3–4 seconds".) If the user must stop, reload, and restart to hear the other version, the comparison has already failed — the reference is gone before the comparison arrives.
2. **ABX methodology exists precisely to keep the reference in short-term memory.** ([ABX test](https://en.wikipedia.org/wiki/ABX_test): "samples A and B are provided just prior to sample X, the difference does not have to be discerned using long-term memory.") Hardware ABX boxes inserted "a fixed length dropout time when any change was made… selected to be **50 ms**" — a deliberate, audible-but-short mute that masks the switch discontinuity.
3. **Formal listening standards are built around a hidden reference**: ITU-R BS.1116, *"Methods for the subjective assessment of small impairments in audio systems"* ([ITU](https://www.itu.int/rec/R-REC-BS.1116/en)), and [MUSHRA](https://en.wikipedia.org/wiki/MUSHRA) with its hidden reference plus low/mid anchors.

#### The interaction: **Audition Revision**

Trigger: hold **A** on a selected history entry, or click *hear before/after*. Behaviour:

- The transport **loops the affected range**, auto-derived from the entry's change extent, padded by one bar on each side for musical context. If the change has no time extent (a master-bus gain), the loop is the current playhead's bar ± 2.
- Transport starts in **"after" (current) state**. Holding the compare key crossfades to **"before"** *at the same loop position, without stopping, without reloading, without moving the master playhead*.
- The switch is a **5 ms equal-power crossfade** between two pre-rendered buffers. (Deliberately not ABX's 50 ms dropout: that value comes from analogue relay switching. In-DAW we can crossfade sample-accurately, and 5 ms is inaudible without introducing a gap that itself becomes a cue.)
- Release the key and it returns to "after". **The user's playhead, loop points, solo/mute state, and selection are all restored on exit.** The audition is a modal overlay on the transport, not a transport command.

```
  Auditioning rev 1846 · "Move 40 clips +1 bar"
  ┌──────────────────────────────────────────────────────────┐
  │  bars 31 ┃················▓▓▓▓▓▓▓▓▓▓▓▓················┃ 67│
  │          ┃                  loop                       ┃  │
  │                                                           │
  │   [ hold A ] ──▶  BEFORE          ● AFTER                 │
  │                                                           │
  │   ⚖ level-matched  −0.4 dB applied to BEFORE              │
  │   ⏎ keep after   ⌫ restore before   Esc leave everything  │
  └──────────────────────────────────────────────────────────┘
```

**Why "hold" rather than "toggle".** A held key is a *momentary* state: the user's hand tells them which version they are hearing, so no glance at the screen is needed, and the default (release) is always the safe one — the current state.

**[confirmed by parallel research]** This is not just an ergonomic hunch — hold-to-audition is the **dominant idiom across shipping plugins from multiple vendors**, not a latching toggle. That is outside evidence for the choice, and it means users arrive already trained on it.

**Why pre-rendered.** The engine cannot materialize an arbitrary revision inside the audio callback. But it does not need to: `src-tauri/src/audio/offline.rs::render()` already renders an arbitrary `(start, frames)` range through *the real graph path* off the RT thread, in fixed blocks, with the discontinuity flag handled. Auditioning revision *R* over range *[a,b)* = materialize *R* off-thread → build a graph → `offline::render` → hand the resulting buffer to a **pre-loaded audition player node** installed by the existing pointer-swap. Two buffers, one crossfade parameter. The RT thread learns nothing about revisions. (Contract 12, §7.)

### 3.4 Regional audible diff — the "history brush" for audio

The Photoshop history brush transposes exactly, and it is the feature that would have no equivalent anywhere else: **select a time range and/or a track set, and restore *only that region* to a chosen revision.**

"The vocal was better three edits ago, but keep everything else." Today that is a manual reconstruction. With an op log it is a filter: take the ops between rev *R* and now, keep those whose `target` is outside the selection, invert those inside it, commit as one batch labelled `Restore Vocals, bars 33–65, from 14:22`.

**Design judgment: this is a v2 feature (§8), but the *data model* must permit it in v1** — which it does, for free, as long as every op carries a resolvable `target` and a derivable time extent. Do not ship an op kind without those.

---

## 4. A/B comparison UX

### 4.1 What the prior art establishes

**Verified — listening-test methodology.** ABX: double-blind, reference held immediately before the unknown, 10–25 trials recommended ("no more than 25 trials… as subject fatigue can set in"), 95% confidence ≈ 15/20 correct. MUSHRA: hidden reference plus deliberately-degraded anchors, so "minor artifacts are not unduly penalized." BS.1116 exists for exactly this class of judgment.

**Verified — the plugin A/B convention. [confirmed by parallel research]** The convention is real, near-universal, and — importantly — **grouped with undo/redo rather than with the preset browser**. FabFilter documents it house-wide under `/help/<plugin>/using/undoredo`, titled "Undo, redo, A/B switch" ([e.g. Pro-Q](https://www.fabfilter.com/help/pro-q/using/undoredo)):

- Exactly **two** state slots.
- "**The A/B button switches from A to B and back.**"
- "**The Copy button copies the active state to the inactive state.**" — a single, direction-implicit Copy.

TDR Nova words the same feature differently and exposes the direction explicitly: "**A/B allows comparison of two alternative control settings. A>B and B<A copies one state over the other.**"

**Design judgment on that difference:** FabFilter's single `Copy` is better. The two-button `A>B` / `B<A` pair forces the user to parse a direction at the exact moment they are thinking about sound, and the failure mode (copying the wrong way and destroying the state you wanted to keep) is silent and unrecoverable within the plugin. "Copy the active state to the inactive one" is always the intended operation — the active state is by definition the one you want to branch from. **Adopt FabFilter's single-Copy semantics.**

That A/B lives next to undo/redo rather than next to presets is a structural hint worth taking seriously: **A/B is a history feature, not a preset feature.** It is the two-slot degenerate case of the branch model in §5.2, which is another argument for §4.3's "variants and history branches are the same mechanism."

**Verified — gain matching, and it is deliberately cheaper than assumed. [confirmed by parallel research]** This corrected a genuine mistake in the first draft. No shipped implementation does measured adaptive loudness matching in the audio path:

- FabFilter Pro-Q's **Auto Gain** "automatically compensates for increase or loss of gain after EQing", but the docs are explicit that it is "**_not_ a dynamic process based on actually measured levels**" — it is "**an educated guess based on the current EQ settings**."
- FabFilter Pro-L's **Unity Gain** is pure arithmetic: output trim = −(input gain).
- TDR Nova's equal-loudness feature is a **readout** that tells you where to put the knob — advisory, not an override.

**This is strong precedent for starting simpler than contract 15 specifies.** See §4.2(c) and the note on contract 15 in §7.

**Verified — delta listen, and the naming trap is real. [confirmed by parallel research]** The cleanest definition found is FabFilter Pro-L 2's **Audition Limiting**: it "**subtracts the processed output from the input audio to audition the 'delta' signal**." But FabFilter Pro-MB's **Audition** is *not* a delta — it auditions the **sidechain trigger signal**. Two different features, the same word, the same vendor.

**Design judgment: do not use the word "Audition" for the difference signal.** It is already ambiguous in the wild, and we are also using "audition" for §3.3's revision playback, which would make it triply overloaded. Call the difference signal **Delta** (`Δ`), and reserve "audition" for "play me this revision."

**Still UNVERIFIED.** Cubase **Track Versions** and Studio One **Scratch Pad** were only partially confirmed by parallel research and remain to be checked. Do not cite them in a spec without verification.

**One claim to state honestly.** "Louder is perceived as better" has no clean citation. [Loudness war](https://en.wikipedia.org/wiki/Loudness_war) actually records the industry *assuming* listeners preferred louder masters "even though that may not have been true", and cites Shepherd's research finding "no connection between sales and loudness." So the defensible framing is not "louder wins because psychoacoustics." It is: **an uncontrolled level difference is a confound, and every formal listening standard eliminates it.** That argument is sufficient.

### 4.2 The design

**Four surfaces, one mental model: the compare key is always momentary and always level-matched.**

**(a) Per-track variant switch — the primary object.** A variant is a named alternative for one track: different clips, different plugin chain, different automation. It lives on the track header.

```
┌─ TRACKS ─────────────────────────────────────────────────┐
│ ▸ Drums     [M][S]  ◂ A ▸                                │
│ ▸ Bass      [M][S]  ◂ A ▸                                │
│ ▾ Vocals    [M][S]  ◂ B ▸  A: comp v1                    │
│                            B: comp v2 ✓                  │
│                            C: doubled                    │
│                            + new variant from current    │
│ ▸ Gtr L     [M][S]  ◂ A ▸                                │
└──────────────────────────────────────────────────────────┘
```

Switching is instant and glitch-free **during playback** — this is the requirement that makes the feature usable at all, and it is why variants must be pre-compiled into the graph rather than compiled on switch (Contract 13).

**Logic Pro Track Alternatives is the closest shipping model, and it settles two open questions. [confirmed by parallel research]** Apple's docs: "Each alternative can contain different regions or arrangements, **while sharing the same channel strip and plug-ins**." Default naming is **alphabetical — A, B, C** (which is why the sketch above uses those letters; it matches what users already know).

The detail worth designing around is that **audition and commit are separate operations**:

- An **On/Off button** on an *inactive* alternative makes it "**audible when you play the project, in place of the active alternative**" — i.e. temporary audition without changing which alternative is active.
- A separate **upward-pointing arrow** *exchanges* it with the active one — i.e. commit.

**Design judgment: adopt this split verbatim.** It is exactly the "hold to compare, then decide" model from §3.3, expressed as persistent UI rather than a held key, and it means the comparison never changes project state as a side effect of listening. Our version: the `◂ A ▸` stepper *auditions* (temporary, reverts on release/blur), and an explicit **Make active** action commits. Two affordances, never one.

The contrast is instructive: Logic's *Project* Alternatives (whole-project, not per-track) **prompts a Save dialog on switch**, which makes it unusable as a comparison loop. That is direct evidence for the position taken here — **variants are per-track, not per-project.** A whole-project alternative cannot be A/B'd in real time, and anything that cannot be A/B'd in real time is not a comparison feature.

**(b) Global compare key — the momentary A/B.** Hold **`C`**: every track with a non-default variant snaps to its A state; release, and it returns. One key answers "what did all my changes actually buy me?" Held, not toggled, for the reason in §3.3 — and now with the vendor-idiom evidence behind it.

**(c) Level-matched by default, with the number shown.** When entering any comparison, the system applies a compensating trim to the quieter side and displays it (`⚖ −0.4 dB applied to A`). The user can defeat it. **Design judgment: default ON, defeat visible.** A silent auto-gain is worse than none, because the user cannot reason about what they are hearing.

**On how the number is computed — revised in light of the FabFilter/TDR evidence.** The first draft specified measured integrated loudness (LUFS-I) over the comparison region. Every shipping implementation is cheaper than that, and deliberately so: Pro-Q's "educated guess based on the current EQ settings", Pro-L's arithmetic `−gain`, Nova's advisory readout. Two consequences:

- **v1 should use derived compensation where the op log already knows the answer.** If the only difference between A and B is a gain op, the trim is arithmetic and exact — no measurement, no latency, no cache invalidation. This covers a large fraction of real comparisons.
- **Measured LUFS-I is the fallback for structural differences** (different clips, different plugin chains) where arithmetic cannot work — and it is the thing to defer if v1 is tight. Contract 15 is written to allow both; see the note there.

That REAPER ships actions to "Calculate peak volume and loudness (LUFS-I) for media" (per its user guide) confirms the measurement is a normal offline job when it *is* needed.

**(d) Blind mode — an explicit, opt-in ritual.** A `?` button hides which side is playing, randomizes assignment per switch, and keeps score across trials, reporting "you picked B in 7 of 10 trials." Following ABX practice, cap it around 20 trials and surface fatigue. This is not everyday UI; it is for the moment someone says "I honestly cannot tell." Its value is as much in *permitting the conclusion "there is no difference"* as in establishing one.

**(e) Comparison across a loop region — the default frame.** All comparison happens inside a loop. Comparing whole songs is not a thing anyone does; comparing eight bars, repeatedly, is.

```
┌─ COMPARE ────────────────────────────────────────────────────────┐
│                                                                  │
│   A: "comp v1"                    B: "comp v2"        [ ⇄ swap ] │
│   ┏━━━━━━━━━━━━━━━┓               ┌───────────────┐              │
│   ┃  ●  playing   ┃               │               │              │
│   ┗━━━━━━━━━━━━━━━┛               └───────────────┘              │
│                                                                  │
│   loop  bars 33–41    ⚖ level-matched (−0.4 dB → B)   ? blind    │
│   [ hold C to compare ]   [ Δ listen to difference ]             │
│                                                                  │
│   picked B: 7/10 trials                                          │
└──────────────────────────────────────────────────────────────────┘
```

**(f) Delta listen — the mastering-desk trick.** A `Δ` button plays *the difference signal* (B − A, sample-aligned, phase-inverted sum) — the operation FabFilter's Pro-L 2 documents as subtracting processed output from input. For small changes this is the single most informative thing you can hear: it isolates exactly what the edit did and nothing else. It is only meaningful when both sides are sample-aligned — true for parameter and plugin changes, false for arrangement changes where clips moved. **The button must disable itself, with a reason, when the two revisions are not time-aligned.** Offering a meaningless delta is worse than not offering one.

### 4.3 What "variant" must mean

**Design judgment: a variant is a named branch of the op log scoped to one track's subtree, not a snapshot of that track.** Snapshot semantics ("copy of the track as it was") break the moment the user changes tempo, or the song structure, or a send destination — the variant silently diverges from the project around it. Branch semantics mean a variant *inherits* everything not explicitly overridden.

Logic's "**sharing the same channel strip and plug-ins**" is exactly this inheritance, drawn at a specific boundary: alternatives vary *content* (regions, arrangement) and share *processing*. That is a defensible default and a good v1 scope — but it should be a default, not a hard rule, because "try this with a different compressor" is a real comparison. The op-log branch model gives us the general case for free; Logic's boundary is what we should *default* to.

The consequence for §5's branching rule: **variants and history branches are the same mechanism with different scope.** Do not build two. (And per §4.1, the plugin A/B convention is the same mechanism again, with the scope set to "one plugin's parameters" and the branch count fixed at two.)

---

## 5. Making it non-scary

### 5.1 The failure mode, precisely

Every history feature in §1 that users fear has the same shape: **an action whose cost is invisible at the moment of commitment.** Photoshop dims the states it is about to delete; Blender says "truncated at that point"; both destroy work as a side effect of an action the user thinks of as "continuing to work."

The conventions that make it feel safe, all observed:

- **Non-destructive restore** (Figma: restore "is a non-destructive action", adding two checkpoints rather than removing any).
- **Persistence past the session** (REAPER's `.RPP-UNDO`: "Even at some later date, you will still be able to revert"). Its inverse is Blender's "lost… even if you save your file", which is the strongest possible argument *for* persistence.
- **Reversibility made visible** (Cubase's separator, with the undone rows still on screen).
- **User-authored landmarks** (Cubase's Details, Figma's named versions).
- **Bounded by size, not by count** — already this repo's stated position: "bounded by size, not count" (`SCALABILITY.md` §4).

### 5.2 The rule: **branch silently, surface loudly**

> **Editing after travelling back never discards anything. It creates a branch, automatically, with no dialog. The branch is then made unmissable in the UI for as long as it is young.**

The three candidate rules and why the other two lose:

**Discard the future** (Photoshop default, Blender) — rejected outright. It makes exploration cost work, so users stop exploring, which defeats the feature's entire purpose. The one thing a browsable history is *for* is making "let me see what that was like" free.

**Ask** — rejected, and this is the interesting one. A modal at the moment of the first edit after travelling back is exactly the wrong moment: the user is mid-thought, in a creative flow, and the question ("branch or overwrite?") demands a model of the system they do not have. Worse, it trains dismissal — after the fifth time, the button gets clicked without reading. **A confirmation dialog is a design smell wherever the safe answer is knowable in advance.** Here it is knowable: branch.

**Branch silently** — chosen. REAPER already proves the data model is affordable for *full project states*; with an op log it is nearly free (a new head pointer). Figma proves the semantics are acceptable to users. And the op log makes it strictly cheaper than either: a branch is a second child edge in the rev DAG.

### 5.3 How the branch is surfaced

Silent creation, loud presentation. Three mechanisms, in descending urgency:

1. **A toast at the moment of branching**, with the only two actions that matter: `Branched from 14:28. [go back to the other path] [name this branch]`. It is a toast, not a modal — it does not block, and ignoring it is the correct default.
2. **The rail forks, visibly, in the history panel**, and the abandoned path stays drawn — dimmed, with its own head marker and its label. Not a `(*2)` footnote. This is REAPER's model with the affordance actually built.
3. **Merge-back is de-emphasized**, per Tonsky: returning to the mainline is "not an effort per se". Draw it thin.

```
┌─ HISTORY ──────────────────────────────────────────────────┐
│                                                            │
│   ●  now  ·  "brighter chorus"                             │
│   │                                                        │
│   ├─ 14:41  Gtr L: +2.5 dB @ 4k                            │
│   ├─ 14:39  Add clip "shaker"                              │
│   │                                                        │
│   ├─ 14:28  ★ before the vocal comp  ◀ branched here       │
│   │  ╲                                                     │
│   │   ╲    ┈┈ 14:35  Vocals: comp v1        (abandoned)    │
│   │    ╲   ┈┈ 14:33  Delete 3 clips                        │
│   │     ●  ┈┈ head · 3 entries · [switch] [delete branch]  │
│   │                                                        │
│   ├─ 14:22  ▣ Agent: "make chorus bigger"                  │
│   └─ 14:05  Record: Vocals take 3                          │
└────────────────────────────────────────────────────────────┘
```

**Design judgment on legibility, following §1's DAG lesson:** the active path is always drawn as a straight vertical mainline; every other path is a stub hanging off it, collapsed to one row showing its head and its length. **The user never sees a general graph.** Expanding a stub *switches* the mainline to it — the branch you are on is always the straight one. This is the one non-negotiable rendering rule; every git GUI that violated it produced the spaghetti that made branching history a byword for confusion.

Branch pruning: unnamed branches with no activity for N days, or beyond a total size budget, get garbage-collected oldest-first — the same size-bounded policy `SCALABILITY.md` §4 already sets for undo. Named branches are never auto-pruned. **Naming is the user's signal that something matters, and it is the only signal the system should need.**

### 5.4 Restore is additive

Following Figma exactly: **"restore revision R" never rewinds the head.** It appends a new revision whose content is R, labelled `Restored: "before the vocal comp" (14:28)`. The state you were in a moment ago is still the row directly beneath. Undo undoes the restore. There is no way to lose the present by visiting the past.

### 5.5 The Figma invariant, stated as a test

> "If you undo a lot, copy something, and redo back to the present, the document should not change."

This is not a nice-to-have; it is a **regression test that must exist in the suite before the history panel ships.** Its DAW form:

```
1. Perform 20 edits.
2. Undo 15.
3. Copy a clip to the clipboard.          ← non-mutating, must not branch
4. Redo 15.
5. Assert: project state at rev 20 is byte-identical to step 1's rev 20.
6. Assert: no branch was created.
```

Step 3 is the whole point, and it is where a naïve implementation fails: if *any* non-mutating action (copy, select, solo, scroll, opening a plugin window) records an op, step 4 becomes a branch and the user silently loses their redo path. Note that REAPER ships this exact hazard as a *preference* — "whether to include item, track, envelope point and/or time selection and/or cursor positions changes in the undo history."

**Design judgment: do not make it a preference. Decide.** Selection, playhead position, solo/mute-for-monitoring, view scroll, and window state are **not** project mutations and never enter the op log. They belong in a separate, non-undoable view-state channel. If a user wants "undo my selection", the answer is no — and the reward is that redo is trustworthy, which is worth far more.

(There is a real edge case: solo and mute are genuinely ambiguous — sometimes a monitoring gesture, sometimes a mix decision. Resolve it by treating **mute as project state** and **solo as view state**, which matches how the two are used and how they persist in every DAW I know of.)

---

## 6. The AI-agent case

### 6.1 What already exists here

This repo has an MCP front door (`ARCHITECTURE.md` §12, `docs/mcp-usage.md`) with a per-call confirmation dialog and an activity feed — `McpFeedEntry { tool, summary, at, resolution: "requested" | "approved" | "denied" | "expired" }` in `src/lib/state/mcp.svelte.ts`, plus a pending-confirmation queue. Both front doors go through one `ControlPlane` (§11), so agent changes and human changes are the same ops.

That gives **pre-execution, per-call** review. What is missing is **post-hoc, per-run** review — and the per-call dialog actively works against it, because an agent making 14 changes produces 14 dialogs, which trains the user to set policy to "allow" and stop reading. **Design judgment: per-call confirmation and per-run review are substitutes, not complements. As the run-level review gets good, the per-call dialog should retreat to genuinely destructive, irreversible operations only** (deleting audio files, overwriting a project) — the ones the op log cannot undo.

### 6.2 What the coding tools do

All verified:

- **Zed's agent panel** shows "which files, how many of them, and how many lines have been edited" in an accordion above the composer; `Ctrl+Shift+R` opens "a special multi-buffer tab with all changes"; "You can accept or reject each individual change hunk, **or the whole set of changes made by the agent**." ([docs](https://zed.dev/docs/ai/agent-panel))
- **Cursor** checkpoints "save snapshots of your codebase during an Agent session", created "automatically… before making significant changes"; restoring reverts **files only, not the chat**; explicitly "stored locally and separate from Git." ([docs](https://cursor.com/docs/agent/chat/checkpoints))
- **VS Code agent mode**: per-edit **Keep** / **Undo** overlays, a **Changes** panel grouped by file, and "a snapshot of affected files before processing each request"; restoring "removes subsequent requests from the conversation history and restores the workspace files." ([docs](https://code.visualstudio.com/docs/agents/run/review-code-edits))
- **Claude Code**: "every user prompt creates a new checkpoint", 100 retained per session, persisted with the conversation. `/rewind` offers **Restore code and conversation**, **Restore conversation** (keep code), **Restore code** (keep conversation) as separate choices. Critically documented limitations: "**Checkpointing does not track files modified by bash commands**", and subagent edits are usually not captured. ([docs](https://code.claude.com/docs/en/checkpointing))

### 6.3 What transfers

**The reviewable unit is the run, not the change.** All four converge on this. One prompt → one checkpoint → one accept/reject unit, with per-item override available but not required. Our version: **an agent run is one collapsed history entry with N children.** The `UndoTransactionInhibitor` pattern (§2.4) spans the run.

**Two axes of undo, separated.** Claude Code's three-way choice (code / conversation / both) is the sharpest idea in the set. Our analogue: **revert the changes but keep the conversation** (so you can say "no, warmer") versus **revert both**. These must be separate menu items, not one button.

**Summary-first, detail-on-demand.** Zed's accordion — *how many files, how many lines* — before any diff. Ours: *how many tracks, how many bars, how much of the song*.

**State the limits in the UI, as Claude Code's docs do.** Anything the agent did that the op log did not capture — a sidecar job that wrote a `.wav`, a rendered stem, an imported file — **cannot be reverted by rewinding revisions.** This must be visible in the run card, not buried in docs. It is the exact analogue of "bash command changes not tracked", and it is the most likely source of a user's worst day with this feature.

### 6.4 What does NOT transfer — and this is the important part

**1. There is no "hunk" in audio.** A code diff decomposes into independent hunks because text lines are independent. `Vocals: −3 dB` and `add compressor to Vocals` are *not* independent — accepting one and rejecting the other yields a mix the agent never proposed and never evaluated. **Per-item accept must default to off**, with granularity at the level of *coherent sub-goals* the agent declares, not individual ops.

This has an architectural consequence: **the agent should segment its own run.** An MCP run carries an optional structure — "1. rebalanced drums (4 ops), 2. widened guitars (3 ops), 3. automated the chorus lift (7 ops)" — and the user accepts or rejects *those*. Without agent-declared segments, run-level accept/reject is the only honest granularity.

**2. Reading a diff is O(1) in time; hearing one is O(duration).** A 40-line code diff is scanned in fifteen seconds. A change spanning a 3-minute song takes 3 minutes to hear, and you need the before as well — six minutes, minimum, for one review. **This is the fundamental asymmetry, and it means the review UI's primary job is not showing changes but making the audition cheap.** Hence: the run card's main affordance is not a diff list, it is **"play me the difference, in the affected region, level-matched."** Everything else is secondary.

**3. The artifact has no canonical reading order.** Code is reviewed top-to-bottom. A mix has no such order — you review it by *time region* (does the chorus work?) and by *frequency/instrument* (is the bass too loud?), and those are orthogonal. The run card should offer both entry points: a timeline strip showing where in the song the agent worked, and a track list showing what it touched. **UNVERIFIED whether any shipping tool does this**; it appears to be genuinely novel and it falls directly out of the medium.

**4. "Show me what it did" means something different.** In code it means the diff. In audio it means a **narrated audition**: play bars 33–41 as it was, then as it is, while the panel highlights which track changed. This is closer to a director's commentary than a review pane.

**5. Correctness is not the question.** Code review asks "is this right?" — a question with an answer. Mix review asks "do I prefer this?" — which is why §4's level-matching and blind-mode machinery matters far more here than any diff rendering. An agent's change that is 0.8 dB louder will "sound better" and the user will accept it for the wrong reason. **The agent-run audition must be level-matched by default, and the trim must be displayed.** This is the strongest single argument in the document for building §4 before §6.

### 6.5 The run card

```
┌─ HISTORY ────────────────────────────────────────────────────────┐
│                                                                  │
│  ▼ ▣ 14:22  Agent · "make the chorus bigger"          [pending]  │
│    ┌────────────────────────────────────────────────────────┐   │
│    │  14 changes · 6 tracks · bars 33–65 (0:58–1:52)        │   │
│    │                                                         │   │
│    │  song ▏░░░░░░░░░░▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░▕         │   │
│    │                  └ where it worked                      │   │
│    │                                                         │   │
│    │  ▸ 1. Rebalanced drums              4 changes    ✓ ✕   │   │
│    │  ▸ 2. Widened guitars                3 changes    ✓ ✕   │   │
│    │  ▸ 3. Automated the chorus lift      7 changes    ✓ ✕   │   │
│    │                                                         │   │
│    │  ⚠ also rendered "chorus-stem.wav" — not revertible     │   │
│    │                                                         │   │
│    │  [ ▶ hear before/after ]  ⚖ level-matched (−0.7 dB)     │   │
│    │  [ keep all ]  [ revert all ]  [ revert, keep chat ]    │   │
│    └────────────────────────────────────────────────────────┘   │
│                                                                  │
│  ├─ 14:05  Record: Vocals take 3                                 │
└──────────────────────────────────────────────────────────────────┘
```

Collapsed by default to one row. Expanded on demand. **Colored by author** (Tonsky's rule) — agent runs get a distinct marker `▣` and tint, so scanning the rail answers "what did I do vs. what did it do" without reading.

**Pending vs. applied.** Design judgment: **agent changes apply immediately and are reverted on rejection, rather than staging as pending.** Staging is right for code because you can read a diff without running it. In audio you *cannot evaluate without hearing*, and you cannot hear without applying. So: apply, mark the run "unreviewed", make revert one keystroke, and never auto-clear the unreviewed flag. The run card stays visually distinct until the user has explicitly kept or reverted it.

---

## 7. What the UI demands of the architecture

These are the contracts. Each is stated so it can be tested.

**Listing and navigation**

1. **`history_list(from_rev, count, filter?) -> [HistoryEntry]` must not materialize any project state.** Listing 500 entries reads 500 metadata records. An implementation that reconstructs states to produce labels is disqualified. Budget: **≤30 ms for 500 entries**, cold.

2. **Every committed revision persists a precomputed `HistoryEntry` at commit time**, not at query time. Shape:
   ```
   { rev, parentRev, timestamp, origin, author: "user"|"agent"|"system",
     label,                       // ≤128 chars, per op-envelope.schema.json
     userLabel?,                  // user rename; presence = pinned landmark
     targets: [entityId],         // deduped, capped at N with overflow count
     trackIds: [id],              // deduped
     timeExtent: {startSamples, endSamples} | null,
     opCount, byteSize,
     runId?, runSegment?,         // agent-run grouping (§6)
     sideEffects: [{kind, path}]  // non-revertible: rendered files, imports
   }
   ```
   Computing this is a pure function of the batch and must run in the commit path. **If it cannot be derived from the batch, the op kind is under-specified** — treat that as a schema bug.

3. **`timeExtent` and `targets` are mandatory for every op kind.** No op may be added to the protocol without a rule for deriving both. This single requirement is what makes §3's hover card, §3.3's audition range, §3.4's regional restore, and §6's "where it worked" strip all possible; without it, every one of them degrades to "something changed somewhere."

4. **Hover produces a change summary in ≤50 ms, from cache, with no IPC round-trip on the common path.** The entry list already carries everything the hover card shows. Implication: `history_list` returns full `HistoryEntry` records, not ids to be hydrated. At ~200 bytes/entry, 500 entries is ~100 KB — one payload, cached in the store.

5. **The history rail must virtualize.** Sessions reach thousands of entries; per `SCALABILITY.md` §5, "no Svelte component may iterate all tracks per frame" — the same rule binds here. Render the visible window only.

**Diffing**

6. **`revision_diff(revA, revB) -> ChangeSet` in ≤150 ms for adjacent revisions**, ≤500 ms for arbitrary pairs within a session. `ChangeSet` is a *presentation* structure — added/removed/moved/modified entities with old and new values — not an op list. The UI must never be asked to interpret raw ops to draw a highlight.

7. **Diff between adjacent revisions is O(batch), not O(project).** It is a read of the stored batch plus its inverses. Only cross-branch or distant-pair diffs may fall back to materialize-and-compare, and those must be async with a progress affordance.

8. **Selecting a history entry must not mutate the project, the playhead, the transport, or the graph.** Selection is view state (§5.5), lives in the frontend, and emits no ops. This is testable: select every entry in a 500-entry history and assert `rev` is unchanged.

**Materialization and audition**

9. **`materialize(rev) -> ProjectState` off the control thread, ≤200 ms for a rev within the current session's journal.** Implementation: nearest snapshot + forward replay. Requires **periodic snapshots** in the journal — the autosave snapshot at 2–5 min already specified in `SCALABILITY.md` §4 gives roughly this, but history browsing wants a tighter cadence or an in-memory snapshot ring (every ~50 revisions).

10. **Materializing a revision must never touch the live graph.** It produces a detached state object. The live graph changes only via the existing prepare-then-pointer-swap path (`ARCHITECTURE.md` §2.3). **No exceptions**, including for audition.

11. **Auditioning a revision must not disturb the current playhead, loop points, transport state, or selection.** Audition is a *modal overlay* over the transport with its own position, entered and exited atomically, restoring all four on exit. Note the interaction with the park handshake (§2.6): audition exit must clear any pending park, since "a fresh instruction supersedes an owed one."

12. **Audition audio is pre-rendered off-thread through the real graph path.** `offline::render(graph, params, start, frames, rate, master_gain, on_progress)` already does exactly this. Audition = materialize → build graph → render range → install a preallocated audition player by pointer swap. **Budget: ≤400 ms from key-press to audible for a 4-bar range** (≈8 s of stereo audio ≈ 1.4 MB — comfortably achievable; the cost is materialization plus graph build, not the render).

13. **Variant switching must be glitch-free during playback**, which means variants are **pre-compiled into the live graph** with an active-selector parameter, not compiled on switch. A variant switch is therefore a `param.set`-class operation on the RT thread (an atomic index + a crossfade ramp), never a graph rebuild. **This is the single hardest requirement in the document and it constrains the graph compiler: the compiler must emit all variants of a track as parallel subgraphs behind a selector node.** Consequence: variants cost CPU and memory even when inactive. Mitigation — cap the number of *live* variants per track (2 is enough for A/B, and matches the two-slot plugin convention confirmed in §4.1; the rest are cold and require a rebuild to activate, which is honest and can be shown in the UI). Logic's "sharing the same channel strip and plug-ins" (§4.2) is the cheap default: if variants vary content but share processing, only the source subgraph needs duplicating.

14. **A/B switching is sample-accurate with a 5 ms equal-power crossfade**, executed on the RT thread from a preallocated ramp. The switch command is POD and travels the existing `engine_cmd` queue. No allocation, no lock, no graph rebuild.

15. **Level matching is computed off-thread and applied as a gain parameter.** **Two tiers, and v1 needs only the first:**
    - **(a) Derived compensation — exact, free, no measurement.** When the difference between the two sides is expressible as gain ops the op log already holds, the trim is arithmetic. This is the tier every shipping plugin implements (FabFilter Pro-L's Unity Gain is literally `−gain`; Pro-Q's Auto Gain is "an educated guess based on the current EQ settings", explicitly "_not_ a dynamic process based on actually measured levels" — see §4.1). No latency, no cache, no invalidation problem.
    - **(b) Measured LUFS-I — the fallback for structural differences** (different clips, different plugin chains) where arithmetic cannot work. Measured from the same offline render used for audition. **Budget: ≤1 s for an 8-bar region**, cached per comparison region, recomputed when the region changes, invalidated when either side changes.

    **No shipped implementation does (b) adaptively in the audio path.** Starting with (a) alone is well-precedented and materially cheaper. In both tiers the applied trim must be **displayed as a number**.

16. **Delta listen requires sample alignment**, and the backend must report whether two revisions are time-aligned so the UI can disable the affordance with a reason. A change that moved clips is not alignable; a change that altered a plugin parameter is. Naming: the feature is **Delta (`Δ`)**, never "Audition" — see §4.1 on FabFilter's own collision between Pro-L 2's delta-signal "Audition Limiting" and Pro-MB's sidechain-signal "Audition".

**Branching**

17. **A branch is a second child edge in the rev DAG, created without user confirmation, costing O(1).** `history_branch_at(rev)` returns a new head. No state copy. If branching costs more than a pointer, the §5.2 rule is unaffordable and the design collapses back to Photoshop's.

18. **`history_branches() -> [BranchSummary]` — head rev, entry count, byte size, last-activity timestamp, name.** Drives the collapsed stubs. Must be O(branches), not O(entries).

19. **History and branches persist across sessions and survive a crash**, per `SCALABILITY.md` §4's journal design (`journal.ndjson`, fsync on 500 ms idle / 5 s max). REAPER's `.RPP-UNDO` is the precedent; Blender's "lost even if you save your file" is the anti-precedent. Bounded **by size, not count**, with named branches and named entries exempt from pruning.

20. **View state never enters the op log.** Selection, playhead, scroll, zoom, solo, window layout travel a separate non-undoable channel. This is contract-level, not stylistic: §5.5's regression test fails without it.

**Agent runs**

21. **A run of agent changes is one `runId` spanning N revisions**, opened and closed by the MCP front door around a tool sequence, with optional agent-declared `runSegment` labels. `ControlPlane` owns the run scope, so both front doors observe it identically.

22. **`history_revert_run(runId)` commits an inverse batch as a *new* revision** — additive, per §5.4 — and returns the list of side effects it could **not** revert (rendered files, imports, sidecar outputs). The UI must display these. Silence here is the failure mode Claude Code's docs warn about with untracked bash edits.

23. **The op log must record `origin` for every batch** — already in the envelope as "opaque id of the submitting client/window/session" — and the control plane must map it to a stable `author` classification (`user` / `agent` / `system`). Coloring by author (§1, §6.5) depends on this being trustworthy, not inferred.

**Cross-cutting**

24. **Every op kind must be invertible from its stored record alone**, per `SCALABILITY.md` §4's `SetTrackGain{track, old, new}` pattern. An op that requires the current state to invert breaks branch switching, because the current state is not the state the op was applied to.

25. **Bulk payloads use copy-on-write so deep history costs O(changed chunks), not O(project).** Already specified for pattern event ropes. Extend the rule: **any structure that can exceed 10k elements and appears in an op payload must be COW or chunk-referenced**, never inlined. Otherwise a 500-entry history over a million-note project is measured in gigabytes.

26. **`ops_subscribe` streams committed batches in strict rev order to every window**, so the history panel is a projection of the same stream that drives the timeline. **The history panel must never poll and must never refetch the full list after a mutation** — it appends. This is `SCALABILITY.md` §5's delta-ready store rule applied to history.

---

## 8. Scope discipline

### 8.1 A genuinely useful v1

The smallest thing that changes how people work:

1. **A gesture-granular history list** with automatic labels, virtualized, newest-first, keyboard-navigable (Live's arrow-keys-plus-Enter, which is cheap and disproportionately good).
2. **Click to travel; travel is non-destructive.** Restore is additive (Figma's rule).
3. **Silent branching on edit-after-travel**, with the fork drawn on the rail and a toast. Even v1 must have this — retrofitting non-destructive branching onto a shipped destructive model means breaking users' mental model twice.
4. **The hover card**: label, time, author, affected tracks, affected bar range. No visual diff yet.
5. **Timeline highlight on select** — affected clips outlined, affected range shaded. This alone answers "what changed here?" for the majority of edits, at a fraction of the cost of a real visual diff.
6. **Audition a revision**: hold to hear before/after over the auto-derived affected range, level-matched, playhead restored on exit. **This is the differentiating feature and it must be in v1** — without it the panel is a worse version of what five other DAWs already ship.
7. **Per-track variants with A/B and a global momentary compare key**, limited to 2 live variants per track, with **audition and commit as separate affordances** (Logic's model, §4.2).
8. **Level matching by derived compensation only** (contract 15 tier (a)) — arithmetic, exact, displayed. Measured LUFS-I deferred.
9. **Agent runs as one collapsed entry**, with `revert run` and `revert run, keep conversation`, and an explicit list of non-revertible side effects.
10. **User rename on any entry** (Cubase's Details, minus the other three columns).
11. **Persistence across sessions**, size-bounded.

### 8.2 Defer, with reasons

- **Regional / selective undo (the audio history brush).** The best idea in the document and the one most likely to sink v1. It needs the op-target model to be provably complete and it needs a spatial selection UI. Ship the *data model* that permits it (contract 3); ship the feature later.
- **Measured LUFS-I level matching** (contract 15 tier (b)). No shipped plugin does adaptive measured matching; derived compensation covers the common cases. Add it when structural A/B comparisons become common.
- **Blind A/B with trial scoring.** Delightful, rarely used, entirely separable. The level-matching machinery it depends on is in v1 anyway.
- **Delta listen.** Needs alignment detection to be trustworthy or it misleads. v1.1.
- **Full visual diff of the arrangement** (ghost outlines, motion arrows, waveform two-pane). The v1 highlight covers most of the value; the full treatment is a large rendering project.
- **More than 2 live variants per track.** Graph cost is real (contract 13). Two covers A/B, which is the actual workflow and the shipped plugin convention.
- **Variants that differ in processing, not just content.** Logic's boundary (share the channel strip, vary the regions) is the cheap default and covers most use. Varying the plugin chain per variant multiplies the graph cost.
- **Cross-branch diff and merge.** Merging two branches of a DAW project is a genuinely unsolved problem — there is no analogue of a three-way text merge for "both branches edited the chorus." Do not attempt it. **Switch between branches; never merge them.** If users need to combine, the mechanism is regional restore (§3.4), which is a copy, not a merge.
- **Collaborative multi-user history.** The op log is built for it, per §5's rev/`baseRev` design, but the interaction design for concurrent history is a separate document. Note that Figma's undo principle is *specifically* about the multiplayer case — so the v1 invariant test (§5.5) is also the down-payment on this.
- **History search / filter by track or type.** Cheap to add once entries carry `trackIds` and `targets`. Not v1, but the data is there from day one.
- **Per-call MCP confirmation as the primary agent review.** It already exists; **shrink it** rather than growing it, as run-level review lands.

### 8.3 The one thing not to compromise

If v1 must be cut further, cut the visuals, not the audio. **A history panel that lists changes is a commodity; a history panel that lets you hear the difference, level-matched, without losing your place, is the reason to build this at all.** Photoshop's History panel is beloved because the medium makes the diff instantaneous — you *see* the old state. Audio does not grant that for free, and the entire design problem is buying it back. Contracts 11, 12, 14, and 15 are the ones that buy it.

---

## Sources

**Undo granularity and coalescing**
- [Yjs UndoManager](https://docs.yjs.dev/api/undo-manager) · [prosemirror-history source](https://github.com/ProseMirror/prosemirror-history/blob/master/src/history.ts) · [@codemirror/commands history source](https://github.com/codemirror/commands/blob/main/src/history.ts) · [JUCE UndoManager](https://docs.juce.com/master/classUndoManager.html) · [tracktion_Edit.cpp](https://github.com/Tracktion/tracktion_engine/blob/master/modules/tracktion_engine/model/edit/tracktion_Edit.cpp) / [tracktion_Edit.h](https://github.com/Tracktion/tracktion_engine/blob/master/modules/tracktion_engine/model/edit/tracktion_Edit.h)

**Version history and history panels**
- [How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/) · [Figma version history](https://help.figma.com/hc/en-us/articles/360038006754-View-a-file-s-version-history) · [Google Docs version history](https://support.google.com/docs/answer/190843)
- [Blender Undo & Redo](https://docs.blender.org/manual/en/latest/interface/undo_redo.html) · [Blender System preferences](https://docs.blender.org/manual/en/latest/editors/preferences/system.html) · [REAPER User Guide v7.78](https://www.reaper.fm/userguide.php) · [Cubase Edit History Dialog](https://www.steinberg.help/r/cubase-pro/15.0/en/cubase_nuendo/topics/project_window/project_window_edit_history_dialog_r.html) · [Ableton Live 12 manual §5.4.2](https://www.ableton.com/en/live-manual/12/managing-files-and-sets/) · [Photoshop History panel (secondary source)](http://www.photoshopforphotographers.com/CC_2013/Help_guide/tp/History_brush.html) · [Reinventing Git interface](https://tonsky.me/blog/reinventing-git-interface/) · [Undo (models)](https://en.wikipedia.org/wiki/Undo)

**Agent change review**
- [Zed agent panel](https://zed.dev/docs/ai/agent-panel) · [Cursor checkpoints](https://cursor.com/docs/agent/chat/checkpoints) · [VS Code: review AI code edits](https://code.visualstudio.com/docs/agents/run/review-code-edits) · [Claude Code checkpointing](https://code.claude.com/docs/en/checkpointing)

**Audio comparison methodology and plugin conventions**
- [ABX test](https://en.wikipedia.org/wiki/ABX_test) · [MUSHRA](https://en.wikipedia.org/wiki/MUSHRA) · [Echoic memory](https://en.wikipedia.org/wiki/Echoic_memory) · [ITU-R BS.1116](https://www.itu.int/rec/R-REC-BS.1116/en) · [Loudness war](https://en.wikipedia.org/wiki/Loudness_war)
- FabFilter "Undo, redo, A/B switch", documented house-wide at `/help/<plugin>/using/undoredo` — [e.g. Pro-Q](https://www.fabfilter.com/help/pro-q/using/undoredo). FabFilter Pro-Q Auto Gain; Pro-L Unity Gain; Pro-L 2 "Audition Limiting"; Pro-MB "Audition" (sidechain). TDR Nova A/B and equal-loudness readout. Logic Pro User Guide, track alternatives (support.apple.com/guide/logicpro/). *These were verified by a parallel research thread rather than fetched directly for this document; quotes are reproduced as reported.*

**Repo grounding**
`docs/SCALABILITY.md` (§4, §5) · `docs/ARCHITECTURE.md` (§2.3, §2.5, §2.6, §11, §12) · `docs/ipc-schemas/op-envelope.schema.json` · `src-tauri/src/audio/offline.rs` · `src/lib/state/mcp.svelte.ts`

**Remaining gaps to close before this becomes a spec**
- **Cubase Track Versions** and **Studio One Scratch Pad** — only partially confirmed; both are per-track/per-project variant features whose UX detail would sharpen §4.2 and §4.3.
- **Git-GUI graph-layout internals** (GitKraken, Fork, Sublime Merge lane assignment) — the §5.3 mainline-with-stubs rule rests on Tonsky's design writing plus the general DAG-legibility argument, not on any vendor's stated algorithm.
- **Photoshop's shipped defaults** (History States count in Performance preferences; automatic first snapshot behaviour) — helpx.adobe.com was unreachable; the numbers used come from a secondary source.
- **Blender's default undo-step count** — the manual documents the setting but not its default.
