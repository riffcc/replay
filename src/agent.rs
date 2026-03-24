//! The core agent — takes an issue, reads the codebase, makes changes.

use anyhow::{Context, Result};
use llm_code_sdk::{
    ApiFormat, Client, MessageCreateParams, MessageParam, SystemPrompt,
    Tool, ToolRunner, ToolRunnerConfig,
};
use llm_code_sdk::tools::{
    BashTool, GlobTool, GrepTool, ListDirectoryTool, ToolEvent, ToolEventCallback,
};
use llm_code_sdk::tools::smart::{SmartReadTool, SmartWriteTool};
use std::path::Path;
use std::sync::Arc;

use crate::beads::Issue;

/// The LLM model to use.
const MODEL: &str = "MiniMax-M2.7-Highspeed";

/// Build the system prompt for a given issue.
fn system_prompt(issue: &Issue) -> String {
    format!(
        r#"You are Replay, a self-improving software agent.

Your task is to solve the following issue by reading the codebase and making changes.

## Issue
- **ID:** {id}
- **Title:** {title}
- **Type:** {issue_type}
- **Priority:** {priority}
{description}

## Rules
- Read the README.md first to understand the project.
- Use SmartRead to understand code structure before editing.
- Use SmartWrite to make targeted, structural edits.
- Run `cargo build` and `cargo test` after changes to verify correctness.
- Make minimal, focused changes that solve the issue.
- Do not add TODOs, placeholders, or incomplete implementations.
- Every change must compile and pass tests.

## When you are done
Respond with a brief summary of what you changed and why."#,
        id = issue.id,
        title = issue.title,
        issue_type = issue.issue_type,
        priority = issue.priority,
        description = issue
            .description
            .as_deref()
            .map(|d| format!("- **Description:** {d}"))
            .unwrap_or_default(),
    )
}

/// Create the tool set for the agent.
fn create_tools(project_root: &Path) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(SmartReadTool::new(project_root)),
        Arc::new(SmartWriteTool::new(project_root)),
        Arc::new(BashTool::new(project_root)),
        Arc::new(GlobTool::new(project_root)),
        Arc::new(GrepTool::new(project_root)),
        Arc::new(ListDirectoryTool::new(project_root)),
    ]
}

/// Run the agent against a single issue. Returns the LLM's summary.
pub async fn solve(issue: &Issue, project_root: &Path) -> Result<String> {
    let api_key = std::env::var("MINIMAX_AUTH_TOKEN")
        .context("MINIMAX_AUTH_TOKEN must be set")?;

    let base_url = std::env::var("MINIMAX_BASE_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/anthropic".into());

    let client = Client::builder(&api_key)
        .base_url(&base_url)
        .format(ApiFormat::Anthropic)
        .build()
        .context("failed to create LLM client")?;

    let tools = create_tools(project_root);

    let on_event: ToolEventCallback = Arc::new(|event| {
        match &event {
            ToolEvent::Text { text } => {
                tracing::info!("{text}");
            }
            ToolEvent::ToolCall { name, .. } => {
                tracing::info!("→ {name}");
            }
            ToolEvent::ToolResult { name, success, .. } => {
                let icon = if *success { "✓" } else { "✗" };
                tracing::info!("  {icon} {name}");
            }
        }
    });

    let config = ToolRunnerConfig {
        max_iterations: Some(50),
        verbose: false,
        on_event: Some(on_event),
        ..Default::default()
    };

    let runner = ToolRunner::with_config(client, tools, config);

    let params = MessageCreateParams {
        model: MODEL.into(),
        max_tokens: 8192,
        messages: vec![MessageParam::user("Solve this issue.")],
        system: Some(SystemPrompt::Text(system_prompt(issue))),
        ..Default::default()
    };

    let response = runner.run(params).await
        .context("agent run failed")?;

    response
        .text()
        .ok_or_else(|| anyhow::anyhow!("agent produced no text response"))
        .map(|s| s.to_string())
}
