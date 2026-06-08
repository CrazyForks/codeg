use crate::models::{ConversationDetail, ConversationSummary};
use crate::parsers::{AgentParser, ParseError};

/// Parser for Hermes Agent transcripts.
///
/// Hermes persists sessions in `~/.hermes/state.db` (SQLite) across two tables:
/// `sessions` (id, source, model, started_at, cwd, title, message_count, …) and
/// `messages` (session_id, role, content, tool_calls, tool_name, timestamp, …).
///
/// NOTE: full historical-transcript parsing is a deferred follow-up (Phase 6 of
/// the Hermes integration). This stub returns no conversations so Hermes
/// integrates cleanly with the aggregated history view without surfacing
/// partial or incorrect data. The live ACP session path (real-time chat,
/// streaming, tools, permissions) is fully supported independently of this.
#[derive(Default)]
pub struct HermesParser;

impl HermesParser {
    pub fn new() -> Self {
        Self
    }
}

impl AgentParser for HermesParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        Ok(Vec::new())
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        Err(ParseError::ConversationNotFound(conversation_id.to_string()))
    }
}
