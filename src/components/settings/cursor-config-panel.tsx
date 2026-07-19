"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import {
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Eye,
  EyeOff,
  Loader2,
  Plus,
  RefreshCw,
  Save,
  ShieldCheck,
  Trash2,
} from "lucide-react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import {
  acpCursorAuthStatus,
  acpCursorListModels,
  acpUpdateAgentConfig,
} from "@/lib/api"
import type { AcpAgentInfo, CursorAuthStatus } from "@/lib/types"
import { cn } from "@/lib/utils"

const CURSOR_API_KEY_ENV = "CURSOR_API_KEY"
const CURSOR_API_BASE_URL_ENV = "CURSOR_API_BASE_URL"
const CURSOR_MODEL_ENV = "CURSOR_MODEL"
/** codeg-side launch knob: "1" inserts the CLI's root `--force` flag (Run
 * Everything) before the `acp` subcommand. The CLI reads no such env var. */
const CURSOR_FORCE_ENV = "CURSOR_FORCE"

const UNSET = "__unset__"
const CUSTOM = "__custom__"

/** Build the env map to persist for Cursor: set-or-delete the API key,
 * endpoint override, default-model launch knob, and the Run Everything
 * (`--force`) launch knob; unrelated keys are preserved untouched. */
export function buildCursorEnv(
  prevEnv: Record<string, string>,
  apiKey: string,
  baseUrl: string,
  model: string,
  force: boolean
): Record<string, string> {
  const env: Record<string, string> = { ...prevEnv }
  const setOrDelete = (key: string, value: string) => {
    const trimmed = value.trim()
    if (trimmed) {
      env[key] = trimmed
    } else {
      delete env[key]
    }
  }
  setOrDelete(CURSOR_API_KEY_ENV, apiKey)
  setOrDelete(CURSOR_API_BASE_URL_ENV, baseUrl.replace(/\/+$/, ""))
  setOrDelete(CURSOR_MODEL_ENV, model)
  setOrDelete(CURSOR_FORCE_ENV, force ? "1" : "")
  return env
}

/** The saved env's Run Everything knob, tolerant of hand-edited values. */
export function isCursorForceEnabled(env: Record<string, string>): boolean {
  const value = (env[CURSOR_FORCE_ENV] ?? "").trim().toLowerCase()
  return value === "1" || value === "true"
}

/** One editable permission-rule row list (allow or deny). */
function RuleListEditor({
  rules,
  onChange,
  placeholder,
  addLabel,
  disabled,
  tone,
}: {
  rules: string[]
  onChange: (rules: string[]) => void
  placeholder: string
  addLabel: string
  disabled: boolean
  tone: "allow" | "deny"
}) {
  return (
    <div className="space-y-1.5">
      {rules.map((rule, index) => (
        <div className="flex items-center gap-1.5" key={index}>
          <Input
            className={cn(
              "h-7 flex-1 font-mono text-xs",
              tone === "deny" && "border-destructive/40"
            )}
            disabled={disabled}
            onChange={(e) => {
              const next = [...rules]
              next[index] = e.target.value
              onChange(next)
            }}
            placeholder={placeholder}
            value={rule}
          />
          <Button
            className="h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive"
            disabled={disabled}
            onClick={() => onChange(rules.filter((_, i) => i !== index))}
            size="icon"
            type="button"
            variant="ghost"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
      ))}
      <Button
        className="h-7 gap-1 px-2 text-xs"
        disabled={disabled}
        onClick={() => onChange([...rules, ""])}
        size="sm"
        type="button"
        variant="outline"
      >
        <Plus className="h-3 w-3" />
        {addLabel}
      </Button>
    </div>
  )
}

/**
 * Dedicated settings panel for Cursor (cursor-agent CLI), four cards saved by
 * ONE button (the raw-JSON card keeps its own):
 *
 * 1. **Auth** — live probe of `cursor-agent status --format json` with a
 *    `cursor-agent login` walkthrough, plus the CURSOR_API_KEY /
 *    CURSOR_API_BASE_URL env alternative (generic per-agent env path).
 * 2. **Default model** — `cursor-agent models` populates a picker
 *    (auto-loaded once authenticated); the choice is stored as the
 *    CURSOR_MODEL env knob, which the launch path passes as the CLI's root
 *    `--model` flag.
 * 3. **Permissions & sandbox** — the permission mode (default prompts vs Run
 *    Everything via the root `--force` flag), `sandbox.mode`, and a visual
 *    editor for `~/.cursor/cli-config.json`'s `permissions.allow/deny` rules
 *    (merged server-side so the CLI's own keys are preserved). There is no
 *    approval-mode key here on purpose: the CLI keeps approval mode in each
 *    chat's store.db metadata, seeded by launch flags — a cli-config.json
 *    `approvalMode` key is never read.
 * 4. **Advanced** — the raw cli-config.json for whole-file edits.
 */
export function CursorConfigPanel({
  agent,
  saving,
  onSaveEnv,
  onSaved,
  onAffectedSessions,
}: {
  agent: AcpAgentInfo
  saving: boolean
  onSaveEnv: (env: Record<string, string>, enabled: boolean) => Promise<unknown>
  onSaved: () => void
  /** Reports how many running sessions a cli-config.json write marked
   * restart-required (the env step reports its own count internally). */
  onAffectedSessions: (count: number) => void
}) {
  const t = useTranslations("AcpAgentSettings")

  // --- auth card state ---
  const [auth, setAuth] = useState<CursorAuthStatus | null>(null)
  const [authLoading, setAuthLoading] = useState(false)
  const [copied, setCopied] = useState(false)
  const [apiKey, setApiKey] = useState(
    () => agent.env[CURSOR_API_KEY_ENV] ?? ""
  )
  const [baseUrl, setBaseUrl] = useState(
    () => agent.env[CURSOR_API_BASE_URL_ENV] ?? ""
  )
  const [showKey, setShowKey] = useState(false)

  // --- model card state ---
  const initialModel = agent.env[CURSOR_MODEL_ENV] ?? ""
  const [model, setModel] = useState(initialModel)
  const [models, setModels] = useState<string[]>([])
  const [modelsError, setModelsError] = useState<string | null>(null)
  const [modelsLoading, setModelsLoading] = useState(false)
  const [modelsLoaded, setModelsLoaded] = useState(false)
  const [customModel, setCustomModel] = useState(false)

  // --- permissions card state ---
  const settings = agent.cursor_settings
  const [force, setForce] = useState(() => isCursorForceEnabled(agent.env))
  const [sandboxMode, setSandboxMode] = useState(
    () => settings?.sandbox_mode ?? ""
  )
  const [allowRules, setAllowRules] = useState<string[]>(
    () => settings?.permissions_allow ?? []
  )
  const [denyRules, setDenyRules] = useState<string[]>(
    () => settings?.permissions_deny ?? []
  )

  // --- unified save state (auth + model + permissions) ---
  const [savingAll, setSavingAll] = useState(false)

  // --- advanced card state ---
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [rawConfig, setRawConfig] = useState(
    () => agent.cursor_cli_config_json ?? ""
  )
  const [savingRaw, setSavingRaw] = useState(false)

  const mountedRef = useRef(true)
  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  const refreshAuth = useCallback(async () => {
    setAuthLoading(true)
    try {
      const status = await acpCursorAuthStatus()
      if (mountedRef.current) setAuth(status)
    } catch {
      // Probe failures already surface through `auth.error`; a transport-level
      // failure just leaves the card in its unknown state.
    } finally {
      if (mountedRef.current) setAuthLoading(false)
    }
  }, [])

  useEffect(() => {
    void refreshAuth()
  }, [refreshAuth])

  const loadModels = useCallback(async () => {
    setModelsLoading(true)
    setModelsError(null)
    try {
      const result = await acpCursorListModels()
      if (!mountedRef.current) return
      setModels(result.models)
      setModelsError(result.error)
      setModelsLoaded(true)
    } catch (e) {
      if (mountedRef.current) {
        setModelsError(e instanceof Error ? e.message : String(e))
        setModelsLoaded(true)
      }
    } finally {
      if (mountedRef.current) setModelsLoading(false)
    }
  }, [])

  const authState: "loading" | "missing" | "ok" | "unauthenticated" =
    authLoading && !auth
      ? "loading"
      : !auth || !auth.installed
        ? "missing"
        : auth.is_authenticated
          ? "ok"
          : "unauthenticated"

  // The picker is only useful populated — fetch the model list once as soon
  // as the CLI reports an authenticated account instead of waiting for a
  // manual "load" click.
  useEffect(() => {
    if (authState !== "ok" || modelsLoaded || modelsLoading) return
    void loadModels()
  }, [authState, loadModels, modelsLoaded, modelsLoading])

  const copyLoginCommand = useCallback(async () => {
    try {
      await navigator.clipboard.writeText("cursor-agent login")
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      // Clipboard may be unavailable (permissions); the command stays visible.
    }
  }, [])

  /** One save for auth + model (env) and permissions (cli-config.json). */
  const saveAll = useCallback(async () => {
    setSavingAll(true)
    const prevEnv = agent.env
    try {
      await onSaveEnv(
        buildCursorEnv(prevEnv, apiKey, baseUrl, model, force),
        agent.enabled
      )
      try {
        const affected = await acpUpdateAgentConfig(agent.agent_type, {
          cursor_structured: {
            sandboxMode,
            permissionsAllow: allowRules,
            permissionsDeny: denyRules,
          },
        })
        onAffectedSessions(affected)
      } catch (e) {
        // A failed rules write must not leave the freshly-saved permission
        // knobs behind — e.g. Run Everything enabled while the new deny
        // rules never landed. Put the env back exactly as it was; if even
        // the rollback fails the error toast below still fires.
        await onSaveEnv(prevEnv, agent.enabled).catch(() => {})
        throw e
      }
      toast.success(t("toasts.cursorSaved"))
      onSaved()
    } catch (e) {
      toast.error(
        `${t("toasts.saveCursorConfigFailed")}: ${
          e instanceof Error ? e.message : String(e)
        }`
      )
    } finally {
      if (mountedRef.current) setSavingAll(false)
    }
  }, [
    agent.agent_type,
    agent.enabled,
    agent.env,
    allowRules,
    apiKey,
    baseUrl,
    denyRules,
    force,
    model,
    onAffectedSessions,
    onSaveEnv,
    onSaved,
    sandboxMode,
    t,
  ])

  const saveRaw = useCallback(async () => {
    setSavingRaw(true)
    try {
      const affected = await acpUpdateAgentConfig(agent.agent_type, {
        cursor_cli_config_json: rawConfig,
      })
      onAffectedSessions(affected)
      toast.success(t("toasts.cursorSaved"))
      onSaved()
    } catch (e) {
      toast.error(
        `${t("toasts.saveCursorConfigFailed")}: ${
          e instanceof Error ? e.message : String(e)
        }`
      )
    } finally {
      if (mountedRef.current) setSavingRaw(false)
    }
  }, [agent.agent_type, onAffectedSessions, onSaved, rawConfig, t])

  const modelSelectValue = useMemo(() => {
    if (customModel) return CUSTOM
    if (!model) return UNSET
    return models.includes(model) ? model : CUSTOM
  }, [customModel, model, models])

  const busy = saving || savingAll

  return (
    <div className="space-y-3 rounded-md border bg-muted/10 p-3">
      <div>
        <label className="text-xs font-medium">{t("configManagement")}</label>
        <p className="mt-1 text-[11px] text-muted-foreground">
          {t("cursor.configDescription")}
        </p>
      </div>

      {/* ---- Auth card ---- */}
      <div className="space-y-2 rounded-md border bg-background/60 p-2.5">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[11px] font-medium">
            {t("cursor.authTitle")}
          </span>
          <div className="flex items-center gap-1.5">
            <span
              className={cn(
                "inline-flex h-2 w-2 rounded-full",
                authState === "ok" && "bg-emerald-500",
                authState === "unauthenticated" && "bg-amber-500",
                authState === "missing" && "bg-muted-foreground/40",
                authState === "loading" &&
                  "bg-muted-foreground/40 animate-pulse"
              )}
            />
            <span className="text-[11px] text-muted-foreground">
              {authState === "loading"
                ? t("cursor.authChecking")
                : authState === "missing"
                  ? t("cursor.authNotInstalled")
                  : authState === "ok"
                    ? (auth?.email ?? t("cursor.authLoggedIn"))
                    : t("cursor.authNotLoggedIn")}
            </span>
            {authState === "ok" && auth?.membership ? (
              <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-[10px] text-emerald-600 dark:text-emerald-400">
                {auth.membership}
              </span>
            ) : null}
            <Button
              className="h-6 w-6"
              disabled={authLoading}
              onClick={() => void refreshAuth()}
              size="icon"
              type="button"
              variant="ghost"
            >
              <RefreshCw
                className={cn("h-3 w-3", authLoading && "animate-spin")}
              />
            </Button>
          </div>
        </div>

        {authState !== "ok" ? (
          <div className="space-y-1.5">
            <p className="text-[11px] text-muted-foreground">
              {t("cursor.loginHint")}
            </p>
            <div className="flex items-center gap-1.5">
              <code className="flex-1 rounded bg-muted px-2 py-1 font-mono text-[11px]">
                cursor-agent login
              </code>
              <Button
                className="h-6 w-6"
                onClick={() => void copyLoginCommand()}
                size="icon"
                type="button"
                variant="ghost"
              >
                {copied ? (
                  <Check className="h-3 w-3 text-emerald-500" />
                ) : (
                  <Copy className="h-3 w-3" />
                )}
              </Button>
            </div>
          </div>
        ) : null}
        {auth?.error ? (
          <p className="text-[11px] text-destructive">{auth.error}</p>
        ) : null}

        <div className="grid gap-2 md:grid-cols-2">
          <div className="space-y-1">
            <label className="text-[11px] text-muted-foreground">
              {t("cursor.apiKeyLabel")}
            </label>
            <div className="flex items-center gap-1.5">
              <Input
                className="h-7 flex-1 text-xs"
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={t("cursor.apiKeyPlaceholder")}
                type={showKey ? "text" : "password"}
                value={apiKey}
              />
              <Button
                className="h-7 w-7 shrink-0"
                onClick={() => setShowKey((v) => !v)}
                size="icon"
                type="button"
                variant="ghost"
              >
                {showKey ? (
                  <EyeOff className="h-3.5 w-3.5" />
                ) : (
                  <Eye className="h-3.5 w-3.5" />
                )}
              </Button>
            </div>
            <p className="text-[10px] text-muted-foreground">
              {t("cursor.apiKeyHint")}
            </p>
          </div>
          <div className="space-y-1">
            <label className="text-[11px] text-muted-foreground">
              {t("cursor.baseUrlLabel")}
            </label>
            <Input
              className="h-7 text-xs"
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://api2.cursor.sh"
              value={baseUrl}
            />
            <p className="text-[10px] text-muted-foreground">
              {t("cursor.baseUrlHint")}
            </p>
          </div>
        </div>
      </div>

      {/* ---- Default model card ---- */}
      <div className="space-y-2 rounded-md border bg-background/60 p-2.5">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[11px] font-medium">
            {t("cursor.modelTitle")}
          </span>
          <Button
            className="h-6 gap-1 px-2 text-[11px]"
            disabled={modelsLoading}
            onClick={() => void loadModels()}
            size="sm"
            type="button"
            variant="ghost"
          >
            {modelsLoading ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : (
              <RefreshCw className="h-3 w-3" />
            )}
            {t("cursor.loadModels")}
          </Button>
        </div>
        <div className="grid gap-2 md:grid-cols-2">
          <Select
            onValueChange={(value) => {
              if (value === CUSTOM) {
                setCustomModel(true)
                return
              }
              setCustomModel(false)
              setModel(value === UNSET ? "" : value)
            }}
            value={modelSelectValue}
          >
            <SelectTrigger className="h-7 w-full text-xs" size="sm">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem className="text-xs" value={UNSET}>
                {t("cursor.modelDefault")}
              </SelectItem>
              {models.map((m) => (
                <SelectItem className="text-xs" key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
              <SelectItem className="text-xs" value={CUSTOM}>
                {t("cursor.modelCustom")}
              </SelectItem>
            </SelectContent>
          </Select>
          {modelSelectValue === CUSTOM ? (
            <Input
              className="h-7 font-mono text-xs"
              onChange={(e) => setModel(e.target.value)}
              placeholder={t("cursor.modelCustomPlaceholder")}
              value={model}
            />
          ) : null}
        </div>
        {modelsError ? (
          <p className="text-[10px] text-muted-foreground">
            {t("cursor.modelsUnavailable")}: {modelsError}
          </p>
        ) : modelsLoaded && models.length === 0 ? (
          <p className="text-[10px] text-muted-foreground">
            {t("cursor.modelsEmpty")}
          </p>
        ) : null}
        <p className="text-[10px] text-muted-foreground">
          {t("cursor.modelHint")}
        </p>
      </div>

      {/* ---- Permissions & sandbox card ---- */}
      <div className="space-y-2 rounded-md border bg-background/60 p-2.5">
        <div className="flex items-center gap-1.5">
          <ShieldCheck className="h-3.5 w-3.5 text-muted-foreground" />
          <span className="text-[11px] font-medium">
            {t("cursor.permissionsTitle")}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground">
          {t("cursor.permissionsDescription")}
        </p>

        <div className="grid gap-2 md:grid-cols-2">
          <div className="space-y-1">
            <label className="text-[11px] text-muted-foreground">
              {t("cursor.permissionModeLabel")}
            </label>
            <Select
              onValueChange={(value) => setForce(value === "force")}
              value={force ? "force" : "default"}
            >
              <SelectTrigger className="h-7 w-full text-xs" size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem className="text-xs" value="default">
                  {t("cursor.permissionModeDefault")}
                </SelectItem>
                <SelectItem className="text-xs" value="force">
                  {t("cursor.permissionModeForce")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1">
            <label className="text-[11px] text-muted-foreground">
              {t("cursor.sandboxLabel")}
            </label>
            <Select
              onValueChange={(value) =>
                setSandboxMode(value === UNSET ? "" : value)
              }
              value={sandboxMode || UNSET}
            >
              <SelectTrigger className="h-7 w-full text-xs" size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem className="text-xs" value={UNSET}>
                  {t("cursor.optionDefault")}
                </SelectItem>
                <SelectItem className="text-xs" value="enabled">
                  {t("cursor.sandboxEnabled")}
                </SelectItem>
                <SelectItem className="text-xs" value="disabled">
                  {t("cursor.sandboxDisabled")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>
        <p className="text-[10px] text-muted-foreground">
          {t("cursor.permissionModeHint")}
        </p>

        <div className="grid gap-3 md:grid-cols-2">
          <div className="space-y-1">
            <label className="text-[11px] text-muted-foreground">
              {t("cursor.allowRulesLabel")}
            </label>
            <RuleListEditor
              addLabel={t("cursor.addRule")}
              disabled={busy}
              onChange={setAllowRules}
              placeholder="Shell(git)"
              rules={allowRules}
              tone="allow"
            />
          </div>
          <div className="space-y-1">
            <label className="text-[11px] text-muted-foreground">
              {t("cursor.denyRulesLabel")}
            </label>
            <RuleListEditor
              addLabel={t("cursor.addRule")}
              disabled={busy}
              onChange={setDenyRules}
              placeholder="Read(.env*)"
              rules={denyRules}
              tone="deny"
            />
          </div>
        </div>
        <p className="text-[10px] text-muted-foreground">
          {t("cursor.rulesSyntaxHint")}
        </p>
      </div>

      {/* ---- One save for auth + model + permissions ---- */}
      <div className="flex justify-end">
        <Button
          className="h-7 gap-1.5 px-2.5 text-xs"
          disabled={busy}
          onClick={() => void saveAll()}
          size="sm"
          type="button"
        >
          {busy ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Save className="h-3.5 w-3.5" />
          )}
          {t("cursor.saveConfig")}
        </Button>
      </div>

      {/* ---- Advanced: raw cli-config.json ---- */}
      <div className="space-y-2">
        <button
          className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground"
          onClick={() => setAdvancedOpen((v) => !v)}
          type="button"
        >
          {advancedOpen ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
          {t("cursor.advancedToggle")}
        </button>
        {advancedOpen ? (
          <div className="space-y-1.5">
            <p className="text-[10px] text-muted-foreground">
              {t("cursor.advancedHint")}
            </p>
            <Textarea
              className="min-h-40 font-mono text-[11px]"
              onChange={(e) => setRawConfig(e.target.value)}
              spellCheck={false}
              value={rawConfig}
            />
            <div className="flex justify-end">
              <Button
                className="h-7 gap-1.5 px-2.5 text-xs"
                disabled={savingRaw || !rawConfig.trim()}
                onClick={() => void saveRaw()}
                size="sm"
                type="button"
                variant="outline"
              >
                {savingRaw ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Save className="h-3.5 w-3.5" />
                )}
                {t("cursor.saveRawConfig")}
              </Button>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  )
}
