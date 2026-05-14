export type UnsubscribeFn = () => void

export interface RemoteTransportConfig {
  id: number
  name: string
  baseUrl: string
  token: string
  windowInstanceId: string
  onUnauthorized?: () => void
}

export interface Transport {
  /**
   * Invoke a backend command (replaces Tauri's invoke()).
   */
  call<T>(command: string, args?: Record<string, unknown>): Promise<T>

  /**
   * Subscribe to a backend event stream (replaces Tauri's listen()).
   * Returns an unsubscribe function.
   */
  subscribe<T>(
    event: string,
    handler: (payload: T) => void
  ): Promise<UnsubscribeFn>

  /**
   * Whether the app is running in a desktop Tauri environment.
   */
  isDesktop(): boolean

  /**
   * Register a callback invoked after a WebSocket-based transport reconnects
   * and the server-side broadcaster receiver is re-subscribed. Used by
   * consumers (e.g. ACP connection store) to recover any events emitted
   * during the disconnect window — the broadcaster drops events when
   * `receiver_count == 0`, so anything fired between `onclose` and the next
   * `__ready__` is lost. Re-fetching backend snapshots is the recovery path.
   *
   * Not fired on the initial connect (consumers handle that separately).
   * Returns an unsubscribe function. Optional — IPC-only transports (e.g.
   * Tauri) leave this undefined.
   */
  onReconnect?(callback: () => void): UnsubscribeFn

  /**
   * Resolves when the server-side broadcaster receiver is currently
   * subscribed (i.e. the most recent WS connection has received its
   * `__ready__` frame). Callers should await this immediately before
   * invoking HTTP commands that emit events via the WebSocket — without
   * the await, events fired during a WS reconnect window are silently
   * dropped by the broadcaster's `receiver_count == 0` guard.
   *
   * Bounded by a transport-internal timeout; falls through (resolves)
   * rather than rejecting to avoid permanent UI hang. Optional — IPC-only
   * transports leave this undefined (no disconnect window to guard).
   */
  waitForReady?(): Promise<void>

  destroy?(): void
}
