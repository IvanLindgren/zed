use std::time::Duration;

use settings::{RegisterSetting, Settings, SettingsContent};

/// Resolved `ai_insights` settings. Defaults live in `assets/settings/default.json`;
/// the fallbacks here only matter if that section is missing.
#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct AiInsightsSettings {
    pub descriptions: bool,
    pub advice: bool,
    pub language: String,
    /// Provider and model for descriptions and advice.
    pub provider: String,
    pub model: String,
    pub effort: String,
    /// Provider and model for edit predictions (`None` = inherit).
    pub completion_provider: Option<String>,
    pub completion_model: Option<String>,
    pub completion_effort: String,
    pub completion_debounce: Duration,
    pub advice_delay: Duration,
    pub max_file_bytes: usize,
}

pub const DEFAULT_PROVIDER: &str = "openai-subscribed";
pub const DEFAULT_MODEL: &str = "gpt-6.1-terra";

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl Settings for AiInsightsSettings {
    fn from_settings(content: &SettingsContent) -> Self {
        let c = content.ai_insights.clone().unwrap_or_default();
        Self {
            descriptions: c.descriptions.unwrap_or(true),
            advice: c.advice.unwrap_or(true),
            language: c.language.unwrap_or_else(|| "ru".into()),
            provider: non_empty(c.provider).unwrap_or_else(|| DEFAULT_PROVIDER.into()),
            model: non_empty(c.model).unwrap_or_else(|| DEFAULT_MODEL.into()),
            effort: c.effort.unwrap_or_else(|| "medium".into()),
            completion_provider: non_empty(c.completion_provider),
            completion_model: non_empty(c.completion_model),
            completion_effort: c.completion_effort.unwrap_or_else(|| "low".into()),
            completion_debounce: Duration::from_millis(c.completion_debounce_ms.unwrap_or(500)),
            advice_delay: Duration::from_millis(c.advice_delay_ms.unwrap_or(2000)),
            max_file_bytes: c.max_file_bytes.unwrap_or(80_000) as usize,
        }
    }
}

impl AiInsightsSettings {
    /// Human-readable language name for prompts ("ru" -> "Russian").
    pub fn language_name(&self) -> &str {
        match self.language.trim().to_ascii_lowercase().as_str() {
            "ru" | "rus" | "russian" | "русский" => "Russian",
            "en" | "eng" | "english" => "English",
            "uk" | "ukrainian" => "Ukrainian",
            "de" | "german" => "German",
            "es" | "spanish" => "Spanish",
            "fr" | "french" => "French",
            "zh" | "chinese" => "Chinese",
            "ja" | "japanese" => "Japanese",
            _ => self.language.as_str(),
        }
    }
}
