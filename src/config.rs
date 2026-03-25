//! User configuration — persisted to ~/.replay/config.toml.

use std::collections::HashMap;
use std::path::PathBuf;

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

fn config_path() -> PathBuf {
    let home = dirs::home_dir().expect("no home directory");
    home.join(".replay").join("config.toml")
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
