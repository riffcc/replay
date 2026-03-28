use std::path::{Path, PathBuf};

use llm_code_sdk::MessageParam;
use serde::Serialize;

#[derive(Debug)]
pub struct RunMemReport {
    started_at: std::time::SystemTime,
    project_root: PathBuf,
    pid: u32,
    initial_history_len: usize,
    initial_message_chars: usize,
    initial_rss_kb: Option<u64>,
}

#[derive(Debug, Serialize)]
struct MemReportRecord {
    timestamp: String,
    pid: u32,
    project_root: String,
    initial_history_len: usize,
    final_history_len: usize,
    initial_message_chars: usize,
    final_message_chars: usize,
    response_chars: usize,
    initial_rss_kb: Option<u64>,
    final_rss_kb: Option<u64>,
    rss_delta_kb: Option<i64>,
}

impl RunMemReport {
    pub fn start(project_root: &Path, initial_history_len: usize, messages: &[MessageParam]) -> Self {
        Self {
            started_at: std::time::SystemTime::now(),
            project_root: project_root.to_path_buf(),
            pid: std::process::id(),
            initial_history_len,
            initial_message_chars: total_message_chars(messages),
            initial_rss_kb: current_rss_kb(),
        }
    }

    pub fn finish(
        self,
        final_history_len: usize,
        messages: &[MessageParam],
        response_text: &str,
    ) -> std::io::Result<PathBuf> {
        let final_rss_kb = current_rss_kb();
        let record = MemReportRecord {
            timestamp: chrono::DateTime::<chrono::Local>::from(self.started_at).to_rfc3339(),
            pid: self.pid,
            project_root: self.project_root.display().to_string(),
            initial_history_len: self.initial_history_len,
            final_history_len,
            initial_message_chars: self.initial_message_chars,
            final_message_chars: total_message_chars(messages),
            response_chars: response_text.len(),
            initial_rss_kb: self.initial_rss_kb,
            final_rss_kb,
            rss_delta_kb: match (self.initial_rss_kb, final_rss_kb) {
                (Some(a), Some(b)) => Some(b as i64 - a as i64),
                _ => None,
            },
        };

        let dir = self.project_root.join("synthesis").join("mem-reports");
        std::fs::create_dir_all(&dir)?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let path = dir.join(format!("{}-pid{}.json", stamp, self.pid));
        let json = serde_json::to_string_pretty(&record)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }
}

fn total_message_chars(messages: &[MessageParam]) -> usize {
    messages.iter().map(message_chars).sum()
}

fn message_chars(msg: &MessageParam) -> usize {
    match &msg.content {
        llm_code_sdk::MessageContent::Text(text) => text.len(),
        llm_code_sdk::MessageContent::Blocks(blocks) => blocks.iter().map(|block| match block {
            llm_code_sdk::ContentBlockParam::Text { text, .. } => text.len(),
            llm_code_sdk::ContentBlockParam::ToolUse { input, .. } => serde_json::to_string(input).map(|s| s.len()).unwrap_or(0),
            llm_code_sdk::ContentBlockParam::ToolResult { content, .. } => match content {
                Some(llm_code_sdk::ToolResultContent::Text(text)) => text.len(),
                Some(llm_code_sdk::ToolResultContent::Blocks(blocks)) => blocks.iter().map(|b| match b {
                    llm_code_sdk::ToolResultContentBlock::Text { text } => text.len(),
                    _ => 0,
                }).sum(),
                None => 0,
            },
            _ => 0,
        }).sum(),
    }
}

#[cfg(target_os = "linux")]
fn current_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let value = rest.split_whitespace().next()?;
            return value.parse::<u64>().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn current_rss_kb() -> Option<u64> {
    None
}
