//! Session coordinator — shared runtime state for Replay.
//!
//! This module tracks active tasks, lane plans, and worktree/group planning.
//! Provider pressure is intentionally tracked elsewhere in provider_pressure.rs.

use std::collections::HashMap;
use std::time::Instant;

use crate::beads::Issue;

/// A task lane in the dry-run / parallel planner.
#[derive(Debug, Clone)]
pub struct TaskLane {
    pub id: String,
    pub issue_id: String,
    pub title: String,
    pub assigned_model: Option<String>,
    pub active: bool,
}

/// Session-wide coordination state for task/lane planning.
#[derive(Debug)]
pub struct CoordinatorState {
    pub tasks: HashMap<String, Issue>,
    pub lanes: HashMap<String, TaskLane>,
    pub active_model: Option<String>,
    pub last_refresh: Instant,
}

impl CoordinatorState {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            lanes: HashMap::new(),
            active_model: None,
            last_refresh: Instant::now(),
        }
    }

    /// Remember the active model for future lane planning.
    pub fn set_active_model(&mut self, model_id: impl Into<String>) {
        self.active_model = Some(model_id.into());
        self.last_refresh = Instant::now();
    }

    pub fn initialize_from_tasks_snapshot(&mut self, issues: impl IntoIterator<Item = Issue>) {
        self.tasks.clear();
        for issue in issues {
            self.tasks.insert(issue.id.clone(), issue);
        }
        self.last_refresh = Instant::now();
    }

    pub fn ingest_issue(&mut self, issue: Issue) {
        self.tasks.insert(issue.id.clone(), issue);
        self.last_refresh = Instant::now();
    }

    pub fn upsert_lane(&mut self, lane: TaskLane) {
        self.lanes.insert(lane.id.clone(), lane);
        self.last_refresh = Instant::now();
    }

    pub fn plan_parallel_work(&self, issue_id: &str) -> Vec<TaskLane> {
        self.tasks
            .get(issue_id)
            .map(|issue| {
                vec![TaskLane {
                    id: format!("lane:{issue_id}"),
                    issue_id: issue.id.clone(),
                    title: issue.title.clone(),
                    assigned_model: self.active_model.clone(),
                    active: true,
                }]
            })
            .unwrap_or_default()
    }
}
