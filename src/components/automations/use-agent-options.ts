"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { describeAgentOptions } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentOptionsSnapshot, AgentType } from "@/lib/types"

// Module-scope probe cache, isolated from the chat selectors (same approach as
// delegation-agent-defaults). 30s TTL absorbs rapid re-opens without a stale
// snapshot surviving a real config change. The inflight map dedups concurrent
// callers so the editor + its config section share a single probe.
const CACHE_TTL_MS = 30_000

interface CachedSnapshot {
  snapshot: AgentOptionsSnapshot
  ts: number
}

const snapshotCache = new Map<AgentType, CachedSnapshot>()
const inflight = new Map<AgentType, Promise<AgentOptionsSnapshot>>()

function readCache(agent: AgentType): AgentOptionsSnapshot | null {
  const entry = snapshotCache.get(agent)
  if (!entry) return null
  if (Date.now() - entry.ts > CACHE_TTL_MS) {
    snapshotCache.delete(agent)
    return null
  }
  return entry.snapshot
}

function fetchOptions(agent: AgentType): Promise<AgentOptionsSnapshot> {
  let promise = inflight.get(agent)
  if (!promise) {
    promise = describeAgentOptions(agent)
      .then((snapshot) => {
        snapshotCache.set(agent, { snapshot, ts: Date.now() })
        inflight.delete(agent)
        return snapshot
      })
      .catch((err) => {
        inflight.delete(agent)
        throw err
      })
    inflight.set(agent, promise)
  }
  return promise
}

export interface AgentOptionsState {
  snapshot: AgentOptionsSnapshot | null
  loading: boolean
  error: string | null
  reload: () => void
  /** Resolve the snapshot for a save-time read, keyed to the CURRENT agent (never
   *  a snapshot retained across an agent switch): the cached one if fresh, else
   *  the in-flight/fresh probe, bounded so a wedged probe never blocks saving
   *  (returns null on timeout/failure → caller falls back to raw overrides). */
  ensure: () => Promise<AgentOptionsSnapshot | null>
}

/**
 * Probe (`describeAgentOptions`) the agent's modes / config options / slash
 * commands via a transient session, with a shared module cache. One probe feeds
 * both the automation editor's config selectors and its `/` command menu — the
 * config snapshot now carries `available_commands` (captured in the same probe).
 */
export function useAgentOptions(agentType: AgentType): AgentOptionsState {
  const [snapshot, setSnapshot] = useState<AgentOptionsSnapshot | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const reqRef = useRef(0)

  const load = useCallback((agent: AgentType, force: boolean) => {
    // Bump FIRST so a cache hit also invalidates any still-in-flight probe for a
    // previously-selected agent — otherwise that slow probe's late result would
    // overwrite the snapshot for the now-current agent.
    const id = ++reqRef.current
    if (force) {
      snapshotCache.delete(agent)
      inflight.delete(agent)
    } else {
      const cached = readCache(agent)
      if (cached) {
        setSnapshot(cached)
        setError(null)
        setLoading(false)
        return
      }
    }
    setLoading(true)
    setError(null)
    setSnapshot(null)
    fetchOptions(agent)
      .then((fresh) => {
        if (reqRef.current !== id) return
        setSnapshot(fresh)
        setLoading(false)
      })
      .catch((e) => {
        if (reqRef.current !== id) return
        setError(toErrorMessage(e))
        setLoading(false)
      })
  }, [])

  useEffect(() => {
    // Debounce so switching agents quickly doesn't fire a probe (CLI spawn) per
    // click; the last agent landed on wins.
    const handle = window.setTimeout(() => {
      void load(agentType, false)
    }, 250)
    return () => window.clearTimeout(handle)
  }, [agentType, load])

  const reload = useCallback(() => load(agentType, true), [agentType, load])

  const ensure = useCallback(async (): Promise<AgentOptionsSnapshot | null> => {
    // Resolve against the CURRENT agent, not the retained React `snapshot`: after
    // an agent switch the previous agent's snapshot lingers until the debounced
    // re-probe lands, and returning it here would pin that agent's defaults into
    // the save. The module cache + inflight map are keyed by agentType, so a hit
    // is instant and a switch rides the effect's in-flight probe (no double spawn).
    const cached = readCache(agentType)
    if (cached) return cached
    // Bound the wait so a wedged probe degrades to "save with raw overrides"
    // rather than hanging the save.
    let timer: number | undefined
    const timeout = new Promise<null>((resolve) => {
      timer = window.setTimeout(() => resolve(null), 5000)
    })
    try {
      return await Promise.race([
        fetchOptions(agentType).catch(() => null),
        timeout,
      ])
    } finally {
      if (timer !== undefined) window.clearTimeout(timer)
    }
  }, [agentType])

  return { snapshot, loading, error, reload, ensure }
}
