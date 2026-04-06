use editor::actions::{GoToHunk, GoToPreviousHunk};
use gpui::{
    Action, App, Context, Entity, EventEmitter, Focusable, IntoElement, Render, WeakEntity, Window,
};
use ui::{IconButton, IconName, IconButtonShape, Tooltip, prelude::*};
use workspace::{ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace, item::ItemHandle};

use crate::vs_file_diff_view::VsFileDiffView;

pub struct VsDiffToolbar {
    diff_view: Option<WeakEntity<VsFileDiffView>>,
    workspace: WeakEntity<Workspace>,
}

impl VsDiffToolbar {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            diff_view: None,
            workspace,
        }
    }

    fn diff_view(&self, _cx: &App) -> Option<Entity<VsFileDiffView>> {
        self.diff_view.as_ref()?.upgrade()
    }

    fn dispatch_action(&self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff_view) = self.diff_view(cx) {
            // Focus the RHS editor so actions like GoToHunk reach it
            let rhs_focus = diff_view.read(cx).rhs_editor.focus_handle(cx);
            rhs_focus.focus(window, cx);
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
        self.diff_view = active_pane_item
            .and_then(|item| item.act_as::<VsFileDiffView>(cx))
            .map(|entity| entity.downgrade());
        if self.diff_view.is_some() {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl Render for VsDiffToolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(diff_view) = self.diff_view(cx) else {
            return div().into_any_element();
        };
        let focus_handle = diff_view.focus_handle(cx);

        let workspace = self.workspace.clone();
        let diff_view_for_open = diff_view.downgrade();

        h_flex()
            .gap_1()
            .child(
                IconButton::new("open-file", IconName::File)
                    .shape(IconButtonShape::Square)
                    .tooltip(Tooltip::text("Open File"))
                    .on_click(move |_, window, cx| {
                        if let Some(diff_view) = diff_view_for_open.upgrade() {
                            let project_path = diff_view.read(cx).project_path.clone();
                            // Get current scroll position to restore after opening
                            let scroll_row = diff_view.update(cx, |dv, cx| {
                                dv.rhs_editor.update(cx, |editor, cx| {
                                    editor.scroll_position(cx).y as u32
                                })
                            });

                            if let (Some(project_path), Some(workspace)) = (project_path, workspace.upgrade()) {
                                let task = workspace.update(cx, |workspace, cx| {
                                    workspace.open_path_preview(project_path, None, true, false, true, window, cx)
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
                        }
                    }),
            )
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
