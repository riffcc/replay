//! Model registry — available LLM models and provider authentication.

use llm_code_sdk::ApiFormat;

/// A model available for use.
#[derive(Debug, Clone)]
pub struct ModelDef {
    /// Display name shown in the model switcher.
    pub name: &'static str,
    /// Model ID sent to the API.
    pub model_id: &'static str,
    /// Provider name for display.
    pub provider: &'static str,
    /// API base URL.
    pub base_url: &'static str,
    /// API format to use.
    pub format: ApiFormat,
    /// How to resolve the API key for this provider.
    pub auth: AuthSource,
    /// Whether this model supports reasoning effort.
    pub supports_effort: bool,
    /// Context window size in tokens.
    pub context_window: u64,
}

/// How to obtain the API key for a provider.
#[derive(Debug, Clone)]
pub enum AuthSource {
    /// Read from an environment variable.
    EnvVar(&'static str),
    /// Read from Codex's auth.json (OAuth bearer token).
    CodexAuth,
}

/// All available models.
pub const MODELS: &[ModelDef] = &[
    ModelDef {
        name: "MiniMax-M2.7",
        model_id: "MiniMax-M2.7",
        provider: "MiniMax",
        base_url: "https://api.minimax.io/anthropic",
        format: ApiFormat::Anthropic,
        auth: AuthSource::EnvVar("MINIMAX_AUTH_TOKEN"),
        supports_effort: false,
        context_window: 200_000,
    },
    ModelDef {
        name: "MiniMax-M2.7 Highspeed",
        model_id: "MiniMax-M2.7-Highspeed",
        provider: "MiniMax",
        base_url: "https://api.minimax.io/anthropic",
        format: ApiFormat::Anthropic,
        auth: AuthSource::EnvVar("MINIMAX_AUTH_TOKEN"),
        supports_effort: false,
        context_window: 200_000,
    },
    ModelDef {
        name: "GLM-5",
        model_id: "glm-5",
        provider: "Z.ai",
        base_url: "https://api.z.ai/api/anthropic",
        format: ApiFormat::Anthropic,
        auth: AuthSource::EnvVar("ZAI_API_KEY"),
        supports_effort: false,
        context_window: 128_000,
    },
    ModelDef {
        name: "GLM-5 Turbo",
        model_id: "glm-5-turbo",
        provider: "Z.ai",
        base_url: "https://api.z.ai/api/anthropic",
        format: ApiFormat::Anthropic,
        auth: AuthSource::EnvVar("ZAI_API_KEY"),
        supports_effort: false,
        context_window: 128_000,
    },
    ModelDef {
        name: "GPT-5.3 Codex",
        model_id: "gpt-5.3-codex",
        provider: "OpenAI",
        base_url: "https://chatgpt.com/backend-api",
        format: ApiFormat::OpenAIResponses,
        auth: AuthSource::CodexAuth,
        supports_effort: true,
        context_window: 200_000,
    },
    ModelDef {
        name: "GPT-5.4",
        model_id: "gpt-5.4",
        provider: "OpenAI",
        base_url: "https://chatgpt.com/backend-api",
        format: ApiFormat::OpenAIResponses,
        auth: AuthSource::CodexAuth,
        supports_effort: true,
        context_window: 272_000,
    },
    ModelDef {
        name: "GPT-5.4 Mini",
        model_id: "gpt-5.4-mini",
        provider: "OpenAI",
        base_url: "https://chatgpt.com/backend-api",
        format: ApiFormat::OpenAIResponses,
        auth: AuthSource::CodexAuth,
        supports_effort: true,
        context_window: 272_000,
    },
];

/// Resolve the API key for a model. Returns None if not available.
pub fn resolve_auth(model: &ModelDef) -> Option<String> {
    match &model.auth {
        AuthSource::EnvVar(var) => std::env::var(var).ok(),
        AuthSource::CodexAuth => read_codex_token(),
    }
}

/// Check if a model's provider has valid authentication.
pub fn is_available(model: &ModelDef) -> bool {
    resolve_auth(model).is_some()
}

/// Read the Codex account ID from ~/.codex/auth.json.
pub fn codex_account_id() -> Option<String> {
    let home = dirs::home_dir()?;
    let auth_path = home.join(".codex").join("auth.json");
    let content = std::fs::read_to_string(&auth_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("tokens")?.get("account_id")?.as_str().map(|s| s.to_string())
}

/// Read the bearer token from ~/.codex/auth.json.
fn read_codex_token() -> Option<String> {
    let home = dirs::home_dir()?;
    let auth_path = home.join(".codex").join("auth.json");
    let content = std::fs::read_to_string(&auth_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Check if tokens exist and have an access_token
    let tokens = json.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?;

    if access_token.is_empty() {
        return None;
    }

    tracing::debug!("codex auth: token len={}, last_refresh={:?}",
        access_token.len(),
        json.get("last_refresh").and_then(|v| v.as_str()),
    );

    Some(access_token.to_string())
}

/// Find a model by model_id.
pub fn find_by_id(model_id: &str) -> Option<&'static ModelDef> {
    MODELS.iter().find(|m| m.model_id == model_id)
}

/// Default model.
pub fn default_model() -> &'static ModelDef {
    &MODELS[1] // MiniMax-M2.7 Highspeed
}
