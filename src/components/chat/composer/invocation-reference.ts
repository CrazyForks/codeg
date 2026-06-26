import type { AgentSkillItem, AvailableCommandInfo } from "@/lib/types"

import type { ReferenceAttrs } from "./types"

/**
 * Builders that turn a runtime command / skill into the inline `reference`
 * badge the composer embeds (refType `skill`). They carry no `uri`, so on send
 * `referenceToMarkdown` serializes them to their literal invocation token
 * `${prefix}${id}` — `/command`, `$skill` — exactly the text the agent CLI
 * executes. `meta.invocationPrefix` drives that prefix; `meta.scope === "expert"`
 * (set elsewhere) is kept for the editor's leading-badge replace logic.
 */

export type InvocationPrefix = "/" | "$"

/** A `/`-triggered ACP slash command → command badge (always `/name`). */
export function commandToReference(cmd: AvailableCommandInfo): ReferenceAttrs {
  return {
    refType: "skill",
    id: cmd.name,
    label: cmd.name,
    uri: null,
    meta: { invocationPrefix: "/" },
  }
}

/** A `/`- or `$`-triggered agent skill → skill badge (`${prefix}${id}`). */
export function skillToReference(
  skill: AgentSkillItem,
  prefix: InvocationPrefix
): ReferenceAttrs {
  return {
    refType: "skill",
    id: skill.id,
    label: skill.name || skill.id,
    uri: null,
    meta: { invocationPrefix: prefix, scope: skill.scope },
  }
}
