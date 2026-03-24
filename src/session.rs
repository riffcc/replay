//! Session persistence — save and resume conversations.
//!
//! Sessions are stored as JSONL files in ~/.replay/sessions/<project-key>/
//! where project-key is the working directory path with / replaced by -.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use llm_code_sdk::MessageParam;
use serde::{Deserialize, Serialize};

/// A single line in the session JSONL file.
#[derive(Debug, Serialize, Deserialize)]
struct SessionEntry {
    role: String,
    content: String,
    #[serde(default)]
    timestamp: Option<String>,
}

/// Metadata about a saved session (for listing).
#[derive(Debug)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub project_dir: String,
    pub modified: std::time::SystemTime,
    pub turn_count: usize,
    pub first_message: String,
}

/// Get the sessions directory for a given working directory.
fn project_sessions_dir(project_dir: &Path) -> PathBuf {
    let key = project_dir
        .to_string_lossy()
        .replace('/', "-")
        .trim_start_matches('-')
        .to_string();

    dirs::home_dir()
        .expect("no home directory")
        .join(".replay")
        .join("sessions")
        .join(key)
}

/// Generate a session filename from the current timestamp.
fn session_filename() -> String {
    let now = chrono::Local::now();
    now.format("%Y%m%d_%H%M%S.jsonl").to_string()
}

/// Save a conversation to a session file. Creates the file if new, appends if resuming.
pub fn save(project_dir: &Path, session_path: Option<&Path>, history: &[MessageParam]) -> Result<PathBuf> {
    let path = match session_path {
        Some(p) => p.to_path_buf(),
        None => {
            let dir = project_sessions_dir(project_dir);
            std::fs::create_dir_all(&dir)?;
            dir.join(session_filename())
        }
    };

    let mut file = std::fs::File::create(&path)
        .with_context(|| format!("failed to create session file: {}", path.display()))?;

    let now = chrono::Local::now().to_rfc3339();
    for msg in history {
        // Extract text from content (works for Text variant)
        let text = serde_json::to_value(&msg.content)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let entry = SessionEntry {
            role: msg.role.clone(),
            content: text,
            timestamp: Some(now.clone()),
        };
        let line = serde_json::to_string(&entry)?;
        writeln!(file, "{line}")?;
    }

    Ok(path)
}

/// Load a conversation from a session file.
pub fn load(session_path: &Path) -> Result<Vec<MessageParam>> {
    let file = std::fs::File::open(session_path)
        .with_context(|| format!("failed to open session: {}", session_path.display()))?;

    let reader = std::io::BufReader::new(file);
    let mut history = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = serde_json::from_str(&line)
            .with_context(|| "failed to parse session entry")?;

        let msg = match entry.role.as_str() {
            "user" => MessageParam::user(&entry.content),
            "assistant" => MessageParam::assistant(&entry.content),
            _ => continue,
        };
        history.push(msg);
    }

    Ok(history)
}

/// Find the most recent session for a given project directory.
pub fn latest(project_dir: &Path) -> Result<Option<PathBuf>> {
    let dir = project_sessions_dir(project_dir);
    if !dir.exists() {
        return Ok(None);
    }

    let mut sessions: Vec<_> = std::fs::read_dir(&dir)?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "jsonl")
                .unwrap_or(false)
        })
        .collect();

    sessions.sort_by_key(|e| std::cmp::Reverse(e.file_name()));

    Ok(sessions.first().map(|e| e.path()))
}

/// List sessions for a specific project directory.
pub fn list_for_project(project_dir: &Path) -> Result<Vec<SessionInfo>> {
    let dir = project_sessions_dir(project_dir);
    list_sessions_in(&dir, &project_dir.to_string_lossy())
}

/// List all sessions across all projects.
pub fn list_all() -> Result<Vec<SessionInfo>> {
    let sessions_root = dirs::home_dir()
        .expect("no home directory")
        .join(".replay")
        .join("sessions");

    if !sessions_root.exists() {
        return Ok(Vec::new());
    }

    let mut all = Vec::new();

    for entry in std::fs::read_dir(&sessions_root)?.flatten() {
        if entry.path().is_dir() {
            let project_name = entry
                .file_name()
                .to_string_lossy()
                .replace('-', "/");
            let mut sessions = list_sessions_in(&entry.path(), &project_name)?;
            all.append(&mut sessions);
        }
    }

    all.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(all)
}

fn list_sessions_in(dir: &Path, project_name: &str) -> Result<Vec<SessionInfo>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();

    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            let modified = entry.metadata()?.modified()?;

            // Read first user message for preview
            let (turn_count, first_message) = session_preview(&path);

            sessions.push(SessionInfo {
                path,
                project_dir: project_name.to_string(),
                modified,
                turn_count,
                first_message,
            });
        }
    }

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(sessions)
}

/// Read a session file to get turn count and first user message.
fn session_preview(path: &Path) -> (usize, String) {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (0, String::new()),
    };

    let reader = std::io::BufReader::new(file);
    let mut count = 0;
    let mut first = String::new();

    for line in reader.lines().flatten() {
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(&line) {
            count += 1;
            if first.is_empty() && entry.role == "user" {
                first = entry.content;
                if first.len() > 80 {
                    first = format!("{}...", &first[..77]);
                }
            }
        }
    }

    (count, first)
}

/// Format a session list for display.
pub fn format_list(sessions: &[SessionInfo]) -> String {
    if sessions.is_empty() {
        return "No sessions found.".to_string();
    }

    let mut out = String::new();
    for (i, s) in sessions.iter().enumerate() {
        let time = humanize_time(s.modified);
        out.push_str(&format!(
            "  {i}: [{time}] {turns} turns — {preview}\n     {path}\n",
            turns = s.turn_count / 2, // user+assistant pairs
            preview = if s.first_message.is_empty() { "(empty)" } else { &s.first_message },
            path = s.path.display(),
        ));
    }
    out
}

fn humanize_time(time: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Local> = time.into();
    datetime.format("%Y-%m-%d %H:%M").to_string()
}
