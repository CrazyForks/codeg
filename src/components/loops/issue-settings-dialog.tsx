"use client"

import { useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2, X } from "lucide-react"

import { updateLoopIssueConfig } from "@/lib/loops-api"
import { toErrorMessage } from "@/lib/app-error"
import { defaultIssueConfig } from "@/lib/loop-config"
import type { IssueConfig, LoopIssueDetail } from "@/lib/types"
import {
  LoopConfigForm,
  type LoopConfigFormState,
  configToFormState,
  formStateToConfig,
} from "@/components/loops/loop-config-form"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"

/** Empty / non-positive → null (unlimited); otherwise the floored integer. */
function parsePositiveOrNull(s: string): number | null {
  const n = Number(s.trim())
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : null
}

function budgetField(n: number | null | undefined): string {
  return n == null ? "" : String(n)
}

/**
 * Editor for a single issue's config, rendered as an in-detail panel that
 * overlays the issue body (mounted only while open, so a close + reopen discards
 * unsaved edits — no `open`-gated re-seed needed). The issue either inherits the
 * space default (read-only preview, resolved at read time by the engine) or uses
 * a custom `IssueConfig` edited through the shared tabbed {@link LoopConfigForm}.
 * The total token budget is per-issue and always editable. Saving persists via
 * `update_loop_issue_config`, which emits `loop://changed` so the detail view
 * refreshes; the engine reads config fresh each dispatch, so edits to a running
 * issue take effect from its next iteration (surfaced as a hint).
 */
export function IssueSettingsPanel({
  issue,
  spaceDefaultConfig = null,
  onClose,
}: {
  issue: LoopIssueDetail
  spaceDefaultConfig?: IssueConfig | null
  onClose: () => void
}) {
  const t = useTranslations("Loops.issueSettings")
  const tCommon = useTranslations("Loops.common")
  const tToasts = useTranslations("Loops.toasts")

  // `config == null` ⇒ the issue inherits the space default. Seeded once on
  // mount; the panel remounts on each open, so no re-seed effect is needed.
  const [inherit, setInherit] = useState(issue.config == null)
  const [form, setForm] = useState<LoopConfigFormState>(() =>
    configToFormState(
      issue.config ?? spaceDefaultConfig ?? defaultIssueConfig()
    )
  )
  const [tokenBudget, setTokenBudget] = useState(() =>
    budgetField(issue.token_budget)
  )
  const [saving, setSaving] = useState(false)

  // Read-only preview of what an inheriting issue resolves to: the space
  // default, or the engine default when the space has none.
  const inheritedForm = configToFormState(
    spaceDefaultConfig ?? defaultIssueConfig()
  )

  const onSave = async () => {
    setSaving(true)
    try {
      // `null` config = inherit the space default (stored as NULL); otherwise
      // the issue's own config. The total token budget is independent.
      await updateLoopIssueConfig(
        issue.id,
        inherit ? null : formStateToConfig(form),
        parsePositiveOrNull(tokenBudget)
      )
      toast.success(tToasts("configSaved"))
      onClose()
    } catch (err) {
      toast.error(tToasts("actionFailed", { message: toErrorMessage(err) }))
    } finally {
      setSaving(false)
    }
  }

  const segBtn = (active: boolean) =>
    "flex-1 rounded-md px-3 py-1 text-xs font-medium transition-colors " +
    (active
      ? "bg-background text-foreground shadow-sm"
      : "text-muted-foreground hover:text-foreground")

  return (
    <div className="absolute inset-0 z-20 flex flex-col bg-background">
      <div className="flex shrink-0 items-start justify-between gap-2 border-b px-5 py-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold">{t("title")}</h3>
          <p className="text-xs text-muted-foreground">{t("description")}</p>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-7 w-7 shrink-0"
          onClick={onClose}
          aria-label={tCommon("cancel")}
        >
          <X className="h-4 w-4" />
        </Button>
      </div>

      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-5 py-4">
        {issue.status === "running" && (
          <p className="rounded-md bg-muted px-3 py-2 text-xs text-muted-foreground">
            {t("runningHint")}
          </p>
        )}

        <div className="flex items-center gap-1 rounded-lg bg-muted p-1">
          <button
            type="button"
            className={segBtn(inherit)}
            onClick={() => setInherit(true)}
          >
            {t("useSpaceDefault")}
          </button>
          <button
            type="button"
            className={segBtn(!inherit)}
            onClick={() => setInherit(false)}
          >
            {t("custom")}
          </button>
        </div>
        {inherit && (
          <p className="text-xs text-muted-foreground">{t("inheritHint")}</p>
        )}

        <LoopConfigForm
          value={inherit ? inheritedForm : form}
          onChange={setForm}
          disabled={inherit}
          limitsExtra={
            // The per-issue total budget lives outside IssueConfig and is always
            // editable (even when the config inherits the space default), so it's
            // rendered here rather than gated by the form's `disabled`.
            <div className="space-y-1.5 border-t pt-3">
              <Label htmlFor="total-budget">{t("tokenBudget")}</Label>
              <Input
                id="total-budget"
                type="number"
                min={1}
                value={tokenBudget}
                onChange={(e) => setTokenBudget(e.target.value)}
                placeholder={t("unlimitedPlaceholder")}
                className="h-8"
              />
              <p className="text-xs text-muted-foreground">
                {t("tokenBudgetHint")}
              </p>
            </div>
          }
        />
      </div>

      <div className="flex shrink-0 items-center justify-end gap-2 border-t px-5 py-3">
        <Button
          type="button"
          variant="outline"
          onClick={onClose}
          disabled={saving}
        >
          {tCommon("cancel")}
        </Button>
        <Button type="button" onClick={onSave} disabled={saving}>
          {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          {t("save")}
        </Button>
      </div>
    </div>
  )
}
