/** Loop workbench navigation state, encoded in the `/workspace` query string so
 *  it survives refresh and is shareable. The single source of truth for which
 *  space/issue/tab/artifact/settings the loop UI is showing. */

export type LoopTab = "issues" | "iterations" | "artifacts" | "inbox" | "memory"

export const LOOP_TABS: LoopTab[] = [
  "issues",
  "iterations",
  "artifacts",
  "inbox",
  "memory",
]

export interface LoopNav {
  /** Whether the loop workbench surface is active (vs the chat workspace). */
  loops: boolean
  space: number | null
  issue: number | null
  tab: LoopTab
  artifact: number | null
  /** Whether the per-issue settings panel is open. */
  settings: boolean
  /** Transient "scroll the graph to this artifact and pulse it" request. Set by
   *  a locate-in-graph action and consumed (cleared) by the DAG once it has
   *  replayed the scroll/pulse — so a cross-tab navigation that mounts the graph
   *  AFTER the request still lands it (Codex r1 I6). Never a durable selection. */
  focus: number | null
}

export const DEFAULT_LOOP_NAV: LoopNav = {
  loops: false,
  space: null,
  issue: null,
  tab: "issues",
  artifact: null,
  settings: false,
  focus: null,
}

// The query-string keys this model owns. Everything else in the URL is foreign
// and must be preserved untouched.
const K = {
  loops: "loops",
  space: "space",
  issue: "issue",
  tab: "tab",
  artifact: "artifact",
  settings: "settings",
  focus: "focus",
} as const

/** Parse a positive integer id, or null for absent/invalid. */
function id(v: string | null): number | null {
  if (v == null) return null
  const n = Number(v)
  return Number.isInteger(n) && n > 0 ? n : null
}

export function parseLoopNav(search: string): LoopNav {
  const p = new URLSearchParams(search)
  const tabRaw = p.get(K.tab)
  const tab = LOOP_TABS.includes(tabRaw as LoopTab)
    ? (tabRaw as LoopTab)
    : "issues"
  // Normalize so an external / stale / hand-edited URL can't express an
  // impossible state (the transitions below maintain the same invariants on app
  // writes).
  return normalizeLoopNav({
    loops: p.get(K.loops) === "1",
    space: id(p.get(K.space)),
    issue: id(p.get(K.issue)),
    tab,
    artifact: id(p.get(K.artifact)),
    settings: p.get(K.settings) === "1",
    focus: id(p.get(K.focus)),
  })
}

/** Enforce the cross-field invariants on a nav that may come from an external URL:
 *  a child is dropped when its parent is absent. Keeps the URL model
 *  self-consistent no matter how it was produced (review NB1). */
export function normalizeLoopNav(nav: LoopNav): LoopNav {
  let { issue, artifact, settings, focus } = nav
  if (nav.space == null) {
    issue = null // an issue belongs to a space
    artifact = null // an artifact is only opened from within a space
  }
  if (issue == null || nav.tab !== "issues") {
    settings = false // per-issue, issues tab
    focus = null // the graph (and its focus pulse) live on the issues tab
  }
  return { ...nav, issue, artifact, settings, focus }
}

/** Merge a nav into an existing search string, preserving foreign params and
 *  dropping loop params that sit at their default (keeps the URL clean). Returns
 *  a string with leading "?" or "". */
export function loopNavToSearch(nav: LoopNav, currentSearch: string): string {
  const p = new URLSearchParams(currentSearch)
  Object.values(K).forEach((k) => p.delete(k))
  if (nav.loops) p.set(K.loops, "1")
  if (nav.space != null) p.set(K.space, String(nav.space))
  if (nav.issue != null) p.set(K.issue, String(nav.issue))
  if (nav.tab !== "issues") p.set(K.tab, nav.tab)
  if (nav.artifact != null) p.set(K.artifact, String(nav.artifact))
  if (nav.settings) p.set(K.settings, "1")
  if (nav.focus != null) p.set(K.focus, String(nav.focus))
  const s = p.toString()
  return s ? `?${s}` : ""
}

// --- Navigation transitions -------------------------------------------------
// Pure (nav, …) -> nav functions that own the CASCADE invariants: a parent
// change clears its now-stale descendants, so no caller can leave Issue A's
// artifact drawer or settings panel open over Issue B. The hook's actions and
// every consumer go through these — raw field patching is intentionally never
// exposed. Tested below.

/** Enter/leave the loop surface; non-destructive — keeps space/issue/tab so
 *  returning restores them (D2). */
export function navSetLoops(nav: LoopNav, loops: boolean): LoopNav {
  return { ...nav, loops }
}

/** Open a space; a DIFFERENT space resets its children (issue/artifact/settings/focus). */
export function navOpenSpace(nav: LoopNav, space: number): LoopNav {
  if (space === nav.space) return nav
  return {
    ...nav,
    space,
    issue: null,
    artifact: null,
    settings: false,
    focus: null,
  }
}

/** Leave the open space (back to the space list); clears all space children. */
export function navCloseSpace(nav: LoopNav): LoopNav {
  return {
    ...nav,
    space: null,
    issue: null,
    artifact: null,
    settings: false,
    focus: null,
  }
}

/** Select (or clear) the issue; changing it clears the artifact + settings + focus. */
export function navSelectIssue(nav: LoopNav, issue: number | null): LoopNav {
  if (issue === nav.issue) return nav
  return { ...nav, issue, artifact: null, settings: false, focus: null }
}

/** Jump straight to a specific issue (cross-space reverse-nav from an iteration). */
export function navGotoIssue(
  nav: LoopNav,
  space: number,
  issue: number
): LoopNav {
  return {
    ...nav,
    loops: true,
    space,
    issue,
    tab: "issues",
    artifact: null,
    settings: false,
    focus: null,
  }
}

/** Switch the space tab; settings + focus only exist on the issues tab, so drop
 *  them elsewhere. */
export function navSetTab(nav: LoopNav, tab: LoopTab): LoopNav {
  const onIssues = tab === "issues"
  return {
    ...nav,
    tab,
    settings: onIssues ? nav.settings : false,
    focus: onIssues ? nav.focus : null,
  }
}

// Opening/closing an artifact by hand is a deliberate navigation that supersedes
// any pending locate, so it drops a still-unconsumed `focus` (Codex r1) — a stale
// focus must never linger to pulse an unrelated node later. (navFocusArtifact sets
// artifact + focus together in its own transition, so it is unaffected.)
export function navOpenArtifact(nav: LoopNav, artifact: number): LoopNav {
  return { ...nav, artifact, focus: null }
}
export function navCloseArtifact(nav: LoopNav): LoopNav {
  return { ...nav, artifact: null, focus: null }
}
export function navSetSettings(nav: LoopNav, settings: boolean): LoopNav {
  return { ...nav, settings }
}

/** Locate an artifact in the issue graph: jump to its issue (cross-space if
 *  needed), open its drawer, and request the graph pulse. The graph consumes
 *  `focus` once it has scrolled/pulsed (see `navClearFocus`); opening the drawer
 *  is the reliable locate, the pulse is best-effort reinforcement. */
export function navFocusArtifact(
  nav: LoopNav,
  space: number,
  issue: number,
  artifact: number
): LoopNav {
  return {
    ...nav,
    loops: true,
    space,
    issue,
    tab: "issues",
    artifact,
    settings: false,
    focus: artifact,
  }
}

/** Consume a pending focus request (the DAG calls this after it pulses). */
export function navClearFocus(nav: LoopNav): LoopNav {
  if (nav.focus == null) return nav
  return { ...nav, focus: null }
}
