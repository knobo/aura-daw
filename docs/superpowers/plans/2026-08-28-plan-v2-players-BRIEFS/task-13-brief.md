## Task 13: The renderer — a pad is a player

**Files:**
- Create: `src/lib/state/players.svelte.ts`
- Modify: `src/lib/utils/control-surface.ts:120-130` (`SurfaceTarget`), `:250-280` (`targetKey`, the bind list)
- Modify: `src/lib/components/surface/SurfacePanel.svelte`, `src/lib/components/surface/Rack.svelte`
- Test: `src/lib/utils/control-surface.test.ts`, `src/lib/components/surface/surface-panel.dom.test.ts`

**Interfaces:**
- Consumes: the `player_fire` / `player_stop` / `players_get` / `player_add` commands (Task 9).
- Produces: `SurfaceTarget` variant `{ kind: "player"; playerId: string }`; `players.svelte.ts` exporting `players` (a `$state` list), `refreshPlayers()`, `firePlayer(id)`, `stopPlayer(id)`, `addAudioPlayer(clipId, raw)`.

- [ ] **Step 1: Write the failing test**

Add to `src/lib/utils/control-surface.test.ts`:

```ts
describe("player targets", () => {
  it("gives a player target a stable key distinct from a clip's", () => {
    expect(targetKey({ kind: "player", playerId: "p1" })).toBe("player:p1");
    expect(targetKey({ kind: "player", playerId: "p1" })).not.toBe(
      targetKey({ kind: "clipLaunch", clipId: "p1" }),
    );
  });

  it("accepts a player target on a pad and on a pad grid cell", () => {
    const layout = addWidget(blankLayout(), "pad");
    const pad = layout.pages[0].widgets[0];
    const bound = bindWidget(layout, pad.id, { kind: "player", playerId: "p1" });
    expect(bound.pages[0].widgets[0].target).toEqual({ kind: "player", playerId: "p1" });
  });
});
```

Add to `src/lib/components/surface/surface-panel.dom.test.ts`:

```ts
it("shows a player pad's source and raw flag", async () => {
  // Read the existing DOM-test fixtures in this file before writing this;
  // mount the panel the same way the neighbouring tests do.
  const { getByTestId } = renderPanelWithPlayerPad({
    id: "p1",
    name: "KICK",
    source: { kind: "audioClip", clipId: "c1" },
    raw: true,
  });
  expect(getByTestId("pad-p1-source").textContent).toContain("KICK");
  expect(getByTestId("pad-p1-raw")).toBeTruthy();
});

it("fires the player on pointerdown and stops it on pointerup in gate mode", async () => {
  const calls: string[] = [];
  const { pad } = renderPanelWithPlayerPad({ id: "p1", trigger: { mode: "gate" } }, calls);
  await fireEvent.pointerDown(pad);
  await fireEvent.pointerUp(pad);
  expect(calls).toEqual(["player_fire:p1", "player_stop:p1"]);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm test -- control-surface surface-panel 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 3: Write the minimal implementation**

In `control-surface.ts`, add the variant and its key:

```ts
  | { kind: "player"; playerId: string }
```

```ts
    case "player":
      return `player:${target.playerId}`;
```

`players.svelte.ts` is a thin mirror — invoke and hold, no time math and no
authoritative state (ADR 0006):

```ts
import { invoke } from "$lib/tauri";

export type PlayerSource =
  | { kind: "audioClip"; clipId: string }
  | { kind: "midiClip"; clipId: string; instrumentId: string | null }
  | { kind: "none" };

export interface Player {
  id: string;
  name: string;
  source: PlayerSource;
  raw: boolean;
  trigger: { mode: "oneShot" | "gate" | "loop" };
}

export const players = $state<{ list: Player[] }>({ list: [] });

export async function refreshPlayers(): Promise<void> {
  players.list = await invoke<Player[]>("players_get");
}

export const firePlayer = (id: string) => invoke("player_fire", { id });
export const stopPlayer = (id: string) => invoke("player_stop", { id });

/// Make a pad out of an audio clip. `raw` is V-6: the file at unity,
/// bypassing the source track entirely.
export async function addAudioPlayer(clipId: string, raw: boolean): Promise<string> {
  const id = await invoke<string>("player_add", {
    source: { kind: "audioClip", clipId },
    raw,
  });
  await refreshPlayers();
  return id;
}
```

In `SurfacePanel.svelte`'s bind menu, add an `Add pad from clip ›` entry
that drills into the project's audio clips (following the `Add rack ›`
drill-down PR #116 landed, for the reason it landed: a flat list runs into
the bottom of the window), with a `raw` checkbox. In `Rack.svelte`, a pad
whose target is a player renders the player's name and a `RAW` marker.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm test 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib
git commit -m "feat(surface): a pad can be a player"
```

---

