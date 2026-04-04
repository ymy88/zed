use git::{
    repository::RepoPath,
    status::{FileStatus, StageStatus},
};
use gpui::{
    actions, uniform_list, App, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ListSizingBehavior, ParentElement, Pixels, Render, SharedString,
    Styled, Task, UniformListScrollHandle, WeakEntity, Window,
};
use gpui_util::ResultExt as _;
use project::{
    git_store::{GitStoreEvent, Repository, RepositoryEvent},
    Project,
};
use ui::{prelude::*, Color, Icon, IconButton, IconName, IconSize, Label, LabelCommon, Tooltip};
use workspace::{
    dock::{DockPosition, PanelEvent},
    Panel, Workspace,
};

use crate::vs_git_panel_settings::VsGitPanelSettings;

const VS_GIT_PANEL_KEY: &str = "VsGitPanel";
const UPDATE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);

actions!(vs_git_panel, [Close, Toggle, ToggleFocus, SelectNext, SelectPrevious,]);

pub fn register(workspace: &mut Workspace) {
    log::debug!("vs_git_panel: registering actions");
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        log::debug!("vs_git_panel: ToggleFocus action triggered");
        workspace.toggle_panel_focus::<VsGitPanel>(window, cx);
    });
    workspace.register_action(|workspace, _: &Toggle, window, cx| {
        if !workspace.toggle_panel_focus::<VsGitPanel>(window, cx) {
            workspace.close_panel::<VsGitPanel>(window, cx);
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeGroup {
    MergeConflicts,
    StagedChanges,
    Changes,
}

impl ChangeGroup {
    fn label(&self) -> &'static str {
        match self {
            ChangeGroup::MergeConflicts => "Merge Conflicts",
            ChangeGroup::StagedChanges => "Staged Changes",
            ChangeGroup::Changes => "Changes",
        }
    }
}

#[derive(Clone)]
enum VsGitListEntry {
    GroupHeader { group: ChangeGroup, count: usize },
    FileEntry {
        repo_path: RepoPath,
        status: FileStatus,
        staging: StageStatus,
        group: ChangeGroup,
    },
}

pub struct VsGitPanel {
    active_repository: Option<Entity<Repository>>,
    entries: Vec<VsGitListEntry>,
    focus_handle: FocusHandle,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    scroll_handle: UniformListScrollHandle,
    selected_entry: Option<usize>,
    staged_count: usize,
    unstaged_count: usize,
    conflict_count: usize,
    update_visible_entries_task: Task<()>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl VsGitPanel {
    fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let git_store = project.read(cx).git_store().clone();
        let active_repository = project.read(cx).active_repository(cx);

        cx.new(|cx| {
            let focus_handle = cx.focus_handle();

            let subscription = cx.subscribe_in(
                &git_store,
                window,
                move |this: &mut Self, _git_store, event, window, cx| match event {
                    GitStoreEvent::RepositoryUpdated(
                        _,
                        RepositoryEvent::StatusesChanged | RepositoryEvent::BranchChanged,
                        true,
                    )
                    | GitStoreEvent::RepositoryAdded
                    | GitStoreEvent::RepositoryRemoved(_)
                    | GitStoreEvent::ActiveRepositoryChanged(_) => {
                        this.schedule_update(window, cx);
                    }
                    _ => {}
                },
            );

            let mut this = Self {
                active_repository,
                entries: Vec::new(),
                focus_handle,
                project,
                workspace: workspace.weak_handle(),
                scroll_handle: UniformListScrollHandle::new(),
                selected_entry: None,
                staged_count: 0,
                unstaged_count: 0,
                conflict_count: 0,
                update_visible_entries_task: Task::ready(()),
                _subscriptions: vec![subscription],
            };

            this.schedule_update(window, cx);
            this
        })
    }

    fn schedule_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_repository = self.project.read(cx).active_repository(cx);
        self.active_repository = active_repository;
        self.update_visible_entries_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(UPDATE_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.update_visible_entries(cx);
            })
            .ok();
        });
    }

    fn update_visible_entries(&mut self, cx: &mut Context<Self>) {
        self.entries.clear();
        self.staged_count = 0;
        self.unstaged_count = 0;
        self.conflict_count = 0;

        let Some(repo) = self.active_repository.as_ref() else {
            cx.notify();
            return;
        };

        let repo = repo.read(cx);

        let mut conflicts = Vec::new();
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();

        for entry in repo.cached_status() {
            let staging = entry.status.staging();
            if entry.status.is_conflicted() {
                conflicts.push(entry);
            } else if staging == StageStatus::Staged {
                staged.push(entry);
            } else if staging == StageStatus::PartiallyStaged {
                staged.push(entry.clone());
                unstaged.push(entry);
            } else {
                unstaged.push(entry);
            }
        }

        if !conflicts.is_empty() {
            self.conflict_count = conflicts.len();
            self.entries.push(VsGitListEntry::GroupHeader {
                group: ChangeGroup::MergeConflicts,
                count: conflicts.len(),
            });
            for entry in conflicts {
                self.entries.push(VsGitListEntry::FileEntry {
                    repo_path: entry.repo_path,
                    status: entry.status,
                    staging: entry.status.staging(),
                    group: ChangeGroup::MergeConflicts,
                });
            }
        }

        if !staged.is_empty() {
            self.staged_count = staged.len();
            self.entries.push(VsGitListEntry::GroupHeader {
                group: ChangeGroup::StagedChanges,
                count: staged.len(),
            });
            for entry in staged {
                self.entries.push(VsGitListEntry::FileEntry {
                    repo_path: entry.repo_path,
                    status: entry.status,
                    staging: StageStatus::Staged,
                    group: ChangeGroup::StagedChanges,
                });
            }
        }

        if !unstaged.is_empty() {
            self.unstaged_count = unstaged.len();
            self.entries.push(VsGitListEntry::GroupHeader {
                group: ChangeGroup::Changes,
                count: unstaged.len(),
            });
            for entry in unstaged {
                self.entries.push(VsGitListEntry::FileEntry {
                    repo_path: entry.repo_path,
                    status: entry.status,
                    staging: StageStatus::Unstaged,
                    group: ChangeGroup::Changes,
                });
            }
        }

        cx.notify();
    }

    fn branch_name(&self, cx: &App) -> SharedString {
        self.active_repository
            .as_ref()
            .and_then(|repo| {
                let snapshot = repo.read(cx).snapshot();
                let branch = snapshot.branch.as_ref()?;
                Some(SharedString::from(branch.name().to_string()))
            })
            .unwrap_or_else(|| "No branch".into())
    }

    fn total_changes(&self) -> usize {
        self.staged_count + self.unstaged_count + self.conflict_count
    }

    fn stage_file(&mut self, repo_path: RepoPath, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let task = repo.update(cx, |repo, cx| repo.stage_entries(vec![repo_path], cx));
        cx.spawn(async move |_this, _cx| {
            task.await.log_err();
        })
        .detach();
    }

    fn unstage_file(&mut self, repo_path: RepoPath, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let task = repo.update(cx, |repo, cx| repo.unstage_entries(vec![repo_path], cx));
        cx.spawn(async move |_this, _cx| {
            task.await.log_err();
        })
        .detach();
    }

    fn discard_file(
        &mut self,
        repo_path: RepoPath,
        status: FileStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let workspace = self.workspace.clone();

        if status.staging().has_staged() {
            let unstage_path = repo_path.clone();
            let unstage_task =
                repo.update(cx, |repo, cx| repo.unstage_entries(vec![unstage_path], cx));
            cx.spawn(async move |_this, _cx| {
                unstage_task.await.log_err();
            })
            .detach();
        }

        if !status.is_created() {
            let checkout_path = repo_path.clone();
            cx.spawn_in(window, async move |this, cx| {
                let open_task = workspace.update(cx, |workspace, cx| {
                    let path = repo
                        .read(cx)
                        .repo_path_to_project_path(&checkout_path, cx);
                    path.map(|p| {
                        workspace
                            .project()
                            .update(cx, |project, cx| project.open_buffer(p, cx))
                    })
                })?;

                if let Some(task) = open_task {
                    task.await.log_err();
                }

                let checkout_task = this.update(cx, |_this, cx| {
                    repo.update(cx, |repo, cx| {
                        repo.checkout_files("HEAD", vec![checkout_path], cx)
                    })
                })?;
                checkout_task.await?;

                this.update(cx, |_this, cx| cx.notify()).ok();
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        } else {
            let delete_path = repo_path.clone();
            cx.spawn_in(window, async move |_this, cx| {
                let delete_task = workspace.update(cx, |workspace, cx| {
                    let path = repo
                        .read(cx)
                        .repo_path_to_project_path(&delete_path, cx);
                    path.map(|p| {
                        workspace
                            .project()
                            .update(cx, |project, cx| project.delete_file(p, true, cx))
                    })
                })?;

                if let Some(Some(task)) = delete_task {
                    task.await?;
                }
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }

    fn stage_group(&mut self, group: ChangeGroup, cx: &mut Context<Self>) {
        let paths: Vec<RepoPath> = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                VsGitListEntry::FileEntry {
                    repo_path,
                    group: g,
                    ..
                } if *g == group => Some(repo_path.clone()),
                _ => None,
            })
            .collect();

        if paths.is_empty() {
            return;
        }
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let task = repo.update(cx, |repo, cx| repo.stage_entries(paths, cx));
        cx.spawn(async move |_this, _cx| {
            task.await.log_err();
        })
        .detach();
    }

    fn unstage_group(&mut self, group: ChangeGroup, cx: &mut Context<Self>) {
        let paths: Vec<RepoPath> = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                VsGitListEntry::FileEntry {
                    repo_path,
                    group: g,
                    ..
                } if *g == group => Some(repo_path.clone()),
                _ => None,
            })
            .collect();

        if paths.is_empty() {
            return;
        }
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let task = repo.update(cx, |repo, cx| repo.unstage_entries(paths, cx));
        cx.spawn(async move |_this, _cx| {
            task.await.log_err();
        })
        .detach();
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        let next = match self.selected_entry {
            Some(ix) => (ix + 1).min(self.entries.len() - 1),
            None => 0,
        };
        self.selected_entry = Some(next);
        self.scroll_handle.scroll_to_item(next, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, _window: &mut Window, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        let prev = match self.selected_entry {
            Some(ix) => ix.saturating_sub(1),
            None => 0,
        };
        self.selected_entry = Some(prev);
        self.scroll_handle.scroll_to_item(prev, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm_selection(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_entry else {
            return;
        };
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        if let VsGitListEntry::FileEntry {
            repo_path, status, group, ..
        } = entry
        {
            self.open_file_diff(repo_path.clone(), *status, *group, window, cx);
        }
    }

    fn open_file_diff(
        &mut self,
        repo_path: RepoPath,
        status: FileStatus,
        group: ChangeGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.active_repository.as_ref() else {
            return;
        };
        let project_path = repo.read(cx).repo_path_to_project_path(&repo_path, cx);
        let Some(project_path) = project_path else {
            return;
        };
        if status.is_deleted() {
            return;
        }

        let workspace = self.workspace.clone();
        let project = self.project.clone();

        let diff_kind = match group {
            ChangeGroup::StagedChanges => crate::vs_file_diff_view::VsDiffKind::Staged,
            _ => crate::vs_file_diff_view::VsDiffKind::Unstaged,
        };

        let diff_view_task = crate::vs_file_diff_view::VsFileDiffView::open(
            project_path,
            project,
            workspace.clone(),
            diff_kind,
            window,
            cx,
        );

        cx.spawn_in(window, async move |_, cx| {
            let diff_view = diff_view_task.await?;

            workspace.update_in(cx, |workspace, window, cx| {
                let pane = workspace.active_pane();
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(diff_view), true, true, None, window, cx);
                });
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_branch_indicator(&self, cx: &App) -> impl IntoElement {
        let branch = self.branch_name(cx);
        h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_1()
            .child(
                Icon::new(IconName::GitBranch)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(branch)
                    .size(LabelSize::Small)
                    .color(Color::Default),
            )
    }

    fn render_entries(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entry_count = self.entries.len();

        uniform_list(
            "vs-git-entries",
            entry_count,
            cx.processor(
                move |this: &mut Self, range: std::ops::Range<usize>, window, cx| {
                    range
                        .map(|ix| {
                            let entry = &this.entries[ix];
                            match entry {
                                VsGitListEntry::GroupHeader { group, count } => this
                                    .render_group_header(*group, *count, window, cx)
                                    .into_any_element(),
                                VsGitListEntry::FileEntry {
                                    repo_path,
                                    status,
                                    staging,
                                    group,
                                } => this
                                    .render_file_entry(
                                        ix,
                                        repo_path.clone(),
                                        *status,
                                        *staging,
                                        *group,
                                        window,
                                        cx,
                                    )
                                    .into_any_element(),
                            }
                        })
                        .collect()
                },
            ),
        )
        .size_full()
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .track_scroll(&self.scroll_handle)
    }

    fn render_group_header(
        &self,
        group: ChangeGroup,
        count: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = h_flex()
            .w_full()
            .px_2()
            .py_0p5()
            .gap_1()
            .bg(cx.theme().colors().surface_background)
            .child(
                h_flex()
                    .gap_1()
                    .flex_grow()
                    .child(
                        Label::new(group.label())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .weight(gpui::FontWeight::BOLD),
                    )
                    .child(
                        Label::new(format!("({})", count))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            );

        match group {
            ChangeGroup::StagedChanges => header.child(
                IconButton::new(
                    ElementId::Name("unstage-all".into()),
                    IconName::Dash,
                )
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text("Unstage All"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.unstage_group(ChangeGroup::StagedChanges, cx);
                })),
            ),
            ChangeGroup::Changes => header.child(
                IconButton::new(ElementId::Name("stage-all".into()), IconName::Plus)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Stage All"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.stage_group(ChangeGroup::Changes, cx);
                    })),
            ),
            ChangeGroup::MergeConflicts => header,
        }
    }

    fn render_file_entry(
        &self,
        ix: usize,
        repo_path: RepoPath,
        status: FileStatus,
        _staging: StageStatus,
        group: ChangeGroup,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ix_u64 = ix as u64;
        let path_ref: &std::sync::Arc<util::rel_path::RelPath> = repo_path.as_ref();
        let filename = path_ref.file_name().unwrap_or("").to_string();

        let parent = path_ref
            .parent()
            .map(|p: &util::rel_path::RelPath| {
                p.display(util::paths::PathStyle::Posix).to_string()
            })
            .filter(|p: &String| !p.is_empty());

        let status_color = git_status_color(status);
        let is_selected = self.selected_entry == Some(ix);

        let mut row = h_flex()
            .id(ElementId::NamedInteger("file-entry".into(), ix_u64))
            .w_full()
            .px_3()
            .py_0p5()
            .gap_1()
            .when(is_selected, |el| {
                el.bg(cx.theme().colors().ghost_element_selected)
            })
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .on_click({
                let click_path = repo_path.clone();
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.selected_entry = Some(ix);
                    this.open_file_diff(click_path.clone(), status, group, window, cx);
                    cx.notify();
                })
            })
            .child(
                h_flex()
                    .gap_1()
                    .flex_grow()
                    .overflow_x_hidden()
                    .child(
                        Label::new(filename)
                            .size(LabelSize::Small)
                            .color(Color::Default),
                    )
                    .children(parent.map(|p| {
                        Label::new(p).size(LabelSize::Small).color(Color::Muted)
                    })),
            )
            .child(
                Label::new(git_status_letter(status))
                    .size(LabelSize::Small)
                    .color(status_color),
            );

        match group {
            ChangeGroup::StagedChanges => {
                let path = repo_path.clone();
                row = row.child(
                    IconButton::new(
                        ElementId::NamedInteger("unstage".into(), ix_u64),
                        IconName::Dash,
                    )
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Unstage"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.unstage_file(path.clone(), cx);
                    })),
                );
            }
            ChangeGroup::Changes => {
                let stage_path = repo_path.clone();
                let discard_path = repo_path.clone();
                row = row
                    .child(
                        IconButton::new(
                            ElementId::NamedInteger("stage".into(), ix_u64),
                            IconName::Plus,
                        )
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Stage"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.stage_file(stage_path.clone(), cx);
                        })),
                    )
                    .child(
                        IconButton::new(
                            ElementId::NamedInteger("discard".into(), ix_u64),
                            IconName::Undo,
                        )
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Discard Changes"))
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                this.discard_file(discard_path.clone(), status, window, cx);
                            },
                        )),
                    );
            }
            ChangeGroup::MergeConflicts => {}
        }

        row
    }

    fn render_empty_state(&self, _cx: &App) -> impl IntoElement {
        v_flex().size_full().justify_center().items_center().child(
            Label::new("No changes")
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
    }
}

fn git_status_color(status: FileStatus) -> Color {
    if status.is_conflicted() {
        Color::Warning
    } else if status.is_created() {
        Color::Created
    } else if status.is_deleted() {
        Color::Deleted
    } else {
        Color::Modified
    }
}

fn git_status_letter(status: FileStatus) -> &'static str {
    if status.is_conflicted() {
        "C"
    } else if status.is_created() {
        "U"
    } else if status.is_deleted() {
        "D"
    } else {
        "M"
    }
}

impl Render for VsGitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_entries = !self.entries.is_empty();

        v_flex()
            .id("vs_git_panel")
            .key_context("VsGitPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_this, _: &Close, _window, cx| {
                cx.emit(PanelEvent::Close);
            }))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm_selection))
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_branch_indicator(cx))
            .map(|el| {
                if has_entries {
                    el.child(self.render_entries(window, cx))
                } else {
                    el.child(self.render_empty_state(cx))
                }
            })
    }
}

impl Focusable for VsGitPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for VsGitPanel {}

impl panel::PanelHeader for VsGitPanel {}

impl Panel for VsGitPanel {
    fn persistent_name() -> &'static str {
        "VsGitPanel"
    }

    fn panel_key() -> &'static str {
        VS_GIT_PANEL_KEY
    }

    fn position(&self, _: &Window, _cx: &App) -> DockPosition {
        VsGitPanelSettings::get_global().dock
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn default_size(&self, _: &Window, _cx: &App) -> Pixels {
        VsGitPanelSettings::get_global().default_width
    }

    fn icon(&self, _: &Window, _cx: &App) -> Option<ui::IconName> {
        Some(ui::IconName::GitBranch)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Source Control")
    }

    fn icon_label(&self, _: &Window, _cx: &App) -> Option<String> {
        let total = self.total_changes();
        (total > 0).then(|| total.to_string())
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn starts_open(&self, _: &Window, _cx: &App) -> bool {
        false
    }

    fn activation_priority(&self) -> u32 {
        4
    }
}

impl VsGitPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: gpui::AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        log::debug!("vs_git_panel: loading panel");
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = VsGitPanel::new(workspace, window, cx);
            log::debug!("vs_git_panel: panel created successfully");
            panel
        })
    }
}
