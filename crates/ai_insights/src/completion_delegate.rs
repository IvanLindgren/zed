//! Inline edit predictions (ghost text) from any configured language model provider.
//! Modeled on the Codestral delegate: one pending request, interpolation while
//! the user keeps typing, Tab accepts through the editor's standard flow.

use std::{ops::Range, sync::Arc, time::Duration};

use anyhow::Result;
use edit_prediction_types::{
    EditPrediction, EditPredictionDelegate, EditPredictionDiscardReason, EditPredictionIconSet,
    EditPredictionRequestTrigger,
};
use gpui::{App, Context, Entity, Task};
use icons::IconName;
use language::{Anchor, Buffer, BufferSnapshot, EditPreview};
use settings::Settings as _;
use text::{Point, ToPoint as _};

use crate::{
    AiInsightsSettings,
    model::{self, Purpose, TextRequest},
};

const LINES_BEFORE: u32 = 80;
const LINES_AFTER: u32 = 30;
const CURSOR_MARKER: &str = "<|CURSOR|>";

const SYSTEM_PROMPT: &str = "You are an inline code completion engine inside a code editor. \
The user message contains a fragment of a source file with the marker <|CURSOR|>. \
Reply with ONLY the exact text that should be inserted at the marker so the code continues naturally: \
finish the current line, or add the next few lines (at most ~12) if that is clearly what comes next. \
Never repeat text that already exists before or after the marker. \
No markdown fences, no explanations, no surrounding quotes. \
If no useful completion exists, reply with an empty message.";

#[derive(Clone)]
struct CurrentCompletion {
    snapshot: BufferSnapshot,
    edits: Arc<[(Range<Anchor>, Arc<str>)]>,
    edit_preview: EditPreview,
}

impl CurrentCompletion {
    fn interpolate(&self, new_snapshot: &BufferSnapshot) -> Option<Vec<(Range<Anchor>, Arc<str>)>> {
        edit_prediction_types::interpolate_edits(&self.snapshot, new_snapshot, &self.edits)
            .filter(|edits| !edits.is_empty())
    }
}

#[derive(Default)]
pub struct AiInsightsEditPredictionDelegate {
    pending_request: Option<Task<Result<()>>>,
    current_completion: Option<CurrentCompletion>,
}

impl AiInsightsEditPredictionDelegate {
    pub fn new() -> Self {
        Self::default()
    }
}

fn build_prompt(snapshot: &BufferSnapshot, cursor: Point, file_name: Option<String>) -> String {
    let start = Point::new(cursor.row.saturating_sub(LINES_BEFORE), 0);
    let end_row = (cursor.row + LINES_AFTER).min(snapshot.max_point().row);
    let end = Point::new(end_row, snapshot.line_len(end_row));
    let prefix: String = snapshot.text_for_range(start..cursor).collect();
    let suffix: String = snapshot.text_for_range(cursor..end).collect();
    let language = snapshot
        .language()
        .map(|language| language.name().to_string())
        .unwrap_or_else(|| "plain text".into());
    let file = file_name.unwrap_or_else(|| "untitled".into());
    format!("File: {file}\nLanguage: {language}\n\n{prefix}{CURSOR_MARKER}{suffix}")
}

/// Strips fences/markers and drops a tail that duplicates the text after the cursor.
fn clean_completion(raw: &str, suffix_after_cursor: &str) -> String {
    let mut text = raw.trim_end_matches(['\n', '\r', ' ']).to_string();
    if text.trim_start().starts_with("```") {
        let mut lines: Vec<&str> = text.lines().collect();
        lines.remove(0);
        if lines.last().is_some_and(|line| line.trim_start().starts_with("```")) {
            lines.pop();
        }
        text = lines.join("\n");
    }
    text = text.replace(CURSOR_MARKER, "");
    let rest_of_line = suffix_after_cursor.lines().next().unwrap_or("").trim();
    if !rest_of_line.is_empty() && text.trim_end().ends_with(rest_of_line) {
        let cut = text.trim_end().len() - rest_of_line.len();
        text.truncate(cut);
    }
    text
}

impl EditPredictionDelegate for AiInsightsEditPredictionDelegate {
    fn name() -> &'static str {
        "ai_insights"
    }

    fn display_name() -> &'static str {
        "AI Insights"
    }

    fn show_predictions_in_menu() -> bool {
        true
    }

    fn icons(&self, _cx: &App) -> EditPredictionIconSet {
        EditPredictionIconSet::new(IconName::Sparkle)
    }

    fn is_enabled(&self, _buffer: &Entity<Buffer>, _cursor_position: Anchor, cx: &App) -> bool {
        // The provider's API key may load lazily on the first request.
        model::is_configured(Purpose::Completion, cx)
    }

    fn is_refreshing(&self, _cx: &App) -> bool {
        self.pending_request.is_some()
    }

    fn refresh(
        &mut self,
        buffer: Entity<Buffer>,
        cursor_position: Anchor,
        debounce_duration: Duration,
        _trigger: EditPredictionRequestTrigger,
        cx: &mut Context<Self>,
    ) {
        let snapshot = buffer.read(cx).snapshot();
        if let Some(current) = self.current_completion.as_ref()
            && current.interpolate(&snapshot).is_some()
        {
            return;
        }

        let debounce = debounce_duration.max(AiInsightsSettings::get_global(cx).completion_debounce);
        let file_name = buffer
            .read(cx)
            .file()
            .map(|file| file.file_name(cx).to_string());

        // Replacing the task drops (cancels) any request still in flight.
        self.pending_request = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(debounce).await;

            let cursor = cursor_position.to_point(&snapshot);
            let prompt = build_prompt(&snapshot, cursor, file_name);
            let line_end = Point::new(cursor.row, snapshot.line_len(cursor.row));
            let rest_of_line: String = snapshot.text_for_range(cursor..line_end).collect();

            let request = TextRequest {
                system: SYSTEM_PROMPT.into(),
                user: prompt,
                stop: Vec::new(),
            };
            let result = cx
                .update(|cx| model::complete_text(Purpose::Completion, request, cx))
                .await;

            let completion = match result {
                Ok(raw) => clean_completion(&raw, &rest_of_line),
                Err(error) => {
                    log::warn!("ai_insights: edit prediction failed: {error:#}");
                    this.update(cx, |this, cx| {
                        this.pending_request = None;
                        cx.notify();
                    })?;
                    return Err(error);
                }
            };

            if completion.trim().is_empty() {
                this.update(cx, |this, cx| {
                    this.pending_request = None;
                    cx.notify();
                })?;
                return Ok(());
            }

            let edits: Arc<[(Range<Anchor>, Arc<str>)]> =
                vec![(cursor_position..cursor_position, completion.into())].into();
            let edit_preview = buffer
                .read_with(cx, |buffer, cx| buffer.preview_edits(edits.clone(), cx))
                .await;

            this.update(cx, |this, cx| {
                this.current_completion = Some(CurrentCompletion {
                    snapshot,
                    edits,
                    edit_preview,
                });
                this.pending_request = None;
                cx.notify();
            })?;
            Ok(())
        }));
    }

    fn accept(&mut self, _cx: &mut Context<Self>) {
        self.pending_request = None;
        self.current_completion = None;
    }

    fn discard(&mut self, _reason: EditPredictionDiscardReason, _cx: &mut Context<Self>) {
        self.pending_request = None;
        self.current_completion = None;
    }

    fn suggest(
        &mut self,
        buffer: &Entity<Buffer>,
        _cursor_position: Anchor,
        cx: &mut Context<Self>,
    ) -> Option<EditPrediction> {
        let current = self.current_completion.as_ref()?;
        let edits = current.interpolate(&buffer.read(cx).snapshot())?;
        Some(EditPrediction::Local {
            id: None,
            edits,
            cursor_position: None,
            edit_preview: Some(current.edit_preview.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::clean_completion;

    #[test]
    fn strips_fences_and_duplicate_suffix() {
        assert_eq!(clean_completion("```rust\nlet x = 1;\n```", ""), "let x = 1;");
        assert_eq!(clean_completion("foo(a, b);", ");"), "foo(a, b");
        assert_eq!(clean_completion("bar()", ""), "bar()");
    }
}
