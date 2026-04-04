use anyhow::Result;
use editor::{Editor, EditorEvent, MultiBuffer, SplittableEditor};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, SharedString, Subscription, Task, WeakEntity, Window,
};
use project::{Project, ProjectPath};
use settings::DiffViewStyle;
use std::any::{Any, TypeId};
use std::sync::Arc;
use ui::{Color, Icon, IconName, Label, LabelCommon as _};
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};

pub struct VsFileDiffView {
    editor: Entity<SplittableEditor>,
    _project: Entity<Project>,
    _split_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl VsFileDiffView {
    pub fn open(
        project_path: ProjectPath,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let buffer_task = project.update(cx, |project, cx| {
            project.open_buffer(project_path.clone(), cx)
        });

        window.spawn(cx, async move |cx| {
            let buffer = buffer_task.await?;

            workspace.update_in(cx, |workspace, window, cx| {
                cx.new(|cx| Self::new(buffer, project, workspace, window, cx))
            })
        })
    }

    fn new(
        buffer: Entity<language::Buffer>,
        project: Entity<Project>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let multibuffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));

        let workspace_entity = workspace.weak_handle().upgrade().expect("workspace exists");
        let editor = cx.new(|cx| {
            let splittable = SplittableEditor::new(
                DiffViewStyle::Unified,
                multibuffer,
                project.clone(),
                workspace_entity,
                window,
                cx,
            );
            splittable.rhs_editor().update(cx, |editor, cx| {
                editor.set_expand_all_diff_hunks(cx);
                editor.disable_diagnostics(cx);
            });
            splittable
        });

        let split_task = cx.spawn_in(window, {
            let editor = editor.clone();
            async move |_this, cx| {
                if let Some(diff_task) = editor.update(cx, |editor, cx| {
                    editor.rhs_editor().read(cx).wait_for_diff_to_load()
                }) {
                    diff_task.await;
                }

                editor
                    .update_in(cx, |editor, window, cx| {
                        editor.split(window, cx);
                    })
                    .ok();
            }
        });

        let subscriptions = vec![cx.subscribe(
            &editor,
            |_this: &mut Self, _, event: &EditorEvent, cx: &mut Context<Self>| {
                cx.emit(event.clone());
            },
        )];

        Self {
            editor,
            _project: project,
            _split_task: split_task,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for VsFileDiffView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone().into_any_element()
    }
}

impl Focusable for VsFileDiffView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
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
            .editor
            .read(cx)
            .rhs_editor()
            .read(cx)
            .buffer()
            .read(cx)
            .all_buffers()
            .into_iter()
            .next()
            .and_then(|buffer| {
                let file = buffer.read(cx).file()?;
                Some(file.full_path(cx).file_name()?.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "untitled".to_string());
        format!("{filename} (Working Tree)").into()
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        self.editor
            .read(cx)
            .rhs_editor()
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
        self.editor.update(cx, |editor, cx| {
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.deactivated(window, cx);
            })
        });
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.read(cx).rhs_editor().clone().into())
        } else if type_id == TypeId::of::<SplittableEditor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }

    fn as_searchable(
        &self,
        _: &Entity<Self>,
        _cx: &App,
    ) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.editor
            .read(cx)
            .rhs_editor()
            .read(cx)
            .for_each_project_item(cx, f)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.rhs_editor().update(cx, |editor, _| {
                editor.set_nav_history(Some(nav_history));
            })
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, cx| editor.navigate(data, window, cx))
        })
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.editor.read(cx).rhs_editor().read(cx).is_dirty(cx)
    }

    fn has_conflict(&self, cx: &App) -> bool {
        self.editor.read(cx).rhs_editor().read(cx).has_conflict(cx)
    }

    fn can_save(&self, cx: &App) -> bool {
        self.editor.read(cx).rhs_editor().read(cx).can_save(cx)
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.editor.update(cx, |editor, cx| {
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.save(options, project, window, cx)
            })
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
        self.editor.update(cx, |editor, cx| {
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.reload(project, window, cx)
            })
        })
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }
}
