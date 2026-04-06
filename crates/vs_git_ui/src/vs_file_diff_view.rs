use anyhow::Result;
use buffer_diff::BufferDiff;
use editor::{Bias, Editor, EditorEvent, MultiBuffer};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Font,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task,
    WeakEntity, Window,
};
use language::HighlightedText;
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

#[derive(Debug, Clone)]
struct DraggedVsDiffHandle;

#[derive(Clone)]
struct HunkIconInfo {
    rhs_start_row: u32,
    hunk_height: u32, // max(rhs_lines, lhs_lines) — total height including spacers
    rhs_spacer_height: u32, // spacer lines inserted on RHS (for deletion hunks)
    rhs_editor: Entity<Editor>,
    uncommitted_diff: Entity<BufferDiff>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VsDiffKind {
    /// Changes section: working copy vs index (git diff)
    Unstaged,
    /// Staged Changes section: index vs HEAD (git diff --cached)
    Staged,
}

pub struct VsFileDiffView {
    lhs_editor: Entity<Editor>,
    pub(crate) rhs_editor: Entity<Editor>,
    _uncommitted_diff: Entity<BufferDiff>,
    unstaged_diff: Entity<BufferDiff>,
    diff_kind: VsDiffKind,
    file_path: Option<SharedString>,
    pub(crate) project_path: Option<ProjectPath>,
    _project: Entity<Project>,
    focus_handle: FocusHandle,
    syncing_scroll: bool,
    left_ratio: f32,
    hunk_icons: Vec<HunkIconInfo>,
    rhs_block_ids: Vec<editor::display_map::CustomBlockId>,
    lhs_block_ids: Vec<editor::display_map::CustomBlockId>,
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

            // Load diffs from git
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

            // Mark unstaged diff so hunks report correct status
            unstaged_diff.update(cx, |diff, _cx| {
                diff.set_all_hunks_unstaged(true);
            });

            // Read base texts
            let index_text = unstaged_diff.read_with(cx, |diff, cx| {
                diff.base_text_buffer().read(cx).text()
            });
            let head_text = uncommitted_diff.read_with(cx, |diff, cx| {
                diff.base_text_buffer().read(cx).text()
            });

            workspace.update_in(cx, |workspace, window, cx| {
                cx.new(|cx| {
                    Self::new(
                        buffer,
                        head_text,
                        index_text,
                        unstaged_diff,
                        uncommitted_diff,
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
        uncommitted_diff: Entity<BufferDiff>,
        diff_kind: VsDiffKind,
        project: Entity<Project>,
        _workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Get file path from working copy buffer for tab title and open file
        let (file_path, project_path) = working_copy_buffer
            .read(cx)
            .file()
            .map(|file| {
                let name: SharedString = file.full_path(cx)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled".to_string())
                    .into();
                let pp = ProjectPath {
                    worktree_id: file.worktree_id(cx),
                    path: file.path().clone(),
                };
                (Some(name), Some(pp))
            })
            .unwrap_or((None, None));

        // LHS: read-only buffer with old content
        let head_text_for_staged = head_text.clone();
        let lhs_text = match diff_kind {
            VsDiffKind::Unstaged => index_text.clone(),
            VsDiffKind::Staged => head_text,
        };
        let lhs_buffer = cx.new(|cx| {
            let mut buffer = language::Buffer::local(&lhs_text, cx);
            buffer.set_capability(language::Capability::ReadOnly, cx);
            buffer
        });
        let lhs_multibuffer = cx.new(|cx| {
            MultiBuffer::singleton(lhs_buffer, cx)
        });
        let lhs_editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(lhs_multibuffer, None, window, cx);
            editor.disable_diagnostics(cx);
            editor.set_show_vertical_scrollbar(false, cx);
            editor.set_show_breakpoints(false, cx);
            editor.set_line_number_suffix("-");
            editor
        });

        // RHS: working copy (Unstaged) or index content (Staged)
        let rhs_buffer = match diff_kind {
            VsDiffKind::Unstaged => working_copy_buffer,
            VsDiffKind::Staged => cx.new(|cx| {
                let mut buffer = language::Buffer::local(&index_text, cx);
                buffer.set_capability(language::Capability::ReadOnly, cx);
                buffer
            }),
        };
        let rhs_multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::singleton(rhs_buffer, cx);
            multibuffer.set_show_deleted_hunks(false, cx);
            if diff_kind == VsDiffKind::Unstaged {
                multibuffer.add_diff(unstaged_diff.clone(), cx);
            }
            multibuffer
        });
        let rhs_editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(rhs_multibuffer, Some(project.clone()), window, cx);
            editor.start_temporary_diff_override();
            editor.set_expand_all_diff_hunks(cx);
            editor.disable_diagnostics(cx);
            editor.set_show_breakpoints(false, cx);
            editor.set_line_number_suffix("+");
            // Hide default hunk controls — we render icons in the divider area instead
            editor.set_render_diff_hunk_controls(
                Arc::new(|_, _, _, _, _, _, _, _| gpui::Empty.into_any_element()),
                cx,
            );
            editor
        });

        // Subscribe to scroll events for sync
        let mut subscriptions = Vec::new();

        subscriptions.push(cx.subscribe_in(
            &rhs_editor,
            window,
            |this: &mut Self, _, event: &EditorEvent, window, cx| {
                match event {
                    EditorEvent::ScrollPositionChanged { local: true, .. } => {
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
                    EditorEvent::DirtyChanged | EditorEvent::Saved => {
                        this.refresh_alignment_blocks(cx);
                    }
                    _ => {}
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

        // Subscribe to unstaged diff changes to reload LHS when index text updates
        // (only for Unstaged view — Staged view's LHS shows HEAD which doesn't change)
        if diff_kind == VsDiffKind::Unstaged {
        subscriptions.push(cx.subscribe_in(
            &unstaged_diff,
            window,
            |this: &mut Self, _, event: &buffer_diff::BufferDiffEvent, window, cx| {
                if let buffer_diff::BufferDiffEvent::DiffChanged(_) = event {
                    // Index text has been updated — reload LHS
                    let new_index_text = this.unstaged_diff.read(cx)
                        .base_text_buffer().read(cx).text();

                    let rhs_scroll = this.rhs_editor.update(cx, |editor, cx| {
                        editor.scroll_position(cx)
                    });
                    this.syncing_scroll = true;

                    this.lhs_editor.update(cx, |editor, cx| {
                        let buffer = editor.buffer().read(cx)
                            .all_buffers().into_iter().next();
                        if let Some(buffer) = buffer {
                            buffer.update(cx, |buffer, cx| {
                                buffer.set_text(new_index_text, cx);
                            });
                        }
                        editor.set_scroll_position(rhs_scroll, window, cx);
                    });

                    this.syncing_scroll = false;
                    this.refresh_alignment_blocks(cx);
                }
            },
        ));
        } // end if Unstaged

        // Setup task: create diff if needed, then add spacer blocks
        let setup_task = cx.spawn_in(window, {
            let rhs_editor = rhs_editor.clone();
            let head_text = head_text_for_staged;
            async move |_this, cx| {
                match diff_kind {
                    VsDiffKind::Unstaged => {
                        // Wait for RHS diff to be ready
                        if let Some(diff_task) = rhs_editor.update(cx, |editor, _cx| {
                            editor.wait_for_diff_to_load()
                        }) {
                            diff_task.await;
                        }
                    }
                    VsDiffKind::Staged => {
                        // Create diff (index vs HEAD) asynchronously
                        let rhs_buffer = rhs_editor.update(cx, |editor, cx| {
                            editor.buffer().read(cx).all_buffers().into_iter().next()
                                .expect("rhs buffer exists")
                        });
                        let rhs_snapshot = rhs_buffer.read_with(cx, |buffer, _| buffer.text_snapshot());
                        let diff = cx.new(|cx| BufferDiff::new(&rhs_snapshot, cx));
                        let update_task = diff.update(cx, |diff, cx| {
                            diff.update_diff(
                                rhs_snapshot.clone(),
                                Some(head_text.as_str().into()),
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
                    }
                }

                // Compute spacer blocks and scroll to first hunk
                _this
                    .update_in(cx, |this, window, cx| {
                        this.refresh_alignment_blocks(cx);

                        // Scroll to first hunk
                        let rhs_snapshot = this.rhs_editor.read(cx).buffer().read(cx).snapshot(cx);
                        if let Some(first_hunk) = rhs_snapshot.diff_hunks().next() {
                            let row = first_hunk.row_range.start.0;
                            // Scroll a few lines above the hunk for context
                            let scroll_row = row.saturating_sub(3) as f32;
                            let scroll_pos = gpui::Point::new(0.0, scroll_row as f64);
                            this.syncing_scroll = true;
                            this.rhs_editor.update(cx, |editor, cx| {
                                editor.set_scroll_position(scroll_pos, window, cx);
                            });
                            this.lhs_editor.update(cx, |editor, cx| {
                                editor.set_scroll_position(scroll_pos, window, cx);
                            });
                            this.syncing_scroll = false;
                        }
                    })
                    .ok();
            }
        });

        Self {
            lhs_editor,
            rhs_editor,
            _uncommitted_diff: uncommitted_diff,
            unstaged_diff,
            diff_kind,
            file_path,
            project_path,
            _project: project,
            focus_handle,
            syncing_scroll: false,
            left_ratio: 0.5,
            hunk_icons: Vec::new(),
            rhs_block_ids: Vec::new(),
            lhs_block_ids: Vec::new(),
            _subscriptions: subscriptions,
            _setup_task: setup_task,
        }
    }

    fn refresh_alignment_blocks(&mut self, cx: &mut App) {
        let rhs_editor = &self.rhs_editor;
        let lhs_editor = &self.lhs_editor;

        // Remove old blocks
        if !self.rhs_block_ids.is_empty() {
            let ids: collections::HashSet<_> = std::mem::take(&mut self.rhs_block_ids).into_iter().collect();
            rhs_editor.update(cx, |editor, cx| {
                editor.remove_blocks(ids, None, cx);
            });
        }
        if !self.lhs_block_ids.is_empty() {
            let ids: collections::HashSet<_> = std::mem::take(&mut self.lhs_block_ids).into_iter().collect();
            lhs_editor.update(cx, |editor, cx| {
                editor.remove_blocks(ids, None, cx);
            });
        }

        let uncommitted_diff = &self._uncommitted_diff;
        self.hunk_icons.clear();
        Self::compute_alignment_blocks(
            rhs_editor, lhs_editor, uncommitted_diff,
            &mut self.rhs_block_ids, &mut self.lhs_block_ids,
            &mut self.hunk_icons,
            self.diff_kind,
            cx,
        );
    }

    fn compute_alignment_blocks(
        rhs_editor: &Entity<Editor>,
        lhs_editor: &Entity<Editor>,
        uncommitted_diff: &Entity<BufferDiff>,
        rhs_block_ids: &mut Vec<editor::display_map::CustomBlockId>,
        lhs_block_ids: &mut Vec<editor::display_map::CustomBlockId>,
        hunk_icons: &mut Vec<HunkIconInfo>,
        diff_kind: VsDiffKind,
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

        for hunk in &hunks {
            let rhs_lines = (hunk.row_range.end.0 as i64) - (hunk.row_range.start.0 as i64);

            // For the LHS, deleted hunks are shown inline via set_show_deleted_hunks(true).
            // The LHS hunk at the same position would have the deleted lines visible.
            // We need to compute the LHS line count from the diff base byte range.
            let base_start = hunk.diff_base_byte_range.start.0;
            let base_end = hunk.diff_base_byte_range.end.0;
            let lhs_lines = if base_end > base_start {
                // Count lines in the base text range
                let diff = rhs_editor.read(cx).buffer().read(cx)
                    .all_buffers().into_iter().next()
                    .and_then(|buffer| {
                        rhs_editor.read(cx).buffer().read(cx)
                            .diff_for(buffer.read(cx).remote_id())
                    });
                if let Some(diff) = diff {
                    let base_text = diff.read(cx).base_text_buffer().read(cx).text();
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

            let diff = rhs_lines - lhs_lines;
            let hunk_height = rhs_lines.max(lhs_lines) as u32;

            // Store hunk position for divider icon rendering (unstaged only)
            if diff_kind == VsDiffKind::Unstaged {
                hunk_icons.push(HunkIconInfo {
                    rhs_start_row: hunk.row_range.start.0,
                    hunk_height,
                    rhs_spacer_height: if diff < 0 { (-diff) as u32 } else { 0 },
                    rhs_editor: rhs_editor.clone(),
                    uncommitted_diff: uncommitted_diff.clone(),
                });
            }

            if diff > 0 {
                // RHS has more lines (additions) → insert spacer in LHS
                // Convert RHS row to LHS row using cumulative offset
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
            } else if diff < 0 {
                // LHS has more lines (deletions) → insert spacer in RHS with icon buttons
                let rhs_row = hunk.row_range.start.0;
                let anchor = rhs_snapshot.anchor_before(
                    multi_buffer::MultiBufferPoint::new(
                        rhs_row.max(1).saturating_sub(1),
                        0,
                    ),
                );

                let height = (-diff) as u32;
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

            lhs_extra_offset += diff;
        }

        // Insert spacer blocks and store IDs
        if !rhs_blocks.is_empty() {
            rhs_editor.update(cx, |editor, cx| {
                rhs_block_ids.extend(editor.insert_blocks(rhs_blocks, None, cx));
            });
        }
        if !lhs_blocks.is_empty() {
            lhs_editor.update(cx, |editor, cx| {
                lhs_block_ids.extend(editor.insert_blocks(lhs_blocks, None, cx));
            });
        }

        log::debug!(
            "vs_file_diff_view: inserted alignment blocks for {} hunks",
            hunks.len()
        );
    }
}

impl Render for VsFileDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = cx.theme().colors().border_variant;

        // Compute icon positions from RHS scroll position (in display rows, accounting for blocks)
        let line_height = _window.line_height();
        let display_snapshot = self.rhs_editor.update(cx, |editor, cx| {
            editor.display_snapshot(cx)
        });
        let scroll_position = self.rhs_editor.update(cx, |editor, cx| {
            editor.scroll_position(cx)
        });
        let scroll_top = line_height * scroll_position.y as f32;

        let mut divider_icons: Vec<AnyElement> = Vec::new();
        for info in &self.hunk_icons {
            // Convert buffer row to display row (accounts for spacer blocks)
            let display_point = display_snapshot.point_to_display_point(
                multi_buffer::MultiBufferPoint::new(info.rhs_start_row, 0),
                Bias::Left,
            );
            let display_row = display_point.row().0 as f32;
            // Subtract RHS spacer height: the spacer block is inserted before
            // rhs_start_row, so point_to_display_point returns the row AFTER
            // the spacer. We need to move the icons back up to cover the spacer.
            let top = line_height * (display_row - info.rhs_spacer_height as f32) - scroll_top;
            let hunk_height_total = line_height * info.hunk_height as f32;

            let rhs_editor = info.rhs_editor.clone();
            let uncommitted_diff = info.uncommitted_diff.clone();
            let hunk_row = info.rhs_start_row;

            divider_icons.push(
                gpui::div()
                    .absolute()
                    .top(top)
                    .left_0()
                    .w_full()
                    .h(hunk_height_total)
                    .child(
                        v_flex()
                            .items_center()
                            .justify_center()
                            .h_full()
                            .gap_0p5()
                            .child(
                                gpui::div()
                                    .id(("stage-mid", hunk_row as u64))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(gpui::hsla(0.0, 0.0, 0.5, 0.2)))
                                    .rounded_sm()
                                    .p_0p5()
                                    .child(ui::Icon::new(ui::IconName::Plus).size(ui::IconSize::XSmall))
                                    .on_click({
                                        let rhs_editor = rhs_editor.clone();
                                        let uncommitted_diff = uncommitted_diff.clone();
                                        move |_event, _window, cx| {
                                            let buffer = rhs_editor.read(cx).buffer().read(cx)
                                                .all_buffers().into_iter().next();
                                            if let Some(buffer) = buffer {
                                                let buffer_snapshot = buffer.read(cx).snapshot();
                                                let file_exists = buffer_snapshot.file()
                                                    .is_some_and(|file| file.disk_state().exists());
                                                let uncommitted_snapshot = uncommitted_diff.read(cx).snapshot(cx);
                                                let matching_hunks: Vec<_> = uncommitted_snapshot
                                                    .hunks(&buffer_snapshot)
                                                    .filter(|h| {
                                                        (h.range.start.row <= hunk_row && h.range.end.row >= hunk_row)
                                                            || (hunk_row == 0 && h.range.start.row == 0)
                                                    })
                                                    .collect();
                                                if !matching_hunks.is_empty() {
                                                    uncommitted_diff.update(cx, |diff, cx| {
                                                        diff.stage_or_unstage_hunks(
                                                            true, &matching_hunks, &buffer_snapshot, file_exists, cx,
                                                        );
                                                    });
                                                }
                                            }
                                        }
                                    })
                            )
                            .child(
                                gpui::div()
                                    .id(("restore-mid", hunk_row as u64))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(gpui::hsla(0.0, 0.0, 0.5, 0.2)))
                                    .rounded_sm()
                                    .p_0p5()
                                    .child(ui::Icon::new(ui::IconName::ArrowRight).size(ui::IconSize::XSmall))
                                    .on_click({
                                        let rhs_editor = rhs_editor.clone();
                                        move |_event, window, cx| {
                                            rhs_editor.update(cx, |editor, cx| {
                                                let point = rope::Point::new(hunk_row, 0);
                                                editor.restore_hunks_in_ranges(
                                                    vec![point..point], window, cx,
                                                );
                                            });
                                        }
                                    })
                            )
                    )
                    .into_any_element(),
            );
        }

        let left_ratio = self.left_ratio;
        let right_ratio = 1.0 - left_ratio;

        h_flex()
            .id("vs-diff-view-container")
            .size_full()
            .on_drag_move::<DraggedVsDiffHandle>(
                cx.listener(|this, event: &gpui::DragMoveEvent<DraggedVsDiffHandle>, _window, _cx| {
                    let bounds = event.bounds;
                    let drag_x = event.event.position.x;
                    let bounds_width = bounds.right() - bounds.left();
                    if bounds_width > px(0.) {
                        let new_ratio = ((drag_x - bounds.left()) / bounds_width).clamp(0.1, 0.9);
                        this.left_ratio = new_ratio;
                    }
                }),
            )
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
                    .w(px(24.))
                    .h_full()
                    .flex_shrink_0()
                    .bg(border_color)
                    .relative()
                    .overflow_hidden()
                    .child(
                        // Invisible drag handle overlay
                        div()
                            .id("vs-diff-resize-handle")
                            .absolute()
                            .left(px(-4.))
                            .w(px(32.))
                            .h_full()
                            .cursor_col_resize()
                            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _window, _cx| {
                                if event.click_count() >= 2 {
                                    this.left_ratio = 0.5;
                                }
                            }))
                            .on_drag(DraggedVsDiffHandle, |_, _, _, cx| cx.new(|_| gpui::Empty))
                    )
                    .children(divider_icons),
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

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let filename = self
            .file_path
            .as_ref()
            .map(|s| s.as_ref())
            .unwrap_or("untitled");
        let suffix = match self.diff_kind {
            VsDiffKind::Staged => "(Index)",
            VsDiffKind::Unstaged => "(Working Tree)",
        };
        format!("{filename} {suffix}").into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        self.file_path.clone()
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
        // For Unstaged view, RHS has a real file so breadcrumbs work
        if self.diff_kind == VsDiffKind::Unstaged {
            return self.rhs_editor.read(cx).breadcrumbs(cx);
        }
        // For Staged view, use the stored file path
        let path = self.project_path.as_ref()?;
        Some((
            vec![HighlightedText {
                text: path.path.as_unix_str().to_string().into(),
                highlights: Vec::new(),
            }],
            None,
        ))
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
        _workspace: &mut Workspace,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // Do NOT forward to rhs_editor — it would set a workspace serialization ID
        // on the inner editor, causing FOREIGN KEY errors when persisting selections
        // (the inner editor is not a registered workspace item).
    }
}
