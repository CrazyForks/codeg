"use client"

import { useCallback, useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { Ban, Loader2, Pause, Play, Settings2 } from "lucide-react"
import { toast } from "sonner"

import {
  cancelLoopIssue,
  getLoopDag,
  getLoopIssue,
  pauseLoopIssue,
  resumeLoopIssue,
  triggerLoopIssue,
} from "@/lib/loops-api"
import type { LoopArtifactRow, LoopIssueDetail } from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"
import { useLoopChanged } from "@/hooks/use-loop-changed"
import { Button } from "@/components/ui/button"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import {
  IssuePriorityBadge,
  IssueRouteBadge,
  IssueStatusBadge,
} from "@/components/loops/issue-badges"

export function IssueDetail({ issueId }: { issueId: number | null }) {
  const t = useTranslations("Loops.issueDetail")
  const tList = useTranslations("Loops.issueList")
  const tCommon = useTranslations("Loops.common")
  const tToasts = useTranslations("Loops.toasts")

  const [issue, setIssue] = useState<LoopIssueDetail | null>(null)
  const [artifacts, setArtifacts] = useState<LoopArtifactRow[]>([])
  const [loading, setLoading] = useState(false)
  const [actionBusy, setActionBusy] = useState(false)
  const [cancelOpen, setCancelOpen] = useState(false)

  const refresh = useCallback(async () => {
    if (issueId == null) {
      setIssue(null)
      setArtifacts([])
      return
    }
    setLoading(true)
    try {
      const [detail, dag] = await Promise.all([
        getLoopIssue(issueId),
        getLoopDag(issueId),
      ])
      setIssue(detail)
      setArtifacts(dag.artifacts)
    } finally {
      setLoading(false)
    }
  }, [issueId])

  useEffect(() => {
    void refresh()
  }, [refresh])

  useLoopChanged(() => {
    void refresh()
  }, issue?.space_id)

  // Run an engine action; the resulting `loop://changed` event refreshes the
  // view. `onOk` carries any success-only side effect (e.g. a toast).
  const runAction = useCallback(
    async (action: () => Promise<void>, onOk?: () => void) => {
      setActionBusy(true)
      try {
        await action()
        onOk?.()
      } catch (err) {
        toast.error(tToasts("actionFailed", { message: toErrorMessage(err) }))
      } finally {
        setActionBusy(false)
      }
    },
    [tToasts]
  )

  if (issueId == null) {
    return (
      <div className="flex h-full items-center justify-center px-6 text-center text-sm text-muted-foreground">
        {t("selectPrompt")}
      </div>
    )
  }

  if (loading && !issue) {
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        <Loader2 className="h-5 w-5 animate-spin" />
      </div>
    )
  }

  if (!issue) return null

  const budget = issue.token_budget
  const tokenText =
    budget != null
      ? t("tokenWithBudget", {
          used: issue.token_used.toLocaleString(),
          budget: budget.toLocaleString(),
        })
      : issue.token_used.toLocaleString()

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden">
      {/* Row ① — title + token usage + actions */}
      <div className="flex shrink-0 items-start justify-between gap-3 px-5 pt-4 pb-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="shrink-0 font-mono text-xs text-muted-foreground">
              #{issue.seq_no}
            </span>
            <h2 className="truncate text-base font-semibold">{issue.title}</h2>
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-1">
            <IssueStatusBadge status={issue.status} />
            <IssuePriorityBadge priority={issue.priority} />
            <IssueRouteBadge route={issue.route} />
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-3">
          <div className="text-right text-xs text-muted-foreground">
            <div>{t("tokenUsage")}</div>
            <div className="font-mono text-sm text-foreground">{tokenText}</div>
          </div>
          {issue.status === "pending" && (
            <Button
              size="sm"
              className="h-8"
              disabled={actionBusy}
              onClick={() =>
                runAction(
                  () => triggerLoopIssue(issue.id),
                  () =>
                    toast.success(
                      tToasts("issueTriggered", { title: issue.title })
                    )
                )
              }
            >
              {actionBusy ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <Play className="mr-1 h-3.5 w-3.5" />
              )}
              {tList("trigger")}
            </Button>
          )}
          {issue.status === "running" && (
            <Button
              size="sm"
              variant="outline"
              className="h-8"
              disabled={actionBusy}
              onClick={() => runAction(() => pauseLoopIssue(issue.id))}
            >
              <Pause className="mr-1 h-3.5 w-3.5" />
              {tList("pause")}
            </Button>
          )}
          {issue.status === "paused" && (
            <Button
              size="sm"
              className="h-8"
              disabled={actionBusy}
              onClick={() => runAction(() => resumeLoopIssue(issue.id))}
            >
              <Play className="mr-1 h-3.5 w-3.5" />
              {tList("resume")}
            </Button>
          )}
          {(issue.status === "running" ||
            issue.status === "paused" ||
            issue.status === "blocked") && (
            <Button
              size="sm"
              variant="ghost"
              className="h-8 text-destructive hover:text-destructive"
              disabled={actionBusy}
              onClick={() => setCancelOpen(true)}
            >
              <Ban className="mr-1 h-3.5 w-3.5" />
              {tList("cancel")}
            </Button>
          )}
          <Button size="icon" variant="ghost" className="h-8 w-8" disabled>
            <Settings2 className="h-4 w-4" />
            <span className="sr-only">{t("settings")}</span>
          </Button>
        </div>
      </div>

      {/* Row ② — graph / board */}
      <div className="min-h-0 flex-1 border-t">
        <Tabs defaultValue="graph" className="flex h-full min-h-0 flex-col">
          <TabsList className="mx-auto mt-2 self-center">
            <TabsTrigger value="graph">{t("subtabGraph")}</TabsTrigger>
            <TabsTrigger value="board">{t("subtabBoard")}</TabsTrigger>
          </TabsList>
          <TabsContent
            value="graph"
            className="min-h-0 flex-1 overflow-auto p-5 data-[state=inactive]:hidden"
          >
            <div className="flex flex-col items-center gap-4">
              <ArtifactNode label={t("rootArtifact")} title={issue.title} />
              <p className="text-center text-xs text-muted-foreground">
                {t("graphPlaceholder")}
              </p>
            </div>
          </TabsContent>
          <TabsContent
            value="board"
            className="min-h-0 flex-1 overflow-auto p-5 data-[state=inactive]:hidden"
          >
            <p className="text-center text-xs text-muted-foreground">
              {t("boardPlaceholder")}
            </p>
          </TabsContent>
        </Tabs>
      </div>

      {/* Row ③ — this issue's iterations / artifacts */}
      <div className="h-48 shrink-0 border-t">
        <Tabs
          defaultValue="iterations"
          className="flex h-full min-h-0 flex-col"
        >
          <TabsList className="mx-5 mt-2 self-start">
            <TabsTrigger value="iterations">
              {t("subtabIterations")}
            </TabsTrigger>
            <TabsTrigger value="artifacts">{t("subtabArtifacts")}</TabsTrigger>
          </TabsList>
          <TabsContent
            value="iterations"
            className="min-h-0 flex-1 overflow-y-auto px-5 py-2 data-[state=inactive]:hidden"
          >
            <p className="text-xs text-muted-foreground">{t("noIterations")}</p>
          </TabsContent>
          <TabsContent
            value="artifacts"
            className="min-h-0 flex-1 overflow-y-auto px-5 py-2 data-[state=inactive]:hidden"
          >
            {artifacts.length <= 1 ? (
              <p className="text-xs text-muted-foreground">
                {t("noArtifacts")}
              </p>
            ) : (
              <ul className="space-y-1 text-sm">
                {artifacts.map((a) => (
                  <li key={a.id} className="flex items-center gap-2">
                    <span className="text-xs text-muted-foreground">
                      {a.kind}
                    </span>
                    <span className="truncate">{a.title}</span>
                  </li>
                ))}
              </ul>
            )}
          </TabsContent>
        </Tabs>
      </div>

      <AlertDialog
        open={cancelOpen}
        onOpenChange={(o) => !o && setCancelOpen(false)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{tList("cancelConfirmTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {tList("cancelConfirmDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={actionBusy}>
              {tCommon("cancel")}
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                void runAction(
                  () => cancelLoopIssue(issue.id),
                  () => setCancelOpen(false)
                )
              }}
              disabled={actionBusy}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {actionBusy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {tList("cancelConfirmAction")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function ArtifactNode({ label, title }: { label: string; title: string }) {
  return (
    <div className="flex min-w-40 max-w-xs flex-col gap-1 rounded-lg border bg-card px-3 py-2 shadow-sm">
      <span className="text-[0.625rem] uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      <span className="truncate text-sm font-medium">{title}</span>
    </div>
  )
}
