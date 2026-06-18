import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { useLoopAttentionBadge } from "./use-loop-attention-badge"

const getLoopAttention = vi.fn()
vi.mock("@/lib/loops-api", () => ({
  getLoopAttention: (...a: unknown[]) => getLoopAttention(...a),
}))

// Capture the realtime + reconnect callbacks so a test can fire them.
let changedHandler: (() => void) | null = null
let reconnectHandler: (() => void) | null = null
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(async (_evt: string, cb: () => void) => {
    changedHandler = cb
    return () => {
      changedHandler = null
    }
  }),
  onTransportReconnect: vi.fn((cb: () => void) => {
    reconnectHandler = cb
    return () => {
      reconnectHandler = null
    }
  }),
}))

function deferred<T>() {
  let resolve!: (v: T) => void
  let reject!: (e?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

const attention = (blocking: number, notice: number) => ({
  total_blocking: blocking,
  total_notice: notice,
  per_space: [],
})

beforeEach(() => {
  vi.clearAllMocks()
  changedHandler = null
  reconnectHandler = null
  // Run rAF synchronously so `schedule()` resolves to a refetch within the act().
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    cb(0)
    return 1
  })
  vi.stubGlobal("cancelAnimationFrame", () => {})
})

describe("useLoopAttentionBadge", () => {
  it("refetches on an event mid-fetch and applies only the newest response", async () => {
    const d1 = deferred<ReturnType<typeof attention>>()
    const d2 = deferred<ReturnType<typeof attention>>()
    getLoopAttention
      .mockReturnValueOnce(d1.promise)
      .mockReturnValueOnce(d2.promise)

    const { result } = renderHook(() => useLoopAttentionBadge())
    expect(getLoopAttention).toHaveBeenCalledTimes(1) // mount fetch (d1 in flight)

    // An event arrives while d1 is still in flight → it must NOT be dropped.
    await act(async () => {
      changedHandler?.()
    })
    expect(getLoopAttention).toHaveBeenCalledTimes(2)

    // The newest fetch (d2) wins.
    await act(async () => {
      d2.resolve(attention(3, 1))
    })
    expect(result.current).toEqual({ totalBlocking: 3, totalNotice: 1 })

    // The stale earlier fetch (d1) resolving later is ignored (seq guard).
    await act(async () => {
      d1.resolve(attention(99, 99))
    })
    expect(result.current).toEqual({ totalBlocking: 3, totalNotice: 1 })
  })

  it("a failed in-flight fetch does not swallow a later update", async () => {
    const d1 = deferred<ReturnType<typeof attention>>()
    const d2 = deferred<ReturnType<typeof attention>>()
    getLoopAttention
      .mockReturnValueOnce(d1.promise)
      .mockReturnValueOnce(d2.promise)

    const { result } = renderHook(() => useLoopAttentionBadge())
    await act(async () => {
      d1.reject(new Error("boom")) // mount fetch fails — keep last (0/0)
    })
    expect(result.current).toEqual({ totalBlocking: 0, totalNotice: 0 })

    await act(async () => {
      changedHandler?.() // a new event triggers a fresh fetch
    })
    expect(getLoopAttention).toHaveBeenCalledTimes(2)
    await act(async () => {
      d2.resolve(attention(2, 0))
    })
    expect(result.current).toEqual({ totalBlocking: 2, totalNotice: 0 })
  })

  it("refetches on transport reconnect", async () => {
    const d1 = deferred<ReturnType<typeof attention>>()
    const d2 = deferred<ReturnType<typeof attention>>()
    getLoopAttention
      .mockReturnValueOnce(d1.promise)
      .mockReturnValueOnce(d2.promise)
    renderHook(() => useLoopAttentionBadge())
    await act(async () => {
      d1.resolve(attention(0, 0))
    })
    await act(async () => {
      reconnectHandler?.()
    })
    expect(getLoopAttention).toHaveBeenCalledTimes(2)
  })
})
