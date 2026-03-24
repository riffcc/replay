//! Beads integration — reads issues from `bd` CLI.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

/// A Beads issue as returned by `bd show --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub description: Option<String>,
    pub priority: u8,
    pub issue_type: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Fetch the next ready issue from Beads.
pub fn next_ready() -> Result<Option<Issue>> {
    let output = Command::new("bd")
        .args(["ready", "--json"])
        .output()
        .context("failed to run `bd ready`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`bd ready` failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let issues: Vec<Issue> = serde_json::from_str(&stdout)
        .context("failed to parse `bd ready` output")?;

    Ok(issues.into_iter().next())
}

/// Fetch full details for a specific issue.
pub fn show(id: &str) -> Result<Issue> {
    let output = Command::new("bd")
        .args(["show", id, "--json"])
        .output()
        .with_context(|| format!("failed to run `bd show {id}`"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`bd show {id}` failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let issue: Issue = serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse `bd show {id}` output"))?;

    Ok(issue)
}

/// Claim an issue (atomically set assignee + in_progress).
pub fn claim(id: &str) -> Result<()> {
    let output = Command::new("bd")
        .args(["update", id, "--claim"])
        .output()
        .with_context(|| format!("failed to run `bd update {id} --claim`"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`bd update {id} --claim` failed: {stderr}");
    }

    Ok(())
}

/// Close a completed issue.
pub fn close(id: &str, reason: &str) -> Result<()> {
    let output = Command::new("bd")
        .args(["close", id, "--reason", reason])
        .output()
        .with_context(|| format!("failed to run `bd close {id}`"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`bd close {id}` failed: {stderr}");
    }

    Ok(())
}

/// Create a follow-up issue linked to a parent.
pub fn create_followup(title: &str, description: &str, discovered_from: &str) -> Result<String> {
    let output = Command::new("bd")
        .args([
            "create",
            "--title", title,
            "--description", description,
            "--type", "task",
            "--dep", &format!("discovered-from:{discovered_from}"),
            "--json",
        ])
        .output()
        .context("failed to run `bd create`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`bd create` failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let created: Issue = serde_json::from_str(&stdout)
        .context("failed to parse `bd create` output")?;

    Ok(created.id)
}
