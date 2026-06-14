import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { IterationDialog } from "./iteration-dialog"
import type { ConnectionState } from "@/contexts/acp-connections-context"
import type {
  AgentType,
  ConversationDetail,
  PendingQuestionState,
} from "@/lib/types"

// next-intl: return a STABLE `t` (per project mock guidance) so effects that
// depend on the translator identity don't re-run every render.
const stableT = (key: string) => key
vi.mock("next-intl", () => ({ useTranslations: () => stableT }))

// The shared child-session hooks are exercised by the sub-agent dialog tests;
// here we stub them so the iteration dialog's own wiring is isolated. The
// connection state is controllable via `mockConn`.
let mockConn: Partial<ConnectionState> | undefined
vi.mock("@/components/message/child-session-hooks", () => ({
  useChildConnectionState: () => mockConn,
  useChildLiveBridge: () => {},
}))

const connectAsViewer = vi.fn()
const disconnect = vi.fn()
const answerQuestion = vi.fn()
const respondPermission = vi.fn()
vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({
    connectAsViewer,
    disconnect,
    answerQuestion,
    respondPermission,
  }),
}))

const refetchDetail = vi.fn()
const setLiveOwnsActiveTurn = vi.fn()
vi.mock("@/contexts/conversation-runtime-context", () => ({
  useConversationRuntime: () => ({ refetchDetail, setLiveOwnsActiveTurn }),
}))

// The conversation summary is the authoritative agent-type source; controllable.
let mockDetail: ConversationDetail | null = null
vi.mock("@/hooks/use-conversation-detail", () => ({
  useConversationDetail: () => ({
    detail: mockDetail,
    loading: false,
    error: null,
    acpLoadError: null,
  }),
}))

// Connection discovery — the heart of the live-attach behavior. The dialog asks
// "is there a live engine-owned connection for this conversation?" on open.
const acpFindConnectionForConversation = vi.fn()
vi.mock("@/lib/api", () => ({
  acpFindConnectionForConversation: (...a: unknown[]) =>
    acpFindConnectionForConversation(...a),
}))

vi.mock("@/components/message/message-list-view", () => ({
  MessageListView: (props: Record<string, unknown>) => (
    <div
      data-testid="message-list-view"
      data-conversation-id={String(props.conversationId)}
      data-agent-type={String(props.agentType)}
    />
  ),
}))

vi.mock("@/components/chat/permission-dialog", () => ({
  PermissionDialog: ({
    permission,
    onRespond,
  }: {
    permission: { request_id: string } | null
    onRespond: (requestId: string, optionId: string) => void
  }) =>
    permission ? (
      <button
        data-testid="permission"
        onClick={() => onRespond(permission.request_id, "approve")}
      >
        permission
      </button>
    ) : null,
}))

vi.mock("@/components/chat/ask-question-card", () => ({
  AskQuestionCard: ({
    question,
    onAnswer,
  }: {
    question: PendingQuestionState
    onAnswer: (
      questionId: string,
      answer: { answers: unknown[]; declined: boolean }
    ) => void
  }) => (
    <button
      data-testid="ask-question"
      onClick={() =>
        onAnswer(question.question_id, { answers: [], declined: false })
      }
    >
      ask {question.question_id}
    </button>
  ),
}))

function askState(): PendingQuestionState {
  return {
    question_id: "q1",
    created_at: "2026-06-14T00:00:00Z",
    questions: [
      {
        id: "q1-0",
        question: "Which approach?",
        header: "Approach",
        multi_select: false,
        options: [{ label: "A", description: "" }],
      },
    ],
  }
}

/** Minimal conversation detail carrying just the agent type the dialog reads. */
function detailWithAgent(agent_type: AgentType): ConversationDetail {
  return {
    summary: { agent_type },
    turns: [],
  } as unknown as ConversationDetail
}

beforeEach(() => {
  vi.clearAllMocks()
  mockConn = { status: "prompting" }
  mockDetail = null
  // Default: no live connection — settled iteration / read persisted transcript.
  acpFindConnectionForConversation.mockResolvedValue(null)
})

describe("IterationDialog", () => {
  it("discovers and attaches as a viewer on open", async () => {
    acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "iter-conn",
      event_seq: 0,
    })
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        agentType="claude_code"
      />
    )
    // Discovery is keyed on the conversation id; agentType is irrelevant to the
    // primary (conversation_id) lookup so any value is fine here.
    await waitFor(() =>
      expect(acpFindConnectionForConversation).toHaveBeenCalledWith(
        42,
        undefined,
        "claude_code"
      )
    )
    await waitFor(() =>
      expect(connectAsViewer).toHaveBeenCalledWith(
        "iter-conn",
        "iter-conn",
        "claude_code",
        null
      )
    )
    // Live connection present → tell the runtime the live stream owns the turn.
    await waitFor(() =>
      expect(setLiveOwnsActiveTurn).toHaveBeenCalledWith(42, true, null)
    )
    expect(screen.getByTestId("message-list-view")).toHaveAttribute(
      "data-conversation-id",
      "42"
    )
  })

  it("falls back to the persisted transcript when no live connection", async () => {
    acpFindConnectionForConversation.mockResolvedValue(null)
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        agentType="claude_code"
      />
    )
    // Persisted detail is always fetched (preserveLive) regardless of liveness.
    await waitFor(() =>
      expect(refetchDetail).toHaveBeenCalledWith(42, { preserveLive: true })
    )
    await waitFor(() =>
      expect(acpFindConnectionForConversation).toHaveBeenCalledTimes(1)
    )
    // No live connection → never attach a viewer; the persisted transcript shows.
    expect(connectAsViewer).not.toHaveBeenCalled()
    expect(setLiveOwnsActiveTurn).toHaveBeenCalledWith(42, false, null)
  })

  it("sources the agent type from the conversation summary when no hint", async () => {
    acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "c1",
      event_seq: 0,
    })
    mockDetail = detailWithAgent("codex")
    render(<IterationDialog open onOpenChange={() => {}} conversationId={7} />)
    await waitFor(() =>
      expect(connectAsViewer).toHaveBeenCalledWith("c1", "c1", "codex", null)
    )
  })

  it("answers the live question through the discovered connection", async () => {
    acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "iter-conn",
      event_seq: 0,
    })
    mockConn = { status: "prompting", pendingAskQuestion: askState() }
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        agentType="claude_code"
      />
    )
    fireEvent.click(await screen.findByTestId("ask-question"))
    expect(answerQuestion).toHaveBeenCalledWith("iter-conn", "q1", {
      answers: [],
      declined: false,
    })
  })

  it("detaches (not disconnect-the-agent) when closed", async () => {
    acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "iter-conn",
      event_seq: 0,
    })
    const { rerender } = render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        agentType="claude_code"
      />
    )
    await waitFor(() => expect(connectAsViewer).toHaveBeenCalled())
    // Closing unmounts the body, whose cleanup detaches the viewer.
    rerender(
      <IterationDialog
        open={false}
        onOpenChange={() => {}}
        conversationId={42}
        agentType="claude_code"
      />
    )
    await waitFor(() => expect(disconnect).toHaveBeenCalledWith("iter-conn"))
  })

  it("does not render the question band when nothing is pending", async () => {
    acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "iter-conn",
      event_seq: 0,
    })
    mockConn = { status: "connected" }
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        agentType="claude_code"
      />
    )
    await waitFor(() => expect(connectAsViewer).toHaveBeenCalled())
    expect(screen.queryByTestId("ask-question")).not.toBeInTheDocument()
    expect(screen.queryByTestId("permission")).not.toBeInTheDocument()
  })
})
