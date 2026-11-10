use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_mcp::McpServerConfig;
use agent_tools::LspServerConfig;
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub permissions: PermissionsSection,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(default)]
    pub hooks: HooksSection,
    #[serde(default)]
    pub lsp: HashMap<String, LspServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HooksSection {
    #[serde(default)]
    pub pre_tool_use: Vec<String>,
    #[serde(default)]
    pub post_tool_use: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentSection {
    pub profile: Option<String>,
    pub max_turns: Option<u32>,
    pub temperature: Option<f32>,
    /// Restrict bash file writes to the workspace and temp dirs (macOS seatbelt).
    #[serde(default)]
    pub sandbox: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    /// Models offered by the in-session `/model` picker.
    #[serde(default)]
    pub models: Option<Vec<String>>,
    /// Whether the model accepts image content parts. Defaults from the model name.
    #[serde(default)]
    pub vision: Option<bool>,
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

fn default_base_url() -> String {
    "https://api.deepseek.com/v1".to_string()
}

fn default_model() -> String {
    "deepseek-flash".to_string()
}

pub fn default_vision(model: &str) -> bool {
    agent_llm::default_vision(model)
}

pub fn default_models(profile: &str) -> Vec<String> {
    if profile.eq_ignore_ascii_case("deepseek") {
        ["deepseek-flash", "deepseek-chat", "deepseek-v4-pro"]
            .iter()
            .map(|model| model.to_string())
            .collect()
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PermissionsSection {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                base_url: default_base_url(),
                api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
                api_key: None,
                model: default_model(),
                models: None,
                vision: None,
                context_window: Some(1_048_576),
                reasoning_content: None,
            },
        );
        Self {
            agent: AgentSection::default(),
            providers,
            permissions: PermissionsSection::default(),
            mcp_servers: HashMap::new(),
            hooks: HooksSection::default(),
            lsp: HashMap::new(),
        }
    }
}

pub fn default_path(home: &Path) -> PathBuf {
    home.join(".whisker").join("config.toml")
}

pub fn load(path: &Path) -> Result<Config> {
    if !path.is_file() {
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::default_vision;

    #[test]
    fn vision_defaults_follow_model_family() {
        assert!(default_vision("deepseek-flash"));
        assert!(default_vision("deepseek-chat"));
        assert!(!default_vision("deepseek-v4-pro"));
    }
}
