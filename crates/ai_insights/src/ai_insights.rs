//! AI insights on top of any language model provider configured in Zed
//! (ChatGPT subscription by default; DeepSeek, OpenAI, Anthropic, Ollama, ...):
//! - edit predictions (ghost text) via [`AiInsightsEditPredictionDelegate`];
//! - a one-line description above each function / data structure;
//! - advice cards ("looks like you're implementing X…") above the current function.
//!
//! Configured under `"ai_insights"` in settings.json; models can also be picked
//! with `ai_insights: select completion model` / `select insights model`.

mod cache;
mod completion_delegate;
mod editor_insights;
mod insights_settings;
pub mod model;
mod model_picker;

use editor::Editor;
use gpui::{App, AppContext as _, Context};
use project::DisableAiSettings;
use settings::Settings as _;

pub use crate::completion_delegate::AiInsightsEditPredictionDelegate;
pub use crate::insights_settings::AiInsightsSettings;
use crate::editor_insights::{EditorInsights, InsightsAddon};

pub fn init(cx: &mut App) {
    cache::init(cx);
    model_picker::init(cx);

    cx.observe_new(|editor: &mut Editor, window, cx: &mut Context<Editor>| {
        if !editor.mode().is_full() || window.is_none() {
            return;
        }
        if DisableAiSettings::get_global(cx).disable_ai {
            return;
        }
        // Only real files: multibuffers (search results, diffs) are skipped.
        if editor.buffer().read(cx).as_singleton().is_none() {
            return;
        }

        let editor_entity = cx.entity();
        let insights = cx.new(|cx| EditorInsights::new(&editor_entity, cx));

        let cancel_target = insights.downgrade();
        let cancel_subscription =
            editor.register_action(move |_: &editor::actions::Cancel, _window, cx| {
                let handled = cancel_target
                    .update(cx, |insights, cx| {
                        if insights.has_advice_card() {
                            insights.dismiss_advice_card(true, cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !handled {
                    cx.propagate();
                }
            });
        insights.update(cx, |insights, _| insights.push_subscription(cancel_subscription));

        editor.register_addon(InsightsAddon { _insights: insights });
    })
    .detach();
}
