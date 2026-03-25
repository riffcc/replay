//! Context compaction — compress old conversation history into a summary.
//!
//! Triggers when context usage approaches the model's limit.
//! Keeps recent turns intact, summarizes older history.

use anyhow::{Context, Result};
use llm_code_sdk::{Client, MessageCreateParams, MessageParam, SystemPrompt};

use crate::models::ModelDef;

/// Compaction threshold: trigger when input tokens exceed this fraction of context window.
const COMPACT_THRESHOLD: f64 = 0.90;

/// Maximum tokens of recent user messages to preserve (not compacted).
const KEEP_RECENT_TOKENS: usize = 20_000;

/// Rough estimate: 4 chars per token for message size estimation.
const CHARS_PER_TOKEN: usize = 4;

const COMPACT_SYSTEM_PROMPT: &str = "\
You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary \
for another LLM that will resume this task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences discovered
- What remains to be done (clear next steps)
- Any critical data, file paths, or references needed to continue

Be concise and structured. Focus on what the next LLM needs to seamlessly continue. \
Do NOT include pleasantries, meta-commentary, or padding. Just the essential context.";

const SUMMARY_PREFIX: &str = "\
[Context compaction summary — a previous conversation was compressed into this handoff. \
Build on the work already done and avoid duplicating it.]\n\n";

/// Check if compaction should trigger based on current token usage.
pub fn should_compact(last_input_tokens: u64, context_window: u64) -> bool {
    if context_window == 0 || last_input_tokens == 0 {
        return false;
    }
    last_input_tokens as f64 / context_window as f64 >= COMPACT_THRESHOLD
}

/// Compact the conversation history by summarizing older messages.
///
/// Returns the new (shorter) history. The original is consumed.
pub async fn compact(
    history: &mut Vec<MessageParam>,
    model: &ModelDef,
) -> Result<()> {
    if history.len() <= 2 {
        // Nothing to compact — need at least a few turns
        return Ok(());
    }

    // Split: find how many recent messages to keep (by estimated token count)
    let mut keep_from = history.len();
    let mut kept_chars = 0;
    let max_keep_chars = KEEP_RECENT_TOKENS * CHARS_PER_TOKEN;

    // Walk backwards, keep recent messages up to the token budget
    for (i, msg) in history.iter().enumerate().rev() {
        let msg_chars = message_char_count(msg);
        if kept_chars + msg_chars > max_keep_chars && keep_from < history.len() {
            break;
        }
        keep_from = i;
        kept_chars += msg_chars;
    }

    // Always compact at least something — keep_from should be > 0
    if keep_from == 0 {
        keep_from = 1; // Keep at least the first message in "old"
    }

    let old_messages: Vec<MessageParam> = history[..keep_from].to_vec();
    let recent_messages: Vec<MessageParam> = history[keep_from..].to_vec();

    if old_messages.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "compacting {} old messages, keeping {} recent",
        old_messages.len(),
        recent_messages.len(),
    );

    // Generate summary of old messages
    let summary = generate_summary(&old_messages, model).await
        .context("compaction summary generation failed")?;

    // Rebuild history: summary + recent messages
    history.clear();
    history.push(MessageParam::user(format!("{SUMMARY_PREFIX}{summary}")));
    history.extend(recent_messages);

    tracing::info!("compaction complete, new history has {} messages", history.len());

    Ok(())
}

/// Generate a summary of the given messages using the model.
async fn generate_summary(
    messages: &[MessageParam],
    model: &ModelDef,
) -> Result<String> {
    let api_key = crate::models::resolve_auth(model)
        .context("no API key for compaction model")?;

    let mut builder = Client::builder(&api_key)
        .base_url(model.base_url)
        .format(model.format);
    if let Some(acc) = crate::models::codex_account_id() {
        builder = builder.account_id(acc);
    }
    let client = builder.build()
        .context("failed to create compaction client")?;

    // Build the compaction request: send the old conversation and ask for a summary
    let mut compact_messages = messages.to_vec();
    compact_messages.push(MessageParam::user(
        "Please summarize the conversation above as a concise handoff for another LLM. \
         Focus on decisions made, current state, and next steps."
            .to_string(),
    ));

    let params = MessageCreateParams {
        model: model.model_id.into(),
        max_tokens: 4096,
        messages: compact_messages,
        system: Some(SystemPrompt::Text(COMPACT_SYSTEM_PROMPT.to_string())),
        ..Default::default()
    };

    let response = client.messages().create(params).await
        .context("compaction API call failed")?;

    response
        .text()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("compaction produced no text"))
}

fn message_char_count(msg: &MessageParam) -> usize {
    match &msg.content {
        llm_code_sdk::MessageContent::Text(t) => t.len(),
        llm_code_sdk::MessageContent::Blocks(blocks) => {
            blocks.iter().map(|b| match b {
                llm_code_sdk::ContentBlockParam::Text { text, .. } => text.len(),
                llm_code_sdk::ContentBlockParam::ToolResult { content, .. } => {
                    match content {
                        Some(llm_code_sdk::ToolResultContent::Text(t)) => t.len(),
                        Some(llm_code_sdk::ToolResultContent::Blocks(blocks)) => {
                            blocks.iter().map(|b| match b {
                                llm_code_sdk::ToolResultContentBlock::Text { text } => text.len(),
                                _ => 0,
                            }).sum()
                        }
                        None => 0,
                    }
                }
                llm_code_sdk::ContentBlockParam::ToolUse { input, .. } => {
                    serde_json::to_string(input).map(|s| s.len()).unwrap_or(0)
                }
                _ => 0,
            }).sum()
        }
    }
}
