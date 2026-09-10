pub mod claude;
pub mod codex;
pub mod commandcode;
pub mod freebuff;
pub mod opencode;
pub mod osagent;

pub use crate::model::UsageRecord;

pub const AGENT_CLAUDE: &str = "Claude Code";
pub const AGENT_CODEX: &str = "Codex CLI";
pub const AGENT_OPENCODE: &str = "OpenCode";
pub const AGENT_COMMANDCODE: &str = "CommandCode";
pub const AGENT_OSAGENT: &str = "OSAgent";
pub const AGENT_FREEBUFF: &str = "FreeBuff";

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
        AGENT_COMMANDCODE => home_dir()
            .map(|h| h.join(".commandcode").join("projects").to_string_lossy().to_string())
            .unwrap_or_default(),
        AGENT_FREEBUFF => home_dir()
            .map(|h| h.join(".config").join("freebuff-desktop").join("projects").to_string_lossy().to_string())
            .unwrap_or_default(),
        AGENT_OSAGENT => home_dir()
            .map(|h| h.join(".osagent").to_string_lossy().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}
