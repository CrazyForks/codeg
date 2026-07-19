import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import {
  buildCursorEnv,
  CursorConfigPanel,
  isCursorForceEnabled,
} from "./cursor-config-panel"
import {
  acpCursorAuthStatus,
  acpCursorListModels,
  acpUpdateAgentConfig,
} from "@/lib/api"
import type { AcpAgentInfo } from "@/lib/types"
import enMessages from "@/i18n/messages/en.json"

vi.mock("@/lib/api", () => ({
  acpCursorAuthStatus: vi.fn(),
  acpCursorListModels: vi.fn(),
  acpUpdateAgentConfig: vi.fn(),
}))

describe("buildCursorEnv", () => {
  it("sets every managed knob and preserves unrelated keys", () => {
    const env = buildCursorEnv(
      { OTHER: "x" },
      "sk-key",
      "https://api.example.com//",
      "gpt-5",
      true
    )
    expect(env).toEqual({
      OTHER: "x",
      CURSOR_API_KEY: "sk-key",
      CURSOR_API_BASE_URL: "https://api.example.com",
      CURSOR_MODEL: "gpt-5",
      CURSOR_FORCE: "1",
    })
  })

  it("removes every managed key when cleared", () => {
    const env = buildCursorEnv(
      {
        CURSOR_API_KEY: "old",
        CURSOR_API_BASE_URL: "https://old",
        CURSOR_MODEL: "m",
        CURSOR_FORCE: "1",
        KEEP: "y",
      },
      " ",
      "",
      "",
      false
    )
    expect(env).toEqual({ KEEP: "y" })
  })
})

describe("isCursorForceEnabled", () => {
  it("accepts 1/true in any case with padding, rejects everything else", () => {
    expect(isCursorForceEnabled({ CURSOR_FORCE: "1" })).toBe(true)
    expect(isCursorForceEnabled({ CURSOR_FORCE: " TRUE " })).toBe(true)
    expect(isCursorForceEnabled({ CURSOR_FORCE: "0" })).toBe(false)
    expect(isCursorForceEnabled({ CURSOR_FORCE: "yes" })).toBe(false)
    expect(isCursorForceEnabled({})).toBe(false)
  })
})

describe("CursorConfigPanel unified save", () => {
  const originalEnv = { CURSOR_API_KEY: "old-key" }
  const agent = {
    agent_type: "cursor",
    enabled: true,
    env: originalEnv,
    cursor_settings: {
      sandbox_mode: null,
      permissions_allow: [],
      permissions_deny: [],
    },
    cursor_cli_config_json: null,
  } as unknown as AcpAgentInfo

  function renderPanel(overrides?: {
    onSaveEnv?: ReturnType<typeof vi.fn>
    onSaved?: ReturnType<typeof vi.fn>
    onAffectedSessions?: ReturnType<typeof vi.fn>
  }) {
    const onSaveEnv = overrides?.onSaveEnv ?? vi.fn().mockResolvedValue(0)
    const onSaved = overrides?.onSaved ?? vi.fn()
    const onAffectedSessions = overrides?.onAffectedSessions ?? vi.fn()
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <CursorConfigPanel
          agent={agent}
          saving={false}
          onSaveEnv={onSaveEnv}
          onSaved={onSaved}
          onAffectedSessions={onAffectedSessions}
        />
      </NextIntlClientProvider>
    )
    return { onSaveEnv, onSaved, onAffectedSessions }
  }

  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(acpCursorAuthStatus).mockResolvedValue({
      installed: false,
      is_authenticated: false,
      raw_status: null,
      email: null,
      membership: null,
      error: null,
    })
    vi.mocked(acpCursorListModels).mockResolvedValue({
      models: [],
      default_model: null,
      error: null,
    })
  })

  it("rolls the env back when the rules write fails", async () => {
    // The widening hazard: the env step already persisted (e.g. Run
    // Everything turned on) but the deny rules never landed. The save must
    // restore the previous env instead of leaving the half-applied state.
    vi.mocked(acpUpdateAgentConfig).mockRejectedValue(new Error("disk full"))
    const { onSaveEnv, onSaved } = renderPanel()
    await screen.findByText(enMessages.AcpAgentSettings.cursor.authNotInstalled)

    fireEvent.change(
      screen.getByPlaceholderText(
        enMessages.AcpAgentSettings.cursor.apiKeyPlaceholder
      ),
      { target: { value: "new-key" } }
    )
    fireEvent.click(
      screen.getByRole("button", {
        name: enMessages.AcpAgentSettings.cursor.saveConfig,
      })
    )

    await waitFor(() => expect(onSaveEnv).toHaveBeenCalledTimes(2))
    expect(onSaveEnv.mock.calls[0][0]).toEqual({ CURSOR_API_KEY: "new-key" })
    // Rollback restores the exact prior env map.
    expect(onSaveEnv.mock.calls[1][0]).toEqual(originalEnv)
    expect(onSaved).not.toHaveBeenCalled()
  })

  it("reports the rules write's affected-session count on success", async () => {
    vi.mocked(acpUpdateAgentConfig).mockResolvedValue(3)
    const { onSaveEnv, onSaved, onAffectedSessions } = renderPanel()
    await screen.findByText(enMessages.AcpAgentSettings.cursor.authNotInstalled)

    fireEvent.click(
      screen.getByRole("button", {
        name: enMessages.AcpAgentSettings.cursor.saveConfig,
      })
    )

    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1))
    expect(onSaveEnv).toHaveBeenCalledTimes(1)
    expect(onAffectedSessions).toHaveBeenCalledWith(3)
  })
})
