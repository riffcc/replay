use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use llm_code_sdk::MessageParam;
use llm_code_sdk::tools::ToolEvent;
use serde::Serialize;

#[derive(Debug)]
pub struct RunMemReport {
    started_at: SystemTime,
    started_monotonic: Instant,
    project_root: PathBuf,
    pid: u32,
    run_id: String,
    model: String,
    instruction_chars: usize,
    initial_history_len: usize,
    initial_message_chars: usize,
    initial_rss_kb: Option<u64>,
    checkpoints: Vec<MemCheckpoint>,
    tool_calls: BTreeMap<String, usize>,
    tool_results: BTreeMap<String, usize>,
    /// Per-tool latency tracking.
    tool_timings: Vec<ToolTiming>,
    /// Start time of the currently executing tool call.
    pending_tool: Option<(String, Instant)>,
}

#[derive(Debug, Clone, Serialize)]
struct ToolTiming {
    name: String,
    latency_ms: u64,
    output_bytes: usize,
    success: bool,
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MemCheckpoint {
    label: String,
    elapsed_ms: u64,
    rss_kb: Option<u64>,
    rss_delta_kb: Option<i64>,
    history_len: Option<usize>,
    message_chars: Option<usize>,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct MemReportRecord {
    timestamp: String,
    pid: u32,
    run_id: String,
    project_root: String,
    model: String,
    instruction_chars: usize,
    outcome: String,
    error: Option<String>,
    duration_ms: u64,
    initial_history_len: usize,
    final_history_len: usize,
    initial_message_chars: usize,
    final_message_chars: usize,
    response_chars: usize,
    initial_rss_kb: Option<u64>,
    final_rss_kb: Option<u64>,
    rss_delta_kb: Option<i64>,
    peak_rss_kb: Option<u64>,
    peak_rss_delta_kb: Option<i64>,
    checkpoint_count: usize,
    checkpoints: Vec<MemCheckpoint>,
    tool_calls: BTreeMap<String, usize>,
    tool_results: BTreeMap<String, usize>,
    tool_timings: Vec<ToolTiming>,
    /// Per-tool aggregate latency stats.
    tool_latency_summary: BTreeMap<String, ToolLatencySummary>,
}

#[derive(Debug, Serialize)]
struct ToolLatencySummary {
    count: usize,
    total_ms: u64,
    avg_ms: u64,
    max_ms: u64,
    min_ms: u64,
}

impl RunMemReport {
    pub fn start(
        project_root: &Path,
        model: impl Into<String>,
        instruction_chars: usize,
        messages: &[MessageParam],
    ) -> Self {
        let started_at = SystemTime::now();
        let pid = std::process::id();
        let mut report = Self {
            started_at,
            started_monotonic: Instant::now(),
            project_root: project_root.to_path_buf(),
            pid,
            run_id: format!(
                "{}-pid{}",
                chrono::Local::now().format("%Y%m%d-%H%M%S-%3f"),
                pid
            ),
            model: model.into(),
            instruction_chars,
            initial_history_len: messages.len(),
            initial_message_chars: total_message_chars(messages),
            initial_rss_kb: current_rss_kb(),
            checkpoints: Vec::new(),
            tool_calls: BTreeMap::new(),
            tool_results: BTreeMap::new(),
            tool_timings: Vec::new(),
            pending_tool: None,
        };
        report.checkpoint_messages("run_start", messages, None);
        report
    }

    pub fn checkpoint(&mut self, label: &str, note: Option<String>) {
        self.checkpoints.push(MemCheckpoint {
            label: label.to_string(),
            elapsed_ms: self.started_monotonic.elapsed().as_millis() as u64,
            rss_kb: current_rss_kb(),
            rss_delta_kb: rss_delta_kb(self.initial_rss_kb, current_rss_kb()),
            history_len: None,
            message_chars: None,
            note: note.map(|n| truncate_chars(&n, 240)),
        });
    }

    pub fn checkpoint_messages(
        &mut self,
        label: &str,
        messages: &[MessageParam],
        note: Option<String>,
    ) {
        self.checkpoints.push(MemCheckpoint {
            label: label.to_string(),
            elapsed_ms: self.started_monotonic.elapsed().as_millis() as u64,
            rss_kb: current_rss_kb(),
            rss_delta_kb: rss_delta_kb(self.initial_rss_kb, current_rss_kb()),
            history_len: Some(messages.len()),
            message_chars: Some(total_message_chars(messages)),
            note: note.map(|n| truncate_chars(&n, 240)),
        });
    }

    pub fn record_tool_event(&mut self, event: &ToolEvent) {
        match event {
            ToolEvent::ToolCall { name, input } => {
                *self.tool_calls.entry(name.clone()).or_insert(0) += 1;
                self.pending_tool = Some((name.clone(), Instant::now()));
                self.checkpoint(
                    &format!("tool_call:{name}"),
                    summarize_tool_call(name, input),
                );
            }
            ToolEvent::ToolResult {
                name,
                success,
                output,
                ..
            } => {
                *self.tool_results.entry(name.clone()).or_insert(0) += 1;

                // Compute latency from matching tool_call
                let latency_ms = self.pending_tool.take()
                    .filter(|(n, _)| n == name)
                    .map(|(_, start)| start.elapsed().as_millis() as u64)
                    .unwrap_or(0);

                self.tool_timings.push(ToolTiming {
                    name: name.clone(),
                    latency_ms,
                    output_bytes: output.len(),
                    success: *success,
                    note: summarize_tool_call(name, &HashMap::new()),
                });

                self.checkpoint(
                    &format!("tool_result:{name}"),
                    Some(format!("success={success} output_bytes={} latency_ms={latency_ms}", output.len())),
                );
            }
            ToolEvent::Text { .. } | ToolEvent::Usage { .. } => {}
        }

        // Flush to disk every checkpoint so OOM doesn't lose the trail
        let _ = self.flush_interim();
    }

    fn compute_latency_summary(&self) -> BTreeMap<String, ToolLatencySummary> {
        let mut by_tool: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for t in &self.tool_timings {
            by_tool.entry(t.name.clone()).or_default().push(t.latency_ms);
        }
        by_tool.into_iter().map(|(name, latencies)| {
            let count = latencies.len();
            let total: u64 = latencies.iter().sum();
            let max = latencies.iter().copied().max().unwrap_or(0);
            let min = latencies.iter().copied().min().unwrap_or(0);
            let avg = if count > 0 { total / count as u64 } else { 0 };
            (name, ToolLatencySummary { count, total_ms: total, avg_ms: avg, max_ms: max, min_ms: min })
        }).collect()
    }

    /// Write an interim report to disk (survives OOM).
    fn flush_interim(&self) -> std::io::Result<()> {
        let final_rss_kb = current_rss_kb();
        let peak_rss_kb = self
            .checkpoints
            .iter()
            .filter_map(|c| c.rss_kb)
            .max()
            .or(final_rss_kb);

        let record = MemReportRecord {
            timestamp: chrono::DateTime::<chrono::Local>::from(self.started_at).to_rfc3339(),
            pid: self.pid,
            run_id: self.run_id.clone(),
            project_root: self.project_root.display().to_string(),
            model: self.model.clone(),
            instruction_chars: self.instruction_chars,
            outcome: "in_progress".to_string(),
            error: None,
            duration_ms: self.started_monotonic.elapsed().as_millis() as u64,
            initial_history_len: self.initial_history_len,
            final_history_len: self.initial_history_len, // unknown mid-run
            initial_message_chars: self.initial_message_chars,
            final_message_chars: 0,
            response_chars: 0,
            initial_rss_kb: self.initial_rss_kb,
            final_rss_kb,
            rss_delta_kb: rss_delta_kb(self.initial_rss_kb, final_rss_kb),
            peak_rss_kb,
            peak_rss_delta_kb: rss_delta_kb(self.initial_rss_kb, peak_rss_kb),
            checkpoint_count: self.checkpoints.len(),
            checkpoints: self.checkpoints.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_results: self.tool_results.clone(),
            tool_timings: self.tool_timings.clone(),
            tool_latency_summary: self.compute_latency_summary(),
        };

        let dir = self.project_root.join("synthesis").join("mem-reports");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", self.run_id));
        let json = serde_json::to_string_pretty(&record)?;
        std::fs::write(&path, json)
    }

    pub fn write_success(
        &mut self,
        messages: &[MessageParam],
        response_text: &str,
    ) -> std::io::Result<PathBuf> {
        self.write_report("ok", None, messages, response_text.chars().count())
    }

    pub fn write_error(
        &mut self,
        messages: &[MessageParam],
        error: &str,
    ) -> std::io::Result<PathBuf> {
        self.write_report("error", Some(truncate_chars(error, 4000)), messages, 0)
    }

    fn write_report(
        &mut self,
        outcome: &str,
        error: Option<String>,
        messages: &[MessageParam],
        response_chars: usize,
    ) -> std::io::Result<PathBuf> {
        self.checkpoint_messages(
            "run_finish",
            messages,
            Some(match &error {
                Some(err) => format!("outcome={outcome} error={err}"),
                None => format!("outcome={outcome}"),
            }),
        );

        let final_rss_kb = self
            .checkpoints
            .last()
            .and_then(|checkpoint| checkpoint.rss_kb)
            .or_else(current_rss_kb);
        let peak_rss_kb = self
            .checkpoints
            .iter()
            .filter_map(|checkpoint| checkpoint.rss_kb)
            .max();

        let record = MemReportRecord {
            timestamp: chrono::DateTime::<chrono::Local>::from(self.started_at).to_rfc3339(),
            pid: self.pid,
            run_id: self.run_id.clone(),
            project_root: self.project_root.display().to_string(),
            model: self.model.clone(),
            instruction_chars: self.instruction_chars,
            outcome: outcome.to_string(),
            error,
            duration_ms: self.started_monotonic.elapsed().as_millis() as u64,
            initial_history_len: self.initial_history_len,
            final_history_len: messages.len(),
            initial_message_chars: self.initial_message_chars,
            final_message_chars: total_message_chars(messages),
            response_chars,
            initial_rss_kb: self.initial_rss_kb,
            final_rss_kb,
            rss_delta_kb: rss_delta_kb(self.initial_rss_kb, final_rss_kb),
            peak_rss_kb,
            peak_rss_delta_kb: rss_delta_kb(self.initial_rss_kb, peak_rss_kb),
            checkpoint_count: self.checkpoints.len(),
            checkpoints: self.checkpoints.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_results: self.tool_results.clone(),
            tool_timings: self.tool_timings.clone(),
            tool_latency_summary: self.compute_latency_summary(),
        };

        let dir = self.project_root.join("synthesis").join("mem-reports");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", self.run_id));
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
        llm_code_sdk::MessageContent::Text(text) => text.chars().count(),
        llm_code_sdk::MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                llm_code_sdk::ContentBlockParam::Text { text, .. } => text.chars().count(),
                llm_code_sdk::ContentBlockParam::ToolUse { input, .. } => serde_json::to_string(input)
                    .map(|s| s.chars().count())
                    .unwrap_or(0),
                llm_code_sdk::ContentBlockParam::ToolResult { content, .. } => match content {
                    Some(llm_code_sdk::ToolResultContent::Text(text)) => text.chars().count(),
                    Some(llm_code_sdk::ToolResultContent::Blocks(blocks)) => blocks
                        .iter()
                        .map(|b| match b {
                            llm_code_sdk::ToolResultContentBlock::Text { text } => text.chars().count(),
                            _ => 0,
                        })
                        .sum(),
                    None => 0,
                },
                _ => 0,
            })
            .sum(),
    }
}

fn rss_delta_kb(initial: Option<u64>, current: Option<u64>) -> Option<i64> {
    match (initial, current) {
        (Some(a), Some(b)) => Some(b as i64 - a as i64),
        _ => None,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn summarize_tool_call(name: &str, input: &HashMap<String, serde_json::Value>) -> Option<String> {
    let s = |key: &str| input.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();

    let summary = match name {
        "bash" => s("command"),
        "grep" => {
            let pattern = s("pattern");
            let path = s("path");
            if path.is_empty() || path == "." {
                pattern
            } else {
                format!("{pattern} in {path}")
            }
        }
        "search" => s("query"),
        "read" | "write" | "glob" | "list_directory" => s("path"),
        "tasks" => {
            let operation = s("operation");
            let id = s("id");
            if id.is_empty() {
                operation
            } else {
                format!("{operation} {id}")
            }
        }
        _ => String::new(),
    };

    if summary.is_empty() {
        None
    } else {
        Some(truncate_chars(&summary, 160))
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
