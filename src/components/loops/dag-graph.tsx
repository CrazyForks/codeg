"use client"

import { useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { TriangleAlert } from "lucide-react"

import {
  buildDag,
  foldReviews,
  placeGhosts,
  type DagCluster,
  type PendingNode,
} from "@/lib/loop-dag"
import type { AttentionKey } from "@/lib/loop-attention"
import type {
  LoopArtifactRow,
  LoopArtifactStatus,
  LoopInboxItemRow,
  LoopIterationRow,
  LoopLinkRow,
  LoopReviewVerdict,
} from "@/lib/types"
import { cn } from "@/lib/utils"

// Layout geometry (px). Read stages occupy fixed columns (x encodes the pipeline
// stage); task clusters fold in their reviews and stack as parallel lanes, with a
// depends_on chain running rightward across columns.
const COL_W = 208
const NODE_W = 176
const HEADER_H = 58
const ROW_PITCH = HEADER_H + 18
const GHOST_GAP = ROW_PITCH - HEADER_H // gap between a column's last real node and its ghost
const PAD = 8
const LANE_GAP = 22
const REVIEW_H = 22
const REVIEW_PAD = 6
const REVIEW_DIVIDER = 1 // border-t between the task header and its reviews

/** Superseded / cancelled nodes are history; when revealed they render dimmed. */
const isDead = (s: LoopArtifactStatus): boolean =>
  s === "superseded" || s === "cancelled"

const STATUS_DOT: Record<LoopArtifactStatus, string> = {
  pending: "bg-muted-foreground/40",
  in_progress: "bg-sky-500",
  awaiting_approval: "bg-amber-500",
  done: "bg-emerald-500",
  blocked: "bg-destructive",
  superseded: "bg-muted-foreground/30",
  cancelled: "bg-muted-foreground/30",
}

/**
 * The single ring a node shows, in priority order: a transient locate pulse wins
 * (so a just-located node is unmistakable), then an attention ring (amber — a
 * pending inbox card concerns it, D8), then the executing ring (sky). `inset` is
 * used inside a bordered cluster header so the ring doesn't clip.
 */
function nodeRingClass(
  opts: { pulsing: boolean; attention: boolean; executing: boolean },
  inset = false
): string {
  const i = inset ? " ring-inset" : ""
  if (opts.pulsing)
    return "ring-2 ring-sky-400 ring-offset-2 ring-offset-background animate-pulse"
  if (opts.attention) return `ring-2 ring-amber-500/70${i}`
  if (opts.executing) return `ring-2 ring-sky-500/50${i}`
  return ""
}

/** A small amber alert glyph marking a node that has pending inbox cards (D8).
 *  Decorative; the count/meaning rides on the node's title + aria-label. */
function AttentionMark() {
  return (
    <TriangleAlert
      aria-hidden
      className="h-3 w-3 shrink-0 text-amber-600 dark:text-amber-400"
    />
  )
}

/** Height of a cluster's folded reviews block (0 when the task has no reviews). */
function reviewsBlockHeight(reviews: LoopArtifactRow[]): number {
  const { latest, olderCount } = foldReviews(reviews)
  const rows = latest.length + (olderCount > 0 ? 1 : 0)
  return rows === 0 ? 0 : REVIEW_DIVIDER + REVIEW_PAD * 2 + rows * REVIEW_H
}

const clusterHeight = (c: DagCluster) =>
  HEADER_H + reviewsBlockHeight(c.reviews)

/**
 * Self-drawn DAG: an SVG layer renders provenance edges (derivation solid,
 * skips_to dashed, dependency subtle) behind absolutely-positioned HTML cards.
 * Read stages are fixed columns; each task is a *cluster* that folds in its own
 * reviews (latest attempt expanded, older attempts collapsed to a count), and
 * parallel task chains stack as lanes. Clicking any node opens its drawer.
 */
export function DagGraph({
  artifacts,
  links,
  liveIterations,
  executingIds,
  attentionMap,
  focus,
  onFocusConsumed,
  onSelect,
  onOpenIteration,
}: {
  artifacts: LoopArtifactRow[]
  links: LoopLinkRow[]
  /** queued|running iterations — drives ghost nodes for in-flight stages. */
  liveIterations: LoopIterationRow[]
  /** Namespaced executing keys (`artifact:{id}`) for nodes with a live iteration. */
  executingIds: Set<string>
  /** Pending inbox cards keyed by the node they concern (D8). A node whose
   *  `artifact:{id}` key has cards shows an amber attention ring + alert glyph. */
  attentionMap?: Map<AttentionKey, LoopInboxItemRow[]>
  /** A locate request: scroll this artifact's node into view and pulse it, then
   *  call `onFocusConsumed`. Replayed on layout changes so it lands even when the
   *  graph mounts after the request (Codex r1 I6). */
  focus?: number | null
  onFocusConsumed?: () => void
  onSelect: (artifactId: number) => void
  /** Open a ghost's live iteration session (when it has a conversation). */
  onOpenIteration?: (pending: PendingNode) => void
}) {
  const tKind = useTranslations("Loops.artifactKind")
  const tStatus = useTranslations("Loops.artifactStatus")
  const tVerdict = useTranslations("Loops.reviewVerdict")
  const tDetail = useTranslations("Loops.issueDetail")
  const tDag = useTranslations("Loops.dag")

  // Dead nodes (superseded / cancelled) are hidden by default so the graph shows
  // the live plan; the toggle reveals them (dimmed) for audit.
  const [showSuperseded, setShowSuperseded] = useState(false)
  const layout = useMemo(
    () =>
      buildDag(artifacts, links, liveIterations, {
        includeSuperseded: showSuperseded,
      }),
    [artifacts, links, liveIterations, showSuperseded]
  )

  // Locate-in-graph: when a `focus` request lands and its node is rendered,
  // scroll to it and pulse it for a moment, then consume the request. Re-runs on
  // layout changes so a focus set before the data arrived still resolves; if the
  // node never renders (e.g. a focus on a hidden superseded node), the request is
  // left for a later layout — the drawer remains the reliable locator regardless.
  const rootRef = useRef<HTMLDivElement>(null)
  const [pulsingId, setPulsingId] = useState<number | null>(null)
  const pulseTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  // The graph has data once any node would render; only then is a missing focus
  // target genuinely absent (vs. still loading / mounting after the request).
  const layoutReady =
    layout.stageNodes.length > 0 ||
    layout.clusters.length > 0 ||
    layout.result != null ||
    layout.reflection != null ||
    layout.pending.length > 0 ||
    layout.supersededCount > 0
  useEffect(() => {
    if (focus == null) return
    const el = rootRef.current?.querySelector<HTMLElement>(
      `[data-artifact-id="${focus}"]`
    )
    if (!el) {
      // Node not in the current layout. If the graph has data, the target is
      // genuinely absent (superseded/hidden/gone) → consume so a stale focus can
      // never pulse an unrelated node later. If the graph is still empty (data
      // loading, or it mounted after the request) keep focus for replay.
      if (layoutReady) onFocusConsumed?.()
      return
    }
    el.scrollIntoView({ block: "center", inline: "center" })
    // Reacting to an external locate request (URL nav) by scrolling the DOM and
    // flashing a transient pulse — a legitimate effect→setState, like the
    // sidebar's localStorage hydrate.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPulsingId(focus)
    if (pulseTimer.current) clearTimeout(pulseTimer.current)
    pulseTimer.current = setTimeout(() => setPulsingId(null), 1600)
    onFocusConsumed?.()
  }, [focus, layout, layoutReady, onFocusConsumed])
  useEffect(
    () => () => {
      if (pulseTimer.current) clearTimeout(pulseTimer.current)
    },
    []
  )
  // Per-node attention count. The issue root additionally surfaces issue-level
  // cards (budget / dependency / coverage / triage / reflect) that
  // `buildAttentionMap` roots at "issue-root"; without this they'd be grouped but
  // never ring any node (Codex r2).
  const nodeAttentionCount = (a: LoopArtifactRow): number =>
    (attentionMap?.get(`artifact:${a.id}`)?.length ?? 0) +
    (a.kind === "issue" ? (attentionMap?.get("issue-root")?.length ?? 0) : 0)

  const geom = useMemo(() => {
    const stageLayout = layout.stageNodes.map((node) => ({
      node,
      x: PAD + node.col * COL_W,
      y: PAD + node.row * ROW_PITCH,
    }))

    // Lane bands: each lane is as tall as its tallest cluster; lanes stack with a
    // fixed gap so variable-height clusters never overlap the lane below.
    const laneHeight: number[] = Array(layout.laneCount).fill(0)
    for (const c of layout.clusters) {
      laneHeight[c.lane] = Math.max(laneHeight[c.lane], clusterHeight(c))
    }
    const laneY: number[] = []
    let acc = PAD
    for (let i = 0; i < layout.laneCount; i += 1) {
      laneY[i] = acc
      acc += laneHeight[i] + LANE_GAP
    }
    const clusterBandHeight = layout.laneCount ? acc - LANE_GAP - PAD : 0

    const clusterLayout = layout.clusters.map((cluster) => ({
      cluster,
      x: PAD + cluster.col * COL_W,
      y: laneY[cluster.lane],
      height: clusterHeight(cluster),
      fold: foldReviews(cluster.reviews),
    }))

    const stageBandHeight = layout.stageRowCount
      ? (layout.stageRowCount - 1) * ROW_PITCH + HEADER_H
      : 0
    const resultY = PAD + Math.max(0, (clusterBandHeight - HEADER_H) / 2)
    const resultLayout = layout.result
      ? {
          artifact: layout.result.artifact,
          col: layout.result.col,
          x: PAD + layout.result.col * COL_W,
          y: resultY,
        }
      : null

    // The post-merge reflection node sits one column past `result`, vertically
    // centered the same way (mirrors the result node render).
    const reflectionLayout = layout.reflection
      ? {
          artifact: layout.reflection.artifact,
          col: layout.reflection.col,
          x: PAD + layout.reflection.col * COL_W,
          y: resultY,
        }
      : null

    // Edge endpoints connect to each artifact's header rect (top of a cluster).
    const boxOf = new Map<number, { x: number; y: number }>()
    for (const s of stageLayout)
      boxOf.set(s.node.artifact.id, { x: s.x, y: s.y })
    for (const c of clusterLayout) {
      boxOf.set(c.cluster.task.id, { x: c.x, y: c.y })
    }
    if (resultLayout) {
      boxOf.set(resultLayout.artifact.id, {
        x: resultLayout.x,
        y: resultLayout.y,
      })
    }
    // Register the reflection node so its derives_from→result edge resolves both
    // endpoints (otherwise the edge would render detached or be dropped).
    if (reflectionLayout) {
      boxOf.set(reflectionLayout.artifact.id, {
        x: reflectionLayout.x,
        y: reflectionLayout.y,
      })
    }

    // Ghost nodes for in-flight read/finalize/reflect stages (no output artifact
    // yet). They carry no edges (their output node doesn't exist), so they're not
    // registered in `boxOf`. Stack each strictly BELOW its column's real-node
    // bottom: real nodes use three y-systems (stage rows, packed lane bands,
    // centered result/reflection), so we measure each column's actual pixel
    // bottom here — the only layer that can — and let `placeGhosts` position
    // beneath it. A column with no real node leaves no entry → ghost sits at PAD.
    const columnBottom = new Map<number, number>()
    const noteBottom = (col: number, bottom: number) =>
      columnBottom.set(col, Math.max(columnBottom.get(col) ?? 0, bottom))
    for (const s of stageLayout) noteBottom(s.node.col, s.y + HEADER_H)
    for (const c of clusterLayout) noteBottom(c.cluster.col, c.y + c.height)
    if (resultLayout) noteBottom(resultLayout.col, resultLayout.y + HEADER_H)
    if (reflectionLayout)
      noteBottom(reflectionLayout.col, reflectionLayout.y + HEADER_H)
    const ghostY = placeGhosts(layout.pending, columnBottom, {
      pad: PAD,
      rowPitch: ROW_PITCH,
      gap: GHOST_GAP,
    })
    const pendingLayout = layout.pending.map((p) => ({
      pending: p,
      x: PAD + p.col * COL_W,
      y: ghostY.get(p.iterationId) ?? PAD,
    }))

    const pendingBottom = pendingLayout.reduce(
      (m, p) => Math.max(m, p.y - PAD + HEADER_H),
      0
    )
    const contentHeight = Math.max(
      stageBandHeight,
      clusterBandHeight,
      resultLayout || reflectionLayout ? HEADER_H : 0,
      pendingBottom
    )
    return {
      stageLayout,
      clusterLayout,
      resultLayout,
      reflectionLayout,
      pendingLayout,
      boxOf,
      width: PAD * 2 + Math.max(layout.colCount - 1, 0) * COL_W + NODE_W,
      height: PAD * 2 + contentHeight,
    }
  }, [layout])

  const canvasEmpty =
    geom.stageLayout.length === 0 &&
    geom.clusterLayout.length === 0 &&
    !geom.resultLayout &&
    !geom.reflectionLayout &&
    geom.pendingLayout.length === 0
  if (canvasEmpty && layout.supersededCount === 0) {
    return null
  }

  return (
    <div ref={rootRef} className="flex flex-col gap-2">
      {layout.supersededCount > 0 && (
        <button
          type="button"
          onClick={() => setShowSuperseded((v) => !v)}
          aria-pressed={showSuperseded}
          className="self-start rounded-md border px-2 py-1 text-xs text-muted-foreground outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          {showSuperseded
            ? tDetail("hideSuperseded")
            : tDetail("showSuperseded", { count: layout.supersededCount })}
        </button>
      )}
      <div
        className="relative"
        style={{ width: geom.width, height: geom.height }}
      >
        <svg
          className="pointer-events-none absolute inset-0 text-muted-foreground"
          width={geom.width}
          height={geom.height}
          aria-hidden
        >
          {layout.edges.map((e) => {
            const a = geom.boxOf.get(e.from)
            const b = geom.boxOf.get(e.to)
            if (!a || !b) return null
            return (
              <path
                key={e.id}
                d={edgePath(a, b)}
                fill="none"
                stroke="currentColor"
                strokeWidth={1.5}
                strokeDasharray={e.dashed ? "4 4" : undefined}
                className={
                  e.dashed
                    ? "opacity-50"
                    : e.kind === "depends_on"
                      ? "opacity-40"
                      : "opacity-25"
                }
              />
            )
          })}
        </svg>

        {geom.stageLayout.map(({ node, x, y }) => (
          <NodeCard
            key={node.artifact.id}
            artifact={node.artifact}
            x={x}
            y={y}
            executing={executingIds.has(`artifact:${node.artifact.id}`)}
            dimmed={isDead(node.artifact.status)}
            attentionCount={nodeAttentionCount(node.artifact)}
            pulsing={pulsingId === node.artifact.id}
            kindLabel={tKind(node.artifact.kind)}
            statusLabel={tStatus(node.artifact.status)}
            executingLabel={tDetail("executingNow")}
            attentionLabelOf={(count) => tDag("attention", { count })}
            onSelect={onSelect}
          />
        ))}

        {geom.resultLayout && (
          <NodeCard
            artifact={geom.resultLayout.artifact}
            x={geom.resultLayout.x}
            y={geom.resultLayout.y}
            executing={executingIds.has(
              `artifact:${geom.resultLayout.artifact.id}`
            )}
            dimmed={isDead(geom.resultLayout.artifact.status)}
            attentionCount={nodeAttentionCount(geom.resultLayout.artifact)}
            pulsing={pulsingId === geom.resultLayout.artifact.id}
            kindLabel={tKind(geom.resultLayout.artifact.kind)}
            statusLabel={tStatus(geom.resultLayout.artifact.status)}
            executingLabel={tDetail("executingNow")}
            attentionLabelOf={(count) => tDag("attention", { count })}
            onSelect={onSelect}
          />
        )}

        {geom.reflectionLayout && (
          <NodeCard
            artifact={geom.reflectionLayout.artifact}
            x={geom.reflectionLayout.x}
            y={geom.reflectionLayout.y}
            executing={executingIds.has(
              `artifact:${geom.reflectionLayout.artifact.id}`
            )}
            dimmed={isDead(geom.reflectionLayout.artifact.status)}
            attentionCount={nodeAttentionCount(geom.reflectionLayout.artifact)}
            pulsing={pulsingId === geom.reflectionLayout.artifact.id}
            kindLabel={tKind(geom.reflectionLayout.artifact.kind)}
            statusLabel={tStatus(geom.reflectionLayout.artifact.status)}
            executingLabel={tDetail("executingNow")}
            attentionLabelOf={(count) => tDag("attention", { count })}
            onSelect={onSelect}
          />
        )}

        {geom.clusterLayout.map(({ cluster, x, y, height, fold }) => (
          <ClusterCard
            key={cluster.task.id}
            cluster={cluster}
            fold={fold}
            x={x}
            y={y}
            height={height}
            dimmed={isDead(cluster.task.status)}
            executingIds={executingIds}
            attentionCount={nodeAttentionCount(cluster.task)}
            pulsing={pulsingId === cluster.task.id}
            kindLabel={tKind(cluster.task.kind)}
            reviewKindLabel={tKind("review")}
            statusLabelOf={(s) => tStatus(s)}
            verdictLabelOf={(v) => tVerdict(v)}
            executingLabel={tDetail("executingNow")}
            attentionLabelOf={(count) => tDag("attention", { count })}
            olderLabelOf={(count) => tDetail("reviewsOlder", { count })}
            onSelect={onSelect}
          />
        ))}

        {geom.pendingLayout.map(({ pending, x, y }) => (
          <PendingCard
            key={`pending:${pending.iterationId}`}
            pending={pending}
            x={x}
            y={y}
            kindLabel={tKind(pending.kind)}
            statusLabel={
              pending.status === "running" ? tDag("running") : tDag("queued")
            }
            onOpen={onOpenIteration}
          />
        ))}
      </div>
    </div>
  )
}

/**
 * Ghost card for an in-flight read/finalize/reflect stage whose output artifact
 * doesn't exist yet (spec D2). Dashed + pulsing; clickable to its live iteration
 * session when one is attached.
 */
function PendingCard({
  pending,
  x,
  y,
  kindLabel,
  statusLabel,
  onOpen,
}: {
  pending: PendingNode
  x: number
  y: number
  kindLabel: string
  statusLabel: string
  onOpen?: (pending: PendingNode) => void
}) {
  const clickable = pending.conversationId != null && onOpen != null
  return (
    <button
      type="button"
      disabled={!clickable}
      onClick={() => onOpen?.(pending)}
      style={{ left: x, top: y, width: NODE_W, height: HEADER_H }}
      aria-label={`${kindLabel}: ${statusLabel}`}
      className={cn(
        "absolute flex flex-col justify-center gap-1 rounded-lg border border-dashed bg-card/60 px-3 py-2 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
        clickable ? "hover:bg-accent" : "cursor-default"
      )}
    >
      <div className="flex items-center gap-1.5">
        <span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-sky-500" />
        <span className="text-[0.625rem] uppercase tracking-wide text-muted-foreground">
          {kindLabel}
        </span>
      </div>
      <span className="truncate text-sm font-medium text-muted-foreground">
        {statusLabel}
      </span>
    </button>
  )
}

function StatusDot({
  status,
  executing,
  title,
}: {
  status: LoopArtifactStatus
  executing: boolean
  title: string
}) {
  return (
    <span
      title={title}
      className={cn(
        "h-2 w-2 shrink-0 rounded-full",
        executing ? "animate-pulse bg-sky-500" : STATUS_DOT[status]
      )}
    />
  )
}

/** A read-stage (issue/requirement/design) or result node. */
function NodeCard({
  artifact,
  x,
  y,
  executing,
  dimmed,
  attentionCount,
  pulsing,
  kindLabel,
  statusLabel,
  executingLabel,
  attentionLabelOf,
  onSelect,
}: {
  artifact: LoopArtifactRow
  x: number
  y: number
  executing: boolean
  dimmed: boolean
  attentionCount: number
  pulsing: boolean
  kindLabel: string
  statusLabel: string
  executingLabel: string
  attentionLabelOf: (count: number) => string
  onSelect: (artifactId: number) => void
}) {
  const attention = attentionCount > 0
  const attentionLabel = attention ? attentionLabelOf(attentionCount) : null
  return (
    <button
      type="button"
      data-artifact-id={artifact.id}
      onClick={() => onSelect(artifact.id)}
      style={{ left: x, top: y, width: NODE_W, height: HEADER_H }}
      aria-label={
        attentionLabel
          ? `${kindLabel}: ${artifact.title} — ${attentionLabel}`
          : `${kindLabel}: ${artifact.title}`
      }
      className={cn(
        "absolute flex flex-col justify-center gap-1 rounded-lg border bg-card px-3 py-2 text-left shadow-sm outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring",
        nodeRingClass({ pulsing, attention, executing }),
        dimmed && "opacity-50"
      )}
    >
      <div className="flex items-center gap-1.5">
        <StatusDot
          status={artifact.status}
          executing={executing}
          title={executing ? executingLabel : statusLabel}
        />
        <span className="text-[0.625rem] uppercase tracking-wide text-muted-foreground">
          {kindLabel}
        </span>
        {attention && (
          <span className="ml-auto" title={attentionLabel ?? undefined}>
            <AttentionMark />
          </span>
        )}
      </div>
      <span className="truncate text-sm font-medium">{artifact.title}</span>
    </button>
  )
}

/** A task and its reviews, rendered as one bordered cluster. */
function ClusterCard({
  cluster,
  fold,
  x,
  y,
  height,
  dimmed,
  executingIds,
  attentionCount,
  pulsing,
  kindLabel,
  reviewKindLabel,
  statusLabelOf,
  verdictLabelOf,
  executingLabel,
  attentionLabelOf,
  olderLabelOf,
  onSelect,
}: {
  cluster: DagCluster
  fold: { latest: LoopArtifactRow[]; olderCount: number }
  x: number
  y: number
  height: number
  dimmed: boolean
  executingIds: Set<string>
  attentionCount: number
  pulsing: boolean
  kindLabel: string
  reviewKindLabel: string
  statusLabelOf: (s: LoopArtifactStatus) => string
  verdictLabelOf: (v: LoopReviewVerdict) => string
  executingLabel: string
  attentionLabelOf: (count: number) => string
  olderLabelOf: (count: number) => string
  onSelect: (artifactId: number) => void
}) {
  const { task } = cluster
  const taskExecuting = executingIds.has(`artifact:${task.id}`)
  const hasReviews = fold.latest.length > 0 || fold.olderCount > 0
  const attention = attentionCount > 0
  const attentionLabel = attention ? attentionLabelOf(attentionCount) : null
  return (
    <div
      data-artifact-id={task.id}
      style={{ left: x, top: y, width: NODE_W, height }}
      className={cn(
        "absolute flex flex-col overflow-hidden rounded-lg border bg-card shadow-sm",
        nodeRingClass({ pulsing, attention, executing: false }),
        dimmed && "opacity-50"
      )}
    >
      <button
        type="button"
        onClick={() => onSelect(task.id)}
        style={{ height: HEADER_H }}
        aria-label={
          attentionLabel
            ? `${kindLabel}: ${task.title} — ${attentionLabel}`
            : `${kindLabel}: ${task.title}`
        }
        className={cn(
          "flex flex-col justify-center gap-1 px-3 py-2 text-left outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
          taskExecuting && "ring-2 ring-inset ring-sky-500/50"
        )}
      >
        <div className="flex items-center gap-1.5">
          <StatusDot
            status={task.status}
            executing={taskExecuting}
            title={taskExecuting ? executingLabel : statusLabelOf(task.status)}
          />
          <span className="text-[0.625rem] uppercase tracking-wide text-muted-foreground">
            {kindLabel}
          </span>
          {attention && (
            <span className="ml-auto" title={attentionLabel ?? undefined}>
              <AttentionMark />
            </span>
          )}
        </div>
        <span className="truncate text-sm font-medium">{task.title}</span>
      </button>

      {hasReviews && (
        <div
          className="flex flex-col gap-0 border-t bg-muted/30"
          style={{ paddingTop: REVIEW_PAD, paddingBottom: REVIEW_PAD }}
        >
          {fold.latest.map((review) => {
            const executing = executingIds.has(`artifact:${review.id}`)
            // Row text keeps the artifact title so sibling reviews stay distinct;
            // the pass/fail outcome shows as a shape glyph (✓/✗) — not color alone
            // — and is named in the accessible label + tooltip.
            const verdictLabel = review.verdict
              ? verdictLabelOf(review.verdict)
              : null
            const statusLabel = executing
              ? executingLabel
              : statusLabelOf(review.status)
            return (
              <button
                key={review.id}
                type="button"
                onClick={() => onSelect(review.id)}
                style={{ height: REVIEW_H }}
                aria-label={
                  verdictLabel
                    ? `${reviewKindLabel}: ${review.title} — ${verdictLabel}`
                    : `${reviewKindLabel}: ${review.title}`
                }
                title={verdictLabel ?? statusLabel}
                className={cn(
                  "flex items-center gap-1.5 px-3 text-left outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                  // A dead review folded under a live task is dimmed on its own;
                  // when the task itself is dead the whole cluster is already dimmed.
                  isDead(review.status) && "opacity-50"
                )}
              >
                <span
                  className={cn(
                    "h-2 w-2 shrink-0 rounded-full",
                    executing
                      ? "animate-pulse bg-sky-500"
                      : STATUS_DOT[review.status]
                  )}
                />
                <span className="flex-1 truncate text-xs text-muted-foreground">
                  {review.title}
                </span>
                {review.verdict && (
                  <span
                    aria-hidden
                    className={cn(
                      "shrink-0 text-xs font-semibold leading-none",
                      review.verdict === "pass"
                        ? "text-emerald-600"
                        : "text-destructive"
                    )}
                  >
                    {review.verdict === "pass" ? "✓" : "✗"}
                  </span>
                )}
              </button>
            )
          })}
          {fold.olderCount > 0 && (
            <span
              style={{ height: REVIEW_H }}
              className="flex items-center px-3 text-[0.625rem] uppercase tracking-wide text-muted-foreground/70"
            >
              {olderLabelOf(fold.olderCount)}
            </span>
          )}
        </div>
      )}
    </div>
  )
}

/**
 * Horizontal S-curve connecting two header rects on the sides that face each
 * other, so an edge never cuts through a node body. Edges run from a dependent
 * (right) back to its source (left).
 */
function edgePath(
  a: { x: number; y: number },
  b: { x: number; y: number }
): string {
  const acy = a.y + HEADER_H / 2
  const bcy = b.y + HEADER_H / 2
  const aRightOfB = a.x >= b.x
  const x1 = aRightOfB ? a.x : a.x + NODE_W
  const x2 = aRightOfB ? b.x + NODE_W : b.x
  const mx = (x1 + x2) / 2
  return `M ${x1} ${acy} C ${mx} ${acy}, ${mx} ${bcy}, ${x2} ${bcy}`
}
