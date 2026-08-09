use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Deserialize, Clone)]
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

static PRICING: OnceLock<HashMap<String, Price>> = OnceLock::new();

fn table() -> &'static HashMap<String, Price> {
    PRICING.get_or_init(|| {
        serde_json::from_str(include_str!("../pricing.json")).expect("valid pricing.json")
    })
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
pub fn lookup(model: &str) -> Option<&'static Price> {
    let table = table();
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
            return Some(p);
        }
        if let Some(rest) = strip_date_suffix(cand) {
            if let Some(p) = table.get(&rest) {
                return Some(p);
            }
        }
        let mut best: Option<(&str, usize)> = None;
        for key in table.keys() {
            if cand.starts_with(key.as_str()) && best.map(|(_, l)| key.len() > l).unwrap_or(true) {
                best = Some((key.as_str(), key.len()));
            }
        }
        if let Some((k, _)) = best {
            return Some(&table[k]);
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
