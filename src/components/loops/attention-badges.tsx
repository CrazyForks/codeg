"use client"

import { useTranslations } from "next-intl"

import { cn } from "@/lib/utils"

/**
 * The pending-inbox attention badges shown on every loop surface (D7): a
 * blocking count (amber, demands a human) and a notice count (muted but still
 * visible — a notice-only space must never be silent, Codex r1 I3). Renders
 * nothing when both are zero.
 */
export function AttentionBadges({
  blocking,
  notice,
  className,
}: {
  blocking: number
  notice: number
  className?: string
}) {
  const t = useTranslations("Loops.attention")
  if (blocking <= 0 && notice <= 0) return null
  // The visible glyph is just a number; the meaning rides on a visually-hidden
  // phrase ("3 blocking") so it is announced reliably even inside a button/tab,
  // where a bare `aria-label` on a span can be dropped (Codex r1).
  return (
    <span className={cn("inline-flex items-center gap-1", className)}>
      {blocking > 0 && (
        <span
          title={t("blocking", { count: blocking })}
          className="inline-flex min-w-[1.125rem] items-center justify-center rounded-full bg-amber-500/15 px-1.5 py-0.5 text-[11px] font-semibold tabular-nums text-amber-700 dark:text-amber-400"
        >
          <span aria-hidden="true">{blocking}</span>
          <span className="sr-only">{t("blocking", { count: blocking })}</span>
        </span>
      )}
      {notice > 0 && (
        <span
          title={t("notice", { count: notice })}
          className="inline-flex min-w-[1.125rem] items-center justify-center rounded-full bg-muted px-1.5 py-0.5 text-[11px] font-medium tabular-nums text-muted-foreground"
        >
          <span aria-hidden="true">{notice}</span>
          <span className="sr-only">{t("notice", { count: notice })}</span>
        </span>
      )}
    </span>
  )
}
