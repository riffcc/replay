//! The Replay engine — called by Rewind when the user triggers a solve.

use anyhow::{Context, Result};
use std::path::Path;

use crate::agent;
use crate::beads;
use crate::beads::Issue;

/// The result of running the engine against an issue.
pub enum Outcome {
    /// Build and tests passed, changes committed and issue closed.
    Solved { summary: String },
    /// Verification failed, follow-up issue created.
    Failed { followup_id: String, summary: String },
}

/// Claim an issue, solve it, verify, commit or follow up.
/// Only call this after the user has explicitly chosen to proceed.
pub async fn run(target: &Path, issue: &Issue) -> Result<Outcome> {
    beads::claim(target, &issue.id)
        .with_context(|| format!("failed to claim {}", issue.id))?;

    tracing::info!("claimed {}", issue.id);

    let model = crate::models::default_model();
    let summary = agent::solve(issue, target, model).await
        .with_context(|| format!("agent failed on {}", issue.id))?;

    tracing::info!("agent summary:\n{summary}");

    let verified = verify_build(target)?;

    if verified {
        commit_changes(target, issue)?;
        beads::close(target, &issue.id, &summary)
            .with_context(|| format!("failed to close {}", issue.id))?;
        tracing::info!("closed {}", issue.id);
        Ok(Outcome::Solved { summary })
    } else {
        let followup_id = beads::create_followup(
            target,
            &format!("Fix: {}", issue.title),
            &format!("Previous attempt failed verification.\n\nAgent summary:\n{summary}"),
            &issue.id,
        ).context("failed to create follow-up issue")?;
        tracing::info!("created follow-up {followup_id}");
        Ok(Outcome::Failed { followup_id, summary })
    }
}

/// Run `cargo build` and `cargo test` to verify changes.
fn verify_build(target: &Path) -> Result<bool> {
    let build = std::process::Command::new("cargo")
        .args(["build"])
        .current_dir(target)
        .output()
        .context("failed to run cargo build")?;

    if !build.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        tracing::error!("cargo build failed:\n{stderr}");
        return Ok(false);
    }

    let test = std::process::Command::new("cargo")
        .args(["test"])
        .current_dir(target)
        .output()
        .context("failed to run cargo test")?;

    if !test.status.success() {
        let stderr = String::from_utf8_lossy(&test.stderr);
        tracing::error!("cargo test failed:\n{stderr}");
        return Ok(false);
    }

    tracing::info!("build and tests passed");
    Ok(true)
}

/// Commit all changes with a message referencing the issue.
fn commit_changes(target: &Path, issue: &Issue) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(target)
        .output()
        .context("git add failed")?;

    if !status.status.success() {
        anyhow::bail!("git add failed");
    }

    let msg = format!("{}: {}", issue.id, issue.title);

    let commit = std::process::Command::new("git")
        .args(["commit", "-m", &msg])
        .current_dir(target)
        .output()
        .context("git commit failed")?;

    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        anyhow::bail!("git commit failed: {stderr}");
    }

    tracing::info!("committed: {msg}");
    Ok(())
}
