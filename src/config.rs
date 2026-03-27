//! User configuration — persisted to ~/.replay/config.toml.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Load the config file, returning key-value pairs.
pub fn load() -> HashMap<String, String> {
    let path = config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    parse_toml(&content)
}

/// Save a single key-value pair (merges with existing config).
pub fn set(key: &str, value: &str) {
    let mut config = load();
    config.insert(key.to_string(), value.to_string());
    let _ = write_config(&config);
}

/// Get a single value.
pub fn get(key: &str) -> Option<String> {
    load().get(key).cloned()
}

/// Save the selected model ID.
pub fn save_model(model_id: &str) {
    set("model", model_id);
}

/// Load the saved model ID.
pub fn saved_model() -> Option<String> {
    get("model")
}

/// Save a display preference (show_usage, show_model, etc.).
pub fn save_display(key: &str, value: bool) {
    set(key, if value { "true" } else { "false" });
}

/// Load a display preference.
pub fn display(key: &str) -> Option<bool> {
    get(key).map(|v| v == "true")
}

#[derive(Debug, Clone, Default)]
pub struct SandboxSettings {
    pub bash_allow_prefixes: Vec<String>,
    pub write_allow_prefixes: Vec<String>,
}
pub fn load_project_sandbox_settings(project_root: &Path) -> SandboxSettings {
    let path = project_settings_path(project_root);
    match std::fs::read_to_string(&path) {
        Ok(content) => parse_sandbox_settings(&content),
        Err(_) => SandboxSettings::default(),
    }
}

pub fn save_project_bash_allow_prefix(project_root: &Path, prefix: &str) -> Result<(), String> {
    let mut settings = load_project_sandbox_settings(project_root);
    if !settings.bash_allow_prefixes.iter().any(|p| p == prefix) {
        settings.bash_allow_prefixes.push(prefix.to_string());
        settings.bash_allow_prefixes.sort();
    }
    write_project_sandbox_settings(project_root, &settings)
}

pub fn save_project_write_allow_prefix(project_root: &Path, prefix: &str) -> Result<(), String> {
    let mut settings = load_project_sandbox_settings(project_root);
    if !settings.write_allow_prefixes.iter().any(|p| p == prefix) {
        settings.write_allow_prefixes.push(prefix.to_string());
        settings.write_allow_prefixes.sort();
    }
    write_project_sandbox_settings(project_root, &settings)
}

fn config_path() -> PathBuf {
    let home = dirs::home_dir().expect("no home directory");
    home.join(".replay").join("config.toml")
}

fn project_settings_path(project_root: &Path) -> PathBuf {
    project_root.join(".replay").join("settings.toml")
}

fn write_config(config: &HashMap<String, String>) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config dir: {e}"))?;
    }

    let mut lines: Vec<String> = config.iter()
        .map(|(k, v)| format!("{k} = \"{v}\""))
        .collect();
    lines.sort();

    std::fs::write(&path, lines.join("\n") + "\n")
        .map_err(|e| format!("failed to write config: {e}"))
}

fn write_project_sandbox_settings(project_root: &Path, settings: &SandboxSettings) -> Result<(), String> {
    let path = project_settings_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create settings dir: {e}"))?;
    }

    let mut out = String::from("[sandbox]\n");
    out.push_str("bash_allow_prefixes = [");
    out.push_str(&settings.bash_allow_prefixes.iter().map(|p| format!("\"{}\"", escape_toml_string(p))).collect::<Vec<_>>().join(", "));
    out.push_str("]\n");
    out.push_str("write_allow_prefixes = [");
    out.push_str(&settings.write_allow_prefixes.iter().map(|p| format!("\"{}\"", escape_toml_string(p))).collect::<Vec<_>>().join(", "));
    out.push_str("]\n");

    std::fs::write(&path, out)
        .map_err(|e| format!("failed to write settings: {e}"))
}

/// Simple TOML parser — handles `key = "value"` lines only.
fn parse_toml(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, rest)) = line.split_once('=') {
            let key = key.trim();
            let value = rest.trim().trim_matches('"');
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

fn parse_sandbox_settings(content: &str) -> SandboxSettings {
    let mut in_sandbox = false;
    let mut settings = SandboxSettings::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_sandbox = line == "[sandbox]";
            continue;
        }
        if !in_sandbox {
            continue;
        }
        if let Some((key, rest)) = line.split_once('=') {
            let key = key.trim();
            let value = rest.trim();
            let items = parse_string_array(value);
            match key {
                "bash_allow_prefixes" => settings.bash_allow_prefixes = items,
                "write_allow_prefixes" => settings.write_allow_prefixes = items,
                _ => {}
            }
        }
    }

    settings
}

fn parse_string_array(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if !(trimmed.starts_with('[') && trimmed.ends_with(']')) {
        return Vec::new();
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    inner
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_matches('"').replace("\\\"", "\"").replace("\\\\", "\\"))
        .collect()
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
