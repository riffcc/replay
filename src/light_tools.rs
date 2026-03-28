use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use llm_code_sdk::tools::{Tool, ToolResult};
use llm_code_sdk::types::{InputSchema, ToolParam};

const MAX_RESULTS: usize = 50;
const MAX_SCAN_FILE_BYTES: u64 = 512 * 1024;
const MAX_SCAN_CHARS: usize = 32 * 1024;

pub struct LightweightGrepTool {
    project_root: PathBuf,
}

impl LightweightGrepTool {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    fn search_file(
        &self,
        path: &Path,
        pattern_lower: &str,
        results: &mut Vec<String>,
        max_results: usize,
    ) {
        if results.len() >= max_results {
            return;
        }

        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return,
        };
        if metadata.len() > MAX_SCAN_FILE_BYTES {
            return;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return,
        };

        let relative = path
            .strip_prefix(&self.project_root)
            .unwrap_or(path)
            .to_string_lossy();

        for (i, line) in content.lines().enumerate() {
            if results.len() >= max_results {
                break;
            }
            if line.to_lowercase().contains(pattern_lower) {
                results.push(format!("{}:{}: {}", relative, i + 1, line.trim()));
            }
        }
    }

    fn search_dir(
        &self,
        dir: &Path,
        pattern_lower: &str,
        results: &mut Vec<String>,
        max_results: usize,
    ) {
        if results.len() >= max_results {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            if results.len() >= max_results {
                break;
            }

            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }

            if path.is_dir() {
                self.search_dir(&path, pattern_lower, results, max_results);
            } else if is_text_file(&path) {
                self.search_file(&path, pattern_lower, results, max_results);
            }
        }
    }
}

#[async_trait]
impl Tool for LightweightGrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn to_param(&self) -> ToolParam {
        ToolParam::new(
            "grep",
            InputSchema::object()
                .required_string("pattern", "Search pattern (case-insensitive)")
                .optional_string("path", "Directory or file to search (defaults to project root)"),
        )
        .with_description("Search for text in files. Returns matching lines with file:line: prefix.")
    }

    async fn call(&self, input: HashMap<String, serde_json::Value>) -> ToolResult {
        let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        if pattern.is_empty() {
            return ToolResult::error("pattern is required");
        }

        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        if path.contains(".palace") {
            return ToolResult::error("Cannot access .palace directory");
        }

        let search_path = if path == "." {
            self.project_root.clone()
        } else {
            self.project_root.join(path)
        };

        let mut results = Vec::new();
        let pattern_lower = pattern.to_lowercase();

        if search_path.is_file() {
            self.search_file(&search_path, &pattern_lower, &mut results, MAX_RESULTS);
        } else if search_path.is_dir() {
            self.search_dir(&search_path, &pattern_lower, &mut results, MAX_RESULTS);
        } else {
            return ToolResult::error(format!("Path not found: {}", path));
        }

        if results.is_empty() {
            ToolResult::success("No matches found")
        } else {
            let truncated = if results.len() >= MAX_RESULTS {
                "\n... (results truncated)"
            } else {
                ""
            };
            ToolResult::success(format!("{}{}", results.join("\n"), truncated))
        }
    }
}

pub struct LightweightSearchTool {
    project_root: PathBuf,
}

impl LightweightSearchTool {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    fn score_path(path: &str, query: &str) -> f64 {
        let path_lower = path.to_lowercase();
        let filename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_lowercase();

        let mut score = 0.0;
        if filename == query {
            score += 120.0;
        }
        if filename.starts_with(query) {
            score += 60.0;
        }
        if filename.contains(query) {
            score += 35.0;
        }
        if path_lower.starts_with(query) {
            score += 20.0;
        }
        if path_lower.contains(query) {
            score += 12.0;
        }
        score
    }

    fn score_content(content: &str, query: &str) -> f64 {
        let content_lower = content.to_lowercase();
        if !content_lower.contains(query) {
            return 0.0;
        }

        let mut score = 0.0;
        let count = content_lower.match_indices(query).take(8).count() as f64;
        score += count * 6.0;
        if content_lower.lines().any(|line| line.trim_start().starts_with(query)) {
            score += 10.0;
        }
        score
    }

    fn scan_file(&self, path: &Path, query: &str) -> Option<(String, f64)> {
        let metadata = path.metadata().ok()?;
        if metadata.len() > MAX_SCAN_FILE_BYTES {
            return None;
        }

        let relative = path
            .strip_prefix(&self.project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let mut score = Self::score_path(&relative, query);
        let content = std::fs::read_to_string(path).ok()?;
        let truncated: String = content.chars().take(MAX_SCAN_CHARS).collect();
        score += Self::score_content(&truncated, query);

        if score > 0.0 {
            Some((relative, score))
        } else {
            None
        }
    }

    fn scan_dir(&self, dir: &Path, query: &str, results: &mut Vec<(String, f64)>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }

            if path.is_dir() {
                self.scan_dir(&path, query, results);
            } else if is_text_file(&path) {
                if let Some(result) = self.scan_file(&path, query) {
                    results.push(result);
                }
            }
        }
    }
}

#[async_trait]
impl Tool for LightweightSearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn to_param(&self) -> ToolParam {
        ToolParam::new(
            "search",
            InputSchema::object()
                .required_string("query", "Search query (supports prefix matching)")
                .optional_string("limit", "Max results to return (default: 10)"),
        )
        .with_description("Search the codebase for files matching a query. Returns paths ranked by relevance.")
    }

    async fn call(&self, input: HashMap<String, serde_json::Value>) -> ToolResult {
        let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
        if query.is_empty() {
            return ToolResult::error("query is required");
        }

        let limit = input
            .get("limit")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(10usize)
            .min(MAX_RESULTS);

        let mut results = Vec::new();
        self.scan_dir(&self.project_root, &query, &mut results);
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        if results.is_empty() {
            ToolResult::success("No matches found")
        } else {
            ToolResult::success(
                results
                    .into_iter()
                    .map(|(path, score)| format!("{} (score: {:.2})", path, score))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    }
}

fn is_text_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "rs" | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "go"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "java"
            | "rb"
            | "sh"
            | "md"
            | "txt"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "html"
            | "css"
            | "scss"
            | "vue"
            | "svelte"
            | "sql"
            | "graphql"
            | "proto"
            | "xml"
            | "env"
            | "conf"
            | "cfg"
            | "ini"
            | "lock"
            | "sum"
            | "lean"
    )
}
