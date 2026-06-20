// Pricing: https://platform.claude.com/docs/en/about-claude/pricing
// Cache writes bill by TTL — 5m = 1.25x base input, 1h = 2x; subscription
// users default to 1h, so flat-5m would materially undercount. 1M-context
// models bill at standard per-token across the whole window (no >200k tier).

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::aggregator::TokenStats;

static GLOBAL_CALCULATOR: LazyLock<CostCalculator> = LazyLock::new(CostCalculator::new);

const FAMILIES: &[(&str, &str)] = &[
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
    ("fable", "Fable"),
    ("mythos", "Mythos"),
];

const OPENAI_FAMILIES: &[(&str, &str)] = &[
    ("gpt-", "GPT-"),
    ("o3-pro", "o3-pro"),
    ("o4-mini", "o4-mini"),
    ("o3-mini", "o3-mini"),
    ("o3", "o3"),
];

pub fn normalize_model_name(model: &str) -> String {
    for &(family, display) in FAMILIES {
        if !model.contains(family) {
            continue;
        }

        let prefix = format!("{family}-");
        if let Some(pos) = model.find(&prefix) {
            let after = &model[pos + prefix.len()..];
            let major_str: String = after.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(major) = major_str.parse::<u32>()
                && major_str.len() <= 2
            {
                let rest = &after[major_str.len()..];
                if let Some(rest) = rest.strip_prefix('-') {
                    let minor_str: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    if minor_str.len() <= 2
                        && let Ok(minor) = minor_str.parse::<u32>()
                    {
                        return format!("{display} {major}.{minor}");
                    }
                }
                return format!("{display} {major}");
            }
        }

        if let Some(pos) = model.find(family) {
            let before = &model[..pos];
            let parts: Vec<&str> = before.split('-').filter(|s| !s.is_empty()).collect();
            let nums: Vec<u32> = parts
                .iter()
                .rev()
                .take(2)
                .filter_map(|s| s.parse::<u32>().ok())
                .collect();
            match nums.len() {
                2 => return format!("{} {}.{}", display, nums[1], nums[0]),
                1 => return format!("{} {}", display, nums[0]),
                _ => {}
            }
        }

        // Family matched but no version digits (e.g. a "-preview" id): keep
        // the raw name. Collapsing to the bare family label would merge it
        // with versioned siblings' namespace and hide WHICH id is unpriced.
        return model.to_string();
    }

    // OpenAI model normalization: strip date suffixes, capitalize GPT prefix.
    for &(prefix, display) in OPENAI_FAMILIES {
        if let Some(rest) = model.strip_prefix(prefix) {
            if display == "GPT-" {
                // gpt-5.5-2026... → GPT-5.5
                let version: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                let after_version = &rest[version.len()..];
                let suffix = after_version.strip_prefix('-').unwrap_or(after_version);
                if !version.is_empty() {
                    if suffix.is_empty()
                        || suffix.chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        return format!("{display}{version}");
                    }
                    return format!("{display}{version}-{suffix}");
                }
            } else {
                // o3, o3-pro, o4-mini — already at display form if no date suffix
                let date_stripped = strip_date_suffix(model);
                return date_stripped.to_string();
            }
        }
    }

    if model.is_empty() {
        "unknown".to_string()
    } else {
        model.to_string()
    }
}

fn strip_date_suffix(model: &str) -> &str {
    // Strip trailing `-YYYYMMDD` date suffix (8+ digits after last `-`)
    if let Some(last_dash) = model.rfind('-') {
        let suffix = &model[last_dash + 1..];
        if suffix.len() >= 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &model[..last_dash];
        }
    }
    model
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_cost_per_mtok: f64,
    pub output_cost_per_mtok: f64,
    /// 5-minute TTL cache write rate = 1.25x base input. API key auth and
    /// Bedrock / Vertex / Foundry default to this TTL.
    pub cache_write_5m_cost_per_mtok: f64,
    /// 1-hour TTL cache write rate = 2x base input. Subscription users
    /// (Pro/Max/Team via Claude Code) get this TTL automatically.
    pub cache_write_1h_cost_per_mtok: f64,
    pub cache_read_cost_per_mtok: f64, // 0.1x input
}

pub struct CostCalculator {
    pricing: HashMap<String, ModelPricing>,
    sorted_keys: Vec<String>,
}

impl Default for CostCalculator {
    fn default() -> Self {
        Self::new()
    }
}

impl CostCalculator {
    pub fn global() -> &'static CostCalculator {
        &GLOBAL_CALCULATOR
    }

    pub fn new() -> Self {
        let mut pricing = HashMap::new();

        // Per-model rates from platform.claude.com/docs/en/about-claude/pricing.
        // 5m cache write = base × 1.25; 1h cache write = base × 2.0; cache read = base × 0.10.

        // Claude Fable 5: base $10 / $50 (verified against platform.claude.com/docs
        // pricing page); cache rates use the standard multipliers (1.25x / 2x / 0.1x).
        pricing.insert(
            "claude-fable-5".to_string(),
            ModelPricing {
                input_cost_per_mtok: 10.0,
                output_cost_per_mtok: 50.0,
                cache_write_5m_cost_per_mtok: 12.5,
                cache_write_1h_cost_per_mtok: 20.0,
                cache_read_cost_per_mtok: 1.0,
            },
        );

        // Claude Mythos 5: same rates as Fable 5 ($10 / $50, standard cache
        // multipliers) per the official models-overview pricing table; offered
        // through Project Glasswing, so it can appear in real data.
        pricing.insert(
            "claude-mythos-5".to_string(),
            ModelPricing {
                input_cost_per_mtok: 10.0,
                output_cost_per_mtok: 50.0,
                cache_write_5m_cost_per_mtok: 12.5,
                cache_write_1h_cost_per_mtok: 20.0,
                cache_read_cost_per_mtok: 1.0,
            },
        );

        // Claude Opus 4.8: base $5 / $25 (verified against platform.claude.com/docs pricing page)
        pricing.insert(
            "claude-opus-4-8".to_string(),
            ModelPricing {
                input_cost_per_mtok: 5.0,
                output_cost_per_mtok: 25.0,
                cache_write_5m_cost_per_mtok: 6.25,
                cache_write_1h_cost_per_mtok: 10.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // Claude Opus 4.7: base $5 input / $25 output (placeholder == 4.6 until official rates ship)
        pricing.insert(
            "claude-opus-4-7".to_string(),
            ModelPricing {
                input_cost_per_mtok: 5.0,
                output_cost_per_mtok: 25.0,
                cache_write_5m_cost_per_mtok: 6.25,
                cache_write_1h_cost_per_mtok: 10.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // Claude Opus 4.6: base $5 / $25
        pricing.insert(
            "claude-opus-4-6".to_string(),
            ModelPricing {
                input_cost_per_mtok: 5.0,
                output_cost_per_mtok: 25.0,
                cache_write_5m_cost_per_mtok: 6.25,
                cache_write_1h_cost_per_mtok: 10.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // Claude Opus 4.5: base $5 / $25
        pricing.insert(
            "claude-opus-4-5".to_string(),
            ModelPricing {
                input_cost_per_mtok: 5.0,
                output_cost_per_mtok: 25.0,
                cache_write_5m_cost_per_mtok: 6.25,
                cache_write_1h_cost_per_mtok: 10.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // Claude Opus 4.1: base $15 / $75
        pricing.insert(
            "claude-opus-4-1".to_string(),
            ModelPricing {
                input_cost_per_mtok: 15.0,
                output_cost_per_mtok: 75.0,
                cache_write_5m_cost_per_mtok: 18.75,
                cache_write_1h_cost_per_mtok: 30.0,
                cache_read_cost_per_mtok: 1.50,
            },
        );

        // Claude Opus 4 (deprecated): base $15 / $75
        pricing.insert(
            "claude-opus-4".to_string(),
            ModelPricing {
                input_cost_per_mtok: 15.0,
                output_cost_per_mtok: 75.0,
                cache_write_5m_cost_per_mtok: 18.75,
                cache_write_1h_cost_per_mtok: 30.0,
                cache_read_cost_per_mtok: 1.50,
            },
        );

        // Claude Sonnet 4.6: base $3 / $15
        pricing.insert(
            "claude-sonnet-4-6".to_string(),
            ModelPricing {
                input_cost_per_mtok: 3.0,
                output_cost_per_mtok: 15.0,
                cache_write_5m_cost_per_mtok: 3.75,
                cache_write_1h_cost_per_mtok: 6.0,
                cache_read_cost_per_mtok: 0.30,
            },
        );

        // Claude Sonnet 4.5: base $3 / $15
        pricing.insert(
            "claude-sonnet-4-5".to_string(),
            ModelPricing {
                input_cost_per_mtok: 3.0,
                output_cost_per_mtok: 15.0,
                cache_write_5m_cost_per_mtok: 3.75,
                cache_write_1h_cost_per_mtok: 6.0,
                cache_read_cost_per_mtok: 0.30,
            },
        );

        // Claude Sonnet 4 (deprecated): base $3 / $15
        pricing.insert(
            "claude-sonnet-4".to_string(),
            ModelPricing {
                input_cost_per_mtok: 3.0,
                output_cost_per_mtok: 15.0,
                cache_write_5m_cost_per_mtok: 3.75,
                cache_write_1h_cost_per_mtok: 6.0,
                cache_read_cost_per_mtok: 0.30,
            },
        );

        // Claude Sonnet 3.7 (deprecated): base $3 / $15
        pricing.insert(
            "claude-3-7-sonnet".to_string(),
            ModelPricing {
                input_cost_per_mtok: 3.0,
                output_cost_per_mtok: 15.0,
                cache_write_5m_cost_per_mtok: 3.75,
                cache_write_1h_cost_per_mtok: 6.0,
                cache_read_cost_per_mtok: 0.30,
            },
        );

        // Claude Sonnet 3.5 (legacy): base $3 / $15
        pricing.insert(
            "claude-3-5-sonnet".to_string(),
            ModelPricing {
                input_cost_per_mtok: 3.0,
                output_cost_per_mtok: 15.0,
                cache_write_5m_cost_per_mtok: 3.75,
                cache_write_1h_cost_per_mtok: 6.0,
                cache_read_cost_per_mtok: 0.30,
            },
        );

        // Claude Opus 3 (deprecated): base $15 / $75
        pricing.insert(
            "claude-3-opus".to_string(),
            ModelPricing {
                input_cost_per_mtok: 15.0,
                output_cost_per_mtok: 75.0,
                cache_write_5m_cost_per_mtok: 18.75,
                cache_write_1h_cost_per_mtok: 30.0,
                cache_read_cost_per_mtok: 1.50,
            },
        );

        // Claude Haiku 4.5: base $1 / $5
        pricing.insert(
            "claude-haiku-4-5".to_string(),
            ModelPricing {
                input_cost_per_mtok: 1.0,
                output_cost_per_mtok: 5.0,
                cache_write_5m_cost_per_mtok: 1.25,
                cache_write_1h_cost_per_mtok: 2.0,
                cache_read_cost_per_mtok: 0.10,
            },
        );

        // Claude Haiku 3.5: base $0.80 / $4
        pricing.insert(
            "claude-3-5-haiku".to_string(),
            ModelPricing {
                input_cost_per_mtok: 0.80,
                output_cost_per_mtok: 4.0,
                cache_write_5m_cost_per_mtok: 1.0,
                cache_write_1h_cost_per_mtok: 1.60,
                cache_read_cost_per_mtok: 0.08,
            },
        );

        // Claude Haiku 3 (legacy): base $0.25 / $1.25 (1h rate inferred 2x base)
        pricing.insert(
            "claude-3-haiku".to_string(),
            ModelPricing {
                input_cost_per_mtok: 0.25,
                output_cost_per_mtok: 1.25,
                cache_write_5m_cost_per_mtok: 0.30,
                cache_write_1h_cost_per_mtok: 0.50,
                cache_read_cost_per_mtok: 0.03,
            },
        );

        // --- OpenAI models (Codex CLI) ---
        // OpenAI has no cache write concept; cache_write fields are 0.
        // cache_read maps to OpenAI's "cached input" discount.

        // GPT-5.5: base $5 / $30, cached input $0.50
        pricing.insert(
            "gpt-5.5".to_string(),
            ModelPricing {
                input_cost_per_mtok: 5.0,
                output_cost_per_mtok: 30.0,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // GPT-5.4: base $2.50 / $15, cached input $0.25
        pricing.insert(
            "gpt-5.4".to_string(),
            ModelPricing {
                input_cost_per_mtok: 2.5,
                output_cost_per_mtok: 15.0,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.25,
            },
        );

        // GPT-4.1: base $2 / $8, cached input $0.50
        pricing.insert(
            "gpt-4.1".to_string(),
            ModelPricing {
                input_cost_per_mtok: 2.0,
                output_cost_per_mtok: 8.0,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // GPT-4.1-mini: base $0.40 / $1.60, cached input $0.10
        pricing.insert(
            "gpt-4.1-mini".to_string(),
            ModelPricing {
                input_cost_per_mtok: 0.40,
                output_cost_per_mtok: 1.60,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.10,
            },
        );

        // GPT-4.1-nano: base $0.10 / $0.40, cached input $0.025
        pricing.insert(
            "gpt-4.1-nano".to_string(),
            ModelPricing {
                input_cost_per_mtok: 0.10,
                output_cost_per_mtok: 0.40,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.025,
            },
        );

        // o3: base $2 / $8, cached input $0.50
        pricing.insert(
            "o3".to_string(),
            ModelPricing {
                input_cost_per_mtok: 2.0,
                output_cost_per_mtok: 8.0,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.50,
            },
        );

        // o3-pro: base $20 / $80
        pricing.insert(
            "o3-pro".to_string(),
            ModelPricing {
                input_cost_per_mtok: 20.0,
                output_cost_per_mtok: 80.0,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.0,
            },
        );

        // o3-mini: base $1.10 / $4.40, cached input $0.55
        pricing.insert(
            "o3-mini".to_string(),
            ModelPricing {
                input_cost_per_mtok: 1.10,
                output_cost_per_mtok: 4.40,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.55,
            },
        );

        // o4-mini: base $1.10 / $4.40, cached input $0.14
        pricing.insert(
            "o4-mini".to_string(),
            ModelPricing {
                input_cost_per_mtok: 1.10,
                output_cost_per_mtok: 4.40,
                cache_write_5m_cost_per_mtok: 0.0,
                cache_write_1h_cost_per_mtok: 0.0,
                cache_read_cost_per_mtok: 0.14,
            },
        );

        let mut sorted_keys: Vec<String> = pricing.keys().cloned().collect();
        sorted_keys.sort_by_key(|k| std::cmp::Reverse(k.len()));

        Self {
            pricing,
            sorted_keys,
        }
    }

    pub fn calculate_cost(&self, tokens: &TokenStats, model: Option<&str>) -> Option<f64> {
        let pricing = self.get_pricing(model)?;
        let million = 1_000_000.0;

        let input_cost = tokens.input_tokens as f64 / million * pricing.input_cost_per_mtok;
        let output_cost = tokens.output_tokens as f64 / million * pricing.output_cost_per_mtok;
        // Apply per-TTL rates from the structured `cache_creation` breakdown.
        // For entries that pre-date the structured field, the aggregator's
        // `TokenStats::add` routes all flat cache writes into the 5m bucket,
        // which matches Anthropic's API key default — so this branch is
        // already correct for legacy data.
        let cache_5m_cost =
            tokens.cache_creation_5m_tokens as f64 / million * pricing.cache_write_5m_cost_per_mtok;
        let cache_1h_cost =
            tokens.cache_creation_1h_tokens as f64 / million * pricing.cache_write_1h_cost_per_mtok;
        let cache_read_cost =
            tokens.cache_read_tokens as f64 / million * pricing.cache_read_cost_per_mtok;

        Some(input_cost + output_cost + cache_5m_cost + cache_1h_cost + cache_read_cost)
    }

    pub fn get_pricing_by_display_name(&self, display_name: &str) -> Option<&ModelPricing> {
        for key in self.pricing.keys() {
            if normalize_model_name(key) == display_name {
                return self.pricing.get(key);
            }
        }
        None
    }

    pub fn has_pricing(&self, model: Option<&str>) -> bool {
        self.get_pricing(model).is_some()
    }

    pub fn models_without_pricing(
        &self,
        model_tokens: &std::collections::HashMap<String, TokenStats>,
    ) -> std::collections::HashSet<String> {
        let mut result = std::collections::HashSet::new();
        for model in model_tokens.keys() {
            if !self.has_pricing(Some(model)) {
                result.insert(Self::simplify_model_name(model));
            }
        }
        result
    }

    pub fn calculate_costs_by_model(
        &self,
        model_tokens: &std::collections::HashMap<String, TokenStats>,
    ) -> Vec<(String, f64)> {
        let mut aggregated: HashMap<String, f64> = HashMap::new();

        for (model, tokens) in model_tokens {
            let simplified = Self::simplify_model_name(model);
            let cost = self.calculate_cost(tokens, Some(model)).unwrap_or(0.0);
            *aggregated.entry(simplified).or_insert(0.0) += cost;
        }

        let mut costs: Vec<(String, f64)> = aggregated.into_iter().collect();
        costs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        costs
    }

    pub fn aggregate_tokens_by_model(
        model_tokens: &std::collections::HashMap<String, TokenStats>,
    ) -> std::collections::HashMap<String, TokenStats> {
        let mut aggregated: HashMap<String, TokenStats> = HashMap::new();

        for (model, tokens) in model_tokens {
            aggregated
                .entry(Self::simplify_model_name(model))
                .or_default()
                .merge(tokens);
        }

        aggregated
    }

    /// Bucket name for a model in the cost / token-by-model views. Synthetic
    /// (`<...>`) and empty models collapse into one `Other` bucket (they
    /// can't be priced); everything else uses its normalized display name.
    /// `Other` is the unpriceable bucket, distinct from `normalize_model_name`'s
    /// `unknown` display fallback.
    fn simplify_model_name(model: &str) -> String {
        if model.starts_with('<') || model.is_empty() {
            return "Other".to_string();
        }
        normalize_model_name(model)
    }

    pub fn get_pricing(&self, model: Option<&str>) -> Option<&ModelPricing> {
        let model_name = model?;

        for key in &self.sorted_keys {
            if let Some(pos) = model_name.find(key.as_str()) {
                let after = &model_name[pos + key.len()..];
                if Self::is_version_boundary(after) {
                    return self.pricing.get(key);
                }
            }
        }

        None
    }

    fn is_version_boundary(after: &str) -> bool {
        if after.is_empty() {
            return true;
        }
        // Context-window suffix like `[1m]` (1M-context variant — Cowork emits
        // `claude-opus-4-7[1m]`) is appended directly with no `-` separator, so
        // accept it as a valid boundary. Also covers `(beta)` / `:thinking`
        // style qualifiers if Anthropic adds them later. Without this, the
        // model fails pricing lookup and cost silently rounds to $0.
        if matches!(after.chars().next(), Some('[' | '(' | ':')) {
            return true;
        }
        let Some(rest) = after.strip_prefix('-') else {
            return false;
        };
        let digit_count = rest.chars().take_while(char::is_ascii_digit).count();
        // 1-3 digits after '-' = version continuation (e.g. "-6", "-45")
        // 4+ digits = date suffix (e.g. "-20260101")
        // 0 digits = other suffix, treat as boundary
        digit_count == 0 || digit_count >= 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_models_have_pricing() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(Some("claude-sonnet-4")).is_some());
        assert!(calculator.get_pricing(Some("claude-opus-4-5")).is_some());
        assert!(calculator.get_pricing(Some("claude-opus-4-8")).is_some());
        assert!(calculator.get_pricing(Some("claude-fable-5")).is_some());
        assert!(calculator.get_pricing(Some("claude-mythos-5")).is_some());
        // Mythos Preview has no published rates — it must stay unpriced (the
        // UI marks it "$?") rather than borrowing Mythos 5's, and must not
        // substring-match the claude-mythos-5 entry.
        assert!(
            calculator
                .get_pricing(Some("claude-mythos-preview"))
                .is_none()
        );
    }

    #[test]
    fn test_normalize_fable_and_mythos_families() {
        assert_eq!(normalize_model_name("claude-fable-5"), "Fable 5");
        assert_eq!(normalize_model_name("claude-mythos-5"), "Mythos 5");
        // Version-less family id keeps its raw name — collapsing it to a
        // bare "Mythos" would sit indistinguishably next to "Mythos 5" while
        // carrying different (absent) pricing.
        assert_eq!(
            normalize_model_name("claude-mythos-preview"),
            "claude-mythos-preview"
        );
    }

    #[test]
    fn test_get_pricing_unknown_model() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(Some("unknown-model-xyz")).is_none());
    }

    #[test]
    fn test_get_pricing_context_window_suffix() {
        // Claude Code's 1M-context variant carries a `[1m]` suffix on the model
        // ID (e.g. Cowork audit.jsonl: `claude-opus-4-7[1m]`). Without explicit
        // boundary handling this would not match the base `claude-opus-4-7`
        // pricing entry and cost would silently fall through to $0.
        let calculator = CostCalculator::new();
        assert!(
            calculator
                .get_pricing(Some("claude-opus-4-7[1m]"))
                .is_some(),
            "[1m] context-window suffix should match the base opus-4-7 pricing"
        );
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-4-6[1m]"))
                .is_some(),
            "[1m] suffix should also work for other families"
        );
    }

    #[test]
    fn test_get_pricing_no_version_fallback() {
        let calculator = CostCalculator::new();
        // "claude-sonnet-5" is not defined; must NOT fall back to "claude-sonnet-4"
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-5-20270101"))
                .is_none(),
            "should not fall back to claude-sonnet-4"
        );
        // "claude-sonnet-4-6" with date suffix should match
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-4-6-20260101"))
                .is_some()
        );
        // "claude-sonnet-4" with date suffix should still match
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-4-20250514"))
                .is_some()
        );
    }

    #[test]
    fn test_get_pricing_none() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(None).is_none());
    }

    #[test]
    fn test_get_pricing_known_model() {
        let calculator = CostCalculator::new();
        let pricing = calculator
            .get_pricing(Some("claude-opus-4-5-20251101"))
            .unwrap();
        assert_eq!(pricing.input_cost_per_mtok, 5.0);
        assert_eq!(pricing.output_cost_per_mtok, 25.0);
    }

    #[test]
    fn test_calculate_cost_unknown_model_returns_none() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator.calculate_cost(&tokens, Some("unknown-model"));
        assert_eq!(cost, None);
    }

    #[test]
    fn test_calculate_cost_none_model_returns_none() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator.calculate_cost(&tokens, None);
        assert_eq!(cost, None);
    }

    #[test]
    fn test_calculate_cost_basic() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 100_000,
            output_tokens: 100_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4"))
            .unwrap();
        // 100K input @ $3/M = $0.30, 100K output @ $15/M = $1.50 → $1.80
        assert!((cost - 1.80).abs() < 0.01);
    }

    #[test]
    fn test_simplify_model_name() {
        assert_eq!(
            CostCalculator::simplify_model_name("claude-opus-4-5-20251101"),
            "Opus 4.5"
        );
        assert_eq!(
            CostCalculator::simplify_model_name("claude-sonnet-4-20250514"),
            "Sonnet 4"
        );
        assert_eq!(
            CostCalculator::simplify_model_name("claude-3-5-haiku-20241022"),
            "Haiku 3.5"
        );
    }

    #[test]
    fn test_calculate_cost_large_token_count() {
        let calculator = CostCalculator::new();
        // Flat rate regardless of token count (no tiered pricing)
        let tokens = TokenStats {
            input_tokens: 500_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4"))
            .unwrap();
        // 500K @ $3/M = $1.50
        assert!(
            (cost - 1.50).abs() < 0.01,
            "Expected ~$1.50, got ${cost:.4}"
        );
    }

    #[test]
    fn test_calculate_cost_with_cache_tokens() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 100_000,
            output_tokens: 0,
            cache_creation_tokens: 50_000,
            cache_read_tokens: 25_000,
            // 50K all 5m TTL — matches Anthropic's API key default.
            cache_creation_5m_tokens: 50_000,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4"))
            .unwrap();
        // input: 100K @ $3/M = $0.30 (input_tokens is already non-cached per API spec)
        // cache_write (5m): 50K @ $3.75/M = $0.1875
        // cache_read: 25K @ $0.30/M = $0.0075
        // Total: ~$0.495
        assert!(
            (cost - 0.495).abs() < 0.01,
            "Expected ~$0.495, got ${cost:.4}"
        );
    }

    #[test]
    fn test_calculate_cost_1h_cache_write_is_2x_base() {
        // Subscription users (Claude Pro/Max/Team via Claude Code) default
        // to 1h TTL. The 1h rate is 2x base input — `cache_write_1h` for
        // Sonnet 4.6 = $6/MTok (base $3 × 2).
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 1_000_000,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4-6"))
            .unwrap();
        assert!(
            (cost - 6.0).abs() < 0.01,
            "1h cache for Sonnet 4.6 should be $6/MTok, got ${cost:.4}"
        );
    }

    #[test]
    fn test_calculate_cost_mixed_5m_and_1h_cache_writes() {
        // Realistic Claude Code workload: most cache writes are 1h, a few
        // are 5m. The two buckets should be billed at their own rates and
        // summed independently — not merged at a single average rate.
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 1_100_000,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 100_000,
            cache_creation_1h_tokens: 1_000_000,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4-6"))
            .unwrap();
        // 5m: 100K @ $3.75/M = $0.375
        // 1h: 1M  @ $6.00/M = $6.000
        // Total: $6.375
        assert!(
            (cost - 6.375).abs() < 0.01,
            "mixed 5m+1h cache cost should be $6.375 for Sonnet 4.6, got ${cost:.4}"
        );
    }

    #[test]
    fn test_token_stats_add_routes_structured_cache_creation() {
        // Verify the aggregator splits 5m and 1h into separate fields when
        // the structured `cache_creation` object is present (modern JSONL).
        use crate::domain::{CacheCreationBreakdown, Usage};
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 30_000,
            cache_read_input_tokens: 0,
            cache_creation: Some(CacheCreationBreakdown {
                ephemeral_5m_input_tokens: 10_000,
                ephemeral_1h_input_tokens: 20_000,
            }),
            service_tier: None,
        };
        let mut stats = TokenStats::default();
        stats.add(&usage);
        assert_eq!(stats.cache_creation_5m_tokens, 10_000);
        assert_eq!(stats.cache_creation_1h_tokens, 20_000);
        assert_eq!(stats.cache_creation_tokens, 30_000);
    }

    #[test]
    fn test_token_stats_add_falls_back_to_flat_as_5m_when_no_structured() {
        // Legacy JSONL without the structured field: the entire flat
        // aggregate goes into the 5m bucket, matching Anthropic's API key
        // default TTL.
        use crate::domain::Usage;
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 0,
            cache_creation: None,
            service_tier: None,
        };
        let mut stats = TokenStats::default();
        stats.add(&usage);
        assert_eq!(stats.cache_creation_5m_tokens, 50_000);
        assert_eq!(stats.cache_creation_1h_tokens, 0);
        assert_eq!(stats.cache_creation_tokens, 50_000);
    }

    #[test]
    fn test_calculate_cost_opus() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 500_000,
            output_tokens: 100_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-opus-4-5"))
            .unwrap();
        // 500K @ $5/M = $2.50, 100K @ $25/M = $2.50 → total $5.00
        assert!(
            (cost - 5.00).abs() < 0.01,
            "Expected ~$5.00, got ${cost:.4}"
        );
    }

    #[test]
    fn test_simplify_model_name_unknown_keeps_raw() {
        // Unknown model families now keep their raw API name so the UI can list them
        // individually (and flag them as missing pricing). Avoids collapsing distinct
        // unknown models into a single opaque "Other" bucket.
        assert_eq!(
            CostCalculator::simplify_model_name("some-random-model"),
            "some-random-model"
        );
    }

    #[test]
    fn test_simplify_model_name_empty_and_placeholder() {
        // Empty / placeholder values still go to "Other" because they are not real models.
        assert_eq!(CostCalculator::simplify_model_name(""), "Other");
        assert_eq!(CostCalculator::simplify_model_name("<unknown>"), "Other");
    }

    #[test]
    fn test_normalize_model_name_future_versions() {
        assert_eq!(normalize_model_name("claude-opus-4-6-20260101"), "Opus 4.6");
        assert_eq!(normalize_model_name("claude-sonnet-5-20260101"), "Sonnet 5");
        assert_eq!(
            normalize_model_name("claude-haiku-5-1-20260101"),
            "Haiku 5.1"
        );
    }

    #[test]
    fn test_normalize_model_name_old_naming() {
        assert_eq!(
            normalize_model_name("claude-3-7-sonnet-20250219"),
            "Sonnet 3.7"
        );
        assert_eq!(normalize_model_name("claude-3-opus-20240229"), "Opus 3");
        assert_eq!(
            normalize_model_name("claude-3-5-haiku-20241022"),
            "Haiku 3.5"
        );
    }

    #[test]
    fn test_all_defined_models_have_pricing() {
        let calculator = CostCalculator::new();
        let expected_models = [
            "claude-fable-5",
            "claude-mythos-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-opus-4",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-sonnet-4",
            "claude-3-7-sonnet",
            "claude-3-5-sonnet",
            "claude-haiku-4-5",
            "claude-3-5-haiku",
            "claude-3-haiku",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.1-nano",
            "o3",
            "o3-pro",
            "o3-mini",
            "o4-mini",
        ];
        for model in expected_models {
            assert!(
                calculator.pricing.contains_key(model),
                "Missing pricing for: {model}"
            );
        }
    }

    #[test]
    fn test_openai_models_have_pricing() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(Some("gpt-5.5")).is_some());
        assert!(calculator.get_pricing(Some("gpt-4.1")).is_some());
        assert!(calculator.get_pricing(Some("gpt-4.1-mini")).is_some());
        assert!(calculator.get_pricing(Some("o3")).is_some());
        assert!(calculator.get_pricing(Some("o3-pro")).is_some());
        assert!(calculator.get_pricing(Some("o4-mini")).is_some());
    }

    #[test]
    fn test_openai_o3_does_not_match_o3_pro() {
        let calculator = CostCalculator::new();
        let o3 = calculator.get_pricing(Some("o3")).unwrap();
        let o3_pro = calculator.get_pricing(Some("o3-pro")).unwrap();
        assert!(
            (o3.input_cost_per_mtok - 2.0).abs() < 0.01,
            "o3 input should be $2/M"
        );
        assert!(
            (o3_pro.input_cost_per_mtok - 20.0).abs() < 0.01,
            "o3-pro input should be $20/M"
        );
    }

    #[test]
    fn test_normalize_openai_model_names() {
        assert_eq!(normalize_model_name("gpt-5.5"), "GPT-5.5");
        assert_eq!(normalize_model_name("gpt-4.1"), "GPT-4.1");
        assert_eq!(normalize_model_name("gpt-4.1-mini"), "GPT-4.1-mini");
        assert_eq!(normalize_model_name("gpt-4.1-nano"), "GPT-4.1-nano");
        assert_eq!(normalize_model_name("o3"), "o3");
        assert_eq!(normalize_model_name("o3-pro"), "o3-pro");
        assert_eq!(normalize_model_name("o4-mini"), "o4-mini");
    }

    #[test]
    fn test_calculate_cost_openai_gpt55() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 100_000,
            output_tokens: 100_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 50_000,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cost = calculator.calculate_cost(&tokens, Some("gpt-5.5")).unwrap();
        // 100K input @ $5/M = $0.50, 100K output @ $30/M = $3.00,
        // 50K cached @ $0.50/M = $0.025 → total $3.525
        assert!(
            (cost - 3.525).abs() < 0.01,
            "Expected ~$3.525, got ${cost:.4}"
        );
    }
}
