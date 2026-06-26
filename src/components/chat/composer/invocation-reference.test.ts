import { describe, expect, it } from "vitest"

import type { AgentSkillItem, AvailableCommandInfo } from "@/lib/types"

import { commandToReference, skillToReference } from "./invocation-reference"
import { referenceToMarkdown } from "./reference-text"

const cmd = (name: string): AvailableCommandInfo => ({
  name,
  description: `${name} command`,
})

const skill = (id: string, name: string): AgentSkillItem =>
  ({
    id,
    name,
    scope: "project",
    layout: "markdown_file",
    path: `/skills/${id}.md`,
    description: "desc",
    read_only: false,
  }) as AgentSkillItem

describe("commandToReference", () => {
  it("builds a skill-kind reference that serializes to /name", () => {
    const ref = commandToReference(cmd("build"))
    expect(ref).toEqual({
      refType: "skill",
      id: "build",
      label: "build",
      uri: null,
      meta: { invocationPrefix: "/" },
    })
    expect(referenceToMarkdown(ref)).toBe("/build")
  })
})

describe("skillToReference", () => {
  it("uses the `$` prefix for a Codex skill and keeps the friendly label", () => {
    const ref = skillToReference(skill("deploy", "Deploy"), "$")
    expect(ref).toMatchObject({
      refType: "skill",
      id: "deploy",
      label: "Deploy",
      uri: null,
      meta: { invocationPrefix: "$", scope: "project" },
    })
    // Serialization uses the id, not the label.
    expect(referenceToMarkdown(ref)).toBe("$deploy")
  })

  it("uses the `/` prefix for a non-Codex skill", () => {
    expect(referenceToMarkdown(skillToReference(skill("x", "X"), "/"))).toBe(
      "/x"
    )
  })

  it("falls back to the id when the skill has no name", () => {
    expect(skillToReference(skill("only-id", ""), "/").label).toBe("only-id")
  })
})
