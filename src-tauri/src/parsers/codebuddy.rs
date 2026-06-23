use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use walkdir::WalkDir;

use crate::models::{
    AgentType, ContentBlock, ConversationDetail, ConversationSummary, MessageRole, MessageTurn,
    TurnRole, TurnUsage, UnifiedMessage,
};
use crate::parsers::{
    compute_session_stats, folder_name_from_path, infer_context_window_max_tokens,
    latest_turn_total_usage_tokens, merge_context_window_stats, relocate_orphaned_tool_results,
    resolve_patch_line_numbers, structurize_read_tool_output, title_from_user_text, AgentParser,
    ParseError,
};

/// Resolve CodeBuddy's config dir, honoring `CODEBUDDY_CONFIG_DIR`, else
/// `~/.codebuddy` (mirrors `resolve_claude_config_dir`).
pub(crate) fn resolve_codebuddy_config_dir() -> PathBuf {
    resolve_codebuddy_config_dir_from(std::env::var_os("CODEBUDDY_CONFIG_DIR"), dirs::home_dir())
}

fn resolve_codebuddy_config_dir_from(
    codebuddy_config_dir_env: Option<OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    codebuddy_config_dir_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir.unwrap_or_default().join(".codebuddy"))
}

/// CodeBuddy (Tencent Cloud) stores its transcripts under
/// `~/.codebuddy/projects/<encoded-cwd>/<sessionId>.jsonl`, borrowing Claude
/// Code's *directory layout* — but the per-line record schema is the OpenAI
/// Agents SDK "items" shape, NOT Claude's: top-level `type`
/// (`message`/`reasoning`/`function_call`/`function_call_result`/`ai-title`/…),
/// a top-level `role` with a `content[]` array of `input_text`/`output_text`
/// items, and millisecond-epoch timestamps. So this parser reads those records
/// directly rather than reusing the Claude parser.
pub struct CodeBuddyParser {
    base_dir: PathBuf,
}

impl CodeBuddyParser {
    pub fn new() -> Self {
        Self {
            base_dir: resolve_codebuddy_config_dir().join("projects"),
        }
    }

    /// Construct a parser pointed at an explicit `projects` directory (test
    /// fixtures).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn parse_summary(&self, path: &Path) -> Option<ConversationSummary> {
        let reader = BufReader::new(fs::File::open(path).ok()?);

        let mut first_ts: Option<DateTime<Utc>> = None;
        let mut last_ts: Option<DateTime<Utc>> = None;
        let mut ai_title: Option<String> = None;
        let mut first_user_text: Option<String> = None;
        let mut model: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut session_id: Option<String> = None;
        let mut message_count: u32 = 0;

        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };

            let record_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if is_content_record(record_type) {
                if let Some(ts) = record_millis(&value) {
                    first_ts.get_or_insert(ts);
                    last_ts = Some(ts);
                }
            }
            if cwd.is_none() {
                cwd = record_cwd(&value);
            }
            if session_id.is_none() {
                session_id = value
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .map(String::from);
            }
            if model.is_none() {
                model = record_model(&value);
            }

            match record_type {
                "ai-title" => {
                    if ai_title.is_none() {
                        ai_title = value
                            .get("aiTitle")
                            .and_then(|t| t.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from);
                    }
                }
                "message" => match value.get("role").and_then(|r| r.as_str()).unwrap_or("") {
                    "user" => {
                        message_count += 1;
                        if first_user_text.is_none() {
                            let text = collect_text(&value, "input_text");
                            if !text.trim().is_empty() {
                                first_user_text = Some(title_from_user_text(text.trim()));
                            }
                        }
                    }
                    "assistant" => message_count += 1,
                    _ => {}
                },
                _ => {}
            }
        }

        let started_at = first_ts?;
        let id = session_id.unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        let folder_name = cwd.as_deref().map(folder_name_from_path);

        Some(ConversationSummary {
            id,
            agent_type: AgentType::CodeBuddy,
            folder_path: cwd,
            folder_name,
            title: ai_title.or(first_user_text),
            started_at,
            ended_at: last_ts,
            message_count,
            model,
            git_branch: None,
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        })
    }

    fn parse_detail(
        &self,
        path: &Path,
        conversation_id: &str,
    ) -> Result<ConversationDetail, ParseError> {
        let reader = BufReader::new(fs::File::open(path)?);

        let mut messages: Vec<UnifiedMessage> = Vec::new();
        let mut first_ts: Option<DateTime<Utc>> = None;
        let mut last_ts: Option<DateTime<Utc>> = None;
        let mut ai_title: Option<String> = None;
        let mut first_user_text: Option<String> = None;
        let mut model: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut message_count: u32 = 0;

        for (idx, line) in reader.lines().enumerate() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };

            let record_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ts_raw = record_millis(&value);
            if is_content_record(record_type) {
                if let Some(ts) = ts_raw {
                    first_ts.get_or_insert(ts);
                    last_ts = Some(ts);
                }
            }
            let ts = ts_raw.or(last_ts).unwrap_or_else(Utc::now);

            if cwd.is_none() {
                cwd = record_cwd(&value);
            }
            if model.is_none() {
                model = record_model(&value);
            }

            match record_type {
                "ai-title" => {
                    if ai_title.is_none() {
                        ai_title = value
                            .get("aiTitle")
                            .and_then(|t| t.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from);
                    }
                }
                "message" => match value.get("role").and_then(|r| r.as_str()).unwrap_or("") {
                    "user" => {
                        message_count += 1;
                        let text = collect_text(&value, "input_text");
                        if first_user_text.is_none() && !text.trim().is_empty() {
                            first_user_text = Some(title_from_user_text(text.trim()));
                        }
                        if !text.trim().is_empty() {
                            messages.push(text_message(
                                format!("cb-user-{idx}"),
                                MessageRole::User,
                                text,
                                ts,
                                None,
                                None,
                            ));
                        }
                    }
                    "assistant" => {
                        message_count += 1;
                        let text = collect_text(&value, "output_text");
                        if !text.trim().is_empty() {
                            messages.push(text_message(
                                format!("cb-assistant-{idx}"),
                                MessageRole::Assistant,
                                text,
                                ts,
                                usage_from_raw(&value),
                                record_model(&value),
                            ));
                        }
                    }
                    _ => {}
                },
                "reasoning" => {
                    let text = reasoning_text(&value);
                    if !text.trim().is_empty() {
                        messages.push(UnifiedMessage {
                            id: format!("cb-reasoning-{idx}"),
                            role: MessageRole::Assistant,
                            content: vec![ContentBlock::Thinking { text }],
                            timestamp: ts,
                            usage: None,
                            duration_ms: None,
                            model: record_model(&value),
                            completed_at: Some(ts),
                        });
                    }
                }
                "function_call" => {
                    messages.push(UnifiedMessage {
                        id: format!("cb-toolcall-{idx}"),
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::ToolUse {
                            tool_use_id: call_id(&value),
                            tool_name: value
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown")
                                .to_string(),
                            input_preview: tool_input_preview(&value),
                            meta: None,
                        }],
                        timestamp: ts,
                        usage: None,
                        duration_ms: None,
                        model: None,
                        completed_at: Some(ts),
                    });
                }
                "function_call_result" => {
                    messages.push(UnifiedMessage {
                        id: format!("cb-toolresult-{idx}"),
                        role: MessageRole::Tool,
                        content: vec![ContentBlock::ToolResult {
                            tool_use_id: call_id(&value),
                            output_preview: tool_output_preview(&value),
                            is_error: tool_is_error(&value),
                            agent_stats: None,
                        }],
                        timestamp: ts,
                        usage: None,
                        duration_ms: None,
                        model: None,
                        completed_at: Some(ts),
                    });
                }
                _ => {}
            }
        }

        let mut turns = group_into_turns(messages);
        relocate_orphaned_tool_results(&mut turns);
        structurize_read_tool_output(&mut turns);
        resolve_patch_line_numbers(&mut turns, cwd.as_deref());

        let used_tokens = latest_turn_total_usage_tokens(&turns);
        let max_tokens = infer_context_window_max_tokens(model.as_deref());
        let session_stats =
            merge_context_window_stats(compute_session_stats(&turns), used_tokens, max_tokens);

        let folder_name = cwd.as_deref().map(folder_name_from_path);
        let summary = ConversationSummary {
            id: conversation_id.to_string(),
            agent_type: AgentType::CodeBuddy,
            folder_path: cwd,
            folder_name,
            title: ai_title.or(first_user_text),
            started_at: first_ts.unwrap_or_else(Utc::now),
            ended_at: last_ts,
            message_count,
            model,
            git_branch: None,
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        };

        Ok(ConversationDetail {
            summary,
            turns,
            session_stats,
        })
    }
}

impl Default for CodeBuddyParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentParser for CodeBuddyParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let mut conversations = Vec::new();
        if !self.base_dir.exists() {
            return Ok(conversations);
        }

        for entry in WalkDir::new(&self.base_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(summary) = self.parse_summary(path) {
                conversations.push(summary);
            }
        }

        conversations.sort_by_key(|c| std::cmp::Reverse(c.started_at));
        Ok(conversations)
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        if self.base_dir.exists() {
            for entry in WalkDir::new(&self.base_dir)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if path.file_stem().map(|s| s.to_string_lossy()).as_deref() == Some(conversation_id)
                {
                    return self.parse_detail(path, conversation_id);
                }
            }
        }

        Err(ParseError::ConversationNotFound(
            conversation_id.to_string(),
        ))
    }
}

/// Epoch-millisecond `timestamp` → `DateTime<Utc>` (CodeBuddy uses numeric ms,
/// not Claude's ISO strings).
fn record_millis(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(value.get("timestamp")?.as_i64()?)
}

fn record_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Record types that carry actual conversation content, as opposed to the
/// `ai-title` / `summary` / `file-history-snapshot` metadata records (which also
/// carry timestamps). Only content records define the session's
/// `started_at`/`ended_at` span and whether a transcript is listed at all — so a
/// metadata-only file is treated as empty rather than surfacing as a
/// zero-message conversation.
fn is_content_record(record_type: &str) -> bool {
    matches!(
        record_type,
        "message" | "reasoning" | "function_call" | "function_call_result"
    )
}

/// Display model name from `providerData`: prefer `requestModelName` (e.g.
/// "GLM-5.1"), falling back to the lowercase `model` id. Each candidate is taken
/// only when present AND non-empty, so a blank/null `requestModelName` does not
/// shadow a valid `model`.
fn record_model(value: &Value) -> Option<String> {
    let provider_data = value.get("providerData")?;
    ["requestModelName", "model"].into_iter().find_map(|key| {
        provider_data
            .get(key)
            .and_then(|m| m.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    })
}

fn call_id(value: &Value) -> Option<String> {
    value
        .get("callId")
        .or_else(|| value.get("id"))
        .and_then(|i| i.as_str())
        .map(String::from)
}

/// Concatenate the `text` of every `content[]` item of the given `item_type`
/// (`input_text` for user turns, `output_text` for assistant turns).
fn collect_text(value: &Value, item_type: &str) -> String {
    let mut out = String::new();
    if let Some(items) = value.get("content").and_then(|c| c.as_array()) {
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) == Some(item_type) {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    out.push_str(text);
                }
            }
        }
    }
    out
}

/// Reasoning text lives in `rawContent[].text` (`reasoning_text` items); some
/// records mirror it under `content[]`, so fall back to that.
fn reasoning_text(value: &Value) -> String {
    for key in ["rawContent", "content"] {
        if let Some(items) = value.get(key).and_then(|c| c.as_array()) {
            let mut out = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    out.push_str(text);
                }
            }
            if !out.trim().is_empty() {
                return out;
            }
        }
    }
    String::new()
}

/// Map CodeBuddy's `providerData.rawUsage` (OpenAI completions shape) onto
/// `TurnUsage`. `prompt_tokens` already includes the cached prefix, so subtract
/// `cached_tokens` to get the non-cached input.
fn usage_from_raw(value: &Value) -> Option<TurnUsage> {
    let raw = value.get("providerData")?.get("rawUsage")?;
    let prompt = raw.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
    let completion = raw
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = raw
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if prompt == 0 && completion == 0 && cached == 0 {
        return None;
    }
    Some(TurnUsage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: completion,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cached,
    })
}

/// `function_call.arguments` is a JSON string (or, defensively, an object).
fn tool_input_preview(value: &Value) -> Option<String> {
    let arguments = value.get("arguments")?;
    if let Some(s) = arguments.as_str() {
        (!s.is_empty()).then(|| s.to_string())
    } else if arguments.is_object() || arguments.is_array() {
        serde_json::to_string(arguments).ok()
    } else {
        None
    }
}

/// `function_call_result.output` is `{type:"text", text}`; fall back to the raw
/// string or `providerData.toolResult.content`.
fn tool_output_preview(value: &Value) -> Option<String> {
    if let Some(output) = value.get("output") {
        if let Some(text) = output.as_str() {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        } else if let Some(text) = output.get("text").and_then(|t| t.as_str()) {
            return Some(text.to_string());
        }
    }
    let content = value.get("providerData")?.get("toolResult")?.get("content")?;
    if let Some(text) = content.as_str() {
        Some(text.to_string())
    } else {
        serde_json::to_string(content).ok()
    }
}

/// A tool call failed when `providerData.toolResult.error` is set (CodeBuddy
/// reports tool failures here even while `status` stays "completed"), the
/// status is a failure, or the output text begins with "Error:".
fn tool_is_error(value: &Value) -> bool {
    if let Some(error) = value
        .get("providerData")
        .and_then(|p| p.get("toolResult"))
        .and_then(|tr| tr.get("error"))
    {
        match error {
            Value::Null => {}
            Value::String(s) => {
                if !s.trim().is_empty() {
                    return true;
                }
            }
            _ => return true,
        }
    }

    if let Some(status) = value.get("status").and_then(|s| s.as_str()) {
        if matches!(
            status.trim().to_ascii_lowercase().as_str(),
            "error" | "failed" | "failure" | "cancelled" | "canceled"
        ) {
            return true;
        }
    }

    value
        .get("output")
        .and_then(|o| o.get("text"))
        .and_then(|t| t.as_str())
        .and_then(|t| t.trim_start().get(..6).map(str::to_string))
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("error:"))
}

fn text_message(
    id: String,
    role: MessageRole,
    text: String,
    ts: DateTime<Utc>,
    usage: Option<TurnUsage>,
    model: Option<String>,
) -> UnifiedMessage {
    UnifiedMessage {
        id,
        role,
        content: vec![ContentBlock::Text { text }],
        timestamp: ts,
        usage,
        duration_ms: None,
        model,
        completed_at: Some(ts),
    }
}

/// Group the flat, chronologically-ordered `UnifiedMessage`s into `MessageTurn`s:
/// User/System messages each become their own turn; an Assistant message starts
/// a turn that absorbs the immediately-following Tool messages (its tool
/// results), stopping at the next Assistant message to keep turns small for
/// virtualization.
fn group_into_turns(messages: Vec<UnifiedMessage>) -> Vec<MessageTurn> {
    let mut turns = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        if matches!(msg.role, MessageRole::User) {
            turns.push(MessageTurn {
                id: format!("turn-{}", turns.len()),
                role: TurnRole::User,
                blocks: msg.content.clone(),
                timestamp: msg.timestamp,
                usage: None,
                duration_ms: None,
                model: None,
                completed_at: msg.completed_at,
            });
            i += 1;
        } else if matches!(msg.role, MessageRole::System) {
            turns.push(MessageTurn {
                id: format!("turn-{}", turns.len()),
                role: TurnRole::System,
                blocks: msg.content.clone(),
                timestamp: msg.timestamp,
                usage: None,
                duration_ms: None,
                model: None,
                completed_at: msg.completed_at,
            });
            i += 1;
        } else {
            // Assistant or Tool — start a group and absorb following Tool messages.
            let mut blocks: Vec<ContentBlock> = msg.content.clone();
            let mut usage = msg.usage.clone();
            let mut duration_ms = msg.duration_ms;
            let mut turn_model = msg.model.clone();
            let timestamp = msg.timestamp;
            let mut completed_at = msg.completed_at;
            i += 1;

            while i < messages.len() && matches!(messages[i].role, MessageRole::Tool) {
                blocks.extend(messages[i].content.clone());
                if usage.is_none() {
                    usage = messages[i].usage.clone();
                }
                if duration_ms.is_none() {
                    duration_ms = messages[i].duration_ms;
                }
                if turn_model.is_none() {
                    turn_model = messages[i].model.clone();
                }
                if messages[i].completed_at.is_some() {
                    completed_at = messages[i].completed_at;
                }
                i += 1;
            }

            turns.push(MessageTurn {
                id: format!("turn-{}", turns.len()),
                role: TurnRole::Assistant,
                blocks,
                timestamp,
                usage,
                duration_ms,
                model: turn_model,
                completed_at,
            });
        }
    }

    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TurnRole;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn config_dir_env_overrides_home() {
        let resolved = resolve_codebuddy_config_dir_from(
            Some(OsString::from("/custom/codebuddy")),
            Some(PathBuf::from("/Users/default")),
        );
        assert_eq!(resolved, PathBuf::from("/custom/codebuddy"));
    }

    #[test]
    fn config_dir_defaults_to_home_dot_codebuddy() {
        let resolved =
            resolve_codebuddy_config_dir_from(None, Some(PathBuf::from("/Users/default")));
        assert_eq!(resolved, PathBuf::from("/Users/default/.codebuddy"));
    }

    #[test]
    fn empty_env_falls_back_to_home() {
        let resolved =
            resolve_codebuddy_config_dir_from(Some(OsString::new()), Some(PathBuf::from("/home/u")));
        assert_eq!(resolved, PathBuf::from("/home/u/.codebuddy"));
    }

    fn write_session(root: &Path, encoded_cwd: &str, session_id: &str, records: &[Value]) {
        let dir = root.join(encoded_cwd);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let mut file =
            std::fs::File::create(dir.join(format!("{session_id}.jsonl"))).expect("create jsonl");
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).expect("serialize"))
                .expect("write line");
        }
    }

    #[test]
    fn parses_item_format_text_session() {
        let root = std::env::temp_dir().join(format!("codeg-cb-text-{}", uuid::Uuid::new_v4()));
        let sid = "sess-text";
        write_session(
            &root,
            "Users-demo-app",
            sid,
            &[
                json!({"type":"message","role":"user","timestamp":1781821844178i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "content":[{"type":"input_text","text":"你会做什么"}]}),
                json!({"type":"ai-title","timestamp":1781821846252i64,"aiTitle":"能力询问","cwd":"/Users/demo/app","sessionId":sid}),
                json!({"type":"reasoning","timestamp":1781821848958i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "rawContent":[{"type":"reasoning_text","text":"thinking about it"}],
                       "providerData":{"requestModelName":"GLM-5.1"}}),
                json!({"type":"message","role":"assistant","timestamp":1781821848958i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "content":[{"type":"output_text","text":"我是 CodeBuddy"}],
                       "providerData":{"requestModelName":"GLM-5.1","model":"glm-5.1",
                         "rawUsage":{"prompt_tokens":24049,"completion_tokens":267,"total_tokens":24316,
                           "prompt_tokens_details":{"cached_tokens":12800}}}}),
            ],
        );

        let parser = CodeBuddyParser::with_base_dir(root.clone());

        let summaries = parser.list_conversations().expect("list");
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.agent_type, AgentType::CodeBuddy);
        assert_eq!(summary.title.as_deref(), Some("能力询问"));
        assert_eq!(summary.folder_path.as_deref(), Some("/Users/demo/app"));
        assert_eq!(summary.model.as_deref(), Some("GLM-5.1"));
        assert_eq!(summary.message_count, 2);

        let detail = parser.get_conversation(sid).expect("detail");
        assert_eq!(detail.summary.agent_type, AgentType::CodeBuddy);

        let has_user_text = detail.turns.iter().any(|t| {
            matches!(t.role, TurnRole::User)
                && t.blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("你会做什么")))
        });
        assert!(has_user_text, "user input_text must become a User turn");

        let has_thinking = detail.turns.iter().any(|t| {
            t.blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Thinking { text } if text.contains("thinking")))
        });
        assert!(has_thinking, "reasoning must become a Thinking block");

        let has_assistant_text = detail.turns.iter().any(|t| {
            matches!(t.role, TurnRole::Assistant)
                && t.blocks.iter().any(
                    |b| matches!(b, ContentBlock::Text { text } if text.contains("CodeBuddy")),
                )
        });
        assert!(has_assistant_text, "assistant output_text must render");

        let usage = detail
            .session_stats
            .as_ref()
            .and_then(|s| s.total_usage.as_ref())
            .expect("usage");
        assert_eq!(usage.output_tokens, 267);
        assert_eq!(usage.cache_read_input_tokens, 12800);
        assert_eq!(usage.input_tokens, 24049 - 12800);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parses_tool_calls_with_error_detection() {
        let root = std::env::temp_dir().join(format!("codeg-cb-tool-{}", uuid::Uuid::new_v4()));
        let sid = "sess-tool";
        write_session(
            &root,
            "Users-demo-app",
            sid,
            &[
                json!({"type":"message","role":"user","timestamp":1782193811000i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "content":[{"type":"input_text","text":"run build"}]}),
                json!({"type":"function_call","timestamp":1782193811284i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "name":"Bash","callId":"call_1","arguments":"{\"command\": \"pnpm build\"}"}),
                json!({"type":"function_call_result","timestamp":1782193811300i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "name":"Bash","callId":"call_1","status":"completed",
                       "output":{"type":"text","text":"Error: Bash error: Internal error"},
                       "providerData":{"toolResult":{"content":"Error: Bash error: Internal error","error":"Bash error: Internal error"}}}),
                json!({"type":"function_call","timestamp":1782193812000i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "name":"Glob","callId":"call_2","arguments":"{\"pattern\": \"*.ts\"}"}),
                json!({"type":"function_call_result","timestamp":1782193812100i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "name":"Glob","callId":"call_2","status":"completed",
                       "output":{"type":"text","text":"a.ts\nb.ts"}}),
            ],
        );

        let parser = CodeBuddyParser::with_base_dir(root.clone());
        let detail = parser.get_conversation(sid).expect("detail");

        let mut uses = Vec::new();
        let mut results = Vec::new();
        for turn in &detail.turns {
            for block in &turn.blocks {
                match block {
                    ContentBlock::ToolUse {
                        tool_name,
                        tool_use_id,
                        input_preview,
                        ..
                    } => uses.push((
                        tool_name.clone(),
                        tool_use_id.clone(),
                        input_preview.clone(),
                    )),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        output_preview,
                        ..
                    } => results.push((tool_use_id.clone(), *is_error, output_preview.clone())),
                    _ => {}
                }
            }
        }

        assert_eq!(uses.len(), 2);
        assert!(uses.iter().any(|(name, id, input)| name == "Bash"
            && id.as_deref() == Some("call_1")
            && input.as_deref().unwrap_or_default().contains("pnpm build")));

        let bash = results
            .iter()
            .find(|(id, _, _)| id.as_deref() == Some("call_1"))
            .expect("bash result");
        assert!(bash.1, "toolResult.error must set is_error even when status=completed");

        let glob = results
            .iter()
            .find(|(id, _, _)| id.as_deref() == Some("call_2"))
            .expect("glob result");
        assert!(!glob.1, "successful result must not be an error");
        assert!(glob.2.as_deref().unwrap_or_default().contains("a.ts"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_session_file_is_handled() {
        let root = std::env::temp_dir().join(format!("codeg-cb-empty-{}", uuid::Uuid::new_v4()));
        let dir = root.join("Users-demo-app");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::File::create(dir.join("empty.jsonl")).expect("create empty");

        let parser = CodeBuddyParser::with_base_dir(root.clone());
        assert!(
            parser.list_conversations().expect("list").is_empty(),
            "an empty transcript has no timestamp and must be skipped from the list"
        );
        let detail = parser.get_conversation("empty").expect("detail");
        assert!(detail.turns.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn metadata_only_session_is_not_listed() {
        let root = std::env::temp_dir().join(format!("codeg-cb-meta-{}", uuid::Uuid::new_v4()));
        let sid = "sess-meta";
        write_session(
            &root,
            "Users-demo-app",
            sid,
            &[
                json!({"type":"file-history-snapshot","timestamp":1781821844000i64,"cwd":"/Users/demo/app","sessionId":sid,"snapshot":{}}),
                json!({"type":"ai-title","timestamp":1781821846000i64,"aiTitle":"orphan","cwd":"/Users/demo/app","sessionId":sid}),
            ],
        );

        let parser = CodeBuddyParser::with_base_dir(root.clone());
        assert!(
            parser.list_conversations().expect("list").is_empty(),
            "a transcript with only metadata records (no message/reasoning/tool) must not be listed"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn model_falls_back_to_model_id_when_request_model_name_blank() {
        let root = std::env::temp_dir().join(format!("codeg-cb-model-{}", uuid::Uuid::new_v4()));
        let sid = "sess-model";
        write_session(
            &root,
            "Users-demo-app",
            sid,
            &[
                json!({"type":"message","role":"user","timestamp":1781821844000i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "content":[{"type":"input_text","text":"hi"}]}),
                json!({"type":"message","role":"assistant","timestamp":1781821845000i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "content":[{"type":"output_text","text":"hello"}],
                       "providerData":{"requestModelName":"","model":"glm-5.1"}}),
            ],
        );

        let parser = CodeBuddyParser::with_base_dir(root.clone());
        let summaries = parser.list_conversations().expect("list");
        assert_eq!(
            summaries[0].model.as_deref(),
            Some("glm-5.1"),
            "a blank requestModelName must fall back to the model id"
        );
        let detail = parser.get_conversation(sid).expect("detail");
        assert_eq!(detail.summary.model.as_deref(), Some("glm-5.1"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_tool_output_is_structurized() {
        let root = std::env::temp_dir().join(format!("codeg-cb-read-{}", uuid::Uuid::new_v4()));
        let sid = "sess-read";
        write_session(
            &root,
            "Users-demo-app",
            sid,
            &[
                json!({"type":"message","role":"user","timestamp":1781821844000i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "content":[{"type":"input_text","text":"read it"}]}),
                json!({"type":"function_call","timestamp":1781821845000i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "name":"Read","callId":"r1","arguments":"{\"file_path\": \"/x\"}"}),
                json!({"type":"function_call_result","timestamp":1781821845100i64,"cwd":"/Users/demo/app","sessionId":sid,
                       "name":"Read","callId":"r1","status":"completed",
                       "output":{"type":"text","text":"   1→hello\n   2→world"}}),
            ],
        );

        let parser = CodeBuddyParser::with_base_dir(root.clone());
        let detail = parser.get_conversation(sid).expect("detail");
        let read_output = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id: Some(id),
                    output_preview,
                    ..
                } if id == "r1" => output_preview.clone(),
                _ => None,
            })
            .expect("read tool result");
        assert!(
            read_output.contains("\"start_line\""),
            "the shared structurize_read_tool_output post-processor must run on Read results, got: {read_output}"
        );
        assert!(
            !read_output.contains("1→"),
            "line-number prefixes must be stripped, got: {read_output}"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
