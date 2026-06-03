import { toErrorMessage } from "./app-error"
import { getTransport, isDesktop } from "./transport"

// All updater imports are dynamic to avoid crashing in non-Tauri browsers.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Update = any

export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" }

export type ServerUpdateCapability = "supervised" | "reexec"

export interface AppUpdateCheckResult {
  currentVersion: string
  update: Update | null
  // Server-mode only (absent in desktop). Whether THIS server process can
  // apply the update in place, how it would restart, the deployment kind,
  // the restart delay to drive the frontend countdown, and whether a
  // previous version is staged for rollback.
  selfUpdateSupported?: boolean
  capability?: ServerUpdateCapability
  runtime?: string
  restartDelayMs?: number
  rollbackAvailable?: boolean
}

export interface ServerUpdateProgress {
  phase: "downloading" | "verifying" | "extracting" | "swapping"
  downloaded: number
  total: number | null
}

export interface ServerUpdateActionResult {
  version?: string
  needRestart: boolean
  restartDelayMs: number
  // Supervisor probation window (seconds): a freshly-upgraded worker that
  // crashes within it is auto-rolled-back. 0 in re-exec mode (no supervisor),
  // so the frontend need not wait it out before declaring success.
  trialSeconds: number
  capability: ServerUpdateCapability
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

export type AppUpdateErrorKind =
  | "source_unreachable"
  | "network"
  | "download_failed"
  | "install_failed"
  | "unknown"

export interface AppUpdateErrorInfo {
  kind: AppUpdateErrorKind
  rawMessage: string
}

export async function getCurrentAppVersion(): Promise<string> {
  if (!isDesktop()) {
    const result =
      await getTransport().call<AppUpdateCheckResult>("check_app_update")
    return result.currentVersion
  }
  try {
    const { getVersion } = await import("@tauri-apps/api/app")
    return await getVersion()
  } catch {
    return "unknown"
  }
}

export async function checkAppUpdate(): Promise<AppUpdateCheckResult> {
  if (!isDesktop()) {
    return getTransport().call<AppUpdateCheckResult>("check_app_update")
  }
  const { getVersion } = await import("@tauri-apps/api/app")
  const { check } = await import("@tauri-apps/plugin-updater")
  const [currentVersion, update] = await Promise.all([getVersion(), check()])
  return { currentVersion, update }
}

export async function installAppUpdate(
  update: NonNullable<Update>,
  onEvent?: (progress: DownloadEvent) => void
): Promise<void> {
  // Web mode: server returns metadata only; downloadAndInstall is unavailable.
  // The browser-side user can't trigger a server-side install, so the caller
  // is expected to surface a "view release" affordance instead.
  if (typeof update?.downloadAndInstall !== "function") return
  await update.downloadAndInstall(onEvent)
}

export async function relaunchApp(): Promise<void> {
  const { relaunch } = await import("@tauri-apps/plugin-process")
  await relaunch()
}

// ─── Server / Docker in-place self-update ──────────────────────────────────

/** Subscribe to download/verify/swap progress emitted by the server. */
export function subscribeServerUpdateProgress(
  handler: (progress: ServerUpdateProgress) => void
): Promise<() => void> {
  return getTransport().subscribe<ServerUpdateProgress>(
    "app_update_progress",
    handler
  )
}

/**
 * Download + verify + swap the new bundle on the server. Resolves once the
 * new files are staged on disk; the caller then triggers {@link restartServer}.
 * Generous timeout: the download can be tens of MB.
 */
export async function performServerUpdate(): Promise<ServerUpdateActionResult> {
  return getTransport().call<ServerUpdateActionResult>(
    "perform_app_update",
    {},
    { timeoutMs: 15 * 60_000 }
  )
}

/** Ask the server to relaunch into the freshly-swapped binary. */
export async function restartServer(): Promise<ServerUpdateActionResult> {
  return getTransport().call<ServerUpdateActionResult>("restart_app")
}

/** Revert to the previously-installed bundle (kept as `.bak`). */
export async function rollbackServer(): Promise<ServerUpdateActionResult> {
  return getTransport().call<ServerUpdateActionResult>("rollback_app")
}

/**
 * Poll `/health` until the restarted server answers or the deadline passes.
 * The WebSocket drops during restart and HTTP calls fail in the meantime —
 * both are swallowed as "not up yet".
 */
export async function waitForServerHealthy(opts: {
  timeoutMs: number
  intervalMs?: number
  initialDelayMs?: number
}): Promise<boolean> {
  const interval = opts.intervalMs ?? 1500
  if (opts.initialDelayMs) await sleep(opts.initialDelayMs)
  const deadline = Date.now() + opts.timeoutMs
  while (Date.now() < deadline) {
    try {
      await getTransport().call("health", {}, { timeoutMs: 4000 })
      return true
    } catch {
      // Server still restarting — keep polling.
    }
    await sleep(interval)
  }
  return false
}

/**
 * Read the running server's version from `/health`. Local-only — no remote
 * manifest fetch — so it can confirm (even when the update source is
 * unreachable) that a restart actually landed on the new version rather than
 * an auto-rolled-back previous one. Returns null if `/health` reports no
 * version (older server) or the call fails.
 */
export async function getRunningServerVersion(): Promise<string | null> {
  try {
    const res = await getTransport().call<{ version?: string }>(
      "health",
      {},
      { timeoutMs: 4000 }
    )
    return res?.version ?? null
  } catch {
    return null
  }
}

export async function closeAppUpdate(
  update: NonNullable<Update>
): Promise<void> {
  if (typeof update?.close !== "function") return
  await update.close()
}

export function normalizeAppUpdateError(error: unknown): AppUpdateErrorInfo {
  const rawMessage = toErrorMessage(error)
  const normalized = rawMessage.toLowerCase()

  if (
    normalized.includes("latest.json") ||
    normalized.includes("/releases/latest/download/")
  ) {
    return { kind: "source_unreachable", rawMessage }
  }

  if (
    normalized.includes("error sending request for url") ||
    normalized.includes("failed to send request") ||
    normalized.includes("network") ||
    normalized.includes("timed out") ||
    normalized.includes("dns") ||
    normalized.includes("connection refused")
  ) {
    return { kind: "network", rawMessage }
  }

  if (
    normalized.includes("download") ||
    normalized.includes("checksum") ||
    normalized.includes("content-length")
  ) {
    return { kind: "download_failed", rawMessage }
  }

  if (
    normalized.includes("install") ||
    normalized.includes("installer") ||
    normalized.includes("permission denied")
  ) {
    return { kind: "install_failed", rawMessage }
  }

  return { kind: "unknown", rawMessage }
}
