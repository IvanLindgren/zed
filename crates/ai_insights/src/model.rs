//! Model access through Zed's language model registry. Any provider configured
//! under LLM Providers works: the ChatGPT subscription (`openai-subscribed`),
//! DeepSeek, OpenAI, Anthropic, Google, OpenRouter, Ollama, LM Studio, ...
//! API keys and sign-in are owned by those providers, never by this crate.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt as _;
use gpui::{App, SharedString, Task};
use language_model::{
    LanguageModel, LanguageModelProvider, LanguageModelProviderId, LanguageModelRegistry,
    LanguageModelRequest, LanguageModelRequestMessage, Role,
};
use settings::Settings as _;

use crate::AiInsightsSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Edit predictions (ghost text).
    Completion,
    /// Symbol descriptions and advice cards.
    Insights,
}

/// What the settings ask for, before looking at the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    /// `None`: use the provider's default (fast model for completions).
    pub model: Option<String>,
    pub effort: String,
}

pub fn model_choice(purpose: Purpose, cx: &App) -> ModelChoice {
    choice_from_settings(purpose, AiInsightsSettings::get_global(cx))
}

fn choice_from_settings(purpose: Purpose, settings: &AiInsightsSettings) -> ModelChoice {
    match purpose {
        Purpose::Insights => ModelChoice {
            provider: settings.provider.clone(),
            model: Some(settings.model.clone()),
            effort: settings.effort.clone(),
        },
        Purpose::Completion => {
            let provider = settings
                .completion_provider
                .clone()
                .unwrap_or_else(|| settings.provider.clone());
            let model = settings.completion_model.clone().or_else(|| {
                (provider == settings.provider).then(|| settings.model.clone())
            });
            ModelChoice {
                provider,
                model,
                effort: settings.completion_effort.clone(),
            }
        }
    }
}

#[derive(Clone)]
pub struct ResolvedModel {
    pub model: LanguageModel,
    pub provider: Arc<dyn LanguageModelProvider>,
    pub effort: String,
}

fn provider_for(choice: &ModelChoice, cx: &App) -> Option<Arc<dyn LanguageModelProvider>> {
    let id = LanguageModelProviderId(SharedString::from(choice.provider.clone()));
    LanguageModelRegistry::read_global(cx).provider(&id)
}

/// Whether the configured provider exists (it may still need its API key loaded).
pub fn is_configured(purpose: Purpose, cx: &App) -> bool {
    provider_for(&model_choice(purpose, cx), cx).is_some()
}

/// The configured model if its provider is authenticated. Falls back to the
/// provider's fast/default model when the model id is not in its catalog.
pub fn resolve_model(purpose: Purpose, cx: &App) -> Option<ResolvedModel> {
    let choice = model_choice(purpose, cx);
    let provider = provider_for(&choice, cx)?;
    if !provider.is_authenticated(cx) {
        return None;
    }
    let found = choice.model.as_deref().and_then(|wanted| {
        provider
            .provided_models(cx)
            .into_iter()
            .find(|model| model.id.0.as_ref() == wanted)
    });
    let model = found.or_else(|| {
        if let Some(wanted) = &choice.model {
            log::debug!("ai_insights: model {wanted} not offered by {}, using a default", choice.provider);
        }
        match purpose {
            Purpose::Completion => provider
                .default_fast_model(cx)
                .or_else(|| provider.default_model(cx)),
            Purpose::Insights => provider.default_model(cx),
        }
    })?;
    Some(ResolvedModel {
        model,
        provider,
        effort: choice.effort,
    })
}

/// The model currently in effect, for showing a checkmark in the picker.
pub fn active_model(purpose: Purpose, cx: &App) -> Option<LanguageModel> {
    resolve_model(purpose, cx).map(|resolved| resolved.model)
}

pub struct TextRequest {
    pub system: String,
    pub user: String,
    pub stop: Vec<String>,
}

fn build_request(resolved: &ResolvedModel, request: TextRequest) -> LanguageModelRequest {
    let effort = resolved.effort.trim().to_ascii_lowercase();
    let wants_thinking = !effort.is_empty() && effort != "none";
    let model = &resolved.model;
    // Models without reasoning ignore effort; models that cannot turn reasoning
    // off keep it on with their default effort.
    let thinking_allowed =
        model.supports_thinking && (wants_thinking || !model.supports_disabling_thinking);
    LanguageModelRequest {
        thread_id: None,
        prompt_cache_key: None,
        prompt_id: None,
        intent: None,
        messages: vec![
            LanguageModelRequestMessage {
                role: Role::System,
                content: vec![request.system.into()],
                cache: false,
                reasoning_details: None,
            },
            LanguageModelRequestMessage {
                role: Role::User,
                content: vec![request.user.into()],
                cache: false,
                reasoning_details: None,
            },
        ],
        tools: Vec::new(),
        tool_choice: None,
        stop: request.stop,
        temperature: None,
        thinking_allowed,
        thinking_effort: (thinking_allowed && wants_thinking).then_some(effort),
        speed: None,
        compact_at_tokens: None,
        // Not every provider supports an output cap; prompts keep replies short instead.
        max_output_tokens: None,
    }
}

/// Sends one request with the model configured for `purpose` and returns the full
/// reply. Loads the provider's credentials first if that has not happened yet.
pub fn complete_text(purpose: Purpose, request: TextRequest, cx: &mut App) -> Task<Result<String>> {
    let choice = model_choice(purpose, cx);
    let Some(provider) = provider_for(&choice, cx) else {
        return Task::ready(Err(anyhow!(
            "ai_insights: unknown provider \"{}\" (see LLM Providers in Agent settings)",
            choice.provider
        )));
    };
    let authenticate = (!provider.is_authenticated(cx)).then(|| provider.authenticate(cx));

    cx.spawn(async move |cx| {
        if let Some(authenticate) = authenticate {
            authenticate.await.map_err(|error| {
                anyhow!(
                    "ai_insights: provider \"{}\" is not set up (API key or sign-in): {error}",
                    choice.provider
                )
            })?;
        }
        let resolved = cx
            .update(|cx| resolve_model(purpose, cx))
            .with_context(|| format!("ai_insights: no model available from \"{}\"", choice.provider))?;
        let lm_request = build_request(&resolved, request);
        let mut stream = resolved
            .provider
            .stream_completion_text(&resolved.model, lm_request, cx)
            .await
            .context("ai_insights: request failed")?;
        let mut text = String::new();
        while let Some(chunk) = stream.stream.next().await {
            text.push_str(&chunk.context("ai_insights: stream failed")?);
        }
        Ok(text)
    })
}

/// Pulls the first JSON object or array out of a model reply (tolerates ``` fences).
pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find(['{', '['])?;
    let close = if text.as_bytes()[start] == b'{' { '}' } else { ']' };
    let end = text.rfind(close)?;
    (end > start).then(|| &text[start..=end])
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ModelChoice, Purpose, choice_from_settings, extract_json};
    use crate::AiInsightsSettings;

    fn settings(completion_provider: Option<&str>, completion_model: Option<&str>) -> AiInsightsSettings {
        AiInsightsSettings {
            descriptions: true,
            advice: true,
            language: "ru".into(),
            provider: "openai-subscribed".into(),
            model: "gpt-6.1-terra".into(),
            effort: "medium".into(),
            completion_provider: completion_provider.map(Into::into),
            completion_model: completion_model.map(Into::into),
            completion_effort: "low".into(),
            completion_debounce: Duration::from_millis(500),
            advice_delay: Duration::from_millis(2000),
            max_file_bytes: 80_000,
        }
    }

    #[test]
    fn completion_inherits_provider_and_model() {
        assert_eq!(
            choice_from_settings(Purpose::Completion, &settings(None, None)),
            ModelChoice {
                provider: "openai-subscribed".into(),
                model: Some("gpt-6.1-terra".into()),
                effort: "low".into(),
            }
        );
    }

    #[test]
    fn other_completion_provider_uses_its_own_default_model() {
        let choice = choice_from_settings(Purpose::Completion, &settings(Some("deepseek"), None));
        assert_eq!(choice.provider, "deepseek");
        assert_eq!(choice.model, None);

        let choice = choice_from_settings(
            Purpose::Completion,
            &settings(Some("deepseek"), Some("deepseek-chat")),
        );
        assert_eq!(choice.model.as_deref(), Some("deepseek-chat"));
    }

    #[test]
    fn insights_ignore_completion_overrides() {
        let choice = choice_from_settings(Purpose::Insights, &settings(Some("deepseek"), Some("x")));
        assert_eq!(choice.provider, "openai-subscribed");
        assert_eq!(choice.model.as_deref(), Some("gpt-6.1-terra"));
        assert_eq!(choice.effort, "medium");
    }

    #[test]
    fn extracts_fenced_json() {
        assert_eq!(
            extract_json("```json\n{\"a\": 1}\n```"),
            Some("{\"a\": 1}")
        );
        assert_eq!(extract_json("no json"), None);
    }
}
