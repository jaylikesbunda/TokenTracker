//! Live quota polling, modeled after the opencode-usage-plugin endpoints
//! (https://github.com/IgorWarzocha/opencode-usage-plugin, MIT) and the
//! Claude Code Usage Monitor's Anthropic OAuth usage API
//! (https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor, MIT):
//!  - Anthropic: GET https://api.anthropic.com/api/oauth/usage (+ profile)
//!  - Codex:     GET https://chatgpt.com/backend-api/wham/usage
//!  - OpenCode:  GET <opencode.ai/_server> with a pasted console cookie,
//!               parsing the inline JS payload (rollingUsage/weeklyUsage/
//!               monthlyUsage), same as wiscaksono/opencode-usage.
//! Credentials are reused from existing local auth files, never stored.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Datelike;
use crate::auth;
use crate::model::{QuotaProvider, QuotaWindow};

const TIMEOUT: Duration = Duration::from_secs(6);

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .user_agent("tokentracker/0.1")
        .build()
        .map_err(|e| e.to_string())
}

const ANTHROPIC_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

fn anthropic_headers(token: &str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
        h.insert(reqwest::header::AUTHORIZATION, v);
    }
    h.insert("anthropic-beta", "oauth-2025-04-20".parse().unwrap());
    h.insert("anthropic-dangerous-direct-browser-access", "true".parse().unwrap());
    h.insert("x-app", "cli".parse().unwrap());
    h.insert(reqwest::header::USER_AGENT, "claude-cli/2.1.2 (external, cli)".parse().unwrap());
    h
}

fn parse_anthropic_usage(data: &serde_json::Value) -> Vec<QuotaWindow> {
    const ORDER: &[(&str, &str)] = &[
        ("five_hour", "5-Hour"),
        ("seven_day", "7-Day (All)"),
        ("seven_day_oauth_apps", "7-Day (OAuth Apps)"),
        ("seven_day_sonnet", "7-Day (Sonnet)"),
        ("seven_day_opus", "7-Day (Opus)"),
        ("seven_day_cowork", "7-Day (Co-work)"),
        ("iguana_necktie", "Iguana Necktie"),
    ];
    let now = chrono::Utc::now().timestamp();
    let mut windows = Vec::new();
    for (key, label) in ORDER {
        if let Some(w) = data.get(*key) {
            if w.is_null() {
                continue;
            }
            // The API reports utilization as a 0..1 fraction; some versions
            // already return a percentage (>1). Normalize + clamp.
            let raw = w
                .get("utilization")
                .and_then(|u| u.as_f64())
                .or_else(|| w.get("used_percentage").and_then(|u| u.as_f64()))
                .unwrap_or(0.0);
            let used = if raw <= 1.0 { raw * 100.0 } else { raw };
            let used = used.clamp(0.0, 100.0);
            let resets = w
                .get("resets_at")
                .and_then(|r| r.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp())
                .filter(|r| *r > now);
            windows.push(QuotaWindow { label: label.to_string(), used_percent: used, resets_at: resets });
        }
    }
    if let Some(extra) = data.get("extra_usage") {
        if !extra.is_null() {
            let raw = extra.get("utilization").and_then(|u| u.as_f64()).unwrap_or(0.0);
            let used = if raw <= 1.0 { raw * 100.0 } else { raw }.clamp(0.0, 100.0);
            windows.push(QuotaWindow { label: "Extra usage".into(), used_percent: used, resets_at: None });
        }
    }
    windows
}

fn parse_anthropic_profile(data: &serde_json::Value) -> Option<String> {
    let org = data.get("organization")?;
    let org_type = org.get("organization_type").and_then(|t| t.as_str()).unwrap_or("");
    let tier = org.get("rate_limit_tier").and_then(|t| t.as_str()).unwrap_or("");
    if tier.to_lowercase().contains("max") || org_type.to_lowercase().contains("max") {
        return Some("Claude Max".into());
    }
    if org_type.to_lowercase().contains("enterprise") {
        return Some("Claude Enterprise".into());
    }
    if org_type.to_lowercase().contains("team") {
        return Some("Claude Team".into());
    }
    if org_type.to_lowercase().contains("pro") {
        return Some("Claude Pro".into());
    }
    None
}

fn fetch_anthropic() -> QuotaProvider {
    let Some(token) = auth::anthropic_token() else {
        return QuotaProvider {
            id: "anthropic".into(),
            name: "Claude Code".into(),
            status: "no-auth".into(),
            message: "No Claude OAuth token found. Run `claude login` in Claude Code (or opencode with an Anthropic account) to enable live limits.".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        };
    };
    let Ok(client) = client() else {
        return QuotaProvider {
            id: "anthropic".into(),
            name: "Claude Code".into(),
            status: "error".into(),
            message: "HTTP client failed".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        };
    };

    let usage_res = client
        .get(ANTHROPIC_USAGE_URL)
        .headers(anthropic_headers(&token))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<serde_json::Value>());

    let mut windows = Vec::new();
    let mut plan = None;
    let mut status = "error".to_string();
    let mut message = String::new();

    match usage_res {
        Ok(data) => {
            windows = parse_anthropic_usage(&data);
            status = "ok".into();
            // best-effort plan type
            if let Ok(profile) = client
                .get(ANTHROPIC_PROFILE_URL)
                .headers(anthropic_headers(&token))
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json::<serde_json::Value>())
            {
                plan = parse_anthropic_profile(&profile);
            }
        }
        Err(e) => {
            message = format!("Usage endpoint unavailable: {}", e);
        }
    }

    QuotaProvider {
        id: "anthropic".into(),
        name: "Claude Code".into(),
        status,
        message,
        plan,
        windows,
        credits: None,
        credits_unlimited: false,
        stats: vec![],
    }
}

fn fetch_codex() -> QuotaProvider {
    let Some((token, account_id)) = auth::codex_token() else {
        return QuotaProvider {
            id: "codex".into(),
            name: "Codex CLI".into(),
            status: "no-auth".into(),
            message: "No Codex credentials found. Sign in to Codex CLI or opencode to enable.".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        };
    };
    let Ok(client) = client() else {
        return QuotaProvider {
            id: "codex".into(),
            name: "Codex CLI".into(),
            status: "error".into(),
            message: "HTTP client failed".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        };
    };

    let mut req = client.get(CODEX_USAGE_URL);
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
        req = req.header(reqwest::header::AUTHORIZATION, v);
    }
    if let Some(id) = account_id {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&id) {
            req = req.header("ChatGPT-Account-Id", v);
        }
    }

    let mut status = "error".to_string();
    let mut message = String::new();
    let mut plan = None;
    let mut windows = Vec::new();
    let mut credits = None;
    let mut credits_unlimited = false;

    match req.send().and_then(|r| r.error_for_status()).and_then(|r| r.json::<serde_json::Value>()) {
        Ok(data) => {
            status = "ok".into();
            plan = data.get("plan_type").and_then(|p| p.as_str()).map(|s| s.to_string());
            if let Some(rl) = data.get("rate_limit") {
                for (key, label) in [("primary_window", "Primary"), ("secondary_window", "Secondary")] {
                    if let Some(w) = rl.get(key) {
                        if w.is_null() {
                            continue;
                        }
                        let used = w.get("used_percent").and_then(|u| u.as_f64()).unwrap_or(0.0);
                        let resets = w.get("reset_at").and_then(|r| r.as_i64());
                        windows.push(QuotaWindow { label: label.into(), used_percent: used, resets_at: resets });
                    }
                }
            }
            if let Some(c) = data.get("credits") {
                if !c.is_null() {
                    credits = c.get("balance").and_then(|b| b.as_str()).map(|s| s.to_string());
                    credits_unlimited = c.get("unlimited").and_then(|u| u.as_bool()).unwrap_or(false);
                }
            }
        }
        Err(e) => message = format!("Usage endpoint unavailable: {}", e),
    }

    QuotaProvider {
        id: "codex".into(),
        name: "Codex CLI".into(),
        status,
        message,
        plan,
        windows,
        credits,
        credits_unlimited,
        stats: vec![],
    }
}

// ---------------------------------------------------------------------------
// opencode.ai subscription usage (wiscaksono/opencode-usage approach)
// ---------------------------------------------------------------------------

/// Path of the saved curl command for the opencode.ai `_server` request.
pub fn curl_config_path() -> Option<PathBuf> {
    let home = crate::sources::home_dir()?;
    #[cfg(windows)]
    let base = home.join("AppData").join("Roaming");
    #[cfg(not(windows))]
    let base = home.join(".config");
    Some(base.join("TokenTracker").join("opencode-curl.txt"))
}

/// Resolve the existing config file, preferring the current path but
/// falling back to the pre-rename `TokenTray` location.
fn existing_curl_config() -> Option<PathBuf> {
    if let Some(p) = curl_config_path().filter(|p| p.is_file()) {
        return Some(p);
    }
    let home = crate::sources::home_dir()?;
    #[cfg(windows)]
    let base = home.join("AppData").join("Roaming");
    #[cfg(not(windows))]
    let base = home.join(".config");
    let legacy = base.join("TokenTray").join("opencode-curl.txt");
    legacy.is_file().then_some(legacy)
}

/// Tokenize a shell command, respecting single/double quotes and escapes.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    for c in input.chars() {
        if escape {
            cur.push(c);
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        if c == '\'' {
            in_single = true;
            continue;
        }
        if c == '"' {
            in_double = true;
            continue;
        }
        if c.is_whitespace() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

struct CurlRequest {
    url: String,
    method: String,
    body: Option<String>,
    headers: Vec<(String, String)>,
    cookie: Option<String>,
}

/// Minimal port of wiscaksono/opencode-usage's CurlParser: extract the URL,
/// method, body, headers and Cookie from a DevTools "Copy as cURL" command.
fn parse_curl(command: &str) -> Option<CurlRequest> {
    let tokens = tokenize(command);
    if tokens.is_empty() {
        return None;
    }
    let mut url: Option<String> = None;
    let mut method = "GET".to_string();
    let mut body: Option<String> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut cookie: Option<String> = None;

    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if i == 0 && t.eq_ignore_ascii_case("curl") {
            i += 1;
            continue;
        }
        if ["--compressed", "-L", "-s", "-S", "-v", "--verbose", "-I", "-i", "--include",
            "--http1.1", "--http2", "-k", "--insecure", "-g", "--globoff"]
            .contains(&t.as_str())
        {
            i += 1;
            continue;
        }
        if (t == "-X" || t == "--request") && i + 1 < tokens.len() {
            method = tokens[i + 1].to_uppercase();
            i += 2;
            continue;
        }
        if (t == "-b" || t == "--cookie") && i + 1 < tokens.len() {
            cookie = Some(tokens[i + 1].clone());
            i += 2;
            continue;
        }
        if (t == "-H" || t == "--header") && i + 1 < tokens.len() {
            let header = &tokens[i + 1];
            if let Some(sep) = header.find(':') {
                let key = header[..sep].trim().to_string();
                let value = header[sep + 1..].trim().to_string();
                if key.eq_ignore_ascii_case("cookie") {
                    cookie = Some(value);
                } else if !key.eq_ignore_ascii_case("accept-encoding") {
                    headers.push((key, value));
                }
            }
            i += 2;
            continue;
        }
        if (t == "--data-raw" || t == "--data" || t == "--data-binary" || t == "--json" || t == "-d")
            && i + 1 < tokens.len()
        {
            body = Some(tokens[i + 1].clone());
            i += 2;
            continue;
        }
        if t.starts_with("http://") || t.starts_with("https://") {
            url = Some(t.clone());
            i += 1;
            continue;
        }
        if t.starts_with("--") && i + 1 < tokens.len() && !tokens[i + 1].starts_with('-') {
            i += 2;
            continue;
        }
        i += 1;
    }

    if body.is_some() && method == "GET" {
        method = "POST".into();
    }

    Some(CurlRequest {
        url: url?,
        method,
        body,
        headers,
        cookie,
    })
}

/// Extract the name of the variable an `$R[n]` object was assigned to, e.g.
/// `rollingUsage: $R[0] = {` -> "rollingUsage".
fn label_before(raw: &str, s: usize) -> String {
    let prefix = &raw[..s];
    let mut chars = prefix.chars().rev().peekable();
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    if !matches!(chars.peek(), Some(':')) {
        return String::new();
    }
    chars.next();
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    let mut name: Vec<char> = Vec::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    name.reverse();
    name.into_iter().collect()
}

/// Parse a SolidStart `_server` response stream: a series of `$R[n] = {...}`
/// JS object assignments. Returns (quota windows, derived stat rows).
/// Remove SolidStart object references (`$R[n] =`) that appear inside values,
/// e.g. `usage: $R[1] = [...]` -> `usage: [...]`, and turn JS Date
/// expressions (`new Date("...")`) into plain strings.
fn strip_r_refs(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"$R[" {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b']' {
                let mut k = j + 1;
                while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'=' {
                    i = k + 1;
                    continue;
                }
            }
        }
        if bytes[i..].starts_with(b"new Date(") {
            // find the opening quote inside the Date constructor
            let mut j = i + 9;
            while j < bytes.len() && bytes[j] != b'"' && bytes[j] != b'\'' {
                j += 1;
            }
            if j < bytes.len() {
                let quote = bytes[j];
                let start = j;
                j += 1;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                if j < bytes.len() {
                    // copy the quoted string, skipping `new Date(` and the closing `)`
                    out.push_str(&s[start..=j]);
                    i = j + 1;
                    while i < bytes.len() && bytes[i] == b')' {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Quote unquoted JS object keys: `{ status: "ok" }` -> `{ "status": "ok" }`.
fn quote_js_keys(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '{' || c == ',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'_' || bytes[j] == b'$') {
                let start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'$') {
                    j += 1;
                }
                let mut k = j;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b':' {
                    out.push(c);
                    out.push('"');
                    out.push_str(&s[start..j]);
                    out.push('"');
                    out.push_str(&s[j..k]);
                    out.push(':');
                    i = k + 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn parse_server_payload(raw: &str) -> (Vec<QuotaWindow>, Vec<(String, String)>) {
    let mut windows: Vec<QuotaWindow> = Vec::new();
    let mut stats: Vec<(String, String)> = Vec::new();
    let now = chrono::Utc::now().timestamp();
    let today = chrono::Local::now().date_naive();

    let mut total_today = 0.0f64;
    let mut total_week = 0.0f64;
    let mut total_month = 0.0f64;
    let mut models: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut pos = 0;
    while let Some(rel) = raw[pos..].find("$R[") {
        let s = pos + rel;
        let Some(eq) = raw[s..].find('=') else { break };
        let value = &raw[s + eq + 1..];
        let trimmed = value.trim_start();
        let array = trimmed.starts_with('[');
        let open_at = if array { value.find('[') } else { value.find('{') };
        let Some(open_rel) = open_at else { break };
        let open_idx = s + eq + 1 + open_rel;
        let (open_ch, close_ch) = if array { ('[', ']') } else { ('{', '}') };
        let mut depth = 0i32;
        let mut end: Option<usize> = None;
        for (i, ch) in raw[open_idx..].char_indices() {
            match ch {
                '[' | '{' => depth += 1,
                ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open_idx + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let _ = (open_ch, close_ch);
        let block = &raw[open_idx..=end];
        let normalized = quote_js_keys(&strip_r_refs(block))
            .replace("!1", "false")
            .replace("!0", "true")
            .replace("undefined", "null");
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&normalized) {
            let label = label_before(raw, s);
            // rate-limit window shape: { status, resetInSec, usagePercent }
            if v.is_object() {
                if let (Some(pct), Some(reset)) = (
                    v.get("usagePercent").and_then(|x| x.as_i64()),
                    v.get("resetInSec").and_then(|x| x.as_i64()),
                ) {
                    let display = match label.as_str() {
                        "rollingUsage" => "Rolling",
                        "weeklyUsage" => "Weekly",
                        "monthlyUsage" => "Monthly",
                        other if !other.is_empty() => other,
                        _ => "Usage",
                    };
                    windows.push(QuotaWindow {
                        label: display.to_string(),
                        used_percent: pct.clamp(0, 100) as f64,
                        resets_at: Some(now + reset),
                    });
                }
            }
            // usage history shape: { usage: [...] } or a bare [...]
            let entries: Vec<&serde_json::Value> = match &v {
                serde_json::Value::Object(o) => o
                    .get("usage")
                    .and_then(|u| u.as_array())
                    .map(|a| a.iter().collect())
                    .unwrap_or_default(),
                serde_json::Value::Array(a) => a.iter().collect(),
                _ => Vec::new(),
            };
            for e in entries {
                let cost = e
                    .get("cost")
                    .or_else(|| e.get("totalCost"))
                    .and_then(|c| c.as_i64())
                    .unwrap_or(0) as f64
                    / 1e8;
                if let Some(model) = e.get("model").and_then(|m| m.as_str()) {
                    models.insert(model.to_string());
                }
                let date = e
                    .get("timeCreated")
                    .and_then(|d| d.as_str())
                    .and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(s)
                            .map(|d| d.with_timezone(&chrono::Local).date_naive())
                            .ok()
                    })
                    .or_else(|| {
                        e.get("date")
                            .and_then(|d| d.as_str())
                            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    });
                if let Some(d) = date {
                    if d == today {
                        total_today += cost;
                    }
                    if (today - d).num_days() <= 6 && d <= today {
                        total_week += cost;
                    }
                    if d.year() == today.year() && d.month() == today.month() {
                        total_month += cost;
                    }
                }
            }
        }
        pos = end + 1;
    }

    if models.len() > 0 || total_today > 0.0 {
        let f = |v: f64| format!("${:.2}", v);
        stats.push(("Today".to_string(), f(total_today)));
        stats.push(("This week".to_string(), f(total_week)));
        stats.push(("This month".to_string(), f(total_month)));
        stats.push(("Models".to_string(), models.len().to_string()));
    }

    (windows, stats)
}

/// Parse the go-page HTML usage items:
/// `<div data-slot="usage-item"><div ...><span data-slot="usage-label">Weekly Usage</span>
///  <span data-slot="usage-value">50%</span></div><div data-slot="progress">
///  <div data-slot="progress-bar" style="width:50%"></div></div>
///  <span data-slot="reset-time">Resets in 11 hours 45 minutes</span></div>`
fn parse_usage_html(raw: &str) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    let mut pos = 0;
    while let Some(rel) = raw[pos..].find(r#"data-slot="usage-item""#) {
        let start = pos + rel;
        let block = &raw[start..(start + 600).min(raw.len())];
        let label = extract_slot(block, "usage-label").unwrap_or_default();
        let value = extract_slot(block, "usage-value").unwrap_or_default();
        let reset = extract_slot(block, "reset-time").unwrap_or_default();
        let display = if label.contains("Rolling") {
            "Rolling"
        } else if label.contains("Weekly") {
            "Weekly"
        } else if label.contains("Monthly") {
            "Monthly"
        } else if !label.is_empty() {
            label.trim()
        } else {
            "Usage"
        };
        let pct = extract_pct(&value).or_else(|| extract_width_pct(block)).unwrap_or(0.0);
        let resets = parse_reset_secs(&reset);
        let now = chrono::Utc::now().timestamp();
        windows.push(QuotaWindow {
            label: display.to_string(),
            used_percent: pct.clamp(0.0, 100.0),
            resets_at: resets.map(|s| now + s),
        });
        pos = start + 1;
    }
    windows
}

/// Strip HTML comments and tags from a small text fragment.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut in_comment = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '-' && chars.peek() == Some(&'-') {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    in_comment = false;
                }
            }
            continue;
        }
        if in_tag {
            if c == '>' {
                in_tag = false;
            }
            continue;
        }
        if c == '<' {
            if chars.peek() == Some(&'!') {
                let mut ahead = chars.clone();
                ahead.next();
                if ahead.peek() == Some(&'-') {
                    in_comment = true;
                    continue;
                }
            }
            in_tag = true;
            continue;
        }
        out.push(c);
    }
    out
}

fn extract_slot<'a>(block: &'a str, slot: &str) -> Option<String> {
    let needle = format!(r#"data-slot="{}">"#, slot);
    let idx = block.find(&needle)?;
    let rest = &block[idx + needle.len()..];
    let end = rest.find("</span>").unwrap_or(rest.len().min(120));
    let cleaned = strip_html(&rest[..end]);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn extract_pct(value: &str) -> Option<f64> {
    let cleaned = strip_html(value);
    let digits: String = cleaned.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    digits.parse::<f64>().ok()
}

fn extract_width_pct(block: &str) -> Option<f64> {
    let needle = r#"data-slot="progress-bar" style="width:"#;
    let idx = block.find(needle)?;
    let rest = &block[idx + needle.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    digits.parse::<f64>().ok()
}

/// "3 hours 17 minutes" / "17 days 16 hours" / "Resets now" -> seconds.
fn parse_reset_secs(s: &str) -> Option<i64> {
    let cleaned = strip_html(s);
    let lower = cleaned.to_lowercase();
    if lower.contains("now") {
        return Some(0);
    }
    let mut secs: i64 = 0;
    let mut any = false;
    for (unit, mult) in [("day", 86400i64), ("hour", 3600), ("minute", 60), ("second", 1)] {
        let mut idx = 0;
        while let Some(rel) = lower[idx..].find(unit) {
            let pos = idx + rel;
            // scan backwards for the number
            let mut start = pos;
            while start > 0 {
                let prev = lower.as_bytes()[start - 1];
                if prev.is_ascii_digit() || prev == b'.' || prev == b' ' {
                    start -= 1;
                } else {
                    break;
                }
            }
            let num_part = lower[start..pos].trim();
            if let Ok(n) = num_part.parse::<i64>() {
                secs += n * mult;
                any = true;
            }
            idx = pos + unit.len();
        }
    }
    if any {
        Some(secs)
    } else {
        None
    }
}

fn find_workspace_id(parsed: &CurlRequest) -> Option<String> {
    let mut hay = parsed.url.clone();
    for (_, v) in &parsed.headers {
        hay.push_str(" ");
        hay.push_str(v);
    }
    if let Some(b) = &parsed.body {
        hay.push_str(" ");
        hay.push_str(b);
    }
    let needle = "wrk_";
    let mut idx = 0;
    while let Some(rel) = hay[idx..].find(needle) {
        let start = idx + rel;
        let id: String = hay[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if id.len() > 4 {
            return Some(id);
        }
        idx = start + 1;
    }
    None
}

fn fetch_opencode_live() -> QuotaProvider {
    let path = existing_curl_config();
    let Some(path) = path else {
        return opencode_local_fallback("No session configured.");
    };
    let command = match std::fs::read_to_string(&path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return opencode_local_fallback("Stored session command is empty."),
    };
    let Some(parsed) = parse_curl(&command) else {
        return opencode_local_fallback("Could not parse the stored curl command.");
    };
    let Ok(client) = client() else {
        return opencode_local_fallback("HTTP client failed.");
    };

    // 1. Replay the captured request (usage history + any rate-limit windows).
    let method = match parsed.method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        _ => reqwest::Method::POST,
    };
    let mut req = client.request(method, &parsed.url);
    if let Some(body) = &parsed.body {
        req = req.body(body.clone());
    }
    for (k, v) in &parsed.headers {
        if let (Ok(kv), Ok(vv)) = (reqwest::header::HeaderName::from_bytes(k.as_bytes()), reqwest::header::HeaderValue::from_str(v)) {
            req = req.header(kv, vv);
        }
    }
    if let Some(cookie) = &parsed.cookie {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(cookie) {
            req = req.header(reqwest::header::COOKIE, v);
        }
    }

    let mut windows: Vec<QuotaWindow> = Vec::new();
    let mut stats: Vec<(String, String)> = Vec::new();
    let mut rpc_ok = false;
    let mut session_expired = false;

    match req.send() {
        Ok(resp) => {
            let status_code = resp.status();
            if status_code == 401 || status_code == 403 {
                session_expired = true;
            } else {
                let text = resp.text().unwrap_or_default();
                if status_code.is_success() {
                    let (w, s) = parse_server_payload(&text);
                    windows = w;
                    stats = s;
                    rpc_ok = true;
                }
            }
        }
        Err(_) => {}
    }

    // 2. Fetch the Go page HTML with the same session — it renders the real
    //    Rolling / Weekly / Monthly usage items server-side.
    if let Some(ws) = find_workspace_id(&parsed) {
        let mut page = client.get(format!("https://opencode.ai/workspace/{}/go", ws));
        page = page
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .header("Sec-Fetch-Dest", "document")
            .header("Sec-Fetch-Mode", "navigate")
            .header("Sec-Fetch-Site", "same-origin")
            .header("Upgrade-Insecure-Requests", "1")
            .header(reqwest::header::REFERER, format!("https://opencode.ai/workspace/{}/go", ws));
        if let Some(cookie) = &parsed.cookie {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(cookie) {
                page = page.header(reqwest::header::COOKIE, v);
            }
        }
        if let Ok(resp) = page.send() {
            if let Ok(text) = resp.text() {
                let html_windows = parse_usage_html(&text);
                if !html_windows.is_empty() {
                    windows = html_windows;
                }
            }
        }
    }

    if session_expired && windows.is_empty() && stats.is_empty() {
        return opencode_local_fallback("opencode.ai session expired — update the saved curl command.");
    }
    if windows.is_empty() && stats.is_empty() {
        return opencode_local_fallback("opencode.ai returned no usage data (session or payload changed?).");
    }
    let (status, message) = if windows.is_empty() {
        (
            "local".to_string(),
            "Could not fetch rate limits from the Go page — showing usage history instead.".to_string(),
        )
    } else {
        ("ok".to_string(), String::new())
    };
    QuotaProvider {
        id: "opencode".into(),
        name: "OpenCode".into(),
        status,
        message,
        plan: None,
        windows,
        credits: None,
        credits_unlimited: false,
        stats,
    }
}

/// OpenCode has no public quota endpoint without a browser session; this
/// fallback reports usage derived from the local session store.
fn opencode_local_fallback(reason: &str) -> QuotaProvider {
    let mut p = fetch_opencode_local();
    p.message = format!("{} Showing usage from your local session store.", reason);
    p
}

fn fetch_opencode_local() -> QuotaProvider {
    let db = crate::sources::home_dir()
        .map(|h| h.join(".local").join("share").join("opencode").join("opencode.db"));
    let Some(db) = db.filter(|p| p.is_file()) else {
        return QuotaProvider {
            id: "opencode".into(),
            name: "OpenCode".into(),
            status: "local".into(),
            message: "No opencode data found.".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        };
    };

    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let week_start = today - chrono::Days::new(6);
    let month_start = chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);

    let day = today.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
    let week = week_start.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
    let month = month_start.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
    let now = chrono::Utc::now().timestamp_millis();

    let run = |from_ms: i64| -> (f64, u64, u64) {
        let Ok(conn) = rusqlite::Connection::open(&db) else { return (0.0, 0, 0) };
        let table = crate::sources::opencode::session_table(&conn);
        let mut stmt = match conn.prepare(&format!(
            "SELECT COALESCE(SUM(cost),0), COALESCE(SUM(tokens_input),0)+COALESCE(SUM(tokens_output),0), COUNT(*) \
             FROM {} WHERE time_created >= ?1 AND time_created < ?2",
            table
        )) {
            Ok(s) => s,
            Err(_) => return (0.0, 0, 0),
        };
        match stmt.query_row(rusqlite::params![from_ms, now], |row| {
            Ok((row.get::<_, f64>(0)?, row.get::<_, u64>(1)?, row.get::<_, u64>(2)?))
        }) {
            Ok(v) => v,
            Err(_) => (0.0, 0, 0),
        }
    };

    let (t_cost, t_tok, t_sess) = run(day);
    let (w_cost, w_tok, w_sess) = run(week);
    let (m_cost, m_tok, m_sess) = run(month);

    let f = |v: f64| format!("${:.2}", v);
    let g = |v: u64| {
        if v >= 1_000_000_000 {
            format!("{:.1}B tok", v as f64 / 1_000_000_000.0)
        } else if v >= 1_000_000 {
            format!("{:.1}M tok", v as f64 / 1_000_000.0)
        } else if v >= 1_000 {
            format!("{}k tok", v as f64 / 1_000.0)
        } else {
            format!("{} tok", v)
        }
    };

    QuotaProvider {
        id: "opencode".into(),
        name: "OpenCode".into(),
        status: "local".into(),
        message: String::new(),
        plan: None,
        windows: vec![],
        credits: None,
        credits_unlimited: false,
        stats: vec![
            (format!("Today \u{00b7} {} sessions", t_sess), format!("{} \u{00b7} {}", g(t_tok), f(t_cost))),
            (format!("This week \u{00b7} {} sessions", w_sess), format!("{} \u{00b7} {}", g(w_tok), f(w_cost))),
            (format!("This month \u{00b7} {} sessions", m_sess), format!("{} \u{00b7} {}", g(m_tok), f(m_cost))),
        ],
    }
}

/// Poll all live quota providers. Each is independent and never blocks
/// others; failures degrade to a per-provider status.
pub fn fetch_all() -> Vec<QuotaProvider> {
    let anthropic = std::thread::spawn(fetch_anthropic);
    let codex = std::thread::spawn(fetch_codex);
    let opencode = std::thread::spawn(fetch_opencode_live);
    vec![
        anthropic.join().unwrap_or_else(|_| QuotaProvider {
            id: "anthropic".into(),
            name: "Claude Code".into(),
            status: "error".into(),
            message: "quota fetch thread panicked".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        }),
        codex.join().unwrap_or_else(|_| QuotaProvider {
            id: "codex".into(),
            name: "Codex CLI".into(),
            status: "error".into(),
            message: "quota fetch thread panicked".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        }),
        opencode.join().unwrap_or_else(|_| QuotaProvider {
            id: "opencode".into(),
            name: "OpenCode".into(),
            status: "error".into(),
            message: "quota fetch thread panicked".into(),
            plan: None,
            windows: vec![],
            credits: None,
            credits_unlimited: false,
            stats: vec![],
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_curl_command() {
        let cmd = r#"curl 'https://opencode.ai/_server?org=abc123' \
  -H 'accept: */*' \
  -H 'cookie: __Secure-accessToken=abc; x=1' \
  --compressed"#;
        let parsed = parse_curl(cmd).expect("parse");
        assert_eq!(parsed.url, "https://opencode.ai/_server?org=abc123");
        assert!(parsed.headers.iter().any(|(k, _)| k == "accept"));
        assert_eq!(parsed.cookie.as_deref(), Some("__Secure-accessToken=abc; x=1"));
        assert_eq!(parsed.method, "GET");
        assert!(parsed.body.is_none());
    }

    #[test]
    fn parses_curl_with_post_body() {
        let cmd = r#"curl 'https://opencode.ai/_server' \
  --compressed \
  -X POST \
  -H 'Accept: */*' \
  -H 'Content-Type: application/json' \
  -H 'Cookie: auth=abc123' \
  --data-raw '{"t":{"t":9,"i":0,"l":4,"a":[{"t":1,"s":"wrk_1"}]},"f":31,"m":[]}'"#;
        let parsed = parse_curl(cmd).expect("parse");
        assert_eq!(parsed.method, "POST");
        assert!(parsed.body.as_deref().unwrap().contains("\"f\":31"));
        assert_eq!(parsed.cookie.as_deref(), Some("auth=abc123"));
        // accept-encoding headers are dropped so the response stays plain
        assert!(!parsed.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding")));
    }

    #[test]
    fn parses_server_payload() {
        let raw = r#"<script>const rollingUsage: $R[0] = { status: "ok", resetInSec: 3600, usagePercent: 57 }; const weeklyUsage: $R[1] = { status: "ok", resetInSec: 86400, usagePercent: 12 }; const monthlyUsage: $R[2] = { status: "ok", resetInSec: 604800, usagePercent: 3 };</script>"#;
        let (windows, stats) = parse_server_payload(raw);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "Rolling");
        assert_eq!(windows[0].used_percent, 57.0);
        assert!(windows[0].resets_at.is_some());
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[2].label, "Monthly");
        assert!(stats.is_empty());
    }

    #[test]
    fn parses_usage_history_payload() {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d");
        let raw = format!(
            r#";0x0000031f;((self.$R=self.$R||{{}})["server-fn:0"]=[],($R=>$R[0]={{usage:$R[1]=[$R[2]={{date:"2026-08-04",model:"kimi-k3",totalCost:305370000,keyId:"k1",plan:"lite"}},$R[3]={{date:"{}",model:"deepseek-v4-flash",totalCost:100000000,keyId:"k1",plan:"lite"}}],keys:$R[7]=[$R[8]={{id:"k1",displayName:"default",deleted:!1}}]}})($R["server-fn:0"]))"#,
            today
        );
        let (windows, stats) = parse_server_payload(&raw);
        assert!(windows.is_empty());
        assert!(stats.len() >= 4, "expected today/week/month/models rows, got {:?}", stats);
        let today_row = stats.iter().find(|(l, _)| l == "Today").unwrap();
        assert_eq!(today_row.1, "$1.00"); // 100000000 micro-cents = $1
    }

    #[test]
    fn parses_mixed_payload() {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d");
        let raw = format!(
            r#"const rollingUsage: $R[0] = {{ status: "ok", resetInSec: 60, usagePercent: 7 }}; const usage: $R[1] = [{{date:"{}",model:"x",totalCost:50000000}}]"#,
            today
        );
        let (windows, stats) = parse_server_payload(&raw);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 7.0);
        assert!(!stats.is_empty());
    }

    #[test]
    fn parses_current_usage_list_payload() {
        let today = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z");
        let raw = format!(
            r#";0x00006762;((self.$R=self.$R||{{}})["server-fn:1"]=[],($R=>$R[0]=[$R[1]={{id:"usg_1",timeCreated:$R[2]=new Date("{today}"),model:"deepseek-v4-flash",cost:68370,keyID:"k1",enrichment:$R[4]={{plan:"lite"}}}},$R[5]={{id:"usg_2",timeCreated:new Date("2026-08-01T00:00:00.000Z"),model:"kimi-k3",cost:100000000}}])($R["server-fn:1"]))"#
        );
        let (windows, stats) = parse_server_payload(&raw);
        assert!(windows.is_empty());
        assert!(stats.len() >= 4, "got {:?}", stats);
        let today_row = stats.iter().find(|(l, _)| l == "Today").unwrap();
        assert_eq!(today_row.1, "$0.00"); // 68370 micro-cents rounds below a cent
        let month_row = stats.iter().find(|(l, _)| l == "This month").unwrap();
        assert_eq!(month_row.1, "$1.00"); // old entry still counted in month total
    }

    #[test]
    fn parses_go_page_html_usage_items() {
        let raw = r#"<div data-hk="1" data-slot="usage-item"><div data-slot="usage-header"><span data-slot="usage-label">Weekly Usage</span><span data-slot="usage-value"><!--$-->50<!--/-->%</span></div><div data-slot="progress"><div data-slot="progress-bar" style="width:50%"></div></div><span data-slot="reset-time"><!--$-->Resets in<!--/--> <!--$-->11 hours 45 minutes<!--/--></span></div> <div data-hk="2" data-slot="usage-item"><div data-slot="usage-header"><span data-slot="usage-label">Rolling Usage</span><span data-slot="usage-value"><!--$-->0<!--/-->%</span></div><div data-slot="progress"><div data-slot="progress-bar" style="width:0%"></div></div><span data-slot="reset-time"><!--$-->Resets in<!--/--> <!--$-->3 hours 18 minutes<!--/--></span></div><div data-hk="3" data-slot="usage-item"><div data-slot="usage-header"><span data-slot="usage-label">Monthly Usage</span><span data-slot="usage-value"><!--$-->25<!--/-->%</span></div><div data-slot="progress"><div data-slot="progress-bar" style="width:25%"></div></div><span data-slot="reset-time"><!--$-->Resets in<!--/--> <!--$-->17 days 16 hours<!--/--></span></div>"#;
        let windows = parse_usage_html(raw);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "Weekly");
        assert_eq!(windows[0].used_percent, 50.0);
        let now = chrono::Utc::now().timestamp();
        let reset = windows[0].resets_at.unwrap() - now;
        assert!((reset - (11 * 3600 + 45 * 60)).abs() < 5);
        assert_eq!(windows[1].label, "Rolling");
        assert_eq!(windows[1].used_percent, 0.0);
        assert_eq!(windows[2].label, "Monthly");
        assert_eq!(windows[2].used_percent, 25.0);
        let reset2 = windows[2].resets_at.unwrap() - now;
        assert!((reset2 - (17 * 86400 + 16 * 3600)).abs() < 5);
    }

    #[test]
    fn parses_reset_seconds() {
        assert_eq!(parse_reset_secs("Resets in 11 hours 45 minutes"), Some(11 * 3600 + 45 * 60));
        assert_eq!(parse_reset_secs("Resets in 3 hours 18 minutes"), Some(3 * 3600 + 18 * 60));
        assert_eq!(parse_reset_secs("Resets in 17 days 16 hours"), Some(17 * 86400 + 16 * 3600));
        assert_eq!(parse_reset_secs("Resets in 30 minutes"), Some(1800));
        assert_eq!(parse_reset_secs("Resets now"), Some(0));
    }

    #[test]
    fn normalizes_anthropic_utilization() {
        let v: serde_json::Value = serde_json::json!({
            "five_hour": {"utilization": 0.42, "resets_at": "2026-06-27T17:00:00Z"},
            "seven_day": {"utilization": 1.3, "resets_at": "2026-06-27T17:00:00Z"}
        });
        let windows = parse_anthropic_usage(&v);
        assert_eq!(windows.len(), 2);
        assert!((windows[0].used_percent - 42.0).abs() < 0.01);
        assert!((windows[1].used_percent - 1.3).abs() < 0.01, ">1 values are already percents");
    }
}
