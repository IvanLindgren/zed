//! Per-editor state: one-line descriptions above symbols and advice cards
//! below the cursor line. Attached to every full-mode editor as an addon.

use std::{
    collections::HashSet as StdHashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use collections::{HashMap, HashSet};
use editor::{
    Editor, EditorEvent,
    display_map::{BlockContext, BlockPlacement, BlockProperties, BlockStyle, CustomBlockId, RenderBlock},
};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, MouseButton, SharedString, Subscription,
    Task, WeakEntity,
};
use language::{Anchor as TextAnchor, BufferSnapshot};
use serde::Deserialize;
use settings::{Settings as _, SettingsStore};
use text::{Point, ToPoint as _};
use ui::{IconButton, IconName, IconSize, Label, LabelSize, div, h_flex, prelude::*, v_flex};

use crate::{
    AiInsightsSettings, cache,
    model::{self, Purpose, TextRequest},
};

const DESCRIPTION_DEBOUNCE: Duration = Duration::from_millis(1500);
const DESCRIPTION_BATCH: usize = 60;
const ADVICE_MIN_INTERVAL: Duration = Duration::from_secs(20);
const ADVICE_LINES_BEFORE: u32 = 60;
const ADVICE_LINES_AFTER: u32 = 15;
const ADVICE_MAX_DISTANCE_ROWS: u32 = 25;
const ADVICE_CHARS_PER_LINE: usize = 95;
/// The card goes above the enclosing function when its header is at most this far up.
const ADVICE_MAX_LIFT_ROWS: u32 = 15;
/// While an edit prediction is on screen the card waits instead of competing with it.
const ADVICE_RETRY_DELAY: Duration = Duration::from_millis(1500);
const ADVICE_MAX_RETRIES: u32 = 20;

/// Keeps [`EditorInsights`] alive for exactly as long as its editor.
pub struct InsightsAddon {
    pub _insights: Entity<EditorInsights>,
}

impl editor::Addon for InsightsAddon {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct DescribedSymbol {
    anchor: TextAnchor,
    indent: u32,
    text: Arc<str>,
}

struct SymbolToDescribe {
    anchor: TextAnchor,
    indent: u32,
    header: String,
    line: u32,
    key: u64,
}

#[derive(Default)]
struct DescriptionsState {
    task: Option<Task<Result<()>>>,
    /// (buffer row, description) of what is currently rendered, to skip no-op rebuilds.
    rendered: Vec<(u32, Arc<str>)>,
    blocks: Vec<CustomBlockId>,
}

struct AdviceCard {
    block_id: CustomBlockId,
    row: u32,
    key: String,
}

#[derive(Default)]
struct AdviceState {
    task: Option<Task<Result<()>>>,
    card: Option<AdviceCard>,
    dismissed: StdHashSet<String>,
    last_context_hash: Option<u64>,
    last_request_at: Option<Instant>,
}

#[derive(Clone, Deserialize)]
struct AdviceReply {
    #[serde(default)]
    show: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    tips: Vec<String>,
}

pub struct EditorInsights {
    editor: WeakEntity<Editor>,
    descriptions: DescriptionsState,
    advice: AdviceState,
    settings: AiInsightsSettings,
    _subscriptions: Vec<Subscription>,
}

impl EditorInsights {
    pub fn new(editor: &Entity<Editor>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(editor, Self::on_editor_event),
            cx.observe_global::<SettingsStore>(Self::on_settings_changed),
        ];
        let mut this = Self {
            editor: editor.downgrade(),
            descriptions: DescriptionsState::default(),
            advice: AdviceState::default(),
            settings: AiInsightsSettings::get_global(cx).clone(),
            _subscriptions: subscriptions,
        };
        this.schedule_descriptions(Duration::from_millis(300), cx);
        this
    }

    pub fn push_subscription(&mut self, subscription: Subscription) {
        self._subscriptions.push(subscription);
    }

    pub fn has_advice_card(&self) -> bool {
        self.advice.card.is_some()
    }

    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let new = AiInsightsSettings::get_global(cx).clone();
        if new == self.settings {
            return;
        }
        let language_changed = new.language != self.settings.language;
        self.settings = new;
        if !self.settings.advice {
            self.dismiss_advice_card(false, cx);
        }
        if language_changed {
            self.descriptions.rendered.clear();
        }
        self.schedule_descriptions(Duration::ZERO, cx);
    }

    fn on_editor_event(&mut self, editor: Entity<Editor>, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::Reparsed(_) | EditorEvent::BufferEdited => {
                self.schedule_descriptions(DESCRIPTION_DEBOUNCE, cx);
            }
            EditorEvent::Saved => self.schedule_descriptions(Duration::ZERO, cx),
            _ => {}
        }
        match event {
            EditorEvent::Edited { .. } => self.schedule_advice(cx),
            EditorEvent::SelectionsChanged { .. } => {
                if let Some(card) = &self.advice.card {
                    let row = cursor_row(&editor, cx);
                    if row.abs_diff(card.row) > ADVICE_MAX_DISTANCE_ROWS {
                        self.dismiss_advice_card(false, cx);
                    }
                }
            }
            _ => {}
        }
    }

    fn singleton_snapshot(&self, cx: &App) -> Option<(BufferSnapshot, Option<String>)> {
        let editor = self.editor.upgrade()?;
        let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let buffer = buffer.read(cx);
        let file_name = buffer.file().map(|file| file.file_name(cx).to_string());
        Some((buffer.snapshot(), file_name))
    }

    // ---------------------------------------------------------------- descriptions

    fn schedule_descriptions(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if !self.settings.descriptions {
            self.descriptions.task = None;
            self.render_descriptions(Vec::new(), cx);
            return;
        }
        self.descriptions.task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Some((snapshot, file_name, settings)) = this.update(cx, |this, cx| {
                this.singleton_snapshot(cx)
                    .map(|(snapshot, name)| (snapshot, name, this.settings.clone()))
            })?
            else {
                return Ok(());
            };
            if snapshot.len() > settings.max_file_bytes || snapshot.language().is_none() {
                this.update(cx, |this, cx| this.render_descriptions(Vec::new(), cx))?;
                return Ok(());
            }

            let language_setting = settings.language.clone();
            let (symbols, source) = cx
                .background_spawn({
                    let snapshot = snapshot.clone();
                    async move { collect_symbols(&snapshot, &language_setting) }
                })
                .await;
            if symbols.is_empty() {
                this.update(cx, |this, cx| this.render_descriptions(Vec::new(), cx))?;
                return Ok(());
            }

            let missing: Vec<&SymbolToDescribe> = cx.update(|cx| {
                symbols
                    .iter()
                    .filter(|symbol| cache::get(symbol.key, cx).is_none())
                    .collect()
            });

            for batch in missing.chunks(DESCRIPTION_BATCH) {
                let request = descriptions_request(batch, &source, file_name.as_deref(), &snapshot, &settings);
                let reply = cx
                    .update(|cx| model::complete_text(Purpose::Insights, request, cx))
                    .await;
                let reply = match reply {
                    Ok(reply) => reply,
                    Err(error) => {
                        log::warn!("ai_insights: descriptions failed: {error:#}");
                        break;
                    }
                };
                let parsed: HashMap<String, String> = model::extract_json(&reply)
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                let entries: Vec<(u64, Arc<str>)> = batch
                    .iter()
                    .enumerate()
                    .map(|(index, symbol)| {
                        let text = parsed
                            .get(&(index + 1).to_string())
                            .map(|text| text.trim().trim_matches('"').to_string())
                            .unwrap_or_default();
                        (symbol.key, Arc::from(text))
                    })
                    .collect();
                cx.update(|cx| cache::insert_many(entries, cx));
                // Show results batch by batch on large files.
                let described = cx.update(|cx| described_symbols(&symbols, cx));
                this.update(cx, |this, cx| this.render_descriptions(described, cx))?;
            }

            let described = cx.update(|cx| described_symbols(&symbols, cx));
            this.update(cx, |this, cx| this.render_descriptions(described, cx))?;
            Ok(())
        }));
    }

    fn render_descriptions(&mut self, symbols: Vec<DescribedSymbol>, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let Some((snapshot, _)) = self.singleton_snapshot(cx) else {
            return;
        };
        let wanted: Vec<(u32, Arc<str>)> = symbols
            .iter()
            .map(|symbol| (symbol.anchor.to_point(&snapshot).row, symbol.text.clone()))
            .collect();
        if wanted == self.descriptions.rendered {
            return;
        }

        let old_blocks: HashSet<CustomBlockId> = self.descriptions.blocks.drain(..).collect();
        let new_blocks = editor.update(cx, |editor, cx| {
            if !old_blocks.is_empty() {
                editor.remove_blocks(old_blocks, None, cx);
            }
            let multibuffer = editor.buffer().read(cx).snapshot(cx);
            let props: Vec<BlockProperties<editor::Anchor>> = symbols
                .iter()
                .zip(&wanted)
                .map(|(symbol, (row, _))| BlockProperties {
                    placement: BlockPlacement::Above(
                        multibuffer.anchor_before(Point::new(*row, 0)),
                    ),
                    height: Some(1),
                    style: BlockStyle::Spacer,
                    render: description_renderer(symbol.text.clone(), symbol.indent),
                    priority: 0,
                })
                .collect();
            if props.is_empty() {
                Vec::new()
            } else {
                editor.insert_blocks(props, None, cx)
            }
        });
        self.descriptions.blocks = new_blocks;
        self.descriptions.rendered = wanted;
    }

    // ---------------------------------------------------------------- advice

    fn schedule_advice(&mut self, cx: &mut Context<Self>) {
        if !self.settings.advice {
            return;
        }
        let delay = self.settings.advice_delay;
        self.advice.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Some((request, context_hash, row)) = this.update(cx, |this, cx| this.advice_request(cx))? else {
                return Ok(());
            };
            this.update(cx, |this, _| {
                this.advice.last_context_hash = Some(context_hash);
                this.advice.last_request_at = Some(Instant::now());
            })?;

            let reply = cx
                .update(|cx| model::complete_text(Purpose::Insights, request, cx))
                .await;
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    log::warn!("ai_insights: advice failed: {error:#}");
                    return Ok(());
                }
            };
            let Some(advice) = model::extract_json(&reply)
                .and_then(|json| serde_json::from_str::<AdviceReply>(json).ok())
            else {
                return Ok(());
            };
            this.update(cx, |this, cx| this.show_advice(advice, row, cx))?;
            Ok(())
        }));
    }

    fn advice_request(&mut self, cx: &mut Context<Self>) -> Option<(TextRequest, u64, u32)> {
        if self
            .advice
            .last_request_at
            .is_some_and(|at| at.elapsed() < ADVICE_MIN_INTERVAL)
        {
            return None;
        }
        let editor = self.editor.upgrade()?;
        let (snapshot, file_name) = self.singleton_snapshot(cx)?;
        snapshot.language()?;
        let row = cursor_row(&editor, cx);
        let cursor = Point::new(row, snapshot.line_len(row));
        let start = Point::new(row.saturating_sub(ADVICE_LINES_BEFORE), 0);
        let end_row = (row + ADVICE_LINES_AFTER).min(snapshot.max_point().row);
        let end = Point::new(end_row, snapshot.line_len(end_row));
        let before: String = snapshot.text_for_range(start..cursor).collect();
        let after: String = snapshot.text_for_range(cursor..end).collect();
        if before.trim().len() < 40 {
            return None;
        }
        let context_hash = cache::stable_hash(&[&before, &after]);
        if self.advice.last_context_hash == Some(context_hash) {
            return None;
        }

        let language = snapshot
            .language()
            .map(|language| language.name().to_string())
            .unwrap_or_default();
        let lang = self.settings.language_name();
        let mut dismissed: Vec<&str> = self.advice.dismissed.iter().map(String::as_str).collect();
        if let Some(card) = &self.advice.card {
            dismissed.push(&card.key);
        }
        let system = format!(
            "You are a senior engineer pairing with a developer inside their code editor. \
Look at the code around <|CURSOR|>. Decide whether the developer is clearly in the middle of \
implementing a recognizable algorithm, data structure, protocol or design pattern \
(for example: LRU cache, Dijkstra, binary heap, trie, rate limiter, observer, state machine, retry with backoff). \
If it is not clearly recognizable, or the code is trivial, reply exactly {{\"show\": false}}. \
Otherwise reply with JSON only: {{\"show\": true, \"key\": \"<short english id, e.g. lru-cache>\", \
\"title\": \"<one short sentence in {lang}, e.g. the equivalent of 'Looks like you are implementing an LRU cache'>\", \
\"tips\": [\"<2-4 concise, concrete, actionable tips in {lang}: pitfalls, edge cases, complexity, a better structure>\"]}}. \
Do not restate the code. No markdown."
        );
        let mut user = format!(
            "File: {}\nLanguage: {language}\n",
            file_name.unwrap_or_else(|| "untitled".into())
        );
        if !dismissed.is_empty() {
            user.push_str(&format!(
                "Already shown or dismissed (do not repeat these keys): {}\n",
                dismissed.join(", ")
            ));
        }
        user.push_str(&format!("\n{before}<|CURSOR|>{after}"));

        Some((
            TextRequest {
                system,
                user,
                stop: Vec::new(),
            },
            context_hash,
            row,
        ))
    }

    fn show_advice(&mut self, advice: AdviceReply, row: u32, cx: &mut Context<Self>) {
        self.try_show_advice(advice, row, 0, cx);
    }

    /// Row the card is placed above: the header of the enclosing function or type
    /// when it is close, otherwise the cursor line itself. Never below the cursor,
    /// where edit predictions render their ghost text.
    fn advice_anchor_row(&self, cursor_row: u32, cx: &App) -> u32 {
        let Some((snapshot, _)) = self.singleton_snapshot(cx) else {
            return cursor_row;
        };
        let offset = snapshot.point_to_offset(Point::new(cursor_row, 0));
        snapshot
            .outline_items_containing(offset..offset, false, None)
            .iter()
            .map(|item| item.range.start.to_point(&snapshot).row)
            .filter(|start| *start <= cursor_row && cursor_row - start <= ADVICE_MAX_LIFT_ROWS)
            .max()
            .unwrap_or(cursor_row)
    }

    fn try_show_advice(&mut self, advice: AdviceReply, row: u32, attempt: u32, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        if editor.read(cx).has_active_edit_prediction() {
            if attempt < ADVICE_MAX_RETRIES {
                self.advice.task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(ADVICE_RETRY_DELAY).await;
                    this.update(cx, |this, cx| this.try_show_advice(advice, row, attempt + 1, cx))?;
                    Ok(())
                }));
            }
            return;
        }

        let key = advice.key.trim().to_ascii_lowercase();
        let tips: Vec<SharedString> = advice
            .tips
            .into_iter()
            .map(|tip| tip.trim().to_string())
            .filter(|tip| !tip.is_empty())
            .take(4)
            .map(SharedString::from)
            .collect();
        if !advice.show
            || advice.title.trim().is_empty()
            || tips.is_empty()
            || self.advice.dismissed.contains(&key)
            || self.advice.card.as_ref().is_some_and(|card| card.key == key)
        {
            return;
        }
        self.dismiss_advice_card(false, cx);
        let anchor_row = self.advice_anchor_row(row, cx);

        let title = SharedString::from(advice.title.trim().to_string());
        let wrapped_lines: usize = tips
            .iter()
            .map(|tip| tip.chars().count().div_ceil(ADVICE_CHARS_PER_LINE).max(1))
            .sum();
        let height = (2 + wrapped_lines) as u32;
        let insights = cx.entity().downgrade();
        let block_id = editor.update(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx).snapshot(cx);
            let anchor_row = anchor_row.min(multibuffer.max_point().row);
            let indent = multibuffer.indent_size_for_line(multi_buffer_row(anchor_row)).len;
            let anchor = multibuffer.anchor_before(Point::new(anchor_row, 0));
            editor
                .insert_blocks(
                    [BlockProperties {
                        placement: BlockPlacement::Above(anchor),
                        height: Some(height),
                        style: BlockStyle::Flex,
                        render: advice_renderer(title, tips, indent, insights),
                        priority: 0,
                    }],
                    None,
                    cx,
                )
                .pop()
        });
        if let Some(block_id) = block_id {
            self.advice.card = Some(AdviceCard { block_id, row, key });
        }
    }

    /// Removes the advice card. With `remember`, the same advice is never shown again
    /// in this editor (Esc or the close button).
    pub fn dismiss_advice_card(&mut self, remember: bool, cx: &mut Context<Self>) {
        let Some(card) = self.advice.card.take() else {
            return;
        };
        if remember {
            self.advice.dismissed.insert(card.key.clone());
        }
        if let Some(editor) = self.editor.upgrade() {
            editor.update(cx, |editor, cx| {
                editor.remove_blocks(HashSet::from_iter([card.block_id]), None, cx);
            });
        }
    }
}

fn multi_buffer_row(row: u32) -> multi_buffer::MultiBufferRow {
    multi_buffer::MultiBufferRow(row)
}

fn cursor_row(editor: &Entity<Editor>, cx: &App) -> u32 {
    let editor = editor.read(cx);
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let head = editor.selections.newest_anchor().head();
    multi_buffer::ToPoint::to_point(&head, &snapshot).row
}

/// Outline items worth describing, plus the source text sent as context.
fn collect_symbols(snapshot: &BufferSnapshot, language_setting: &str) -> (Vec<SymbolToDescribe>, String) {
    const SKIP_PREFIXES: &[&str] = &[
        "impl ", "mod ", "namespace ", "use ", "import ", "package ", "let ", "const ", "static ",
        "var ", "#", "extern ", "region ",
    ];
    let outline = snapshot.outline(None);
    let language = snapshot
        .language()
        .map(|language| language.name().to_string())
        .unwrap_or_default();
    let mut seen_rows = StdHashSet::new();
    let mut symbols = Vec::new();
    for item in &outline.items {
        if item.depth > 2 {
            continue;
        }
        let header = item.text.trim();
        if header.is_empty() || SKIP_PREFIXES.iter().any(|prefix| header.starts_with(prefix)) {
            continue;
        }
        let start = item.range.start.to_point(snapshot);
        let end = item.range.end.to_point(snapshot);
        if item.body_range.is_none() && end.row == start.row {
            continue;
        }
        if !seen_rows.insert(start.row) {
            continue;
        }
        let source: String = snapshot.text_for_range(item.range.clone()).collect();
        let key = cache::stable_hash(&["v1", &language, language_setting, &source]);
        symbols.push(SymbolToDescribe {
            anchor: item.range.start,
            indent: snapshot.indent_size_for_line(start.row).len,
            header: header.to_string(),
            line: start.row + 1,
            key,
        });
    }
    (symbols, snapshot.text())
}

fn described_symbols(symbols: &[SymbolToDescribe], cx: &App) -> Vec<DescribedSymbol> {
    symbols
        .iter()
        .filter_map(|symbol| {
            let text = cache::get(symbol.key, cx)?;
            (!text.is_empty()).then(|| DescribedSymbol {
                anchor: symbol.anchor,
                indent: symbol.indent,
                text,
            })
        })
        .collect()
}

fn descriptions_request(
    batch: &[&SymbolToDescribe],
    source: &str,
    file_name: Option<&str>,
    snapshot: &BufferSnapshot,
    settings: &AiInsightsSettings,
) -> TextRequest {
    let lang = settings.language_name();
    let language = snapshot
        .language()
        .map(|language| language.name().to_string())
        .unwrap_or_default();
    let system = format!(
        "You annotate source code for a developer who just opened a file. \
For each numbered symbol write ONE short line in {lang} (at most ~12 words) saying what it does or what it represents. \
Be concrete and specific to this code; do not start with filler like 'This function'. \
Reply with a JSON object only, mapping the symbol number (as a string) to its description. \
Use an empty string for symbols that are trivial (getters, obvious one-liners) or not worth describing."
    );
    let mut user = format!(
        "File: {}\nLanguage: {language}\n\nSymbols:\n",
        file_name.unwrap_or("untitled")
    );
    for (index, symbol) in batch.iter().enumerate() {
        user.push_str(&format!("{}. {} (line {})\n", index + 1, symbol.header, symbol.line));
    }
    user.push_str("\nSource:\n");
    user.push_str(source);
    TextRequest {
        system,
        user,
        stop: Vec::new(),
    }
}

fn muted_color(cx: &BlockContext) -> gpui::Hsla {
    let colors = cx.app.theme().colors();
    colors.editor_code_lens_foreground.unwrap_or(colors.text_muted)
}

fn description_renderer(text: Arc<str>, indent: u32) -> RenderBlock {
    let text = SharedString::from(text.to_string());
    Arc::new(move |cx: &mut BlockContext| -> AnyElement {
        let style = &cx.editor_style.text;
        let font = style.font();
        let font_size = style.font_size.to_pixels(cx.window.rem_size()) * 0.9;
        div()
            .id(cx.block_id)
            .pl(cx.em_width * indent as f32)
            .h_full()
            .flex()
            .flex_row()
            .items_end()
            .child(
                div()
                    .font(font)
                    .text_size(font_size)
                    .text_color(muted_color(cx))
                    .child(text.clone()),
            )
            .into_any_element()
    })
}

fn advice_renderer(
    title: SharedString,
    tips: Vec<SharedString>,
    indent: u32,
    insights: WeakEntity<EditorInsights>,
) -> RenderBlock {
    Arc::new(move |cx: &mut BlockContext| -> AnyElement {
        let colors = cx.app.theme().colors();
        let insights = insights.clone();
        div()
            .id(cx.block_id)
            .pl(cx.margins.gutter.full_width() + cx.em_width * indent as f32)
            .pr_4()
            .py_0p5()
            .size_full()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .size_full()
                    .max_w(cx.max_width * 0.8)
                    .px_2()
                    .py_0p5()
                    .gap_0p5()
                    .rounded_sm()
                    .border_l_2()
                    .border_color(colors.border_focused)
                    .bg(colors.editor_subheader_background)
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(Label::new(title.clone()).size(LabelSize::Small))
                            .child(
                                IconButton::new("ai-insights-dismiss", IconName::Close)
                                    .icon_size(IconSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        insights
                                            .update(cx, |this, cx| this.dismiss_advice_card(true, cx))
                                            .ok();
                                    }),
                            ),
                    )
                    .children(tips.iter().map(|tip| {
                        Label::new(format!("• {tip}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                    })),
            )
            .into_any_element()
    })
}
