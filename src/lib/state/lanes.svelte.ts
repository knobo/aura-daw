/**
 * Lane VIEW state: which lanes and groups are folded, and the live
 * drag-to-reorder gesture.
 *
 * Deliberately NOT document state. Folding a lane changes nothing another
 * musician opening the project would need, and routing it through the op
 * log would put "collapsed the drum group" on the undo stack between two
 * real edits — one of the more annoying things a DAW can do with Ctrl+Z.
 * It IS remembered per project (localStorage keyed by project dir), because
 * re-folding twelve lanes after every restart is the other annoying thing.
 *
 * Everything the ARRANGEMENT owns — order, names, group membership — goes
 * through `project`/the backend instead; see `project.arrangeLanes`.
 */

import { project } from "./project.svelte";

/** localStorage key for one project's fold state. Unsaved sessions share
 * the `""` key: they have no dir yet, and losing the fold state of a
 * scratch session is not worth a second storage scheme. */
function storageKey(projectDir: string | null): string {
  return `aura.lanes.folds:${projectDir ?? ""}`;
}

interface PersistedFolds {
  tracks: string[];
  groups: string[];
}

class LanesView {
  /** Track ids folded to a strip. */
  collapsedTracks = $state<Set<string>>(new Set());
  /** Group names folded away (their lanes are not painted at all). */
  collapsedGroups = $state<Set<string>>(new Set());

  /** The lane being dragged, or "" — the id, not the row, so a re-render
   * mid-drag (a clip arriving, a meter tick) cannot leave the gesture
   * holding a stale object. */
  draggingTrackId = $state("");
  /** Live drop indicator while dragging: y in lane-column px, plus the
   * group the lane would join. `null` between gestures. */
  dropIndicator = $state<{ y: number; group: string | null } | null>(null);

  /** Which lane's name is being edited inline ("" = none). One at a time:
   * two open editors would race each other's commit. */
  renamingTrackId = $state("");
  /** Which group's name is being edited inline ("" = none). */
  renamingGroup = $state("");

  /** Selected track ids (4.5 — bulk M/S/A). A gesture, not a document: not
   * persisted, not part of `PersistedFolds`. Mirrors `clipSelection`'s
   * replace-the-Set convention so `$state` reactivity actually fires — a
   * mutated-in-place Set is invisible to Svelte. */
  selection = $state<Set<string>>(new Set());
  /** Anchor for shift-extend — the last id a plain click or ctrl-toggle
   * landed on. Independent of `selection` itself: toggling a lane OFF
   * still moves the anchor there, same as clip selection's `apply`. */
  #anchor: string | null = null;

  /** The dir the currently loaded fold state came from, so `sync` can tell
   * "same project, don't reload" from "project changed, swap folds". */
  #loadedFor: string | null | undefined = undefined;

  isTrackCollapsed(trackId: string): boolean {
    return this.collapsedTracks.has(trackId);
  }
  isGroupCollapsed(group: string): boolean {
    return this.collapsedGroups.has(group);
  }

  toggleTrack(trackId: string) {
    const next = new Set(this.collapsedTracks);
    if (!next.delete(trackId)) next.add(trackId);
    this.collapsedTracks = next;
    this.#save();
  }

  toggleGroup(group: string) {
    const next = new Set(this.collapsedGroups);
    if (!next.delete(group)) next.add(group);
    this.collapsedGroups = next;
    this.#save();
  }

  /** Fold (or unfold) every lane at once — the "give me the overview"
   * gesture. Uses the CURRENT track list, so lanes added later start
   * unfolded, which is what "new lane, show me it" wants. */
  setAllCollapsed(collapsed: boolean) {
    this.collapsedTracks = collapsed ? new Set(project.tracks.map((t) => t.id)) : new Set();
    this.#save();
  }

  /** True when at least one lane is folded — drives the toggle-all label. */
  anyCollapsed(): boolean {
    return this.collapsedTracks.size > 0 || this.collapsedGroups.size > 0;
  }

  /** A renamed group carries its fold state across: the user folded THAT
   * group, and renaming it is not unfolding it. */
  renameGroupFold(from: string, to: string) {
    if (!this.collapsedGroups.has(from)) return;
    const next = new Set(this.collapsedGroups);
    next.delete(from);
    if (to) next.add(to);
    this.collapsedGroups = next;
    this.#save();
  }

  isSelected(trackId: string): boolean {
    return this.selection.has(trackId);
  }

  selectOnly(trackId: string) {
    this.selection = new Set([trackId]);
    this.#anchor = trackId;
  }

  toggleSelected(trackId: string) {
    const next = new Set(this.selection);
    if (!next.delete(trackId)) next.add(trackId);
    this.selection = next;
    this.#anchor = trackId;
  }

  /** Shift-click: the contiguous run between the anchor and `trackId` over
   * `orderedTrackIds` (the VISIBLE order — a folded group's hidden members
   * must not get swept in). No anchor yet, or the anchor/target fell off
   * the given order (a folded group, a deleted lane), falls back to
   * selecting just `trackId` — the same "can't extend, so start over"
   * shift-click already does at the clip level. */
  extendTo(trackId: string, orderedTrackIds: string[]) {
    const anchor = this.#anchor;
    const ai = anchor ? orderedTrackIds.indexOf(anchor) : -1;
    const ti = orderedTrackIds.indexOf(trackId);
    if (ai < 0 || ti < 0) {
      this.selection = new Set([trackId]);
      return;
    }
    const [lo, hi] = ai <= ti ? [ai, ti] : [ti, ai];
    this.selection = new Set(orderedTrackIds.slice(lo, hi + 1));
    // Anchor stays put — a second shift-click re-extends from the same
    // origin, matching how a text/file selection behaves.
  }

  clearSelection() {
    this.selection = new Set();
    this.#anchor = null;
  }

  /** Select every member of a lane group — the group header's "select
   * all of mine" gesture. */
  selectGroup(group: string) {
    this.selection = new Set(
      project.tracks.filter((t) => t.group?.trim() === group).map((t) => t.id),
    );
  }

  selectAll() {
    this.selection = new Set(project.tracks.map((t) => t.id));
  }

  /**
   * Load the folds belonging to the currently open project, and drop
   * references to tracks and groups that no longer exist.
   *
   * The pruning matters: a stale id in `collapsedTracks` is invisible, but
   * a stale GROUP name is not — re-creating a group with a name that was
   * folded three sessions ago would silently create it folded.
   */
  sync() {
    const dir = project.projectDir;
    if (dir !== this.#loadedFor) {
      this.#loadedFor = dir;
      this.#load(dir);
      // A project swap ends any in-flight gesture: the lane being dragged
      // belongs to the document that just went away.
      this.draggingTrackId = "";
      this.dropIndicator = null;
      this.renamingTrackId = "";
      this.renamingGroup = "";
      // A selection is a gesture over THIS document; the new one starts
      // with nothing selected rather than inheriting ids that happen to
      // collide with the old project's.
      this.selection = new Set();
      this.#anchor = null;
    }
    const liveTracks = new Set(project.tracks.map((t) => t.id));
    const liveGroups = new Set(
      project.tracks.map((t) => t.group?.trim()).filter((g): g is string => !!g),
    );
    const tracks = new Set([...this.collapsedTracks].filter((id) => liveTracks.has(id)));
    const groups = new Set([...this.collapsedGroups].filter((g) => liveGroups.has(g)));
    if (tracks.size !== this.collapsedTracks.size) this.collapsedTracks = tracks;
    if (groups.size !== this.collapsedGroups.size) this.collapsedGroups = groups;
    const selected = new Set([...this.selection].filter((id) => liveTracks.has(id)));
    if (selected.size !== this.selection.size) this.selection = selected;
  }

  #load(dir: string | null) {
    this.collapsedTracks = new Set();
    this.collapsedGroups = new Set();
    try {
      const raw = localStorage.getItem(storageKey(dir));
      if (!raw) return;
      const parsed = JSON.parse(raw) as Partial<PersistedFolds>;
      // Hand-editable storage: validate rather than trust. A malformed
      // entry must leave the lanes unfolded, never throw during render.
      if (Array.isArray(parsed.tracks)) {
        this.collapsedTracks = new Set(parsed.tracks.filter((x) => typeof x === "string"));
      }
      if (Array.isArray(parsed.groups)) {
        this.collapsedGroups = new Set(parsed.groups.filter((x) => typeof x === "string"));
      }
    } catch {
      /* unreadable storage (private mode, quota, garbage) — start unfolded */
    }
  }

  #save() {
    try {
      const payload: PersistedFolds = {
        tracks: [...this.collapsedTracks],
        groups: [...this.collapsedGroups],
      };
      localStorage.setItem(storageKey(this.#loadedFor ?? null), JSON.stringify(payload));
    } catch {
      /* storage full or unavailable — folding still works this session */
    }
  }
}

export const lanes = new LanesView();
