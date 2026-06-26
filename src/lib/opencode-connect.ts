/**
 * Pure model for OpenCode's "connect a provider" flow.
 *
 * OpenCode reads two files (both managed by codeg):
 *  - `opencode.json` — non-secret provider definitions (`provider.<id>` with
 *    `npm` / `name` / `options.baseURL` / `models`), plus `model` /
 *    `small_model` / `disabled_providers`.
 *  - `auth.json` — credentials keyed by provider id: `{ type: "api", key }`,
 *    `{ type: "oauth", ... }`, or `{ type: "wellknown", ... }`.
 *
 * Well-known providers (present in the models.dev catalog) only need a
 * credential in `auth.json`; their models auto-load from models.dev. Custom /
 * OpenAI-compatible providers additionally need a `provider.<id>` block.
 *
 * Secrets live ONLY in `auth.json` — these functions never write an API key
 * into `opencode.json`.
 *
 * Every function is pure: it parses the given JSON text, mutates a clone, and
 * returns new JSON text. An empty resulting object serializes to "" so callers
 * can treat "" as "no file".
 */

import type { OpenCodeCatalogProvider } from "./types"

export const DEFAULT_OPENAI_COMPATIBLE_NPM = "@ai-sdk/openai-compatible"

const PROVIDER_ID_PATTERN = /^[A-Za-z0-9_.-]+$/

/** Why a proposed custom-provider id is invalid, or null when it's acceptable. */
export type CustomProviderIdIssue = "pattern" | "exists" | "in-catalog"

/**
 * Validate an id for a NEW custom (OpenAI-compatible) provider. A catalog id is
 * rejected with "in-catalog": those connect through the catalog dialog, and a
 * custom block for a catalog id would render in the well-known list rather than
 * the custom section. NOTE: when `catalogIds` is empty (catalog still loading or
 * failed) the "in-catalog" check cannot fire — callers must gate entry to the
 * custom dialog on catalog readiness so the check runs against a known set.
 */
export function customProviderIdIssue(params: {
  id: string
  existingProviderIds: string[]
  catalogIds: string[]
}): CustomProviderIdIssue | null {
  const id = params.id.trim()
  if (!id) return null
  if (!PROVIDER_ID_PATTERN.test(id)) return "pattern"
  if (params.existingProviderIds.includes(id)) return "exists"
  if (params.catalogIds.includes(id)) return "in-catalog"
  return null
}

export type OpenCodeAuthKind = "api" | "oauth" | "none"

export interface OpenCodeConnectedProvider {
  id: string
  /** Display name: config `name` › catalog `name` › id. */
  name: string
  /** Credential type recorded in auth.json (or "none" when only a config block exists). */
  authKind: OpenCodeAuthKind
  /** Whether the provider is enabled (not in `disabled_providers`, and allowed by `enabled_providers`). */
  enabled: boolean
  /** Whether the provider exists in the models.dev catalog. */
  inCatalog: boolean
  /** Whether `opencode.json` carries a `provider.<id>` block. */
  hasConfigBlock: boolean
  baseUrl: string
  npm: string
  /** Model ids defined in the config block (custom models). */
  modelIds: string[]
}

export interface OpenCodeModelOption {
  value: string
  label: string
  /** Context window (tokens) from the catalog, when known. */
  context?: number | null
  /** Whether the model is a reasoning model, from the catalog. */
  reasoning?: boolean
}

export interface OpenCodeModelOptionGroup {
  providerId: string
  label: string
  models: OpenCodeModelOption[]
}

/** Compact context-window label, e.g. 200000 → "200K", 1000000 → "1M". */
export function formatContextWindow(tokens: number): string {
  if (!Number.isFinite(tokens) || tokens <= 0) return ""
  if (tokens >= 1_000_000) {
    const millions = tokens / 1_000_000
    return `${Number.isInteger(millions) ? millions : millions.toFixed(1)}M`
  }
  if (tokens >= 1000) return `${Math.round(tokens / 1000)}K`
  return String(tokens)
}

// ─── JSON helpers (immutable) ───

function parseObject(text: string): Record<string, unknown> {
  if (!text || !text.trim()) return {}
  try {
    const value = JSON.parse(text)
    return value && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {}
  } catch {
    return {}
  }
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function stringify(obj: Record<string, unknown>): string {
  return Object.keys(obj).length === 0 ? "" : JSON.stringify(obj, null, 2)
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((x): x is string => typeof x === "string")
    : []
}

// ─── Connect (API key) ───

export interface ApiKeyConnectParams {
  configText: string
  authJsonText: string
  providerId: string
  apiKey: string
  /**
   * Present for custom / OpenAI-compatible providers that need a config block.
   * Omit for well-known providers (catalog drives their models).
   */
  custom?: {
    name?: string
    npm?: string
    baseUrl?: string
    modelIds?: string[]
  }
  /** Optional base URL override for a well-known provider (proxy/custom endpoint). */
  baseUrlOverride?: string
}

/** Whether a `model`/`small_model` value targets the given provider. */
export function modelReferencesProvider(
  model: unknown,
  providerId: string
): boolean {
  if (typeof model !== "string") return false
  const trimmed = model.trim()
  return trimmed === providerId || trimmed.startsWith(`${providerId}/`)
}

/** Drop `model`/`small_model` entries that point at the given provider. */
function clearModelReferences(
  config: Record<string, unknown>,
  providerId: string
): void {
  for (const key of ["model", "small_model", "smallModel", "small-model"]) {
    if (modelReferencesProvider(config[key], providerId)) delete config[key]
  }
}

/**
 * Remove any `provider.<id>.options.apiKey` (a leaked secret) from opencode.json,
 * cleaning up an options object or provider block left empty as a result.
 * Secrets belong only in auth.json.
 */
function scrubProviderSecret(
  config: Record<string, unknown>,
  providerId: string
): void {
  const root = asObject(config.provider)
  if (!root) return
  const block = asObject(root[providerId])
  if (!block) return
  const options = asObject(block.options)
  if (options && "apiKey" in options) {
    delete options.apiKey
    if (Object.keys(options).length === 0) delete block.options
  }
  if (Object.keys(block).length === 0) {
    delete root[providerId]
    if (Object.keys(root).length === 0) delete config.provider
  }
}

/**
 * Connect a provider with an API key. Writes the credential to `auth.json` and,
 * for custom providers (or a well-known base-URL override), the non-secret
 * provider block to `opencode.json`. Throws on an empty provider id.
 */
export function applyApiKeyConnect(params: ApiKeyConnectParams): {
  configText: string
  authJsonText: string
} {
  const id = params.providerId.trim()
  if (!id) throw new Error("provider id is required")

  const auth = parseObject(params.authJsonText)
  const key = params.apiKey.trim()
  if (key) {
    auth[id] = { type: "api", key }
  }

  const config = parseObject(params.configText)

  const writeProviderBlock = (
    mutate: (block: Record<string, unknown>) => void
  ) => {
    const providerRoot = asObject(config.provider) ?? {}
    const existing = asObject(providerRoot[id]) ?? {}
    mutate(existing)
    providerRoot[id] = existing
    config.provider = providerRoot
  }

  if (params.custom) {
    const { name, npm, baseUrl, modelIds } = params.custom
    writeProviderBlock((block) => {
      block.npm =
        npm?.trim() ||
        (typeof block.npm === "string" && block.npm.trim()) ||
        DEFAULT_OPENAI_COMPATIBLE_NPM
      block.name =
        name?.trim() || (typeof block.name === "string" && block.name) || id
      const options = asObject(block.options) ?? {}
      if (baseUrl?.trim()) options.baseURL = baseUrl.trim()
      // Secrets never belong in opencode.json.
      delete options.apiKey
      block.options = options
      const models = asObject(block.models) ?? {}
      for (const raw of modelIds ?? []) {
        const modelId = raw.trim()
        if (!modelId) continue
        if (!asObject(models[modelId])) models[modelId] = { name: modelId }
      }
      block.models = models
    })
  } else if (params.baseUrlOverride !== undefined) {
    // Well-known provider with an explicitly provided base URL override: a
    // non-empty value sets it; an empty value clears any existing override
    // (edit mode), leaving other provider options intact. An OMITTED override
    // (`undefined`) is left untouched — reconnecting/rotating a key must not
    // delete an existing base URL.
    const baseUrl = params.baseUrlOverride.trim()
    if (baseUrl) {
      writeProviderBlock((block) => {
        const options = asObject(block.options) ?? {}
        options.baseURL = baseUrl
        delete options.apiKey
        block.options = options
      })
    } else {
      const providerRoot = asObject(config.provider)
      const existing = providerRoot ? asObject(providerRoot[id]) : null
      if (existing) {
        const options = asObject(existing.options)
        if (options && "baseURL" in options) {
          delete options.baseURL
          if (Object.keys(options).length === 0) delete existing.options
        }
      }
    }
  }

  // Secrets live only in auth.json — scrub any stale options.apiKey for this
  // provider; this also removes a provider block left empty by clearing the
  // base URL override above.
  scrubProviderSecret(config, id)

  return { configText: stringify(config), authJsonText: stringify(auth) }
}

/**
 * Set (or clear, when empty) a provider's API key in auth.json, ensuring no
 * secret remains in opencode.json. Used by the advanced provider editor so its
 * API-key field never dual-writes into the config file. Throws on empty id.
 */
export function setProviderApiKey(params: {
  configText: string
  authJsonText: string
  providerId: string
  apiKey: string
}): { configText: string; authJsonText: string } {
  const id = params.providerId.trim()
  if (!id) throw new Error("provider id is required")
  const auth = parseObject(params.authJsonText)
  const key = params.apiKey.trim()
  if (key) auth[id] = { type: "api", key }
  else delete auth[id]
  const config = parseObject(params.configText)
  scrubProviderSecret(config, id)
  return { configText: stringify(config), authJsonText: stringify(auth) }
}

// ─── Disconnect ───

export interface DisconnectParams {
  configText: string
  authJsonText: string
  providerId: string
  /** Also remove the `provider.<id>` block from opencode.json (for custom providers). */
  removeConfigBlock?: boolean
}

/** Disconnect a provider: drop its credential, and optionally its config block. */
export function disconnectProvider(params: DisconnectParams): {
  configText: string
  authJsonText: string
} {
  const id = params.providerId.trim()
  const auth = parseObject(params.authJsonText)
  delete auth[id]

  const config = parseObject(params.configText)
  if (params.removeConfigBlock) {
    const providerRoot = asObject(config.provider)
    if (providerRoot) {
      delete providerRoot[id]
      if (Object.keys(providerRoot).length === 0) delete config.provider
      else config.provider = providerRoot
    }
  }
  for (const listKey of ["disabled_providers", "enabled_providers"]) {
    if (!Array.isArray(config[listKey])) continue
    const next = stringArray(config[listKey]).filter((x) => x !== id)
    if (next.length) config[listKey] = next
    else delete config[listKey]
  }
  // Don't leave model/small_model pointing at a provider we just removed.
  clearModelReferences(config, id)

  return { configText: stringify(config), authJsonText: stringify(auth) }
}

// ─── Enable / disable ───

/** Toggle a provider's enabled state via `disabled_providers` (priority over enabled_providers). */
export function setProviderEnabled(params: {
  configText: string
  providerId: string
  enabled: boolean
}): string {
  const id = params.providerId.trim()
  const config = parseObject(params.configText)
  let disabled = stringArray(config.disabled_providers)
  if (params.enabled) {
    disabled = disabled.filter((x) => x !== id)
  } else if (!disabled.includes(id)) {
    disabled = [...disabled, id]
  }
  if (disabled.length) config.disabled_providers = disabled
  else delete config.disabled_providers
  return stringify(config)
}

// ─── Secret migration (opencode.json options.apiKey → auth.json) ───

/**
 * Move any `provider.<id>.options.apiKey` from opencode.json into auth.json and
 * strip it from the config. Keeps an existing auth.json key if present. This
 * also dodges OpenCode issue #5674 (options.apiKey occasionally not forwarded)
 * by routing credentials through the canonical auth.json path.
 */
export function migrateSecretsToAuth(params: {
  configText: string
  authJsonText: string
}): { configText: string; authJsonText: string; changed: boolean } {
  const config = parseObject(params.configText)
  const providerRoot = asObject(config.provider)
  if (!providerRoot) {
    return { ...params, changed: false }
  }
  const auth = parseObject(params.authJsonText)
  let changed = false
  for (const id of Object.keys(providerRoot)) {
    const block = asObject(providerRoot[id])
    const options = block ? asObject(block.options) : null
    if (!options) continue
    const key = typeof options.apiKey === "string" ? options.apiKey.trim() : ""
    if (!key) continue
    const existing = asObject(auth[id])
    const existingKey =
      existing && typeof existing.key === "string" ? existing.key.trim() : ""
    if (!existingKey) auth[id] = { type: "api", key }
    delete options.apiKey
    changed = true
  }
  if (!changed) return { ...params, changed: false }
  return {
    configText: stringify(config),
    authJsonText: stringify(auth),
    changed: true,
  }
}

// ─── Derive connected-providers view ───

export function buildConnectedProviders(params: {
  configText: string
  authJsonText: string
  catalog: OpenCodeCatalogProvider[]
}): OpenCodeConnectedProvider[] {
  const config = parseObject(params.configText)
  const auth = parseObject(params.authJsonText)
  const providerRoot = asObject(config.provider) ?? {}
  const catalogById = new Map(params.catalog.map((p) => [p.id, p]))
  const disabled = new Set(stringArray(config.disabled_providers))
  const enabledList = stringArray(config.enabled_providers)

  const ids = new Set<string>([
    ...Object.keys(providerRoot),
    ...Object.keys(auth),
  ])

  const result: OpenCodeConnectedProvider[] = []
  for (const id of ids) {
    const block = asObject(providerRoot[id])
    const authEntry = asObject(auth[id])
    const authType =
      authEntry && typeof authEntry.type === "string" ? authEntry.type : ""
    const hasKey =
      !!authEntry &&
      typeof authEntry.key === "string" &&
      authEntry.key.trim().length > 0
    const authKind: OpenCodeAuthKind =
      authType === "oauth"
        ? "oauth"
        : hasKey || authType === "wellknown" || authType === "api"
          ? "api"
          : "none"
    const catalog = catalogById.get(id)
    const options = block ? asObject(block.options) : null
    const baseUrl =
      options && typeof options.baseURL === "string" ? options.baseURL : ""
    const npm = block && typeof block.npm === "string" ? block.npm : ""
    const models = block ? asObject(block.models) : null
    const enabled =
      !disabled.has(id) &&
      (enabledList.length === 0 || enabledList.includes(id))
    result.push({
      id,
      name:
        (block && typeof block.name === "string" && block.name) ||
        catalog?.name ||
        id,
      authKind,
      enabled,
      inCatalog: Boolean(catalog),
      hasConfigBlock: Boolean(block),
      baseUrl,
      npm,
      modelIds: models ? Object.keys(models) : [],
    })
  }

  result.sort(
    (a, b) =>
      a.name.toLowerCase().localeCompare(b.name.toLowerCase()) ||
      a.id.localeCompare(b.id)
  )
  return result
}

/**
 * Model picker options: enabled connected providers, each offering its catalog
 * models (well-known) unioned with any custom models from the config block.
 * Values are `provider/model-id`, matching OpenCode's `model` field.
 */
export function buildConnectedModelOptions(params: {
  connected: OpenCodeConnectedProvider[]
  catalog: OpenCodeCatalogProvider[]
}): OpenCodeModelOptionGroup[] {
  const catalogById = new Map(params.catalog.map((p) => [p.id, p]))
  const groups: OpenCodeModelOptionGroup[] = []
  for (const provider of params.connected) {
    if (!provider.enabled) continue
    const catalog = catalogById.get(provider.id)
    const metaById = new Map(catalog?.models.map((m) => [m.id, m]) ?? [])
    const ids = new Set<string>(provider.modelIds)
    if (catalog) for (const model of catalog.models) ids.add(model.id)
    if (ids.size === 0) continue
    groups.push({
      providerId: provider.id,
      label: provider.name,
      models: Array.from(ids)
        .sort()
        .map((modelId) => {
          const meta = metaById.get(modelId)
          return {
            value: `${provider.id}/${modelId}`,
            label: `${provider.id}/${modelId}`,
            context: meta?.context ?? null,
            reasoning: meta?.reasoning ?? false,
          }
        }),
    })
  }
  return groups
}
