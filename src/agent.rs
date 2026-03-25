//! The core agent — takes an issue, reads the codebase, makes changes.

use anyhow::{Context, Result};
use llm_code_sdk::{
    ActivateSkillTool, ApiFormat, Client, MessageCreateParams, MessageParam, SkillRegistry,
    SkillResourceTool, SystemPrompt, Tool, ToolRunner, ToolRunnerConfig,
};
use llm_code_sdk::tools::{
    BashTool, GlobTool, GrepTool, ListDirectoryTool, SearchTool, SurveyTool, TasksTool,
    ToolEvent, ToolEventCallback,
};
use llm_code_sdk::tools::smart::{SmartReadTool, SmartWriteTool};
use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::beads::Issue;

/// The LLM model to use.
const MODEL: &str = "MiniMax-M2.7-Highspeed";

/// Build the system prompt for a given issue.
fn system_prompt(issue: &Issue, project_root: &Path) -> String {
    let root_str = project_root.to_string_lossy();
    format!(
        r#"You are Replay, a self-improving software agent.

Your task is to solve the following issue by reading the codebase and making changes.

## Working Directory
- **Project Root:** {root_str}

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
        root_str = root_str,
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
fn create_tools(project_root: &Path, skill_registry: &Arc<RwLock<SkillRegistry>>, survey_callback: llm_code_sdk::tools::SurveyCallback) -> Vec<Arc<dyn Tool>> {
    // Shared read tracker enforces read-before-write
    let tracker = llm_code_sdk::tools::smart::ReadTracker::new();
    let reader = SmartReadTool::with_tracker(project_root, tracker.clone());
    let writer = SmartWriteTool::with_tracker(project_root, tracker);

    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(reader),
        Arc::new(writer),
        Arc::new(BashTool::new(project_root)),
        Arc::new(GlobTool::new(project_root)),
        Arc::new(GrepTool::new(project_root)),
        Arc::new(ListDirectoryTool::new(project_root)),
        Arc::new(SearchTool::new(project_root)),
        Arc::new(TasksTool::new(project_root)),
        Arc::new(SurveyTool::with_callback(survey_callback)),
    ];

    // Only add skill tools if there are skills to activate
    if !skill_registry.read().unwrap().is_empty() {
        tools.push(Arc::new(ActivateSkillTool::new(Arc::clone(skill_registry))));
        tools.push(Arc::new(SkillResourceTool::new(Arc::clone(skill_registry))));
    }

    // Connect MCP servers (builtins + any configured)
    let mcp_tools = llm_code_sdk::tools::mcp::connect_servers(&llm_code_sdk::tools::mcp::builtin_servers());
    tools.extend(mcp_tools);

    tools
}

/// Discover agent skills and load AGENTS.md into the system prompt.
fn build_system_prompt(project_root: &Path, skill_registry: &Arc<RwLock<SkillRegistry>>) -> Option<String> {
    let mut parts = Vec::new();

    // Working directory context
    parts.push(format!("Working directory: {}", project_root.display()));

    // AGENTS.md at project root — always included directly
    let agents_md = project_root.join("AGENTS.md");
    if agents_md.exists() {
        if let Ok(content) = std::fs::read_to_string(&agents_md) {
            parts.push(content);
        }
    }

    // Skill catalog (names + descriptions only — progressive disclosure)
    let catalog = skill_registry.read().unwrap().catalog_prompt();
    if !catalog.is_empty() {
        parts.push(catalog);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Execute a freeform instruction with conversation history. Returns the LLM's response.
pub async fn execute(
    instruction: &str,
    project_root: &Path,
    on_event: ToolEventCallback,
    survey_callback: llm_code_sdk::tools::SurveyCallback,
    history: &mut Vec<MessageParam>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<String> {
    let api_key = std::env::var("MINIMAX_AUTH_TOKEN")
        .context("MINIMAX_AUTH_TOKEN must be set")?;

    let base_url = std::env::var("MINIMAX_BASE_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/anthropic".into());

    let client = Client::builder(&api_key)
        .base_url(&base_url)
        .format(ApiFormat::Anthropic)
        .build()
        .context("failed to create LLM client")?;

    // Discover agent skills: ~/.replay/skills/ first, then project-local .replay/skills/
    let skill_registry = Arc::new(RwLock::new(SkillRegistry::new()));
    {
        let mut reg = skill_registry.write().unwrap();
        let global_skills = dirs::home_dir()
            .expect("no home directory")
            .join(".replay")
            .join("skills");
        reg.discover(&global_skills);

        let local_skills = project_root.join(".replay").join("skills");
        reg.discover(&local_skills);
    }

    let tools = create_tools(project_root, &skill_registry, survey_callback);

    let config = ToolRunnerConfig {
        max_iterations: Some(50),
        verbose: false,
        on_event: Some(on_event),
        cancel: Some(cancel),
        ..Default::default()
    };

    let runner = ToolRunner::with_config(client, tools, config);

    let system = build_system_prompt(project_root, &skill_registry)
        .map(SystemPrompt::Text);

    history.push(MessageParam::user(instruction));

    let params = MessageCreateParams {
        model: MODEL.into(),
        max_tokens: 32000,
        messages: history.clone(),
        system,
        ..Default::default()
    };

    let response = runner.run(params).await
        .context("agent run failed")?;

    let text = response
        .text()
        .ok_or_else(|| anyhow::anyhow!("agent produced no text response"))?
        .to_string();

    history.push(MessageParam::assistant(&text));

    Ok(text)
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

    let skill_registry = Arc::new(RwLock::new(SkillRegistry::new()));
    let noop_survey: llm_code_sdk::tools::SurveyCallback = Arc::new(|_| {
        llm_code_sdk::tools::SurveyResponse { selected: vec![] }
    });
    let tools = create_tools(project_root, &skill_registry, noop_survey);

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
            ToolEvent::Usage { .. } => {}
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
        max_tokens: 32000,
        messages: vec![MessageParam::user("Solve this issue.")],
        system: Some(SystemPrompt::Text(system_prompt(issue, project_root))),
        ..Default::default()
    };

    let response = runner.run(params).await
        .context("agent run failed")?;

    response
        .text()
        .ok_or_else(|| anyhow::anyhow!("agent produced no text response"))
        .map(|s| s.to_string())
}
