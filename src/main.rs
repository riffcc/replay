//! Replay — self-improving software orchestration.
//!
//! Loop: read issue → solve with LLM → evaluate → follow up.

mod agent;
mod beads;

pub const VERSION: &str = "0.1.0";

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let project_root = std::env::current_dir()
        .context("failed to get current directory")?;

    tracing::info!("replay starting in {}", project_root.display());

    // Step 1: Find the next ready issue.
    let issue = beads::next_ready()
        .context("failed to query beads")?
        .ok_or_else(|| anyhow::anyhow!("no ready issues — nothing to do"))?;

    tracing::info!("[{}] {}", issue.id, issue.title);

    // Step 2: Claim it.
    beads::claim(&issue.id)
        .with_context(|| format!("failed to claim {}", issue.id))?;

    tracing::info!("claimed {}", issue.id);

    // Step 3: Let the agent solve it.
    let summary = agent::solve(&issue, &project_root).await
        .with_context(|| format!("agent failed on {}", issue.id))?;

    tracing::info!("agent summary:\n{summary}");

    // Step 4: Verify the build.
    let verified = verify_build(&project_root)?;

    if verified {
        // Step 5: Commit and close.
        commit_changes(&issue)?;
        beads::close(&issue.id, &summary)
            .with_context(|| format!("failed to close {}", issue.id))?;
        tracing::info!("closed {}", issue.id);
    } else {
        // Step 5 (alt): File a follow-up.
        let followup_id = beads::create_followup(
            &format!("Fix: {}", issue.title),
            &format!("Previous attempt failed verification.\n\nAgent summary:\n{summary}"),
            &issue.id,
        ).context("failed to create follow-up issue")?;
        tracing::info!("created follow-up {followup_id}");
    }

    Ok(())
}

/// Run `cargo build` and `cargo test` to verify changes.
fn verify_build(project_root: &PathBuf) -> Result<bool> {
    let build = std::process::Command::new("cargo")
        .args(["build"])
        .current_dir(project_root)
        .output()
        .context("failed to run cargo build")?;

    if !build.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        tracing::error!("cargo build failed:\n{stderr}");
        return Ok(false);
    }

    let test = std::process::Command::new("cargo")
        .args(["test"])
        .current_dir(project_root)
        .output()
        .context("failed to run cargo test")?;

    if !test.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        tracing::error!("cargo test failed:\n{stderr}");
        return Ok(false);
    }

    tracing::info!("build and tests passed");
    Ok(true)
}

/// Commit all changes with a message referencing the issue.
fn commit_changes(issue: &beads::Issue) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(["add", "-A"])
        .output()
        .context("git add failed")?;

    if !status.status.success() {
        anyhow::bail!("git add failed");
    }

    let msg = format!("{}: {}", issue.id, issue.title);

    let commit = std::process::Command::new("git")
        .args(["commit", "-m", &msg])
        .output()
        .context("git commit failed")?;

    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        anyhow::bail!("git commit failed: {stderr}");
    }

    tracing::info!("committed: {msg}");
    Ok(())
}
