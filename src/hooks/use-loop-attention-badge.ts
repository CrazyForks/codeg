"use client"

import { useCallback, useEffect, useRef, useState } from "react"

import { getLoopAttention } from "@/lib/loops-api"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import { LOOP_CHANGED_EVENT, type LoopChanged } from "@/lib/types"

export interface LoopAttentionBadge {
  totalBlocking: number
  totalNotice: number
}

/**
 * Global "who needs me" badge for the sidebar Loops entry (D7). The sidebar lives
 * OUTSIDE `LoopRealtimeProvider`, so this hook owns its own subscription: it
 * refetches `get_loop_attention` on any `loop://changed` (coalesced into one
 * animation frame) and on transport reconnect. A failed refetch keeps the last
 * value, so a transient error never flickers the badge to zero.
 */
export function useLoopAttentionBadge(): LoopAttentionBadge {
  const [badge, setBadge] = useState<LoopAttentionBadge>({
    totalBlocking: 0,
    totalNotice: 0,
  })
  const frame = useRef<number | null>(null)
  const seq = useRef(0)

  // A sequence guard (not an in-flight gate): every trigger starts a fetch and
  // only the newest response is applied, so an event arriving mid-fetch is never
  // dropped — its fetch supersedes the older one (Codex r1). A failed fetch keeps
  // the last value and, being stale, is ignored if a newer fetch has started.
  const refetch = useCallback(() => {
    const my = ++seq.current
    void getLoopAttention()
      .then((a) => {
        if (my === seq.current)
          setBadge({
            totalBlocking: a.total_blocking,
            totalNotice: a.total_notice,
          })
      })
      .catch(() => {
        // Keep the last value — a transient failure must not blank the badge.
      })
  }, [])

  const schedule = useCallback(() => {
    if (frame.current != null) return
    frame.current = requestAnimationFrame(() => {
      frame.current = null
      refetch()
    })
  }, [refetch])

  useEffect(() => {
    refetch()
    let disposed = false
    let unsub: (() => void) | undefined
    void subscribe<LoopChanged>(LOOP_CHANGED_EVENT, () => {
      if (!disposed) schedule()
    }).then((fn) => {
      if (disposed) fn()
      else unsub = fn
    })
    const offReconnect = onTransportReconnect(() => schedule())
    return () => {
      disposed = true
      unsub?.()
      offReconnect?.()
      if (frame.current != null) {
        cancelAnimationFrame(frame.current)
        frame.current = null
      }
    }
  }, [refetch, schedule])

  return badge
}
