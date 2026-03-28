//! Spawn tool — lets the main agent spawn subagents.
//!
//! Subagent tool calls route through the same display callback as the
//! main agent. From the user's perspective, it's just the agent doing work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use llm_code_sdk::tools::{Tool, ToolEventCallback, ToolResult};
use llm_code_sdk::types::{InputSchema, ToolParam};

use crate::app::AppState;
use crate::models::ModelDef;

/// Tool for spawning subagents.
pub struct SpawnTool {
    project_root: PathBuf,
    model_id: Arc<Mutex<String>>,
    app_state: Arc<Mutex<AppState>>,
    /// The same event callback the main agent uses.
    on_event: ToolEventCallback,
}

impl SpawnTool {
    pub fn new(
        project_root: impl Into<PathBuf>,
        model_id: Arc<Mutex<String>>,
        app_state: Arc<Mutex<AppState>>,
        on_event: ToolEventCallback,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            model_id,
            app_state,
            on_event,
        }
    }
}

#[async_trait]
impl Tool for SpawnTool {
    fn name(&self) -> &str {
        "spawn"
    }

    fn to_param(&self) -> ToolParam {
        ToolParam::new(
            "spawn",
            InputSchema::object()
                .required_string("task", "Description of what the subagent should do")
                .optional_string("context", "Additional context for the subagent"),
        )
        .with_description(
            "Spawn a subagent to work on a task. The subagent gets its own \
             conversation and tools. Its work appears in the main output. \
             Returns a summary when done.",
        )
    }

    async fn call(&self, input: HashMap<String, serde_json::Value>) -> ToolResult {
        let task = input.get("task").and_then(|v| v.as_str()).unwrap_or("");
        if task.is_empty() {
            return ToolResult::error("'task' is required");
        }

        let context = input.get("context").and_then(|v| v.as_str()).unwrap_or("");
        let full_task = if context.is_empty() {
            task.to_string()
        } else {
            format!("{task}\n\nContext: {context}")
        };

        let job_id = {
            let mut s = self.app_state.lock().unwrap();
            s.add_job(task.to_string())
        };

        let model_id = self.model_id.lock().unwrap().clone();
        let model = crate::models::find_by_id(&model_id)
            .unwrap_or(crate::models::default_model());

        let callback = Arc::clone(&self.on_event);

        match crate::subagent::run(&full_task, &self.project_root, model, callback).await {
            Ok(result) => {
                let mut s = self.app_state.lock().unwrap();
                let status = if result.success {
                    crate::app::JobStatus::Done
                } else {
                    crate::app::JobStatus::Failed
                };
                s.update_job(job_id, status, result.tool_calls);

                if result.success {
                    ToolResult::success(result.summary)
                } else {
                    ToolResult::error(result.summary)
                }
            }
            Err(e) => {
                let mut s = self.app_state.lock().unwrap();
                s.update_job(job_id, crate::app::JobStatus::Failed, 0);
                ToolResult::error(format!("{e:#}"))
            }
        }
    }
}
