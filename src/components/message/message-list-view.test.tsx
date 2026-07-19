import { describe, expect, it } from "vitest"

import {
  mergeConsecutiveAssistantTurns,
  singletonSourceTurns,
} from "./message-list-view"
import type { MessageTurn } from "@/lib/types"

function turn(id: string): MessageTurn {
  return { id, role: "assistant", blocks: [], timestamp: "" }
}

type ThreadItem = Parameters<typeof mergeConsecutiveAssistantTurns>[0][number]
type TurnItem = Extract<ThreadItem, { kind: "turn" }>

function assistantItem(
  id: string,
  groupOverrides: Partial<TurnItem["group"]> = {}
): ThreadItem {
  return {
    key: `persisted-${id}`,
    kind: "turn",
    group: {
      id,
      role: "assistant",
      parts: [{ type: "text", text: `reply ${id}` }],
      resources: [],
      images: [],
      ...groupOverrides,
    },
    phase: "persisted",
    showStats: false,
    isRoleTransition: false,
    previousUserIndex: null,
    sourceTurns: [],
  }
}

describe("singletonSourceTurns", () => {
  it("returns the same array reference for the same turn", () => {
    const t = turn("t1")
    const first = singletonSourceTurns(t)
    const second = singletonSourceTurns(t)
    // Reference stability is the whole point: it lets HistoricalMessageGroup's
    // memo bail out when an unchanged historical turn re-renders per token.
    expect(first).toBe(second)
    expect(first).toEqual([t])
  })

  it("returns distinct arrays for distinct turns", () => {
    const a = singletonSourceTurns(turn("a"))
    const b = singletonSourceTurns(turn("b"))
    expect(a).not.toBe(b)
  })
})

describe("mergeConsecutiveAssistantTurns", () => {
  it("surfaces completion time patched onto a non-last sub-turn", () => {
    // Real-device bug (Cursor session 118b6805): the post-turn metadata
    // patch head-aligns onto the FIRST local sub-turn when the parser emits
    // fewer turns than the live stream split into. The merged footer must
    // still show that completion time (and its duration), not the last
    // sub-turn's empty fields.
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", {
        duration_ms: 15_975,
        completed_at: "2026-07-19T05:25:22.851Z",
      }),
      assistantItem("b"),
    ])
    expect(merged).toHaveLength(1)
    const item = merged[0] as TurnItem
    expect(item.group.completed_at).toBe("2026-07-19T05:25:22.851Z")
    expect(item.group.duration_ms).toBe(15_975)
  })

  it("keeps the latest completion when several sub-turns carry one", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", { completed_at: "2026-07-19T05:25:10.000Z" }),
      assistantItem("b", { completed_at: "2026-07-19T05:25:22.851Z" }),
    ])
    expect(merged).toHaveLength(1)
    const item = merged[0] as TurnItem
    expect(item.group.completed_at).toBe("2026-07-19T05:25:22.851Z")
  })
})
