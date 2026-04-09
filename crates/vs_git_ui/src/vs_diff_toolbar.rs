use editor::actions::{GoToHunk, GoToPreviousHunk};
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    WeakEntity, Window,
};
use ui::{IconButton, IconName, IconButtonShape, Tooltip, prelude::*};
use workspace::{ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace, item::ItemHandle};

use crate::vs_commit_diff_view::VsCommitDiffView;
use crate::vs_file_diff_view::VsFileDiffView;

enum ActiveDiffView {
    File(WeakEntity<VsFileDiffView>),
    Commit(WeakEntity<VsCommitDiffView>),
}

impl ActiveDiffView {
    fn rhs_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        match self {
            ActiveDiffView::File(weak) => {
                let view = weak.upgrade()?;
                Some(view.read(cx).rhs_editor.focus_handle(cx))
            }
            ActiveDiffView::Commit(weak) => {
                let view = weak.upgrade()?;
                Some(view.read(cx).rhs_editor().focus_handle(cx))
            }
        }
    }

    fn focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        match self {
            ActiveDiffView::File(weak) => Some(weak.upgrade()?.focus_handle(cx)),
            ActiveDiffView::Commit(weak) => Some(weak.upgrade()?.focus_handle(cx)),
        }
    }
}

pub struct VsDiffToolbar {
    active_view: Option<ActiveDiffView>,
    workspace: WeakEntity<Workspace>,
}

impl VsDiffToolbar {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            active_view: None,
            workspace,
        }
    }

    fn dispatch_action(&self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(active_view) = &self.active_view {
            if let Some(rhs_focus) = active_view.rhs_focus_handle(cx) {
                rhs_focus.focus(window, cx);
            }
        }
        let action = action.boxed_clone();
        cx.defer(move |cx| {
            cx.dispatch_action(action.as_ref());
        })
    }
}

impl EventEmitter<ToolbarItemEvent> for VsDiffToolbar {}

impl ToolbarItemView for VsDiffToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.active_view = active_pane_item.and_then(|item| {
            if let Some(file_view) = item.act_as::<VsFileDiffView>(cx) {
                Some(ActiveDiffView::File(file_view.downgrade()))
            } else if let Some(commit_view) = item.act_as::<VsCommitDiffView>(cx) {
                Some(ActiveDiffView::Commit(commit_view.downgrade()))
            } else {
                None
            }
        });
        if self.active_view.is_some() {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl Render for VsDiffToolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active_view) = &self.active_view else {
            return div().into_any_element();
        };
        let Some(focus_handle) = active_view.focus_handle(cx) else {
            return div().into_any_element();
        };

        let mut toolbar = h_flex().gap_1();

        // "Open File" button
        {
            let project_path = match active_view {
                ActiveDiffView::File(weak) => weak.upgrade().and_then(|v| v.read(cx).project_path.clone()),
                ActiveDiffView::Commit(weak) => weak.upgrade().and_then(|v| v.read(cx).project_path.clone()),
            };
            if let Some(project_path) = project_path {
                let workspace = self.workspace.clone();
                let rhs_editor = match active_view {
                    ActiveDiffView::File(weak) => weak.upgrade().map(|v| v.read(cx).rhs_editor.clone()),
                    ActiveDiffView::Commit(weak) => weak.upgrade().map(|v| v.read(cx).rhs_editor().clone()),
                };
                toolbar = toolbar.child(
                    IconButton::new("open-file", IconName::File)
                        .shape(IconButtonShape::Square)
                        .tooltip(Tooltip::text("Open File"))
                        .on_click(move |_, window, cx| {
                            let scroll_row = rhs_editor.as_ref().map(|e| {
                                e.update(cx, |editor, cx| editor.scroll_position(cx).y as u32)
                            }).unwrap_or(0);
                            if let Some(workspace) = workspace.upgrade() {
                                let task = workspace.update(cx, |workspace, cx| {
                                    workspace.open_path_preview(project_path.clone(), None, true, false, true, window, cx)
                                });
                                window.spawn(cx, async move |cx| {
                                    let item = task.await?;
                                    if let Some(editor) = item.downcast::<editor::Editor>() {
                                        editor.update_in(cx, |editor, window, cx| {
                                            let point = gpui::Point::new(0.0, scroll_row as f64);
                                            editor.set_scroll_position(point, window, cx);
                                        })?;
                                    }
                                    anyhow::Ok(())
                                }).detach_and_log_err(cx);
                            }
                        }),
                );
            }
        }

        toolbar
            .child(
                IconButton::new("prev-hunk", IconName::ArrowUp)
                    .shape(IconButtonShape::Square)
                    .tooltip(Tooltip::for_action_title_in(
                        "Go to previous change",
                        &GoToPreviousHunk,
                        &focus_handle,
                    ))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dispatch_action(&GoToPreviousHunk, window, cx)
                    })),
            )
            .child(
                IconButton::new("next-hunk", IconName::ArrowDown)
                    .shape(IconButtonShape::Square)
                    .tooltip(Tooltip::for_action_title_in(
                        "Go to next change",
                        &GoToHunk,
                        &focus_handle,
                    ))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dispatch_action(&GoToHunk, window, cx)
                    })),
            )
            .into_any_element()
    }
}
