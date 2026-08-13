/**
 * Loop-while-editing orchestration: when a MIDI clip opens in the piano roll,
 * loop its region on the transport — solo (exclusive on the clip's track) or
 * against the full mix — and put everything back on close. Owns one snapshot
 * of the pre-edit transport/solo state; re-entering with another clip
 * retargets the loop without re-snapshotting, so exit always restores the
 * state from before the first open.
 */

import { project } from "./project.svelte";
import { transport } from "./transport.svelte";

export interface ClipLoopTarget {
  trackId: string;
  startSamples: number;
  endSamples: number;
}

interface Snapshot {
  loopEnabled: boolean;
  loopStartSamples: number;
  loopEndSamples: number;
  wasPlaying: boolean;
  positionSamples: number;
  soloed: Map<string, boolean>;
}

class ClipEditLoopStore {
  /** Solo choice (exclusive solo vs full mix); remembered across opens. */
  solo = $state(true);

  private snapshot: Snapshot | null = null;
  private trackId: string | null = null;

  get active(): boolean {
    return this.snapshot !== null;
  }

  async enter(target: ClipLoopTarget) {
    if (target.endSamples <= target.startSamples) return;
    this.snapshot ??= {
      loopEnabled: transport.snap.loopEnabled,
      loopStartSamples: transport.snap.loopStartSamples,
      loopEndSamples: transport.snap.loopEndSamples,
      wasPlaying: transport.isPlaying,
      positionSamples: transport.snap.positionSamples,
      soloed: new Map(project.tracks.map((t) => [t.id, t.soloed])),
    };
    this.trackId = target.trackId;
    await transport.setLoop(true, target.startSamples, target.endSamples);
    await transport.seek(target.startSamples);
    if (this.solo) await this.applyExclusiveSolo(target.trackId);
    if (!transport.isPlaying) await transport.play();
  }

  async setSolo(on: boolean) {
    this.solo = on;
    if (!this.snapshot || !this.trackId) return;
    if (on) await this.applyExclusiveSolo(this.trackId);
    else await this.restoreSolo();
  }

  async exit() {
    const snap = this.snapshot;
    if (!snap) return;
    this.snapshot = null;
    this.trackId = null;
    await this.restoreSolo(snap);
    await transport.setLoop(snap.loopEnabled, snap.loopStartSamples, snap.loopEndSamples);
    if (!snap.wasPlaying) {
      await transport.pause();
      await transport.seek(snap.positionSamples);
    }
  }

  private async applyExclusiveSolo(trackId: string) {
    for (const t of project.tracks) {
      await project.setSolo(t.id, t.id === trackId);
    }
  }

  private async restoreSolo(snap: Snapshot | null = this.snapshot) {
    if (!snap) return;
    for (const [id, soloed] of snap.soloed) {
      await project.setSolo(id, soloed);
    }
  }
}

export const clipEditLoop = new ClipEditLoopStore();
