//! Process manager — tracks all spawned processes (PTY, pipe, fire-and-forget).
//!
//! Every bash command goes through here. Short-lived commands return inline.
//! Long-lived commands become background terminals accessible via /jobs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use replay_pty::{ProcessHandle, SpawnedProcess, TerminalSize};
use tokio::sync::mpsc;

/// How a process was spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    Pty,
    Pipe,
    PipeNoStdin,
}

/// Status of a managed process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Exited(i32),
    TimedOut,
    Killed,
}

impl std::fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProcessStatus::Running => write!(f, "Running"),
            ProcessStatus::Exited(0) => write!(f, "Success"),
            ProcessStatus::Exited(code) => write!(f, "Exited with code {code}"),
            ProcessStatus::TimedOut => write!(f, "Timed out"),
            ProcessStatus::Killed => write!(f, "Killed"),
        }
    }
}

/// A managed process.
pub struct ManagedProcess {
    pub id: u32,
    pub command: String,
    pub mode: SpawnMode,
    pub status: ProcessStatus,
    pub started: Instant,
    pub handle: ProcessHandle,
    pub output: String,
}

/// Standard environment for spawned processes.
fn process_env() -> HashMap<String, String> {
    [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", ""),
        ("SSH_ASKPASS", ""),
        ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new"),
        ("DEBIAN_FRONTEND", "noninteractive"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("TERM", "xterm-256color"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The process manager.
pub struct ProcessManager {
    processes: HashMap<u32, ManagedProcess>,
    next_id: u32,
    cwd: PathBuf,
}

impl ProcessManager {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            processes: HashMap::new(),
            next_id: 1,
            cwd: cwd.into(),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// Spawn a process. Returns (id, stdout_rx, stderr_rx, exit_rx).
    pub async fn spawn(
        &mut self,
        command: &str,
        mode: SpawnMode,
    ) -> Result<(u32, mpsc::Receiver<Vec<u8>>, mpsc::Receiver<Vec<u8>>, tokio::sync::oneshot::Receiver<i32>), String> {
        let id = self.next_id;
        self.next_id += 1;

        let args = vec!["-c".to_string(), command.to_string()];
        let env = process_env();
        let arg0: Option<String> = None;

        let spawned = match mode {
            SpawnMode::Pty => {
                let size = TerminalSize { rows: 24, cols: 120 };
                replay_pty::spawn_pty_process("bash", &args, &self.cwd, &env, &arg0, size)
                    .await.map_err(|e| format!("PTY spawn: {e}"))?
            }
            SpawnMode::Pipe => {
                replay_pty::spawn_pipe_process("bash", &args, &self.cwd, &env, &arg0)
                    .await.map_err(|e| format!("Pipe spawn: {e}"))?
            }
            SpawnMode::PipeNoStdin => {
                replay_pty::spawn_pipe_process_no_stdin("bash", &args, &self.cwd, &env, &arg0)
                    .await.map_err(|e| format!("Pipe spawn: {e}"))?
            }
        };

        let entry = ManagedProcess {
            id,
            command: command.to_string(),
            mode,
            status: ProcessStatus::Running,
            started: Instant::now(),
            handle: spawned.session,
            output: String::new(),
        };

        self.processes.insert(id, entry);

        Ok((id, spawned.stdout_rx, spawned.stderr_rx, spawned.exit_rx))
    }

    /// Execute a command, wait for completion or timeout. Returns (output, exit_code, process_id).
    pub async fn exec_wait(
        &mut self,
        command: &str,
        timeout_secs: u64,
    ) -> Result<(String, i32, u32), String> {
        let (id, mut stdout_rx, mut stderr_rx, exit_rx) = self.spawn(command, SpawnMode::PipeNoStdin).await?;

        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let timeout = Duration::from_secs(timeout_secs);

        let out_clone = std::sync::Arc::clone(&output);
        let result = tokio::time::timeout(timeout, async move {
            let mut exit_rx = exit_rx;

            loop {
                tokio::select! {
                    chunk = stdout_rx.recv() => {
                        if let Some(data) = chunk {
                            let mut out = out_clone.lock().unwrap();
                            out.push_str(&String::from_utf8_lossy(&data));
                            if out.len() > 1024 * 1024 {
                                out.truncate(1024 * 1024);
                                out.push_str("\n(output truncated)");
                            }
                        }
                    }
                    chunk = stderr_rx.recv() => {
                        if let Some(data) = chunk {
                            let mut out = out_clone.lock().unwrap();
                            out.push_str(&String::from_utf8_lossy(&data));
                        }
                    }
                    code = &mut exit_rx => {
                        // Drain remaining
                        while let Ok(data) = stdout_rx.try_recv() {
                            out_clone.lock().unwrap().push_str(&String::from_utf8_lossy(&data));
                        }
                        while let Ok(data) = stderr_rx.try_recv() {
                            out_clone.lock().unwrap().push_str(&String::from_utf8_lossy(&data));
                        }
                        return code.unwrap_or(1);
                    }
                }
            }
        }).await;

        let collected = output.lock().unwrap().clone();

        match result {
            Ok(code) => {
                if let Some(proc) = self.processes.get_mut(&id) {
                    proc.status = ProcessStatus::Exited(code);
                    proc.output = collected.clone();
                }
                let (clean, new_cwd) = extract_cwd(&collected);
                if let Some(dir) = new_cwd {
                    let path = PathBuf::from(&dir);
                    if path.is_dir() {
                        self.cwd = path;
                    }
                }
                Ok((clean, code, id))
            }
            Err(_) => {
                self.kill(id);
                if let Some(proc) = self.processes.get_mut(&id) {
                    proc.status = ProcessStatus::TimedOut;
                    proc.output = collected.clone();
                }
                Ok((format!("{collected}\n(timed out after {timeout_secs}s)"), 124, id))
            }
        }
    }

    pub fn kill(&mut self, id: u32) {
        if let Some(proc) = self.processes.get_mut(&id) {
            proc.handle.terminate();
            if proc.status == ProcessStatus::Running {
                proc.status = ProcessStatus::Killed;
            }
        }
    }

    pub fn write_stdin(&self, id: u32, data: &[u8]) -> Result<(), String> {
        let proc = self.processes.get(&id).ok_or_else(|| format!("No process #{id}"))?;
        let sender = proc.handle.writer_sender();
        sender.try_send(data.to_vec()).map_err(|e| format!("Write failed: {e}"))
    }

    pub fn get(&self, id: u32) -> Option<&ManagedProcess> {
        self.processes.get(&id)
    }

    pub fn list(&self) -> Vec<ProcessSummary> {
        let mut summaries: Vec<ProcessSummary> = self.processes.values().map(|p| {
            ProcessSummary {
                id: p.id,
                command: p.command.clone(),
                status: p.status.clone(),
                elapsed: p.started.elapsed(),
                mode: p.mode,
            }
        }).collect();
        summaries.sort_by_key(|s| s.id);
        summaries
    }

    pub fn clean(&mut self) -> usize {
        let dead: Vec<u32> = self.processes.iter()
            .filter(|(_, p)| !matches!(p.status, ProcessStatus::Running))
            .map(|(id, _)| *id)
            .collect();
        let count = dead.len();
        for id in dead {
            self.processes.remove(&id);
        }
        count
    }

    pub fn running_count(&self) -> usize {
        self.processes.values().filter(|p| p.status == ProcessStatus::Running).count()
    }
}

/// Summary for display.
#[derive(Debug, Clone)]
pub struct ProcessSummary {
    pub id: u32,
    pub command: String,
    pub status: ProcessStatus,
    pub elapsed: Duration,
    pub mode: SpawnMode,
}

impl ProcessSummary {
    pub fn format_line(&self) -> String {
        let icon = match &self.status {
            ProcessStatus::Running => "◐",
            ProcessStatus::Exited(0) => "✔",
            ProcessStatus::Exited(_) | ProcessStatus::Killed => "✗",
            ProcessStatus::TimedOut => "✗",
        };
        let cmd = if self.command.len() > 60 {
            format!("{}...", &self.command[..57])
        } else {
            self.command.clone()
        };
        let elapsed = self.elapsed.as_secs();
        format!("  {icon} {cmd} ({}, {elapsed}s)", self.status)
    }
}

/// Extract __CWD__ marker from output.
fn extract_cwd(output: &str) -> (String, Option<String>) {
    let mut lines: Vec<&str> = output.lines().collect();
    let mut cwd = None;

    if let Some(pos) = lines.iter().rposition(|l| l.starts_with("__CWD__")) {
        cwd = Some(lines[pos].strip_prefix("__CWD__").unwrap_or("").to_string());
        lines.remove(pos);
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
    }

    (lines.join("\n"), cwd)
}

/// Format /jobs output.
pub fn format_jobs(manager: &ProcessManager) -> String {
    let procs = manager.list();
    if procs.is_empty() {
        return "No background terminals.".to_string();
    }

    let mut out = String::from("Background terminals\n\n");
    for p in &procs {
        out.push_str(&p.format_line());
        out.push('\n');
    }

    let running = manager.running_count();
    out.push('\n');
    if running > 0 {
        out.push_str(&format!(
            "  {running} background terminal{} running · /jobs to view · /clean to close\n",
            if running == 1 { "" } else { "s" }
        ));
    } else {
        out.push_str("  No terminals running · /clean to remove completed\n");
    }
    out
}
