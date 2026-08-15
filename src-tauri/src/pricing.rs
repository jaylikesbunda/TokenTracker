use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

const PRICING_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Default, Deserialize, Clone)]
pub struct Price {
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    pub cache_read_input_token_cost: Option<f64>,
}

static PRICING: OnceLock<RwLock<HashMap<String, Price>>> = OnceLock::new();
static LAST_ATTEMPT: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn table() -> &'static RwLock<HashMap<String, Price>> {
    PRICING.get_or_init(|| RwLock::new(
        serde_json::from_str(include_str!("../pricing.json")).expect("valid pricing.json"),
    ))
}

fn cache_path() -> Option<PathBuf> {
    let home = crate::sources::home_dir()?;
    #[cfg(windows)]
    let base = home.join("AppData").join("Roaming");
    #[cfg(not(windows))]
    let base = home.join(".config");
    Some(base.join("TokenTracker").join("pricing.json"))
}

fn load_json(raw: &str) -> Option<HashMap<String, Price>> {
    serde_json::from_str(raw).ok()
}

/// Load the most recently downloaded sheet, if one is available.
pub fn initialize() {
    let Some(path) = cache_path().filter(|p| p.is_file()) else { return };
    let Ok(raw) = std::fs::read_to_string(path) else { return };
    let Some(prices) = load_json(&raw) else { return };
    if let Ok(mut current) = table().write() {
        *current = prices;
    }
}

fn keep_model(key: &str) -> bool {
    let name = key.rsplit('/').next().unwrap_or(key);
    [
        "claude-", "gpt-", "o1-", "o3-", "o4-", "o5-", "deepseek-", "gemini-", "glm-",
        "kimi-", "moonshot-", "qwen-", "qwen2", "qwen3", "minimax-", "mistral-", "llama-",
        "grok-", "command-",
    ].iter().any(|prefix| name.starts_with(prefix)) || key.starts_with("dashscope/qwen-")
}

fn curated_sheet(raw: &str) -> Option<HashMap<String, Price>> {
    let source: HashMap<String, serde_json::Value> = serde_json::from_str(raw).ok()?;
    let mut prices = HashMap::new();
    for (key, value) in source {
        if !keep_model(&key) {
            continue;
        }
        let price: Price = serde_json::from_value(value).ok()?;
        if price.input_cost_per_token == 0.0 && price.output_cost_per_token == 0.0 {
            continue;
        }
        prices.insert(key, price);
    }
    for (alias, source) in [
        ("z-ai/glm-5.1", "zai/glm-5.1"),
        ("z-ai/glm-5.2", "zai/glm-5.2"),
        ("moonshotai/kimi-k2.6", "moonshot/kimi-k2.6"),
        ("minimax-m3", "openrouter/minimax/minimax-m2.5"),
    ] {
        if let Some(price) = prices.get(source).cloned() {
            prices.insert(alias.to_string(), price);
        }
    }
    for free in ["deepseek-v4-flash-free", "qwen3.6-plus-free", "mimo-v2.5-free", "nemotron-3-ultra-free"] {
        prices.insert(free.to_string(), Price::default());
    }
    Some(prices)
}

/// Refresh the runtime price sheet once daily. Failures retain the last known
/// good sheet and are retried no more than once every six hours.
pub fn refresh_if_due() -> bool {
    let path = cache_path();
    let fresh_on_disk = path.as_ref().and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.elapsed().ok())
        .map(|age| age < REFRESH_INTERVAL)
        .unwrap_or(false);
    if fresh_on_disk {
        return false;
    }
    let attempts = LAST_ATTEMPT.get_or_init(|| Mutex::new(None));
    let mut last = attempts.lock().unwrap();
    if last.map(|at| at.elapsed() < RETRY_INTERVAL).unwrap_or(false) {
        return false;
    }
    *last = Some(Instant::now());
    drop(last);

    let Ok(client) = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build() else {
        return false;
    };
    let Ok(response) = client.get(PRICING_URL).send().and_then(|r| r.error_for_status()) else {
        return false;
    };
    let Ok(raw) = response.text() else { return false };
    let Some(prices) = curated_sheet(&raw) else { return false };
    let Some(path) = path else { return false };
    let Ok(serialized) = serde_json::to_string(&prices) else { return false };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() { return false; }
    }
    if std::fs::write(path, serialized).is_err() { return false; }
    if let Ok(mut current) = table().write() {
        *current = prices;
        true
    } else {
        false
    }
}

fn strip_date_suffix(model: &str) -> Option<String> {
    let (head, tail) = model.rsplit_once('-')?;
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if tail.len() == 8 || tail.len() == 10 {
        Some(head.to_string())
    } else {
        None
    }
}

/// Look up a price. Tries, in order: the lowercased full key ("zai/glm-5.1"),
/// the provider-stripped key ("glm-5.1"), date-suffix variants, then the
/// longest table key that prefixes the model name.
pub fn lookup(model: &str) -> Option<Price> {
    let table = table().read().ok()?;
    let m = model.trim().to_lowercase();
    if m.is_empty() {
        return None;
    }
    let mut candidates = vec![m.clone()];
    if let Some(idx) = m.rfind('/') {
        candidates.push(m[idx + 1..].to_string());
    }

    for cand in &candidates {
        if let Some(p) = table.get(cand) {
            return Some(p.clone());
        }
        if let Some(rest) = strip_date_suffix(cand) {
            if let Some(p) = table.get(&rest) {
                return Some(p.clone());
            }
        }
        let mut best: Option<(&str, usize)> = None;
        for key in table.keys() {
            if cand.starts_with(key.as_str()) && best.map(|(_, l)| key.len() > l).unwrap_or(true) {
                best = Some((key.as_str(), key.len()));
            }
        }
        if let Some((k, _)) = best {
            return table.get(k).cloned();
        }
    }
    None
}

/// Compute USD cost for a request. Unknown models price at $0.00.
pub fn cost(model: &str, input: u64, output: u64, cache_creation: u64, cache_read: u64) -> f64 {
    match lookup(model) {
        Some(p) => {
            input as f64 * p.input_cost_per_token
                + output as f64 * p.output_cost_per_token
                + cache_creation as f64 * p.cache_creation_input_token_cost.unwrap_or(0.0)
                + cache_read as f64 * p.cache_read_input_token_cost.unwrap_or(0.0)
        }
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_matches() {
        assert!(lookup("claude-opus-5").is_some());
        assert!(lookup("gpt-5.1-codex").is_some());
        assert!(lookup("deepseek-v4-flash").is_some());
        assert!(lookup("gemini-2.5-flash").is_some());
    }

    #[test]
    fn date_suffix_stripping() {
        assert!(lookup("gpt-5.1-codex-2025-11-20").is_some());
        assert!(lookup("claude-sonnet-4-5-20250929").is_some());
    }

    #[test]
    fn prefix_matching() {
        assert!(lookup("claude-sonnet-4-5-20250929-v1:0").is_some());
    }

    #[test]
    fn provider_prefix_and_case() {
        assert!(lookup("z-ai/glm-5.1").is_some());
        assert!(lookup("zai/glm-5.1").is_some());
        assert!(lookup("moonshotai/kimi-k2.6").is_some());
        assert!(lookup("MiniMax-M3").is_some() || lookup("minimax-m3").is_some());
        assert!(lookup("opencode-go/deepseek-v4-flash").is_some());
        assert!(lookup("gpt-5.5").is_some());
        assert!(lookup("gpt-5.6-terra").is_some());
        assert!(lookup("gpt-5.4-mini").is_some());
    }

    #[test]
    fn free_variants_resolve() {
        assert!(lookup("deepseek-v4-flash-free").is_some());
        assert!(lookup("qwen3.6-plus-free").is_some());
        assert!(lookup("mimo-v2.5-free").is_some());
        assert!(lookup("nemotron-3-ultra-free").is_some());
    }

    #[test]
    fn unknown_model_prices_zero() {
        assert_eq!(cost("totally-unknown-model-9000", 1000, 1000, 0, 0), 0.0);
    }
}
