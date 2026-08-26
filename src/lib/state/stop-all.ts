/**
 * One key that makes everything shut up: Escape.
 *
 * Sound comes out of this app from three independent places, and stopping
 * the arrangement only silences the first:
 *
 *  1. the transport — the arrangement playhead;
 *  2. the launch overlay — a pad or a launch scene on the shadow playhead,
 *     which the engine renders EXCLUSIVELY while the transport is stopped
 *     (`LaunchPlayhead.exclusive`), so it keeps sounding after a stop;
 *  3. the audition preview stream — a browser row double-clicked.
 *
 * A performer who taps the wrong pad needs one key for all three, not a
 * tour of three panels. Escape is that key (`App.svelte`'s keydown).
 *
 * Two deliberate choices:
 *
 * - **Pause, not stop.** The playhead stays where it is, exactly like the
 *   second press of Space. Escape is "stop the noise", not "lose my place",
 *   and hammering it must not rewind the arrangement.
 * - **Idempotent.** Every leg no-ops when its source is silent, so Escape
 *   is safe to press when nothing is playing at all.
 */
import { audition } from "./audition.svelte";
import { launch } from "./launch.svelte";
import { transport } from "./transport.svelte";

export async function stopAllSound(): Promise<void> {
  const legs = [
    // `pause`, not `stop`: keep the playhead. Skipped when already stopped
    // so a second Escape cannot become a seek.
    transport.isPlaying ? transport.pause() : Promise.resolve(),
    launch.stopOverlay(),
    audition.stop(),
  ];
  // One failing leg must not leave the others sounding.
  await Promise.allSettled(legs);
}
