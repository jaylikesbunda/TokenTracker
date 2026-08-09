#!/usr/bin/env python3
"""Fetch LiteLLM pricing data and curate the model subset used by TokenTray.

Regenerates src-tauri/pricing.json. Run:  python scripts/fetch_pricing.py
"""
import json
import urllib.request

URL = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"

PREFIXES = [
    "claude-", "gpt-", "o1-", "o3-", "o4-", "o5-",
    "deepseek-", "gemini-", "glm-", "kimi-", "moonshot-", "qwen-",
    "qwen2", "qwen3", "minimax-", "mistral-", "llama-", "grok-", "command-",
    "dashscope/qwen-",
]

# Known-free variants (no price by design) so they are never flagged unpriced.
FREE_OVERRIDES = {
    "deepseek-v4-flash-free": {},
    "qwen3.6-plus-free": {},
    "mimo-v2.5-free": {},
    "nemotron-3-ultra-free": {},
}

# Model ids as reported by opencode's gateway, aliased to LiteLLM keys.
ALIASES = {
    "z-ai/glm-5.1": "zai/glm-5.1",
    "z-ai/glm-5.2": "zai/glm-5.2",
    "moonshotai/kimi-k2.6": "moonshot/kimi-k2.6",
    "minimax-m3": "openrouter/minimax/minimax-m2.5",  # m3 not in LiteLLM yet; m2.5 estimate
    "MiniMax-M3": "openrouter/minimax/minimax-m2.5",
}

KEEP = ("input_cost_per_token", "output_cost_per_token",
        "cache_creation_input_token_cost", "cache_read_input_token_cost")


def main() -> None:
    print("fetching", URL)
    data = json.load(urllib.request.urlopen(URL, timeout=60))
    sel = {}
    for key, value in data.items():
        name = key.rsplit("/", 1)[-1]
        if not any(name.startswith(p) for p in PREFIXES):
            continue  # keep bare keys and provider-prefixed keys alike
        if value.get("input_cost_per_token") is None and value.get("output_cost_per_token") is None:
            continue
        sel[key] = {k: value[k] for k in KEEP if k in value and value[k] is not None}
    for key, over in FREE_OVERRIDES.items():
        sel[key] = dict(over)
    for alias, source in ALIASES.items():
        if source in sel:
            sel[alias] = dict(sel[source])
    out = "src-tauri/pricing.json"
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(sel, fh, indent=1, sort_keys=True)
    print(f"wrote {len(sel)} models to {out}")


if __name__ == "__main__":
    main()
