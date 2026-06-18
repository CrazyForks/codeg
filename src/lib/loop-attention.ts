/**
 * Maps an issue's pending inbox cards onto the DAG/board nodes they concern (D8),
 * so a node can show an attention ring and clicking it locates the card.
 *
 * Resolution order per card:
 *   1. `subject_artifact_id` — the backend's authoritative resolution (B4/D9).
 *   2. fallback by `subject_key` prefix when the backend left it null. Only
 *      task-level prefixes may treat the suffix as an artifact id (task ≡
 *      artifact); an issue-keyed suffix is NEVER an artifact id (Codex r1 I4).
 *   3. anything else (issue-level cards, unknown prefixes) roots at `issue-root`.
 *
 * Exhaustive: every card lands somewhere, none is silently dropped.
 */

import type { LoopInboxItemRow } from "./types"

export type AttentionKey = `artifact:${number}` | "issue-root"

/** Task-level prefixes whose `{prefix}:{id}` suffix IS the task artifact id. */
const TASK_LEVEL_PREFIXES = new Set([
  "no_progress",
  "validation_blocked",
  "infra_failure",
  "oscillation",
])

/** Every prefix the engine files (so only a genuinely new/unknown one warns). */
const KNOWN_PREFIXES = new Set([
  // task-level
  "no_progress",
  "validation_blocked",
  "infra_failure",
  "oscillation",
  // design-level (suffix = issue id)
  "design",
  "design_rejected",
  // result-level (suffix = issue id)
  "merge",
  "merge_blocked",
  "merge_rejected",
  "finalize_dirty",
  "unverifiable",
  "integration_gap",
  // iteration-level (suffix = iteration id / question id)
  "dispatch_failed",
  "stalled",
  "question",
  // issue-level (no backing artifact)
  "dependency_unsatisfiable",
  "coverage_gap",
  "triage_no_route",
  "budget",
  "reflect_failed",
])

// Warn at most once per unknown prefix — `buildAttentionMap` runs on every render,
// so an unknown subject must not spam the console every frame (Codex r1 M3).
const warnedPrefixes = new Set<string>()

function splitPrefix(subjectKey: string): { prefix: string; rest: string } {
  const idx = subjectKey.indexOf(":")
  if (idx < 0) return { prefix: subjectKey, rest: "" }
  return { prefix: subjectKey.slice(0, idx), rest: subjectKey.slice(idx + 1) }
}

function resolveKey(item: LoopInboxItemRow): AttentionKey {
  // 1. Backend-authoritative.
  if (item.subject_artifact_id != null) {
    return `artifact:${item.subject_artifact_id}`
  }
  // 2. Prefix fallback — task-level only treats the suffix as an artifact id.
  const { prefix, rest } = splitPrefix(item.subject_key)
  if (TASK_LEVEL_PREFIXES.has(prefix)) {
    const id = Number(rest)
    if (Number.isInteger(id) && id > 0) return `artifact:${id}`
  }
  // 3. Issue-level / unknown → root at the issue. Warn once per unknown prefix.
  if (prefix && !KNOWN_PREFIXES.has(prefix) && !warnedPrefixes.has(prefix)) {
    warnedPrefixes.add(prefix)
    console.warn(
      `[loop-attention] unknown inbox subject prefix "${prefix}"; rooting at issue`
    )
  }
  return "issue-root"
}

/** Group pending inbox cards by the artifact (or issue root) they concern. */
export function buildAttentionMap(
  items: LoopInboxItemRow[]
): Map<AttentionKey, LoopInboxItemRow[]> {
  const map = new Map<AttentionKey, LoopInboxItemRow[]>()
  for (const item of items) {
    const key = resolveKey(item)
    const bucket = map.get(key)
    if (bucket) bucket.push(item)
    else map.set(key, [item])
  }
  return map
}
