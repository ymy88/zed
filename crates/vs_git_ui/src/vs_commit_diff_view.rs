use buffer_diff::BufferDiff;
use editor::{Editor, EditorEvent, MultiBuffer};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, Styled as _, Subscription, Task, Window,
};
use language::HighlightedText;
use project::ProjectPath;
use std::any::TypeId;
use std::sync::Arc;
use theme::ActiveTheme;
use ui::{Color, Icon, IconName, Label, LabelCommon as _, prelude::*};
use workspace::{
    Item, ToolbarItemLocation,
    item::{ItemEvent, TabContentParams},
};

pub struct VsCommitDiffView {
    lhs_editor: Entity<Editor>,
    rhs_editor: Entity<Editor>,
    pub(crate) full_path: SharedString,
    filename: SharedString,
    pub(crate) sha_short: SharedString,
    pub(crate) project_path: Option<ProjectPath>,
    focus_handle: FocusHandle,
    syncing_scroll: bool,
    left_ratio: f32,
    _subscriptions: Vec<Subscription>,
    _setup_task: Task<()>,
}

impl VsCommitDiffView {
    pub fn new(
        old_text: String,
        new_text: String,
        full_path: SharedString,
        filename: SharedString,
        sha_short: SharedString,
        project_path: Option<ProjectPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // LHS: old text (read-only)
        let lhs_buffer = cx.new(|cx| {
            let mut buffer = language::Buffer::local(&old_text, cx);
            buffer.set_capability(language::Capability::ReadOnly, cx);
            buffer
        });
        let lhs_multibuffer = cx.new(|cx| MultiBuffer::singleton(lhs_buffer, cx));
        let lhs_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(lhs_multibuffer, None, window, cx);
            editor.disable_diagnostics(cx);
            editor.set_show_vertical_scrollbar(false, cx);
            editor.set_show_breakpoints(false, cx);
            editor
        });

        // RHS: new text (read-only, with diff highlighting)
        let rhs_buffer = cx.new(|cx| {
            let mut buffer = language::Buffer::local(&new_text, cx);
            buffer.set_capability(language::Capability::ReadOnly, cx);
            buffer
        });
        let rhs_multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::singleton(rhs_buffer, cx);
            multibuffer.set_show_deleted_hunks(false, cx);
            multibuffer
        });
        let rhs_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(rhs_multibuffer, None, window, cx);
            editor.start_temporary_diff_override();
            editor.disable_diagnostics(cx);
            editor.set_show_breakpoints(false, cx);
            // Hide default hunk controls
            editor.set_render_diff_hunk_controls(
                Arc::new(|_, _, _, _, _, _, _, _| gpui::Empty.into_any_element()),
                cx,
            );
            editor
        });

        let mut subscriptions = Vec::new();

        subscriptions.push(cx.subscribe_in(
            &rhs_editor,
            window,
            |this: &mut Self, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    if !this.syncing_scroll {
                        this.syncing_scroll = true;
                        let pos = this.rhs_editor.update(cx, |editor, cx| {
                            editor.scroll_position(cx)
                        });
                        this.lhs_editor.update(cx, |editor, cx| {
                            editor.set_scroll_position(pos, window, cx);
                        });
                        this.syncing_scroll = false;
                    }
                }
            },
        ));

        subscriptions.push(cx.subscribe_in(
            &lhs_editor,
            window,
            |this: &mut Self, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    if !this.syncing_scroll {
                        this.syncing_scroll = true;
                        let pos = this.lhs_editor.update(cx, |editor, cx| {
                            editor.scroll_position(cx)
                        });
                        this.rhs_editor.update(cx, |editor, cx| {
                            editor.set_scroll_position(pos, window, cx);
                        });
                        this.syncing_scroll = false;
                    }
                }
            },
        ));

        // Setup: create diff, attach to RHS, compute alignment blocks
        let setup_task = cx.spawn_in(window, {
            let rhs_editor = rhs_editor.clone();
            let lhs_editor = lhs_editor.clone();
            async move |_this, cx| {
                // Create and attach diff
                let rhs_buffer = rhs_editor.update(cx, |editor, cx| {
                    editor.buffer().read(cx).all_buffers().into_iter().next()
                        .expect("rhs buffer exists")
                });
                let rhs_snapshot = rhs_buffer.read_with(cx, |buffer, _| buffer.text_snapshot());
                let diff = cx.new(|cx| BufferDiff::new(&rhs_snapshot, cx));
                let update_task = diff.update(cx, |diff, cx| {
                    diff.update_diff(
                        rhs_snapshot.clone(),
                        Some(old_text.as_str().into()),
                        Some(true),
                        None,
                        cx,
                    )
                });
                let update = update_task.await;
                let set_task = diff.update(cx, |diff, cx| {
                    diff.set_snapshot(update, &rhs_snapshot, cx)
                });
                set_task.await;
                rhs_editor.update(cx, |editor, cx| {
                    editor.buffer().update(cx, |multibuffer, cx| {
                        multibuffer.add_diff(diff, cx);
                    });
                    editor.set_expand_all_diff_hunks(cx);
                });

                // Compute and insert alignment spacer blocks
                _this
                    .update_in(cx, |_this, _window, cx| {
                        insert_alignment_blocks(&rhs_editor, &lhs_editor, cx);

                        // Scroll to first hunk
                        let rhs_snapshot = rhs_editor.read(cx).buffer().read(cx).snapshot(cx);
                        if let Some(first_hunk) = rhs_snapshot.diff_hunks().next() {
                            let row = first_hunk.row_range.start.0;
                            let scroll_row = row.saturating_sub(3) as f32;
                            let scroll_pos = gpui::Point::new(0.0, scroll_row as f64);
                            _this.syncing_scroll = true;
                            _this.rhs_editor.update(cx, |editor, cx| {
                                editor.set_scroll_position(scroll_pos, _window, cx);
                            });
                            _this.lhs_editor.update(cx, |editor, cx| {
                                editor.set_scroll_position(scroll_pos, _window, cx);
                            });
                            _this.syncing_scroll = false;
                        }
                    })
                    .ok();
            }
        });

        Self {
            lhs_editor,
            rhs_editor,
            full_path,
            filename,
            sha_short,
            project_path,
            focus_handle,
            syncing_scroll: false,
            left_ratio: 0.5,
            _subscriptions: subscriptions,
            _setup_task: setup_task,
        }
    }

    pub fn rhs_editor(&self) -> &Entity<Editor> {
        &self.rhs_editor
    }
}

fn insert_alignment_blocks(
    rhs_editor: &Entity<Editor>,
    lhs_editor: &Entity<Editor>,
    cx: &mut App,
) {
    use editor::display_map::{BlockPlacement, BlockProperties, BlockStyle};

    let rhs_snapshot = rhs_editor.read(cx).buffer().read(cx).snapshot(cx);
    let lhs_snapshot = lhs_editor.read(cx).buffer().read(cx).snapshot(cx);

    let hunks: Vec<_> = rhs_snapshot.diff_hunks().collect();
    if hunks.is_empty() {
        return;
    }

    let mut rhs_blocks = Vec::new();
    let mut lhs_blocks = Vec::new();
    let mut lhs_extra_offset: i64 = 0;
    let mut lhs_highlight_ranges: Vec<std::ops::Range<multi_buffer::Anchor>> = Vec::new();

    for hunk in &hunks {
        let rhs_lines = (hunk.row_range.end.0 as i64) - (hunk.row_range.start.0 as i64);

        let base_start = hunk.diff_base_byte_range.start.0;
        let base_end = hunk.diff_base_byte_range.end.0;
        let lhs_lines = if base_end > base_start {
            let diff_entity = rhs_editor.read(cx).buffer().read(cx)
                .all_buffers().into_iter().next()
                .and_then(|buffer| {
                    rhs_editor.read(cx).buffer().read(cx)
                        .diff_for(buffer.read(cx).remote_id())
                });
            if let Some(diff_entity) = diff_entity {
                let base_text = diff_entity.read(cx).base_text_buffer().read(cx).text();
                let clamped_end = base_end.min(base_text.len());
                let clamped_start = base_start.min(clamped_end);
                let slice = &base_text[clamped_start..clamped_end];
                let newlines = slice.chars().filter(|&c| c == '\n').count() as i64;
                if !slice.is_empty() && !slice.ends_with('\n') {
                    newlines + 1
                } else if newlines > 0 {
                    newlines
                } else if !slice.is_empty() {
                    1
                } else {
                    0
                }
            } else {
                0
            }
        } else {
            0
        };

        let line_diff = rhs_lines - lhs_lines;

        // Highlight LHS rows that correspond to deleted/modified lines
        if lhs_lines > 0 {
            let lhs_start_row = (hunk.row_range.start.0 as i64 - lhs_extra_offset).max(0) as u32;
            let lhs_end_row = (lhs_start_row as i64 + lhs_lines).max(0) as u32;
            let lhs_end_row = lhs_end_row.min(lhs_snapshot.max_point().row + 1);
            let start_anchor = lhs_snapshot.anchor_before(
                multi_buffer::MultiBufferPoint::new(lhs_start_row, 0),
            );
            let end_anchor = lhs_snapshot.anchor_before(
                multi_buffer::MultiBufferPoint::new(lhs_end_row, 0),
            );
            lhs_highlight_ranges.push(start_anchor..end_anchor);
        }

        if line_diff > 0 {
            // RHS has more lines → spacer in LHS
            let lhs_row = ((hunk.row_range.start.0 as i64 - lhs_extra_offset) + lhs_lines)
                .max(0) as u32;
            let lhs_row = lhs_row.min(lhs_snapshot.max_point().row);
            let anchor = lhs_snapshot.anchor_before(
                multi_buffer::MultiBufferPoint::new(lhs_row.saturating_sub(1).max(0), 0),
            );

            let height = line_diff as u32;
            lhs_blocks.push(BlockProperties {
                placement: BlockPlacement::Below(anchor),
                height: Some(height),
                style: BlockStyle::Fixed,
                render: Arc::new(move |cx| {
                    let color = cx.app.theme().colors().border_variant;
                    let scale = cx.window.scale_factor();
                    let line_h = f32::from(cx.line_height) * scale;
                    let pattern_size = (line_h / 2.0).floor().max(2.0);
                    let background = gpui::pattern_slash(color, 2.0, pattern_size - 2.0);
                    gpui::div()
                        .h(cx.line_height * height as f32)
                        .w(cx.max_width)
                        .bg(background)
                        .into_any_element()
                }),
                priority: 0,
            });
        } else if line_diff < 0 {
            // LHS has more lines → spacer in RHS
            let rhs_row = hunk.row_range.start.0;
            let anchor = rhs_snapshot.anchor_before(
                multi_buffer::MultiBufferPoint::new(rhs_row.max(1).saturating_sub(1), 0),
            );

            let height = (-line_diff) as u32;
            rhs_blocks.push(BlockProperties {
                placement: BlockPlacement::Below(anchor),
                height: Some(height),
                style: BlockStyle::Fixed,
                render: Arc::new(move |cx| {
                    let color = cx.app.theme().colors().border_variant;
                    let scale = cx.window.scale_factor();
                    let line_h = f32::from(cx.line_height) * scale;
                    let pattern_size = (line_h / 2.0).floor().max(2.0);
                    let background = gpui::pattern_slash(color, 2.0, pattern_size - 2.0);
                    gpui::div()
                        .h(cx.line_height * height as f32)
                        .w(cx.max_width)
                        .bg(background)
                        .into_any_element()
                }),
                priority: 0,
            });
        }

        lhs_extra_offset += line_diff;
    }

    if !rhs_blocks.is_empty() {
        rhs_editor.update(cx, |editor, cx| {
            editor.insert_blocks(rhs_blocks, None, cx);
        });
    }
    if !lhs_blocks.is_empty() {
        lhs_editor.update(cx, |editor, cx| {
            editor.insert_blocks(lhs_blocks, None, cx);
        });
    }

    // Apply LHS row highlights for deleted/modified lines
    if !lhs_highlight_ranges.is_empty() {
        struct LhsDiffHighlight;
        let deleted_color = cx.theme().colors().version_control_deleted;
        let opacity = if cx.theme().appearance().is_light() { 0.16 } else { 0.12 };
        let color = deleted_color.opacity(opacity);
        lhs_editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<LhsDiffHighlight>();
            for range in lhs_highlight_ranges {
                editor.highlight_rows::<LhsDiffHighlight>(
                    range,
                    color,
                    editor::RowHighlightOptions {
                        include_gutter: true,
                        ..Default::default()
                    },
                    cx,
                );
            }
        });
    }
}

impl Render for VsCommitDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let left_ratio = self.left_ratio;
        let right_ratio = 1.0 - left_ratio;
        let border_color = cx.theme().colors().border_variant;

        h_flex()
            .size_full()
            .child(
                div()
                    .flex_shrink()
                    .min_w_0()
                    .h_full()
                    .flex_basis(relative(left_ratio))
                    .overflow_hidden()
                    .child(self.lhs_editor.clone()),
            )
            .child(
                div()
                    .w(px(1.))
                    .h_full()
                    .flex_shrink_0()
                    .bg(border_color),
            )
            .child(
                div()
                    .flex_shrink()
                    .min_w_0()
                    .h_full()
                    .flex_basis(relative(right_ratio))
                    .overflow_hidden()
                    .child(self.rhs_editor.clone()),
            )
    }
}

impl Focusable for VsCommitDiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for VsCommitDiffView {}

impl Item for VsCommitDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> gpui::AnyElement {
        Label::new(format!("{} ({})", self.filename, self.sha_short))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        format!("{} ({})", self.filename, self.sha_short).into()
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("VS Commit Diff Opened")
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.rhs_editor.clone().into())
        } else {
            None
        }
    }

    fn breadcrumb_location(&self, _: &App) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(&self, _cx: &App) -> Option<(Vec<HighlightedText>, Option<gpui::Font>)> {
        Some((
            vec![HighlightedText {
                text: format!("{} @ {}", self.full_path, self.sha_short).into(),
                highlights: Vec::new(),
            }],
            None,
        ))
    }
}
