//! Credential reuse — no logins, no stored passwords. Reads existing tokens
//! from the same local files opencode / Claude Code / Codex CLI already use.

use std::path::PathBuf;

use serde_json::Value;

fn opencode_auth_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("HOME").map(PathBuf::from);
    base.map(|h| h.join(".local").join("share").join("opencode").join("auth.json"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("opencode").join("auth.json")))
}

fn read_json(path: &PathBuf) -> Option<Value> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Anthropic (Claude Code) access token. Sources, in order:
///  1. CLAUDE_CODE_OAUTH_TOKEN env var
///  2. opencode auth.json -> anthropic.access
///  3. Claude config dir (CLAUDE_CONFIG_DIR, else ~/.claude) credentials.json
///     / .credentials.json, trying claudeAiOauth.accessToken, claudeAiOauth
///     .access_token, oauth.accessToken, and top-level accessToken variants
///  4. Windows Credential Manager entries whose target contains "claude"
pub fn anthropic_token() -> Option<String> {
    if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    if let Some(path) = opencode_auth_path() {
        if let Some(auth) = read_json(&path) {
            if let Some(access) = auth.get("anthropic").and_then(|a| a.get("access")).and_then(|a| a.as_str()) {
                if !access.is_empty() {
                    return Some(access.to_string());
                }
            }
        }
    }
    for dir in claude_config_dirs() {
        for name in [".credentials.json", "credentials.json"] {
            let path = dir.join(name);
            if let Some(creds) = read_json(&path) {
                for chain in [
                    &["claudeAiOauth", "accessToken"][..],
                    &["claudeAiOauth", "access_token"][..],
                    &["claudeAiOauth", "oauth_access_token"][..],
                    &["oauth", "accessToken"][..],
                    &["accessToken"][..],
                    &["access_token"][..],
                    &["oauth_access_token"][..],
                ] {
                    if let Some(access) = get_nested(&creds, chain).filter(|s| !s.is_empty()) {
                        return Some(access);
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    if let Some(access) = credential_manager_scan("claude") {
        return Some(access);
    }
    None
}

fn get_nested(v: &Value, chain: &[&str]) -> Option<String> {
    let mut cur = v;
    for key in chain {
        cur = cur.get(*key)?;
    }
    cur.as_str().map(|s| s.to_string())
}

fn claude_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(h) = home() {
        dirs.push(h.join(".claude"));
    }
    dirs
}

/// Codex / OpenAI access token (+ optional account id). Sources, in order:
///  1. opencode auth.json -> openai.access (+ accountId)
///  2. ~/.codex/auth.json -> tokens.access_token (+ account_id)
pub fn codex_token() -> Option<(String, Option<String>)> {
    if let Some(path) = opencode_auth_path() {
        if let Some(auth) = read_json(&path) {
            if let Some(openai) = auth.get("openai") {
                let access = openai.get("access").and_then(|a| a.as_str()).map(|s| s.to_string());
                let account_id = openai.get("accountId").and_then(|a| a.as_str()).map(|s| s.to_string());
                if let Some(access) = access.filter(|s| !s.is_empty()) {
                    return Some((access, account_id));
                }
            }
        }
    }
    if let Some(h) = home() {
        let path = h.join(".codex").join("auth.json");
        if let Some(auth) = read_json(&path) {
            if let Some(tokens) = auth.get("tokens") {
                let access = tokens.get("access_token").and_then(|a| a.as_str()).map(|s| s.to_string());
                let account_id = tokens.get("account_id").and_then(|a| a.as_str()).map(|s| s.to_string());
                if let Some(access) = access.filter(|s| !s.is_empty()) {
                    return Some((access, account_id));
                }
            }
        }
    }
    None
}

/// Search Windows Credential Manager (generic credentials) for a target whose
/// name contains `needle`, and return its user name / secret blob. Claude Code
/// stores its OAuth token there as a generic credential.
#[cfg(windows)]
fn credential_manager_scan(needle: &str) -> Option<String> {
    use windows_sys::Win32::Security::Credentials::{
        CredEnumerateW, CredFree, CREDENTIALW,
    };

    let mut count: u32 = 0;
    let mut ptr: *mut *mut CREDENTIALW = std::ptr::null_mut();
    let ok = unsafe { CredEnumerateW(std::ptr::null(), 0, &mut count, &mut ptr) };
    if ok == 0 || ptr.is_null() {
        return None;
    }
    let mut result: Option<String> = None;
    for i in 0..count as isize {
        let cred = unsafe { &**ptr.offset(i) };
        let target = unsafe { read_wide(cred.TargetName) };
        if target.is_empty() || !target.to_lowercase().contains(&needle.to_lowercase()) {
            continue;
        }
        // The `keyring` crate (used by Claude Code) stores the secret as
        // UTF-16LE bytes inside the credential blob.
        if !cred.CredentialBlob.is_null() && cred.CredentialBlobSize > 0 {
            let bytes = unsafe {
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize)
            };
            if let Some(secret) = decode_blob(bytes) {
                result = Some(secret);
                break;
            }
        }
        let user = unsafe { read_wide(cred.UserName) };
        if !user.trim().is_empty() {
            result = Some(user.trim().to_string());
            break;
        }
    }
    unsafe { CredFree(ptr as _) };
    result
}

#[cfg(windows)]
unsafe fn read_wide(ptr: *mut u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = (0..).find(|&n| *ptr.add(n) == 0).unwrap_or(0);
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

#[cfg(windows)]
fn decode_blob(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 == 0 && bytes.chunks(2).all(|c| c[1] == 0) {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16_lossy(&units);
        if !s.trim().is_empty() {
            return Some(s.trim().to_string());
        }
    }
    let s = String::from_utf8_lossy(bytes);
    if !s.trim().is_empty() {
        Some(s.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_opencode_anthropic_token() {
        let dir = std::env::temp_dir().join(format!("tt-auth-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        std::fs::write(&path, r#"{"anthropic":{"access":"tok-123","refresh":"r"}}"#).unwrap();
        let v = read_json(&path).unwrap();
        assert_eq!(
            v.get("anthropic").and_then(|a| a.get("access")).and_then(|a| a.as_str()),
            Some("tok-123")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
