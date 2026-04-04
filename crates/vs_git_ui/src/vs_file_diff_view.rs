use anyhow::Result;
use buffer_diff::BufferDiff;
use editor::{Editor, EditorEvent, MultiBuffer};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Font,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task,
    WeakEntity, Window,
};
use language::{Capability, HighlightedText};
use project::{Project, ProjectPath};
use std::any::{Any, TypeId};
use std::sync::Arc;
use theme::ActiveTheme;
use ui::{Color, Icon, IconName, Label, LabelCommon as _, prelude::*};
use workspace::{
    Item, ItemNavHistory, ToolbarItemLocation, Workspace,
    item::{ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VsDiffKind {
    /// Changes section: working copy vs index (git diff)
    Unstaged,
    /// Staged Changes section: index vs HEAD (git diff --cached)
    Staged,
}

pub struct VsFileDiffView {
    lhs_editor: Entity<Editor>,
    rhs_editor: Entity<Editor>,
    diff_kind: VsDiffKind,
    _project: Entity<Project>,
    focus_handle: FocusHandle,
    syncing_scroll: bool,
    _subscriptions: Vec<Subscription>,
    _setup_task: Task<()>,
}

impl VsFileDiffView {
    pub fn open(
        project_path: ProjectPath,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        diff_kind: VsDiffKind,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let buffer_task = project.update(cx, |project, cx| {
            project.open_buffer(project_path.clone(), cx)
        });

        window.spawn(cx, async move |cx| {
            let buffer = buffer_task.await?;

            // Load both HEAD and index text from git diffs
            let uncommitted_diff = project
                .update(cx, |project, cx| {
                    project.open_uncommitted_diff(buffer.clone(), cx)
                })
                .await?;

            let unstaged_diff = project
                .update(cx, |project, cx| {
                    project.open_unstaged_diff(buffer.clone(), cx)
                })
                .await?;

            // Read base texts
            let head_text = uncommitted_diff.read_with(cx, |diff, cx| {
                let base = diff.base_text_buffer().read(cx);
                base.text()
            });
            let index_text = unstaged_diff.read_with(cx, |diff, cx| {
                let base = diff.base_text_buffer().read(cx);
                base.text()
            });

            workspace.update_in(cx, |workspace, window, cx| {
                cx.new(|cx| {
                    Self::new(
                        buffer,
                        head_text,
                        index_text,
                        unstaged_diff,
                        diff_kind,
                        project,
                        workspace,
                        window,
                        cx,
                    )
                })
            })
        })
    }

    fn new(
        working_copy_buffer: Entity<language::Buffer>,
        head_text: String,
        index_text: String,
        unstaged_diff: Entity<BufferDiff>,
        diff_kind: VsDiffKind,
        project: Entity<Project>,
        _workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Save head_text for the Staged setup task before it gets consumed
        let head_text_for_staged = if diff_kind == VsDiffKind::Staged {
            Some(head_text.clone())
        } else {
            None
        };

        // Determine LHS and RHS content based on diff kind
        let (lhs_text, rhs_buffer) = match diff_kind {
            VsDiffKind::Unstaged => {
                // LHS = index text, RHS = working copy
                (index_text, working_copy_buffer)
            }
            VsDiffKind::Staged => {
                // LHS = HEAD text, RHS = index text (read-only buffer)
                let index_buffer = cx.new(|cx| {
                    let mut buffer = language::Buffer::local(index_text, cx);
                    buffer.set_capability(Capability::ReadOnly, cx);
                    buffer
                });
                (head_text, index_buffer)
            }
        };

        // Create LHS editor (read-only, base text)
        let lhs_buffer = cx.new(|cx| {
            let mut buffer = language::Buffer::local(lhs_text, cx);
            buffer.set_capability(Capability::ReadOnly, cx);
            buffer
        });
        let lhs_multibuffer =
            cx.new(|cx| MultiBuffer::singleton(lhs_buffer, cx));
        let lhs_editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(lhs_multibuffer, None, window, cx);
            editor.disable_diagnostics(cx);
            editor.set_show_vertical_scrollbar(false, cx);
            editor.set_line_number_suffix("-");
            editor
        });

        // Create RHS editor
        let rhs_multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::singleton(rhs_buffer, cx);
            multibuffer.set_show_deleted_hunks(false, cx);
            if diff_kind == VsDiffKind::Unstaged {
                // Add the unstaged diff so decorations show working vs index
                multibuffer.add_diff(unstaged_diff, cx);
            }
            multibuffer
        });
        let rhs_project = match diff_kind {
            VsDiffKind::Unstaged => Some(project.clone()),
            VsDiffKind::Staged => None,
        };
        let rhs_editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(rhs_multibuffer, rhs_project, window, cx);
            if diff_kind == VsDiffKind::Unstaged {
                // Prevent auto-loading uncommitted diff (we already added unstaged diff)
                editor.start_temporary_diff_override();
            }
            editor.disable_diagnostics(cx);
            editor.set_line_number_suffix("+");
            editor
        });

        // Subscribe to scroll events for sync
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
                cx.emit(event.clone());
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

        // Setup task: wait for diff, then add spacer blocks for alignment
        let setup_task = cx.spawn_in(window, {
            let rhs_editor = rhs_editor.clone();
            let lhs_editor = lhs_editor.clone();
            async move |_this, cx| {
                match diff_kind {
                    VsDiffKind::Unstaged => {
                        // Unstaged diff already added to multibuffer in constructor.
                        // No need to wait.
                    }
                    VsDiffKind::Staged => {
                        // For Staged view, create a diff manually (index vs HEAD)
                        let head_text = head_text_for_staged
                            .expect("head_text saved for staged view");
                        let (diff, update_task) = rhs_editor.update(cx, |editor, cx| {
                            let rhs_buffer = editor.buffer().read(cx)
                                .all_buffers().into_iter().next()
                                .expect("rhs buffer exists");
                            let rhs_snapshot = rhs_buffer.read(cx).text_snapshot();

                            let diff = cx.new(|cx| BufferDiff::new(&rhs_snapshot, cx));
                            let update_task = diff.update(cx, |diff, cx| {
                                diff.update_diff(
                                    rhs_snapshot,
                                    Some(head_text.into()),
                                    Some(true),
                                    None,
                                    cx,
                                )
                            });
                            (diff, update_task)
                        });

                        let update = update_task.await;
                        let rhs_snapshot = rhs_editor.update(cx, |editor, cx| {
                            let buffer = editor.buffer().read(cx)
                                .all_buffers().into_iter().next()
                                .expect("rhs buffer exists");
                            buffer.read(cx).text_snapshot()
                        });
                        let set_snapshot_task = diff.update(cx, |diff, cx| {
                            diff.set_snapshot(update, &rhs_snapshot, cx)
                        });
                        set_snapshot_task.await;
                        rhs_editor.update(cx, |editor, cx| {
                            editor.buffer().update(cx, |multibuffer, cx| {
                                multibuffer.add_diff(diff, cx);
                            });
                        });
                    }
                }

                // Compute spacer blocks from diff hunks
                rhs_editor
                    .update_in(cx, |_rhs, window, cx| {
                        Self::insert_alignment_blocks(
                            &rhs_editor, &lhs_editor, window, cx,
                        );
                    })
                    .ok();
            }
        });

        Self {
            lhs_editor,
            rhs_editor,
            diff_kind,
            _project: project,
            focus_handle,
            syncing_scroll: false,
            _subscriptions: subscriptions,
            _setup_task: setup_task,
        }
    }

    fn insert_alignment_blocks(
        rhs_editor: &Entity<Editor>,
        lhs_editor: &Entity<Editor>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        use editor::display_map::{BlockPlacement, BlockProperties, BlockStyle};

        let rhs_snapshot = rhs_editor.read(cx).buffer().read(cx).snapshot(cx);
        let lhs_snapshot = lhs_editor.read(cx).buffer().read(cx).snapshot(cx);

        // Get the full LHS text for line counting in base ranges
        let lhs_full_text: String = lhs_snapshot.text().to_string();

        let hunks: Vec<_> = rhs_snapshot.diff_hunks().collect();
        if hunks.is_empty() {
            return;
        }

        let spacer_color = gpui::hsla(0.0, 0.0, 0.3, 0.1);
        let deleted_color = gpui::hsla(0.0, 0.5, 0.4, 0.15); // red tint for LHS
        let added_color = gpui::hsla(0.33, 0.5, 0.4, 0.15); // green tint for RHS

        let mut rhs_blocks = Vec::new();
        let mut lhs_blocks = Vec::new();
        let mut lhs_highlights: Vec<(multi_buffer::Anchor, multi_buffer::Anchor)> = Vec::new();
        let mut rhs_highlights: Vec<(multi_buffer::Anchor, multi_buffer::Anchor)> = Vec::new();

        // Track how many extra lines have been inserted on each side
        // so we can adjust anchor positions
        let mut lhs_extra_offset: i64 = 0;

        for hunk in &hunks {
            let rhs_lines = (hunk.row_range.end.0 as i64) - (hunk.row_range.start.0 as i64);

            // Count LHS lines from diff_base_byte_range
            let base_start = hunk.diff_base_byte_range.start.0;
            let base_end = hunk.diff_base_byte_range.end.0;
            let lhs_lines = if base_end > base_start && base_end <= lhs_full_text.len() {
                let slice = &lhs_full_text[base_start..base_end];
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
            };

            let diff = rhs_lines - lhs_lines;

            if diff > 0 {
                // RHS has more lines (additions) → insert spacer in LHS
                let lhs_row = ((hunk.row_range.start.0 as i64 - lhs_extra_offset) + lhs_lines)
                    .max(0) as u32;
                let lhs_row = lhs_row.min(lhs_snapshot.max_point().row);
                let anchor = lhs_snapshot.anchor_before(
                    multi_buffer::MultiBufferPoint::new(lhs_row.saturating_sub(1).max(0), 0),
                );

                let height = diff as u32;
                lhs_blocks.push(BlockProperties {
                    placement: BlockPlacement::Below(anchor),
                    height: Some(height),
                    style: BlockStyle::Fixed,
                    render: Arc::new(move |cx| {
                        gpui::div()
                            .h(cx.line_height * height as f32)
                            .w_full()
                            .bg(spacer_color)
                            .into_any_element()
                    }),
                    priority: 0,
                });
            } else if diff < 0 {
                // LHS has more lines (deletions) → insert spacer in RHS
                let rhs_row = hunk.row_range.end.0.saturating_sub(1);
                let anchor = rhs_snapshot.anchor_before(
                    multi_buffer::MultiBufferPoint::new(rhs_row, 0),
                );

                let height = (-diff) as u32;
                rhs_blocks.push(BlockProperties {
                    placement: BlockPlacement::Below(anchor),
                    height: Some(height),
                    style: BlockStyle::Fixed,
                    render: Arc::new(move |cx| {
                        gpui::div()
                            .h(cx.line_height * height as f32)
                            .w_full()
                            .bg(spacer_color)
                            .into_any_element()
                    }),
                    priority: 0,
                });
            }

            // Highlight LHS rows for this hunk (deleted/modified lines)
            if lhs_lines > 0 {
                let lhs_start_row = ((hunk.row_range.start.0 as i64 - lhs_extra_offset)
                    .max(0)) as u32;
                let lhs_end_row = (lhs_start_row as i64 + lhs_lines).max(0) as u32;
                let lhs_end_row = lhs_end_row.min(lhs_snapshot.max_point().row + 1);
                if lhs_start_row < lhs_end_row {
                    let start = lhs_snapshot.anchor_before(
                        multi_buffer::MultiBufferPoint::new(lhs_start_row, 0),
                    );
                    let end = lhs_snapshot.anchor_before(
                        multi_buffer::MultiBufferPoint::new(lhs_end_row.saturating_sub(1), 0),
                    );
                    lhs_highlights.push((start, end));
                }
            }

            // Highlight RHS rows for this hunk (added/modified lines)
            if rhs_lines > 0 {
                let start = rhs_snapshot.anchor_before(
                    multi_buffer::MultiBufferPoint::new(hunk.row_range.start.0, 0),
                );
                let end = rhs_snapshot.anchor_before(
                    multi_buffer::MultiBufferPoint::new(
                        hunk.row_range.end.0.saturating_sub(1),
                        0,
                    ),
                );
                rhs_highlights.push((start, end));
            }

            lhs_extra_offset += diff;
        }

        // Insert spacer blocks
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

        // Apply row highlights
        struct LhsDiffHighlight;
        struct RhsDiffHighlight;

        for (start, end) in lhs_highlights {
            lhs_editor.update(cx, |editor, cx| {
                editor.highlight_rows::<LhsDiffHighlight>(
                    start..end,
                    deleted_color,
                    editor::RowHighlightOptions::default(),
                    cx,
                );
            });
        }
        for (start, end) in rhs_highlights {
            rhs_editor.update(cx, |editor, cx| {
                editor.highlight_rows::<RhsDiffHighlight>(
                    start..end,
                    added_color,
                    editor::RowHighlightOptions::default(),
                    cx,
                );
            });
        }

        log::info!(
            "vs_file_diff_view: inserted alignment blocks and highlights for {} hunks",
            hunks.len()
        );
    }
}

impl Render for VsFileDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = cx.theme().colors().border_variant;

        h_flex()
            .size_full()
            .child(
                div()
                    .flex_shrink()
                    .min_w_0()
                    .h_full()
                    .flex_basis(relative(0.5))
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
                    .flex_basis(relative(0.5))
                    .overflow_hidden()
                    .child(self.rhs_editor.clone()),
            )
    }
}

impl Focusable for VsFileDiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for VsFileDiffView {}

impl Item for VsFileDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let filename = self
            .rhs_editor
            .read(cx)
            .buffer()
            .read(cx)
            .all_buffers()
            .into_iter()
            .next()
            .and_then(|buffer| {
                let file = buffer.read(cx).file()?;
                Some(
                    file.full_path(cx)
                        .file_name()?
                        .to_string_lossy()
                        .to_string(),
                )
            })
            .unwrap_or_else(|| "untitled".to_string());
        let suffix = match self.diff_kind {
            VsDiffKind::Staged => "(Index)",
            VsDiffKind::Unstaged => "(Working Tree)",
        };
        format!("{filename} {suffix}").into()
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        self.rhs_editor
            .read(cx)
            .buffer()
            .read(cx)
            .all_buffers()
            .into_iter()
            .next()
            .and_then(|buffer| {
                let file = buffer.read(cx).file()?;
                Some(file.full_path(cx).to_string_lossy().to_string().into())
            })
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("VS File Diff Opened")
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rhs_editor.update(cx, |editor, cx| {
            editor.deactivated(window, cx);
        });
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

    fn as_searchable(
        &self,
        _: &Entity<Self>,
        _cx: &App,
    ) -> Option<Box<dyn SearchableItemHandle>> {
        None
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.rhs_editor.read(cx).for_each_project_item(cx, f)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rhs_editor.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.rhs_editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn breadcrumb_location(&self, _: &App) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(&self, cx: &App) -> Option<(Vec<HighlightedText>, Option<Font>)> {
        self.rhs_editor.read(cx).breadcrumbs(cx)
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.rhs_editor.read(cx).is_dirty(cx)
    }

    fn has_conflict(&self, cx: &App) -> bool {
        self.rhs_editor.read(cx).has_conflict(cx)
    }

    fn can_save(&self, cx: &App) -> bool {
        self.rhs_editor.read(cx).can_save(cx)
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.rhs_editor.update(cx, |editor, cx| {
            editor.save(options, project, window, cx)
        })
    }

    fn save_as(
        &mut self,
        _: Entity<Project>,
        _: ProjectPath,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) -> Task<Result<()>> {
        unreachable!()
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.rhs_editor.update(cx, |editor, cx| {
            editor.reload(project, window, cx)
        })
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rhs_editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx);
        });
    }
}
