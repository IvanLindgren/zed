//! Access to the ChatGPT subscription through Zed's built-in `openai-subscribed`
//! language model provider. Nothing here handles tokens: the provider owns auth.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt as _;
use gpui::{App, Task};
use language_model::{
    LanguageModel, LanguageModelProvider, LanguageModelProviderId, LanguageModelRegistry,
    LanguageModelRequest, LanguageModelRequestMessage, Role,
};
use settings::Settings as _;

use crate::AiInsightsSettings;

pub const SUBSCRIPTION_PROVIDER_ID: LanguageModelProviderId =
    LanguageModelProviderId::new("openai-subscribed");

#[derive(Clone)]
pub struct ResolvedModel {
    pub model: LanguageModel,
    pub provider: Arc<dyn LanguageModelProvider>,
}

/// The configured subscription model (default `gpt-6.1-terra`), falling back to the
/// provider's fast model when that id is not in the catalog. `None` when signed out.
pub fn resolve_model(cx: &App) -> Option<ResolvedModel> {
    let provider = LanguageModelRegistry::read_global(cx).provider(&SUBSCRIPTION_PROVIDER_ID)?;
    if !provider.is_authenticated(cx) {
        return None;
    }
    let wanted = AiInsightsSettings::get_global(cx).model.clone();
    let model = provider
        .provided_models(cx)
        .into_iter()
        .find(|model| model.id.0.as_ref() == wanted.as_str())
        .or_else(|| {
            log::debug!("ai_insights: model {wanted} not in catalog, using the fast model");
            provider.default_fast_model(cx)
        })?;
    Some(ResolvedModel { model, provider })
}

pub struct TextRequest {
    pub system: String,
    pub user: String,
    /// "none", "low", "medium", "high", ...
    pub effort: String,
    pub max_output_tokens: Option<u64>,
    pub stop: Vec<String>,
}

/// Runs one non-streaming-to-the-caller request and returns the full text.
pub fn complete_text(request: TextRequest, cx: &mut App) -> Task<Result<String>> {
    let Some(resolved) = resolve_model(cx) else {
        return Task::ready(Err(anyhow!(
            "ChatGPT subscription is not signed in (openai-subscribed provider)"
        )));
    };
    let effort = request.effort.trim().to_ascii_lowercase();
    let thinking_allowed = !effort.is_empty() && effort != "none";
    let lm_request = LanguageModelRequest {
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
        thinking_effort: thinking_allowed.then_some(effort),
        speed: None,
        compact_at_tokens: None,
        max_output_tokens: request.max_output_tokens,
    };

    cx.spawn(async move |cx| {
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
    use super::extract_json;

    #[test]
    fn extracts_fenced_json() {
        assert_eq!(
            extract_json("```json\n{\"a\": 1}\n```"),
            Some("{\"a\": 1}")
        );
        assert_eq!(extract_json("no json"), None);
    }
}
