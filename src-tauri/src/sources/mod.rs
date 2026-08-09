pub mod claude;
pub mod codex;
pub mod opencode;

pub use crate::model::UsageRecord;

pub const AGENT_CLAUDE: &str = "Claude Code";
pub const AGENT_CODEX: &str = "Codex CLI";
pub const AGENT_OPENCODE: &str = "OpenCode";

/// Resolve the user profile / home directory.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

pub fn data_dir_for(agent: &str) -> String {
    match agent {
        AGENT_CLAUDE => claude::data_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        AGENT_CODEX => home_dir()
            .map(|h| h.join(".codex").to_string_lossy().to_string())
            .unwrap_or_default(),
        AGENT_OPENCODE => home_dir()
            .map(|h| h.join(".local").join("share").join("opencode").to_string_lossy().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}
