import { afterEach, describe, expect, it, vi } from "vitest"

import { buildAttentionMap } from "./loop-attention"
import type { LoopInboxItemRow } from "./types"

function card(over: Partial<LoopInboxItemRow>): LoopInboxItemRow {
  return {
    id: 1,
    issue_id: 1,
    issue_seq: 1,
    iteration_id: null,
    kind: "blocked",
    subject_key: "no_progress:5",
    payload: {},
    status: "pending",
    subject_artifact_id: null,
    subject_title: null,
    created_at: "2026-06-18T00:00:00Z",
    ...over,
  }
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe("buildAttentionMap", () => {
  it("keys a card on its backend-resolved subject_artifact_id", () => {
    const map = buildAttentionMap([
      card({ id: 1, subject_artifact_id: 42, subject_key: "design:1" }),
    ])
    expect(map.get("artifact:42")?.map((c) => c.id)).toEqual([1])
  })

  it("falls back to the suffix only for task-level prefixes", () => {
    const map = buildAttentionMap([
      // task-level, no backend id → suffix is the task artifact id.
      card({ id: 1, subject_artifact_id: null, subject_key: "no_progress:7" }),
    ])
    expect(map.get("artifact:7")?.map((c) => c.id)).toEqual([1])
  })

  it("never treats an issue-keyed suffix as an artifact id (I4)", () => {
    // design:9 with no backend id must NOT become artifact:9 — it roots at issue.
    const map = buildAttentionMap([
      card({ id: 1, subject_artifact_id: null, subject_key: "design:9" }),
    ])
    expect(map.has("artifact:9")).toBe(false)
    expect(map.get("issue-root")?.map((c) => c.id)).toEqual([1])
  })

  it("roots issue-level cards (budget) at issue-root", () => {
    const map = buildAttentionMap([
      card({ id: 1, kind: "budget_exhausted", subject_key: "budget:1" }),
    ])
    expect(map.get("issue-root")?.map((c) => c.id)).toEqual([1])
  })

  it("aggregates multiple cards on the same node into one array", () => {
    const map = buildAttentionMap([
      card({ id: 1, subject_artifact_id: 5 }),
      card({
        id: 2,
        subject_artifact_id: 5,
        subject_key: "validation_blocked:5",
      }),
    ])
    expect(map.get("artifact:5")?.map((c) => c.id)).toEqual([1, 2])
  })

  it("warns once for an unknown prefix and still roots it at issue-root", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    const map = buildAttentionMap([
      card({ id: 1, subject_key: "mystery_kind:3", subject_artifact_id: null }),
      card({ id: 2, subject_key: "mystery_kind:4", subject_artifact_id: null }),
    ])
    expect(map.get("issue-root")?.map((c) => c.id)).toEqual([1, 2])
    // Deduped: a single warn despite two unknown-prefix cards in one pass.
    expect(warn).toHaveBeenCalledTimes(1)
  })
})
