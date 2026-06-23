import { describe, expect, it } from "vitest"

import {
  buildCodeBuddyEnv,
  codeBuddyEnvironmentFromEnv,
} from "./acp-agent-settings"

describe("codeBuddyEnvironmentFromEnv", () => {
  it("maps internal / ioa and defaults to overseas", () => {
    expect(codeBuddyEnvironmentFromEnv({})).toBe("overseas")
    expect(
      codeBuddyEnvironmentFromEnv({
        CODEBUDDY_INTERNET_ENVIRONMENT: "internal",
      })
    ).toBe("internal")
    expect(
      codeBuddyEnvironmentFromEnv({ CODEBUDDY_INTERNET_ENVIRONMENT: "ioa" })
    ).toBe("ioa")
  })

  it("is tolerant of case and surrounding whitespace", () => {
    expect(
      codeBuddyEnvironmentFromEnv({
        CODEBUDDY_INTERNET_ENVIRONMENT: " Internal ",
      })
    ).toBe("internal")
  })

  it("falls back to overseas for an unknown value", () => {
    expect(
      codeBuddyEnvironmentFromEnv({ CODEBUDDY_INTERNET_ENVIRONMENT: "mars" })
    ).toBe("overseas")
  })
})

describe("buildCodeBuddyEnv", () => {
  it("writes a trimmed API key and clears it when blank", () => {
    expect(buildCodeBuddyEnv({}, "sk-123", "overseas")).toEqual({
      CODEBUDDY_API_KEY: "sk-123",
    })
    expect(buildCodeBuddyEnv({}, "  sk-x  ", "overseas")).toEqual({
      CODEBUDDY_API_KEY: "sk-x",
    })
    expect(
      buildCodeBuddyEnv({ CODEBUDDY_API_KEY: "old" }, "   ", "overseas")
    ).toEqual({})
  })

  it("sets the environment var for China / iOA", () => {
    expect(buildCodeBuddyEnv({}, "k", "internal")).toEqual({
      CODEBUDDY_API_KEY: "k",
      CODEBUDDY_INTERNET_ENVIRONMENT: "internal",
    })
    expect(buildCodeBuddyEnv({}, "k", "ioa")).toEqual({
      CODEBUDDY_API_KEY: "k",
      CODEBUDDY_INTERNET_ENVIRONMENT: "ioa",
    })
  })

  it("DELETES the environment var for the overseas build (must be unset, not empty)", () => {
    expect(
      buildCodeBuddyEnv(
        { CODEBUDDY_INTERNET_ENVIRONMENT: "internal" },
        "k",
        "overseas"
      )
    ).toEqual({ CODEBUDDY_API_KEY: "k" })
  })

  it("preserves unrelated env keys", () => {
    expect(buildCodeBuddyEnv({ FOO: "bar" }, "k", "internal")).toEqual({
      FOO: "bar",
      CODEBUDDY_API_KEY: "k",
      CODEBUDDY_INTERNET_ENVIRONMENT: "internal",
    })
  })
})
