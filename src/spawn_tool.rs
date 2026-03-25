//! Spawn tool — lets the main agent spawn subagents for parallel work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use llm_code_sdk::tools::{Tool, ToolResult};
use llm_code_sdk::types::{InputSchema, ToolParam};

use crate::app::AppState;
use crate::models::ModelDef;

/// Tool for spawning background subagents.
pub struct SpawnTool {
    project_root: PathBuf,
    model_id: Arc<Mutex<String>>,
    app_state: Arc<Mutex<AppState>>,
}

impl SpawnTool {
    pub fn new(
        project_root: impl Into<PathBuf>,
        model_id: Arc<Mutex<String>>,
        app_state: Arc<Mutex<AppState>>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            model_id,
            app_state,
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
            "Spawn a background subagent to work on a task independently. \
             The subagent gets its own tools and conversation. \
             Returns a summary when the subagent completes. \
             Use for parallelizable work that doesn't need user input.",
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

        // Register job
        let job_id = {
            let mut s = self.app_state.lock().unwrap();
            let id = s.add_job(task.to_string());
            s.push_output(format!("🚀 #{id} Spawning: {task}"));
            id
        };

        let model_id = self.model_id.lock().unwrap().clone();
        let model = crate::models::find_by_id(&model_id)
            .unwrap_or(crate::models::default_model());

        let state = Arc::clone(&self.app_state);
        let progress: crate::subagent::ProgressCallback = Arc::new(move |task_name, status| {
            let mut s = state.lock().unwrap();
            s.push_output(format!("  \x1b[2m[{}] {}\x1b[0m", &task_name[..task_name.len().min(20)], status));
        });

        match crate::subagent::run(&full_task, &self.project_root, model, Some(progress)).await {
            Ok(result) => {
                let mut s = self.app_state.lock().unwrap();
                let (icon, status) = if result.success {
                    ("✅", crate::app::JobStatus::Done)
                } else {
                    ("❌", crate::app::JobStatus::Failed)
                };
                s.update_job(job_id, status, result.tool_calls);
                s.push_output(format!("{icon} #{job_id} done ({} calls): {}", result.tool_calls, &result.task[..result.task.len().min(50)]));

                if result.success {
                    ToolResult::success(result.summary)
                } else {
                    ToolResult::error(result.summary)
                }
            }
            Err(e) => {
                let mut s = self.app_state.lock().unwrap();
                s.update_job(job_id, crate::app::JobStatus::Failed, 0);
                s.push_output(format!("❌ #{job_id} failed: {e:#}"));
                ToolResult::error(format!("{e:#}"))
            }
        }
    }
}
