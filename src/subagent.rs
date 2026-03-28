//! Subagents — agent instances that share the main output stream.
//!
//! A subagent gets its own conversation and tools but its tool events
//! route through the same display callback as the main agent.
//! From the user's perspective, it's just the agent doing more work.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use llm_code_sdk::{
    Client, MessageCreateParams, MessageParam,
    SystemPrompt, Tool, ToolRunner, ToolRunnerConfig,
};
use llm_code_sdk::tools::{
    BashTool, GrepTool, SearchTool, TasksTool, ToolEvent, ToolEventCallback,
};
use llm_code_sdk::tools::smart::{SmartReadTool, SmartWriteTool};

use crate::models::ModelDef;

/// Result of a subagent run.
#[derive(Debug)]
pub struct SubagentResult {
    pub task: String,
    pub summary: String,
    pub success: bool,
    pub tool_calls: usize,
}

/// Spawn and run a subagent for a given task.
///
/// `on_event` is the same callback the main agent uses — subagent tool
/// calls appear in the main stream identically to the main agent's.
pub async fn run(
    task: &str,
    project_root: &Path,
    model: &ModelDef,
    on_event: ToolEventCallback,
) -> Result<SubagentResult> {
    let api_key = crate::models::resolve_auth(model)
        .context(format!("no API key for {} ({})", model.name, model.provider))?;

    let mut builder = Client::builder(&api_key)
        .base_url(model.base_url)
        .format(model.format);
    if let Some(acc) = crate::models::codex_account_id() {
        builder = builder.account_id(acc);
    }
    let client = builder.build()
        .context("failed to create LLM client")?;

    // Independent read tracker for this subagent
    let tracker = llm_code_sdk::tools::smart::ReadTracker::new();
    let reader = SmartReadTool::with_tracker(project_root, tracker.clone());
    let writer = SmartWriteTool::with_tracker(project_root, tracker);

    // No survey tool — subagents can't ask the user questions
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(reader),
        Arc::new(writer),
        Arc::new(BashTool::new(project_root).with_timeout(30)),
        Arc::new(GrepTool::new(project_root)),
        Arc::new(SearchTool::new(project_root)),
        Arc::new(TasksTool::new(project_root)),
    ];

    // Count tool calls via the shared callback
    let tool_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_count_clone = Arc::clone(&tool_count);

    let wrapped_callback: ToolEventCallback = Arc::new(move |event| {
        if matches!(&event, ToolEvent::ToolCall { .. }) {
            tool_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // Route everything through the main display callback
        on_event(event);
    });

    let config = ToolRunnerConfig {
        max_iterations: Some(30),
        verbose: false,
        on_event: Some(wrapped_callback),
        ..Default::default()
    };

    let runner = ToolRunner::with_config(client, tools, config);

    let system_prompt = format!(
        "Working directory: {}\n\n\
         You are a subagent. Complete the following task efficiently.\n\
         When done, summarize what you did in 2-3 sentences.",
        project_root.display()
    );

    // Load AGENTS.md if available
    let agents_md = project_root.join("AGENTS.md");
    let system = if agents_md.exists() {
        if let Ok(content) = std::fs::read_to_string(&agents_md) {
            format!("{system_prompt}\n\n{content}")
        } else {
            system_prompt
        }
    } else {
        system_prompt
    };

    let params = MessageCreateParams {
        model: model.model_id.into(),
        max_tokens: 32000,
        messages: vec![MessageParam::user(task)],
        system: Some(SystemPrompt::Text(system)),
        ..Default::default()
    };

    let response = runner.run(params).await
        .context("subagent run failed");

    let calls = tool_count.load(std::sync::atomic::Ordering::Relaxed);

    match response {
        Ok(msg) => {
            let summary = msg.text()
                .unwrap_or("(no summary)")
                .to_string();
            Ok(SubagentResult {
                task: task.to_string(),
                summary,
                success: true,
                tool_calls: calls,
            })
        }
        Err(e) => {
            Ok(SubagentResult {
                task: task.to_string(),
                summary: format!("Failed: {e:#}"),
                success: false,
                tool_calls: calls,
            })
        }
    }
}
