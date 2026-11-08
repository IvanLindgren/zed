//! Model picker for AI Insights: the agent's language model selector shown as a
//! modal, writing the choice to `ai_insights` in settings.json. Lists every model
//! from providers configured under LLM Providers (DeepSeek, OpenAI, Anthropic, ...).

use agent_ui::language_model_selector::{LanguageModelSelector, language_model_selector};
use fs::Fs;
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, Subscription, Window,
};
use language_model::LanguageModel;
use settings::update_settings_file;
use ui::{prelude::*, v_flex};
use workspace::{ModalView, Workspace};
use zed_actions::ai_insights::{SelectCompletionModel, SelectInsightsModel};

use crate::model::{self, Purpose};

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &SelectCompletionModel, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                ModelPickerModal::new(Purpose::Completion, window, cx)
            });
        });
        workspace.register_action(|workspace, _: &SelectInsightsModel, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                ModelPickerModal::new(Purpose::Insights, window, cx)
            });
        });
    })
    .detach();
}

pub struct ModelPickerModal {
    picker: Entity<LanguageModelSelector>,
    _subscription: Subscription,
}

impl ModelPickerModal {
    fn new(purpose: Purpose, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let picker = cx.new(|cx| {
            language_model_selector(
                move |cx| model::active_model(purpose, cx),
                move |model, cx| save_choice(purpose, &model, cx),
                |_model, _favorite, _cx| {},
                true,
                focus_handle,
                window,
                cx,
            )
        });
        let subscription = cx.subscribe(&picker, |_, _, _: &DismissEvent, cx| {
            cx.emit(DismissEvent);
        });
        Self {
            picker,
            _subscription: subscription,
        }
    }
}

fn save_choice(purpose: Purpose, model: &LanguageModel, cx: &mut App) {
    let provider = model.provider_id().0.to_string();
    let model_id = model.id().0.to_string();
    let fs = <dyn Fs>::global(cx);
    update_settings_file(fs, cx, move |settings, _| {
        let insights = settings.ai_insights.get_or_insert_default();
        match purpose {
            Purpose::Completion => {
                insights.completion_provider = Some(provider);
                insights.completion_model = Some(model_id);
            }
            Purpose::Insights => {
                insights.provider = Some(provider);
                insights.model = Some(model_id);
            }
        }
    });
}

impl EventEmitter<DismissEvent> for ModelPickerModal {}

impl Focusable for ModelPickerModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl ModalView for ModelPickerModal {}

impl Render for ModelPickerModal {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("AiInsightsModelPicker")
            .w(rems(28.))
            .child(self.picker.clone())
    }
}
