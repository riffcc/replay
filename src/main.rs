//! Replay — self-improving software orchestration.
//!
//! Loop: read issue → solve with LLM → evaluate → follow up.

mod agent;
mod beads;

pub const VERSION: &str = "0.1.0";

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

fn parse_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--target" {
            if i + 1 < args.len() {
                return PathBuf::from(&args[i + 1]);
            }
            eprintln!("error: --target requires a path");
            std::process::exit(1);
        }
        i += 1;
    }
    std::env::current_dir().expect("failed to get current directory")
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let target = parse_args().canonicalize()
        .context("--target path does not exist")?;

    tracing::info!("replay targeting {}", target.display());

    // Step 1: Find the next ready issue.
    let issue = match beads::next_ready(&target)
        .context("failed to query beads")?
    {
        Some(issue) => issue,
        None => {
            tracing::info!("no ready issues — launching rewind");
            launch_rewind()?;
            return Ok(());
        }
    };

    tracing::info!("[{}] {}", issue.id, issue.title);

    // Step 2: Claim it.
    beads::claim(&target, &issue.id)
        .with_context(|| format!("failed to claim {}", issue.id))?;

    tracing::info!("claimed {}", issue.id);

    // Step 3: Let the agent solve it.
    let summary = agent::solve(&issue, &target).await
        .with_context(|| format!("agent failed on {}", issue.id))?;

    tracing::info!("agent summary:\n{summary}");

    // Step 4: Verify the build.
    let verified = verify_build(&target)?;

    if verified {
        // Step 5: Commit and close.
        commit_changes(&target, &issue)?;
        beads::close(&target, &issue.id, &summary)
            .with_context(|| format!("failed to close {}", issue.id))?;
        tracing::info!("closed {}", issue.id);
    } else {
        // Step 5 (alt): File a follow-up.
        let followup_id = beads::create_followup(
            &target,
            &format!("Fix: {}", issue.title),
            &format!("Previous attempt failed verification.\n\nAgent summary:\n{summary}"),
            &issue.id,
        ).context("failed to create follow-up issue")?;
        tracing::info!("created follow-up {followup_id}");
    }

    Ok(())
}

/// Launch the Rewind TUI binary.
fn launch_rewind() -> Result<()> {
    let status = std::process::Command::new("rewind")
        .spawn()
        .context("failed to launch rewind — is it installed?")?
        .wait()
        .context("rewind process failed")?;

    if !status.success() {
        anyhow::bail!("rewind exited with status: {status}");
    }

    Ok(())
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
fn commit_changes(target: &Path, issue: &beads::Issue) -> Result<()> {
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
