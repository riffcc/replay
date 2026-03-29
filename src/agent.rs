//! The core agent — takes an issue, reads the codebase, makes changes.

use anyhow::{Context, Result};
use async_trait::async_trait;
use llm_code_sdk::{
    ActivateSkillTool, Client, MessageCreateParams, MessageParam, SkillRegistry,
    SkillResourceTool, SystemPrompt, Tool, ToolRunner, ToolRunnerConfig,
};
use llm_code_sdk::tools::{
    BashTool, GrepTool, SearchTool, SurveyOption, SurveyRequest,
    SurveyResponse, SurveyTool, TasksTool, ToolEvent, ToolEventCallback,
};

use llm_code_sdk::tools::smart::{SmartReadTool, SmartWriteTool};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::beads::Issue;
use crate::config;
use crate::models::ModelDef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerousAllAccess,
}

impl Default for SandboxMode {
    fn default() -> Self {
        Self::WorkspaceWrite
    }
}

impl SandboxMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "dangerous-all-access" => Some(Self::DangerousAllAccess),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerousAllAccess => "dangerous-all-access",
        }
    }
}

#[derive(Clone)]
pub struct PermissionCallbacks {
    pub bash: llm_code_sdk::tools::SurveyCallback,
    pub write: llm_code_sdk::tools::SurveyCallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandRisk {
    Safe,
    NeedsConfirmation,
    Dangerous,
}

struct SandboxedBashTool {
    inner: BashTool,
    survey_callback: llm_code_sdk::tools::SurveyCallback,
    sandbox_mode: SandboxMode,
    project_root: PathBuf,
    session_allow_prefixes: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl SandboxedBashTool {
    fn new(
        inner: BashTool,
        survey_callback: llm_code_sdk::tools::SurveyCallback,
        sandbox_mode: SandboxMode,
        project_root: &Path,
        session_allow_prefixes: Arc<tokio::sync::Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            inner,
            survey_callback,
            sandbox_mode,
            project_root: project_root.to_path_buf(),
            session_allow_prefixes,
        }
    }

    fn normalize_command(command: &str) -> String {
        command.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn shortest_obvious_prefix(command: &str) -> String {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return String::new();
        }
        match parts[0] {
            "cargo" | "git" if parts.len() >= 2 => format!("{} {}", parts[0], parts[1]),
            cmd if matches!(cmd, "python" | "python3" | "uv" | "node" | "bun") => {
                if parts.len() >= 2 {
                    format!("{} {}", parts[0], parts[1])
                } else {
                    parts[0].to_string()
                }
            }
            cmd if cmd.starts_with("./") || cmd.starts_with("../") || cmd.starts_with('/') => {
                parts[0].to_string()
            }
            _ => parts[0].to_string(),
        }
    }

    fn command_risk(command: &str) -> CommandRisk {
        let normalized = command.trim().to_lowercase();
        if normalized.is_empty() {
            return CommandRisk::Safe;
        }

        // Truly system-level operations — these are dangerous regardless of sandbox
        let system_level = [
            "mkfs", "shutdown", "reboot", "halt", "poweroff",
            "sudo ", "su ", ":(){",
        ];
        if system_level.iter().any(|p| normalized.contains(p)) {
            return CommandRisk::Dangerous;
        }

        // Shell features we can't statically analyze — redirections could
        // write anywhere, pipes/chains could combine safe commands unsafely
        if normalized.contains('>')
            || normalized.contains('|')
            || normalized.contains("&&")
            || normalized.contains(';')
            || normalized.contains('`')
            || normalized.contains("$(")
            || normalized.contains("tee ")
        {
            return CommandRisk::NeedsConfirmation;
        }

        // Read-only / build commands are safe (no shell features above)
        let safe_prefixes = [
            "pwd", "ls", "ls ", "dir", "find ", "which ", "where ", "echo ", "printf ",
            "cat ", "head ", "tail ", "sed -n ", "grep ", "rg ", "git status", "git diff",
            "git log", "git show", "git branch", "git rev-parse", "cargo build", "cargo test",
            "cargo check", "cargo fmt", "cargo clippy", "rm ", "rm -rf ", "rm -f ",
            "mkdir ", "cp ", "mv ", "touch ",
        ];
        if safe_prefixes.iter().any(|p| normalized == *p || normalized.starts_with(p)) {
            return CommandRisk::Safe;
        }

        // Network access needs confirmation
        let network = ["curl ", "wget ", "scp ", "ssh ", "nc ", "ncat "];
        if network.iter().any(|p| normalized.starts_with(p)) {
            return CommandRisk::NeedsConfirmation;
        }

        CommandRisk::NeedsConfirmation
    }

    /// Extract absolute paths from command arguments that fall outside the project root.
    /// Only examines the command portion before heredocs or quoted bodies.
    fn cross_root_paths(&self, command: &str) -> Vec<String> {
        // Truncate at heredoc markers to avoid parsing script bodies
        let cmd_part = if let Some(pos) = command.find("<<") {
            &command[..pos]
        } else {
            command
        };

        cmd_part.split_whitespace()
            .filter(|arg| arg.starts_with('/') && arg.len() > 1) // skip bare "/"
            .filter(|arg| {
                // Must look like a filesystem path, not a flag or operator
                let clean = arg.trim_matches(|c: char| c == '\'' || c == '"' || c == ';' || c == ')' || c == '(');
                if clean.len() <= 1 { return false; }
                let p = Path::new(clean);
                !p.starts_with(&self.project_root)
            })
            .map(|s| s.trim_matches(|c: char| c == '\'' || c == '"' || c == ';' || c == ')' || c == '(').to_string())
            .collect()
    }

    async fn is_allowed(&self, command: &str) -> bool {
        if self.sandbox_mode == SandboxMode::DangerousAllAccess {
            return true;
        }

        let normalized = Self::normalize_command(command);
        let risk = Self::command_risk(&normalized);

        // Check if the command touches paths outside the project root.
        // Even "safe" commands need confirmation when they reach outside.
        let outside_paths = self.cross_root_paths(&normalized);
        if !outside_paths.is_empty() && self.sandbox_mode != SandboxMode::DangerousAllAccess {
            // Check allow lists for the outside paths
            let project_settings = config::load_project_sandbox_settings(&self.project_root);
            let session = self.session_allow_prefixes.lock().await;
            let all_allowed = outside_paths.iter().all(|p| {
                let path_matches = |prefix: &str| {
                    let normalized_prefix = prefix.trim_end_matches('/');
                    p == normalized_prefix || p.starts_with(&format!("{normalized_prefix}/"))
                };
                project_settings.bash_allow_prefixes.iter().any(|prefix| path_matches(prefix))
                    || session.iter().any(|prefix| path_matches(prefix))
            });
            drop(session);

            if !all_allowed {
                let dirs: Vec<String> = outside_paths.iter()
                    .map(|p| p.to_string())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                let dir_display = dirs.join(", ");
                return self.confirm_cross_root(&dir_display, &dirs).await;
            }
        }

        // Normal risk classification (applies to all commands, cross-root or not)
        let prefix = Self::shortest_obvious_prefix(&normalized);
        if prefix.is_empty() {
            return true;
        }

        let project_settings = config::load_project_sandbox_settings(&self.project_root);
        if project_settings.bash_allow_prefixes.iter().any(|p| normalized.starts_with(p)) {
            return true;
        }

        let session = self.session_allow_prefixes.lock().await;
        if session.iter().any(|p| normalized.starts_with(p)) {
            return true;
        }
        drop(session);

        self.confirm(&normalized, &prefix, risk).await
    }

    async fn confirm_cross_root(&self, dir_list: &str, dir_prefixes: &[String]) -> bool {
        let response = (self.survey_callback)(SurveyRequest {
            prompt: format!("Allow command accessing {dir_list} (outside the project directory)?"),
            options: vec![
                SurveyOption {
                    label: "Allow once".to_string(),
                    description: Some("Run this command now".to_string()),
                },
                SurveyOption {
                    label: "Allow for this session".to_string(),
                    description: Some(format!("Allow access to {dir_list} until Replay exits")),
                },
                SurveyOption {
                    label: "Always allow for this project".to_string(),
                    description: Some("Save this setting to .replay/".to_string()),
                },
                SurveyOption {
                    label: "Deny".to_string(),
                    description: Some("Block this command".to_string()),
                },
            ],
            multi: false,
        });

        match response.selected.first().copied() {
            Some(0) => true,
            Some(1) => {
                let mut session = self.session_allow_prefixes.lock().await;
                for dir in dir_prefixes {
                    if !session.iter().any(|p| p == dir) {
                        session.push(dir.clone());
                    }
                }
                true
            }
            Some(2) => {
                for dir in dir_prefixes {
                    let _ = config::save_project_bash_allow_prefix(&self.project_root, dir);
                }
                true
            }
            _ => false,
        }
    }

    async fn confirm(&self, command: &str, prefix: &str, risk: CommandRisk) -> bool {
        let risk_label = match risk {
            CommandRisk::Safe => return true,
            CommandRisk::NeedsConfirmation => "Allow",
            CommandRisk::Dangerous => "Allow dangerous",
        };

        let response = (self.survey_callback)(SurveyRequest {
            prompt: format!("{risk_label} command `{prefix}`?"),
            options: vec![
                SurveyOption {
                    label: "Allow once".to_string(),
                    description: Some("Run this command now".to_string()),
                },
                SurveyOption {
                    label: "Allow for this session".to_string(),
                    description: Some("Allow this command until Replay exits".to_string()),
                },
                SurveyOption {
                    label: "Always allow for this project".to_string(),
                    description: Some("Save this setting to .replay/".to_string()),
                },
                SurveyOption {
                    label: "Deny".to_string(),
                    description: Some("Block this command".to_string()),
                },
            ],
            multi: false,
        });

        match response.selected.first().copied() {
            Some(0) => true,
            Some(1) => {
                let mut session = self.session_allow_prefixes.lock().await;
                if !session.iter().any(|p| p == prefix) {
                    session.push(prefix.to_string());
                }
                true
            }
            Some(2) => {
                let _ = config::save_project_bash_allow_prefix(&self.project_root, prefix);
                true
            }
            _ => false,
        }
    }
}

#[async_trait]
impl Tool for SandboxedBashTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn to_param(&self) -> llm_code_sdk::types::ToolParam {
        self.inner.to_param()
    }

    async fn call(&self, input: HashMap<String, serde_json::Value>) -> llm_code_sdk::tools::ToolResult {
        if input.get("process_id").is_some() {
            return self.inner.call(input).await;
        }

        let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if !self.is_allowed(command).await {
            return llm_code_sdk::tools::ToolResult::error("Command blocked by sandbox policy");
        }

        self.inner.call(input).await
    }
}

struct SandboxedWriteTool {
    inner: SmartWriteTool,
    survey_callback: llm_code_sdk::tools::SurveyCallback,
    project_root: PathBuf,
    session_allow_prefixes: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl SandboxedWriteTool {
    fn new(
        inner: SmartWriteTool,
        survey_callback: llm_code_sdk::tools::SurveyCallback,
        project_root: &Path,
        session_allow_prefixes: Arc<tokio::sync::Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            inner,
            survey_callback,
            project_root: project_root.to_path_buf(),
            session_allow_prefixes,
        }
    }

    fn normalize_path(path: &str) -> String {
        path.trim().to_string()
    }

    /// Derive the directory prefix for permission grants (no trailing slash).
    /// For `/tmp/test.txt` → `/tmp`
    /// For `/home/user/projects/foo/bar.rs` → `/home/user/projects/foo`
    fn directory_prefix(path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let p = Path::new(trimmed);
        match p.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.display().to_string(),
            _ => trimmed.to_string(),
        }
    }

    /// Check if a path falls under an allowed prefix.
    fn path_allowed(path: &str, prefix: &str) -> bool {
        let normalized_prefix = prefix.trim_end_matches('/');
        path == normalized_prefix || path.starts_with(&format!("{normalized_prefix}/"))
    }

    fn is_cross_root(&self, path: &str) -> bool {
        let requested = Path::new(path);
        requested.is_absolute() && !requested.starts_with(&self.project_root)
    }

    async fn is_allowed(&self, path: &str) -> bool {
        if !self.is_cross_root(path) {
            return true;
        }

        let normalized = Self::normalize_path(path);
        let prefix = Self::directory_prefix(&normalized);
        if prefix.is_empty() {
            return false;
        }

        let project_settings = config::load_project_sandbox_settings(&self.project_root);
        if project_settings.write_allow_prefixes.iter().any(|p| Self::path_allowed(&normalized, p)) {
            return true;
        }

        let session = self.session_allow_prefixes.lock().await;
        if session.iter().any(|p| Self::path_allowed(&normalized, p)) {
            return true;
        }
        drop(session);

        let response = (self.survey_callback)(SurveyRequest {
            prompt: format!("Allow writing to {prefix} (outside the project directory)?"),
            options: vec![
                SurveyOption {
                    label: "Allow once".to_string(),
                    description: Some("Allow this write now".to_string()),
                },
                SurveyOption {
                    label: "Allow for this session".to_string(),
                    description: Some("Allow writes until Replay exits".to_string()),
                },
                SurveyOption {
                    label: "Always allow for this project".to_string(),
                    description: Some("Save this setting to .replay/".to_string()),
                },
                SurveyOption {
                    label: "Deny".to_string(),
                    description: Some("Block this write".to_string()),
                },
            ],
            multi: false,
        });

        match response.selected.first().copied() {
            Some(0) => true,
            Some(1) => {
                let mut session = self.session_allow_prefixes.lock().await;
                if !session.iter().any(|p| p == &prefix) {
                    session.push(prefix.clone());
                }
                true
            }
            Some(2) => {
                let _ = config::save_project_write_allow_prefix(&self.project_root, &prefix);
                true
            }
            _ => false,
        }
    }
}

#[async_trait]
impl Tool for SandboxedWriteTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn to_param(&self) -> llm_code_sdk::types::ToolParam {
        self.inner.to_param()
    }

    async fn call(&self, input: HashMap<String, serde_json::Value>) -> llm_code_sdk::tools::ToolResult {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if !self.is_allowed(path).await {
            return llm_code_sdk::tools::ToolResult::error("Write blocked by sandbox policy");
        }
        self.inner.call(input).await
    }
}

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
/// Return type includes the process registry handle for the host.
async fn create_tools(
    project_root: &Path,
    skill_registry: &Arc<RwLock<SkillRegistry>>,
    permission_callbacks: PermissionCallbacks,
    spawn_tool: Option<Arc<dyn Tool>>,
    sandbox_mode: SandboxMode,
    session_permissions: &SessionPermissions,
) -> (Vec<Arc<dyn Tool>>, Arc<tokio::sync::Mutex<llm_code_sdk::tools::BgProcessRegistry>>) {
    let tracker = llm_code_sdk::tools::smart::ReadTracker::new();
    let reader = SmartReadTool::with_tracker(project_root, tracker.clone());
    let writer = SmartWriteTool::with_tracker(project_root, tracker);

    let bash = BashTool::new(project_root).with_timeout(30);
    let process_registry = bash.process_registry();

    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(reader),
        Arc::new(GrepTool::new(project_root)),
        Arc::new(SearchTool::new(project_root)),
        Arc::new(SurveyTool::with_callback(permission_callbacks.bash.clone())),
    ];

    if sandbox_mode != SandboxMode::ReadOnly {
        let write_tool: Arc<dyn Tool> = if sandbox_mode == SandboxMode::DangerousAllAccess {
            Arc::new(writer)
        } else {
            Arc::new(SandboxedWriteTool::new(
                writer,
                permission_callbacks.write.clone(),
                project_root,
                Arc::clone(&session_permissions.write_allow_prefixes),
            ))
        };
        tools.insert(1, write_tool);

        let bash_tool: Arc<dyn Tool> = if sandbox_mode == SandboxMode::DangerousAllAccess {
            Arc::new(bash)
        } else {
            Arc::new(SandboxedBashTool::new(
                bash,
                permission_callbacks.bash.clone(),
                sandbox_mode,
                project_root,
                Arc::clone(&session_permissions.bash_allow_prefixes),
            ))
        };
        tools.insert(2, bash_tool);
        tools.push(Arc::new(TasksTool::new(project_root)));

        if let Some(spawn) = spawn_tool {
            tools.push(spawn);
        }
    }

    if !skill_registry.read().unwrap().is_empty() {
        tools.push(Arc::new(ActivateSkillTool::new(Arc::clone(skill_registry))));
        tools.push(Arc::new(SkillResourceTool::new(Arc::clone(skill_registry))));
    }

    let mcp_tools = llm_code_sdk::tools::mcp::connect_servers(&llm_code_sdk::tools::mcp::builtin_servers()).await;
    tools.extend(mcp_tools);

    (tools, process_registry)
}


/// Discover agent skills and load AGENTS.md into the system prompt.
fn build_system_prompt(
    project_root: &Path,
    skill_registry: &Arc<RwLock<SkillRegistry>>,
    sandbox_mode: SandboxMode,
) -> Option<String> {
    let mut parts = Vec::new();

    parts.push(format!("Working directory: {}", project_root.display()));
    parts.push(format!("Sandbox mode: {}", sandbox_mode.as_str()));

    match sandbox_mode {
        SandboxMode::ReadOnly => {
            parts.push("Sandbox policy: read-only mode. Only non-mutating read/discovery tools are available; write, bash, tasks, and spawn are disabled.".to_string());
        }
        SandboxMode::WorkspaceWrite => {
            parts.push("Sandbox policy: workspace-write mode. Mutating operations are intended to stay within the project workspace; extra prompts/escalations for dangerous commands, network access, and cross-root writes are handled by dedicated permission flows.".to_string());
        }
        SandboxMode::DangerousAllAccess => {
            parts.push("Sandbox policy: dangerous-all-access mode. Full unrestricted tool access is available.".to_string());
        }
    }

    // Tool usage guidance — transitive prompting, not negative prompting
    parts.push(
"## Tool usage

When inspecting code, use read — it supports multi-range reads, symbol extraction, \
AST views, and layered analysis. When searching for patterns, use grep — it groups \
results by structural unit with call graph context by default. When finding files, \
use glob. When running builds, tests, git, or repo CLIs, use bash.

STRONGLY prefer native tools over bash for file inspection (not cat, head, tail, sed -n). \
STRONGLY prefer native tools over bash for search (not grep, rg, find). \
The native tools are faster, structured, and memory-efficient. \
Bash is for executing commands that have side effects.".to_string());

    let agents_md = project_root.join("AGENTS.md");
    if agents_md.exists() {
        if let Ok(content) = std::fs::read_to_string(&agents_md) {
            parts.push(content);
        }
    }

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

pub type ProcessRegistry = Arc<tokio::sync::Mutex<llm_code_sdk::tools::BgProcessRegistry>>;

/// Session-scoped permission state that persists across agent turns.
#[derive(Clone)]
pub struct SessionPermissions {
    pub bash_allow_prefixes: Arc<tokio::sync::Mutex<Vec<String>>>,
    pub write_allow_prefixes: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl SessionPermissions {
    pub fn new() -> Self {
        Self {
            bash_allow_prefixes: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            write_allow_prefixes: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }
}

pub async fn execute(
    instruction: &str,
    project_root: &Path,
    on_event: ToolEventCallback,
    permission_callbacks: PermissionCallbacks,
    history: &mut Vec<MessageParam>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    model: &ModelDef,
    reasoning_effort: Option<String>,
    spawn_tool: Option<Arc<dyn Tool>>,
    sandbox_mode: SandboxMode,
    session_permissions: &SessionPermissions,
) -> Result<(String, ProcessRegistry)> {
    let api_key = crate::models::resolve_auth(model)
        .context(format!("no API key for {} ({})", model.name, model.provider))?;

    let mut builder = Client::builder(&api_key)
        .base_url(model.base_url)
        .format(model.format);
    if let Some(acc) = crate::models::codex_account_id() {
        builder = builder.account_id(acc);
    }
    let client = builder.build().context("failed to create LLM client")?;

    history.push(MessageParam::user(instruction));
    let mut mem_report = crate::mem::RunMemReport::start(
        project_root,
        model.model_id.to_string(),
        instruction.chars().count(),
        history,
    );

    let skill_registry = Arc::new(RwLock::new(SkillRegistry::new()));
    {
        let mut reg = skill_registry.write().unwrap();
        let global_skills = dirs::home_dir().expect("no home directory").join(".replay").join("skills");
        reg.discover(&global_skills);

        let local_skills = project_root.join(".replay").join("skills");
        reg.discover(&local_skills);
    }
    mem_report.checkpoint_messages("after_skill_discovery", history, None);

    let (tools, process_registry) = create_tools(
        project_root,
        &skill_registry,
        permission_callbacks,
        spawn_tool,
        sandbox_mode,
        session_permissions,
    ).await;
    mem_report.checkpoint_messages(
        "after_tool_setup",
        history,
        Some(format!("tool_count={}", tools.len())),
    );

    let mem_report = Arc::new(std::sync::Mutex::new(mem_report));
    let on_event_outer = on_event.clone();
    let mem_report_events = Arc::clone(&mem_report);
    let combined_on_event: ToolEventCallback = Arc::new(move |event| {
        if let Ok(mut report) = mem_report_events.lock() {
            report.record_tool_event(&event);
        }
        on_event_outer(event);
    });

    let config = ToolRunnerConfig {
        max_iterations: Some(50),
        verbose: false,
        on_event: Some(combined_on_event),
        cancel: Some(cancel),
        ..Default::default()
    };

    let runner = ToolRunner::with_config(client, tools, config);
    let system = build_system_prompt(project_root, &skill_registry, sandbox_mode).map(SystemPrompt::Text);
    if let Ok(mut report) = mem_report.lock() {
        report.checkpoint_messages("after_system_prompt", history, None);
    }

    let params = MessageCreateParams {
        model: model.model_id.into(),
        max_tokens: 32000,
        messages: history.clone(),
        system,
        reasoning_effort: if model.supports_effort { reasoning_effort } else { None },
        ..Default::default()
    };
    if let Ok(mut report) = mem_report.lock() {
        report.checkpoint_messages(
            "before_runner_run",
            &params.messages,
            Some(format!("message_count={}", params.messages.len())),
        );
    }

    let response = match runner.run(params).await {
        Ok(response) => response,
        Err(err) => {
            if let Ok(mut report) = mem_report.lock() {
                let _ = report.write_error(history, &err.to_string());
            }
            return Err(err).context("agent run failed");
        }
    };
    if let Ok(mut report) = mem_report.lock() {
        report.checkpoint_messages("after_runner_run", history, None);
    }

    let text = response
        .text()
        .ok_or_else(|| anyhow::anyhow!("agent produced no text response"))?
        .to_string();

    history.push(MessageParam::assistant(&text));
    if let Ok(mut report) = mem_report.lock() {
        let _ = report.write_success(history, &text);
    }
    Ok((text, process_registry))
}


pub async fn solve(issue: &Issue, project_root: &Path, model: &ModelDef) -> Result<String> {
    let api_key = crate::models::resolve_auth(model)
        .context(format!("no API key for {} ({})", model.name, model.provider))?;

    let mut builder = Client::builder(&api_key)
        .base_url(model.base_url)
        .format(model.format);
    if let Some(acc) = crate::models::codex_account_id() {
        builder = builder.account_id(acc);
    }
    let client = builder.build().context("failed to create LLM client")?;

    let skill_registry = Arc::new(RwLock::new(SkillRegistry::new()));
    let noop_survey: llm_code_sdk::tools::SurveyCallback = Arc::new(|_| {
        SurveyResponse { selected: vec![] }
    });

    let permission_callbacks = PermissionCallbacks {
        bash: noop_survey.clone(),
        write: noop_survey,
    };
    let session_permissions = SessionPermissions::new();
    let (tools, _process_registry) = create_tools(
        project_root,
        &skill_registry,
        permission_callbacks,
        None,
        SandboxMode::DangerousAllAccess,
        &session_permissions,
    )
    .await;

    let on_event: ToolEventCallback = Arc::new(|event| match &event {
        ToolEvent::Text { text } => tracing::info!("{text}"),
        ToolEvent::ToolCall { name, .. } => tracing::info!("→ {name}"),
        ToolEvent::ToolResult { name, success, .. } => {
            let icon = if *success { "✓" } else { "✗" };
            tracing::info!("  {icon} {name}");
        }
        ToolEvent::Usage { .. } => {}
    });

    let config = ToolRunnerConfig {
        max_iterations: Some(50),
        verbose: false,
        on_event: Some(on_event),
        ..Default::default()
    };

    let runner = ToolRunner::with_config(client, tools, config);
    let params = MessageCreateParams {
        model: model.model_id.into(),
        max_tokens: 32000,
        messages: vec![MessageParam::user("Solve this issue.")],
        system: Some(SystemPrompt::Text(system_prompt(issue, project_root))),
        ..Default::default()
    };

    let response = runner.run(params).await.context("agent run failed")?;
    response
        .text()
        .ok_or_else(|| anyhow::anyhow!("agent produced no text response"))
        .map(|s| s.to_string())
}
