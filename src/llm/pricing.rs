//! Best-effort LLM pricing estimates.
//!
//! Maps model names to per-token costs (USD). These are approximate —
//! actual costs depend on provider agreements, caching, and batching.
//! Unknown models fall back to a conservative default.

/// Per-token pricing for a model.
struct ModelPricing {
    /// Cost per input token in USD.
    input: f64,
    /// Cost per output token in USD.
    output: f64,
    /// Cost per cache-read input token in USD (typically discounted).
    cached_input: f64,
    /// Cost per cache-write input token in USD. Same as input if not separately priced.
    cache_write: f64,
}

/// Look up pricing for a model name. Matches on the model portion
/// (after the provider/ prefix) so "anthropic/claude-sonnet-4-20250514"
/// and "claude-sonnet-4-20250514" both match.
fn lookup_pricing(model_name: &str) -> ModelPricing {
    let model = model_name
        .split_once('/')
        .map(|(_, m)| m)
        .unwrap_or(model_name);

    let per_m = |price: f64| price / 1_000_000.0;

    // Anthropic cache-write pricing is 1.25× input. OpenAI cache-write is same as input.
    // ORDER IS LOAD-BEARING for the Anthropic arms: specific prefixes
    // (claude-opus-4-8) must precede their generic parent (claude-opus-4) —
    // the first matching arm wins.
    // Prices verified 2026-07-29 against platform.claude.com pricing docs
    // (Claude Opus 5 confirmed $5/$25 — same tier as Opus 4.5+).
    match model {
        m if m.starts_with("claude-fable-5") || m.starts_with("claude-mythos-5") => {
            ModelPricing {
                input: per_m(10.0),
                output: per_m(50.0),
                cached_input: per_m(1.0),
                cache_write: per_m(12.5),
            }
        }
        // Opus 5 and Opus 4.5+ are $5/$25 — only Opus 4.0/4.1 remain at $15/$75.
        m if m.starts_with("claude-opus-5")
            || m.starts_with("claude-opus-4-5")
            || m.starts_with("claude-opus-4-6")
            || m.starts_with("claude-opus-4-7")
            || m.starts_with("claude-opus-4-8") =>
        {
            ModelPricing {
                input: per_m(5.0),
                output: per_m(25.0),
                cached_input: per_m(0.50),
                cache_write: per_m(6.25),
            }
        }
        m if m.starts_with("claude-opus-4") => ModelPricing {
            input: per_m(15.0),
            output: per_m(75.0),
            cached_input: per_m(1.5),
            cache_write: per_m(18.75),
        },
        // Sonnet 5 intro pricing ($2/$10) runs through 2026-08-31, then $3/$15.
        m if m.starts_with("claude-sonnet-5") => ModelPricing {
            input: per_m(2.0),
            output: per_m(10.0),
            cached_input: per_m(0.20),
            cache_write: per_m(2.5),
        },
        m if m.starts_with("claude-sonnet-4") => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
            cache_write: per_m(3.75),
        },
        m if m.starts_with("claude-3-5-sonnet") => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
            cache_write: per_m(3.75),
        },
        // Haiku 4.5 is $1/$5 — it was previously lumped in with 3.5-haiku
        // at $0.80/$4 and under-reported by 20%.
        m if m.starts_with("claude-haiku-4") => ModelPricing {
            input: per_m(1.0),
            output: per_m(5.0),
            cached_input: per_m(0.10),
            cache_write: per_m(1.25),
        },
        m if m.starts_with("claude-3-5-haiku") => ModelPricing {
            input: per_m(0.80),
            output: per_m(4.0),
            cached_input: per_m(0.08),
            cache_write: per_m(1.0),
        },

        m if m.starts_with("claude-3-opus") => ModelPricing {
            input: per_m(15.0),
            output: per_m(75.0),
            cached_input: per_m(1.5),
            cache_write: per_m(18.75),
        },
        m if m.starts_with("claude-3-sonnet") => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
            cache_write: per_m(3.75),
        },
        m if m.starts_with("claude-3-haiku") => ModelPricing {
            input: per_m(0.25),
            output: per_m(1.25),
            cached_input: per_m(0.03),
            cache_write: per_m(0.3125),
        },

        m if m.starts_with("gpt-4o-mini") => ModelPricing {
            input: per_m(0.15),
            output: per_m(0.60),
            cached_input: per_m(0.075),
            cache_write: per_m(0.15),
        },
        m if m.starts_with("gpt-4o") => ModelPricing {
            input: per_m(2.50),
            output: per_m(10.0),
            cached_input: per_m(1.25),
            cache_write: per_m(2.50),
        },
        m if m.starts_with("gpt-4-turbo") => ModelPricing {
            input: per_m(10.0),
            output: per_m(30.0),
            cached_input: per_m(5.0),
            cache_write: per_m(10.0),
        },

        m if m.starts_with("o3-mini") => ModelPricing {
            input: per_m(1.10),
            output: per_m(4.40),
            cached_input: per_m(0.55),
            cache_write: per_m(1.10),
        },
        m if m.starts_with("o3") => ModelPricing {
            input: per_m(10.0),
            output: per_m(40.0),
            cached_input: per_m(5.0),
            cache_write: per_m(10.0),
        },
        m if m.starts_with("o1-mini") => ModelPricing {
            input: per_m(3.0),
            output: per_m(12.0),
            cached_input: per_m(1.5),
            cache_write: per_m(3.0),
        },
        m if m.starts_with("o1") => ModelPricing {
            input: per_m(15.0),
            output: per_m(60.0),
            cached_input: per_m(7.5),
            cache_write: per_m(15.0),
        },

        m if m.starts_with("gemini-2.0-flash") || m.starts_with("gemini-2.5-flash") => {
            ModelPricing {
                input: per_m(0.075),
                output: per_m(0.30),
                cached_input: per_m(0.01875),
                cache_write: per_m(0.075),
            }
        }
        m if m.starts_with("gemini-2.5-pro") || m.starts_with("gemini-2.0-pro") => ModelPricing {
            input: per_m(1.25),
            output: per_m(10.0),
            cached_input: per_m(0.3125),
            cache_write: per_m(1.25),
        },
        m if m.starts_with("gemini-1.5-pro") => ModelPricing {
            input: per_m(1.25),
            output: per_m(5.0),
            cached_input: per_m(0.3125),
            cache_write: per_m(1.25),
        },
        m if m.starts_with("gemini-1.5-flash") => ModelPricing {
            input: per_m(0.075),
            output: per_m(0.30),
            cached_input: per_m(0.01875),
            cache_write: per_m(0.075),
        },

        m if m.starts_with("deepseek-chat") || m.starts_with("deepseek-v3") => ModelPricing {
            input: per_m(0.27),
            output: per_m(1.10),
            cached_input: per_m(0.07),
            cache_write: per_m(0.27),
        },
        m if m.starts_with("deepseek-reasoner") || m.starts_with("deepseek-r1") => ModelPricing {
            input: per_m(0.55),
            output: per_m(2.19),
            cached_input: per_m(0.14),
            cache_write: per_m(0.55),
        },

        _ => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
            cache_write: per_m(3.75),
        },
    }
}

/// Estimate cost in USD for a completion call.
///
/// `cached_input_tokens` are subtracted from `input_tokens` for pricing
/// since cached tokens are billed at the (lower) cached rate.
pub fn estimate_cost(
    model_name: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
) -> f64 {
    let pricing = lookup_pricing(model_name);

    let uncached_input = input_tokens.saturating_sub(cached_input_tokens);
    (uncached_input as f64 * pricing.input)
        + (output_tokens as f64 * pricing.output)
        + (cached_input_tokens as f64 * pricing.cached_input)
}

/// Estimate cost using the full extended usage breakdown.
pub fn estimate_cost_extended(model_name: &str, usage: &super::usage::ExtendedUsage) -> f64 {
    let pricing = lookup_pricing(model_name);

    (usage.input_tokens as f64 * pricing.input)
        + (usage.output_tokens as f64 * pricing.output)
        + (usage.cache_read_tokens as f64 * pricing.cached_input)
        + (usage.cache_write_tokens as f64 * pricing.cache_write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_sonnet_pricing() {
        // 1000 input + 500 output tokens on claude-sonnet-4
        let cost = estimate_cost("anthropic/claude-sonnet-4-20250514", 1000, 500, 0);
        // $3/M input + $15/M output = 0.003 + 0.0075 = 0.0105
        assert!((cost - 0.0105).abs() < 1e-10);
    }

    #[test]
    fn test_cached_tokens_reduce_cost() {
        let no_cache = estimate_cost("anthropic/claude-sonnet-4-20250514", 1000, 500, 0);
        let with_cache = estimate_cost("anthropic/claude-sonnet-4-20250514", 1000, 500, 500);
        assert!(with_cache < no_cache);
    }

    #[test]
    fn test_unknown_model_uses_fallback() {
        let cost = estimate_cost("unknown-provider/mystery-model", 1000, 500, 0);
        assert!(cost > 0.0);
    }

    #[test]
    fn test_opus_4_8_is_not_legacy_opus_priced() {
        // Opus 4.5+ is $5/$25; the generic claude-opus-4 arm ($15/$75) must
        // not shadow it — arm order is load-bearing.
        let cost = estimate_cost("anthropic/claude-opus-4-8", 1_000_000, 1_000_000, 0);
        assert!((cost - 30.0).abs() < 1e-6, "opus-4-8 should cost $5+$25, got {cost}");
        let legacy = estimate_cost("anthropic/claude-opus-4-1-20250805", 1_000_000, 1_000_000, 0);
        assert!((legacy - 90.0).abs() < 1e-6, "opus-4.1 should cost $15+$75, got {legacy}");
    }

    #[test]
    fn test_opus_5_pricing() {
        // Opus 5 replaced Opus 4.8 as the worker default (2026-07-29) but is
        // priced the same: $5+$25, not the legacy $15+$75 claude-opus-4 arm.
        let cost = estimate_cost("anthropic/claude-opus-5", 1_000_000, 1_000_000, 0);
        assert!((cost - 30.0).abs() < 1e-6, "opus-5 should cost $5+$25, got {cost}");
    }

    #[test]
    fn test_fable_5_pricing() {
        let cost = estimate_cost("anthropic/claude-fable-5", 1_000_000, 1_000_000, 0);
        assert!((cost - 60.0).abs() < 1e-6, "fable-5 should cost $10+$50, got {cost}");
    }

    #[test]
    fn test_sonnet_5_intro_pricing() {
        let cost = estimate_cost("anthropic/claude-sonnet-5", 1_000_000, 1_000_000, 0);
        assert!((cost - 12.0).abs() < 1e-6, "sonnet-5 should cost $2+$10, got {cost}");
    }

    #[test]
    fn test_haiku_4_5_not_priced_as_3_5_haiku() {
        let cost = estimate_cost("anthropic/claude-haiku-4-5-20251001", 1_000_000, 1_000_000, 0);
        assert!((cost - 6.0).abs() < 1e-6, "haiku-4.5 should cost $1+$5, got {cost}");
    }
}
