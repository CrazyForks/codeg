/**
 * Line-level LCS diff between two texts.
 *
 * Returns an ordered list of lines, each tagged added / removed / unchanged —
 * the shape a colored revision view needs (unlike the merge engine's
 * hunk-oriented `computeLineDiff`, which is keyed for three-way alignment).
 *
 * Pure and dependency-free. The O(n·m) table is bounded by a pair budget;
 * beyond it the diff falls back to "all removed, then all added" so a
 * pathologically large revision can't lock the render thread.
 */

export type DiffLineType = "context" | "add" | "del"

export interface DiffLine {
  type: DiffLineType
  text: string
}

/** Cap on `old.length * new.length` before the naive fallback kicks in. */
const LCS_PAIR_BUDGET = 200_000

/** Split into lines, dropping the empty trailing line a final "\n" produces. */
function splitLines(text: string): string[] {
  if (text === "") return []
  const lines = text.split("\n")
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop()
  return lines
}

function naive(oldLines: string[], newLines: string[]): DiffLine[] {
  return [
    ...oldLines.map((text): DiffLine => ({ type: "del", text })),
    ...newLines.map((text): DiffLine => ({ type: "add", text })),
  ]
}

/**
 * Diff `oldText` → `newText` by line. Equal lines are `context`, lines only in
 * the old text are `del`, lines only in the new text are `add`. At each change
 * point deletions precede additions (conventional diff order).
 */
export function diffLines(oldText: string, newText: string): DiffLine[] {
  const oldLines = splitLines(oldText)
  const newLines = splitLines(newText)
  const m = oldLines.length
  const n = newLines.length

  if (m === 0 && n === 0) return []
  if (m === 0) return newLines.map((text) => ({ type: "add", text }))
  if (n === 0) return oldLines.map((text) => ({ type: "del", text }))
  if (m * n > LCS_PAIR_BUDGET) return naive(oldLines, newLines)

  // LCS length table over lines.
  const dp: number[][] = Array.from({ length: m + 1 }, () =>
    new Array<number>(n + 1).fill(0)
  )
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      dp[i][j] =
        oldLines[i - 1] === newLines[j - 1]
          ? dp[i - 1][j - 1] + 1
          : Math.max(dp[i - 1][j], dp[i][j - 1])
    }
  }

  // Backtrack from the end, then reverse so the result reads top-to-bottom.
  // Prefer the `add` branch on ties so that, after the reverse, deletions
  // precede additions at a change point (conventional diff order).
  const out: DiffLine[] = []
  let i = m
  let j = n
  while (i > 0 && j > 0) {
    if (oldLines[i - 1] === newLines[j - 1]) {
      out.push({ type: "context", text: oldLines[i - 1] })
      i--
      j--
    } else if (dp[i][j - 1] >= dp[i - 1][j]) {
      out.push({ type: "add", text: newLines[j - 1] })
      j--
    } else {
      out.push({ type: "del", text: oldLines[i - 1] })
      i--
    }
  }
  while (i > 0) {
    out.push({ type: "del", text: oldLines[i - 1] })
    i--
  }
  while (j > 0) {
    out.push({ type: "add", text: newLines[j - 1] })
    j--
  }
  out.reverse()
  return out
}
