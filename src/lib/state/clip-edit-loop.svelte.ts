/**
 * Loop-while-editing orchestration: when a MIDI clip opens in the piano roll,
 * arm its region as the transport loop — solo (exclusive on the clip's track)
 * or against the full mix — and put everything back on close. Playback itself
 * only starts if the clipOpenAutoplay preference says so. Owns one snapshot
 * of the pre-edit transport/solo state; re-entering with another clip
 * retargets the loop without re-snapshotting, so exit always restores the
 * state from before the first open.
 */

import { prefs } from "../prefs/prefs.svelte";
import { backend } from "../tauri";
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
  /** Piano-roll Play is running (this clip only, once through). */
  playOnce = $state(false);

  private snapshot: Snapshot | null = null;
  private trackId: string | null = null;
  /** Track whose manual mute we lifted for solo mode (mute wins over solo). */
  private unmutedId: string | null = null;
  private playOnceEnd = 0;
  private loopBeforePlay: { enabled: boolean; start: number; end: number } | null = null;

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
    // Autoplay is opt-in (clipOpenAutoplay pref): by default opening a clip
    // arms the loop and parks the playhead, but leaves the transport alone.
    if (prefs.values.clipOpenAutoplay && !transport.isPlaying) await transport.play();
  }

  async setSolo(on: boolean) {
    this.solo = on;
    if (!this.snapshot || !this.trackId) return;
    if (on) await this.applyExclusiveSolo(this.trackId);
    else await this.restoreSolo();
  }

  async playThisClip(clipId: string, startSamples: number, endSamples: number) {
    if (endSamples <= startSamples) return;
    if (this.playOnce && transport.isPlaying) {
      await this.stopThisClip();
      return;
    }
    this.playOnce = true;
    this.playOnceEnd = endSamples;
    this.loopBeforePlay = {
      enabled: transport.snap.loopEnabled,
      start: transport.snap.loopStartSamples,
      end: transport.snap.loopEndSamples,
    };
    await backend.launchSetDriveFocus?.(clipId);
    await transport.setLoop(false, 0, 0);
    await transport.seek(startSamples);
    await transport.play();
  }

  async stopThisClip() {
    const was = this.playOnce;
    this.playOnce = false;
    this.playOnceEnd = 0;
    await backend.launchSetDriveFocus?.(null);
    if (was && transport.isPlaying) await transport.pause();
    const loop = this.loopBeforePlay;
    this.loopBeforePlay = null;
    if (was && loop) {
      await transport.setLoop(loop.enabled, loop.start, loop.end);
    }
  }

  /** Call from the piano roll while play-once is armed. */
  tickPlayOnce(positionSamples: number) {
    if (!this.playOnce) return;
    if (!transport.isPlaying || positionSamples >= this.playOnceEnd) {
      void this.stopThisClip();
    }
  }

  async exit() {
    const snap = this.snapshot;
    if (!snap) return;
    this.snapshot = null;
    this.trackId = null;
    await this.stopThisClip();
    await this.restoreSolo(snap);
    await transport.setLoop(snap.loopEnabled, snap.loopStartSamples, snap.loopEndSamples);
    if (!snap.wasPlaying) {
      await transport.pause();
      await transport.seek(snap.positionSamples);
    }
  }

  /**
   * Drop any active loop-while-editing state WITHOUT replaying transport or
   * solo writes (finding 8). `exit()` restores the snapshot it holds — that
   * is right when the SAME project's piano roll closes, but wrong when the
   * project itself is being swapped out (`projectops.adopt()`): the
   * snapshot's loop region and solo map describe the OLD project's tracks,
   * and replaying them (or leaving them to be replayed later by a stray
   * `exit()`) would apply stale writes onto the NEW one. Just forget it.
   */
  reset() {
    this.snapshot = null;
    this.trackId = null;
    this.unmutedId = null;
    this.playOnce = false;
    this.playOnceEnd = 0;
    this.loopBeforePlay = null;
    void backend.launchSetDriveFocus?.(null);
  }

  private async applyExclusiveSolo(trackId: string) {
    await this.remute(trackId);
    if (project.trackById(trackId)?.muted) {
      await project.setMute(trackId, false);
      this.unmutedId = trackId;
    }
    for (const t of project.tracks) {
      await project.setSolo(t.id, t.id === trackId);
    }
  }

  private async restoreSolo(snap: Snapshot | null = this.snapshot) {
    if (!snap) return;
    await this.remute();
    for (const [id, soloed] of snap.soloed) {
      await project.setSolo(id, soloed);
    }
  }

  /** Put back a mute we lifted, unless it was lifted for `keptTrackId`. */
  private async remute(keptTrackId: string | null = null) {
    if (!this.unmutedId || this.unmutedId === keptTrackId) return;
    await project.setMute(this.unmutedId, true);
    this.unmutedId = null;
  }
}

export const clipEditLoop = new ClipEditLoopStore();
