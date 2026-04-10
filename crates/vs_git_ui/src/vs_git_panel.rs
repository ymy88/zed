use collections::HashSet;
use git::{
    repository::{CommitFile, CommitFileStatus, FileHistoryEntry, RepoPath},
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
    Project, ProjectPath,
};
use ui::{prelude::*, Color, ContextMenu, DropdownMenu, DropdownStyle, Icon, IconButton, IconName, IconSize, Label, LabelCommon, Tooltip};
use workspace::{
    dock::{DockPosition, PanelEvent},
    Panel, Workspace,
};

use crate::vs_git_panel_settings::VsGitPanelSettings;

#[derive(Debug, Clone)]
struct DraggedHistoryHandle;

const VS_GIT_PANEL_KEY: &str = "VsGitPanel";
const UPDATE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);
const HISTORY_PAGE_SIZE: usize = 20;

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
    HistoryHeader,
    CommitEntry {
        sha: SharedString,
        subject: SharedString,
        author: SharedString,
        timestamp: i64,
        expanded: bool,
    },
    CommitFileEntry {
        sha: SharedString,
        path: RepoPath,
        status: CommitFileStatus,
    },
    LoadMoreButton,
}

pub struct VsGitPanel {
    active_repository: Option<Entity<Repository>>,
    status_entries: Vec<VsGitListEntry>,
    history_entries: Vec<VsGitListEntry>,
    focus_handle: FocusHandle,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    status_scroll_handle: UniformListScrollHandle,
    history_scroll_handle: UniformListScrollHandle,
    selected_entry: Option<usize>,
    staged_count: usize,
    unstaged_count: usize,
    conflict_count: usize,
    update_visible_entries_task: Task<()>,
    commit_entries: Vec<FileHistoryEntry>,
    expanded_commits: HashSet<SharedString>,
    commit_files: std::collections::HashMap<SharedString, Vec<CommitFile>>,
    history_loaded: bool,
    history_collapsed: bool,
    history_height: f32,
    base_branch: Option<SharedString>,
    compared_files: Vec<(RepoPath, CommitFileStatus)>,
    compared_collapsed: bool,
    has_more_commits: bool,
    load_history_task: Task<()>,
    git_worktrees: Vec<git::repository::Worktree>,
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
                status_entries: Vec::new(),
                history_entries: Vec::new(),
                focus_handle,
                project,
                workspace: workspace.weak_handle(),
                status_scroll_handle: UniformListScrollHandle::new(),
                history_scroll_handle: UniformListScrollHandle::new(),
                selected_entry: None,
                staged_count: 0,
                unstaged_count: 0,
                conflict_count: 0,
                update_visible_entries_task: Task::ready(()),
                commit_entries: Vec::new(),
                expanded_commits: HashSet::default(),
                commit_files: std::collections::HashMap::new(),
                history_loaded: false,
                history_collapsed: false,
                history_height: 0.4,
                base_branch: None,
                compared_files: Vec::new(),
                compared_collapsed: false,
                has_more_commits: true,
                load_history_task: Task::ready(()),
                git_worktrees: Vec::new(),
                _subscriptions: vec![subscription],
            };

            this.schedule_update(window, cx);
            this.load_worktrees(cx);
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

        self.load_history(0, window, cx);
        self.load_compared_files(window, cx);
        self.load_worktrees(cx);
    }

    fn load_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            self.git_worktrees.clear();
            return;
        };
        let rx = repo.update(cx, |repo, _| repo.worktrees());
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(worktrees)) = rx.await {
                this.update(cx, |this, cx| {
                    this.git_worktrees = worktrees
                        .into_iter()
                        .filter(|wt| wt.ref_name.is_some())
                        .collect();
                    cx.notify();
                }).ok();
            }
        }).detach();
    }

    fn load_compared_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };

        // Determine current branch and base branch
        let snapshot = repo.read(cx).snapshot();
        let current_branch = snapshot.branch.as_ref().map(|b| b.name().to_string());
        let upstream_ref = snapshot.branch.as_ref()
            .and_then(|b| b.upstream.as_ref())
            .map(|u| u.ref_name.clone());

        // If we have an upstream, use that; otherwise find the default branch
        if let Some(upstream) = upstream_ref {
            let base_name: SharedString = upstream.split('/').last()
                .unwrap_or(&upstream).to_string().into();
            let rx = repo.update(cx, |repo, _cx| {
                repo.diff_name_status(upstream.to_string())
            });
            cx.spawn_in(window, async move |this, cx| {
                if let Ok(Ok(files)) = rx.await {
                    this.update(cx, |this, cx| {
                        this.base_branch = Some(base_name);
                        this.compared_files = files;
                        this.rebuild_entries(cx);
                    }).ok();
                }
            }).detach();
        } else {
            // No upstream — try default branch
            let rx = repo.update(cx, |repo, _cx| {
                repo.default_branch(true)
            });
            let current_branch = current_branch.clone();
            cx.spawn_in(window, async move |this, cx| {
                if let Ok(Ok(Some(default_branch))) = rx.await {
                    let default_name = default_branch.split('/').last()
                        .unwrap_or(&default_branch).to_string();

                    // Don't compare if we're on the default branch
                    let is_default = current_branch.as_deref() == Some(&default_name);
                    if is_default {
                        return;
                    }

                    let base_name: SharedString = default_name.into();
                    let base_ref = default_branch.clone();
                    let rx = this.update(cx, |this, cx| {
                        let repo = this.active_repository.clone();
                        repo.map(|repo| {
                            repo.update(cx, |repo, _cx| {
                                repo.diff_name_status(base_ref.to_string())
                            })
                        })
                    });
                    if let Ok(Some(rx)) = rx {
                        if let Ok(Ok(files)) = rx.await {
                            this.update(cx, |this, cx| {
                                this.base_branch = Some(base_name);
                                this.compared_files = files;
                                this.rebuild_entries(cx);
                            }).ok();
                        }
                    }
                }
            }).detach();
        }
    }

    fn load_history(&mut self, skip: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let git_store = self.project.read(cx).git_store().clone();
        let log_task = git_store.update(cx, |gs, cx| gs.branch_log(&repo, skip, HISTORY_PAGE_SIZE, cx));

        self.load_history_task = cx.spawn_in(window, async move |this, cx| {
            let entries = log_task.await;
            this.update(cx, |this, cx| {
                match entries {
                    Ok(entries) => {
                        this.has_more_commits = entries.len() >= HISTORY_PAGE_SIZE;
                        if skip == 0 {
                            this.commit_entries = entries;
                        } else {
                            this.commit_entries.extend(entries);
                        }
                        this.history_loaded = true;
                        this.rebuild_entries(cx);
                    }
                    Err(error) => {
                        log::error!("Failed to load branch log: {error}");
                    }
                }
            })
            .ok();
        });
    }

    fn rebuild_entries(&mut self, cx: &mut Context<Self>) {
        self.update_visible_entries(cx);
    }

    fn toggle_commit(&mut self, sha: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if self.expanded_commits.contains(&sha) {
            self.expanded_commits.remove(&sha);
            self.rebuild_entries(cx);
        } else {
            self.expanded_commits.insert(sha.clone());
            if self.commit_files.contains_key(&sha) {
                self.rebuild_entries(cx);
            } else {
                self.load_commit_files(sha, window, cx);
            }
        }
    }

    fn load_commit_files(&mut self, sha: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let commit_sha = sha.to_string();
        let rx = repo.update(cx, |repo, _cx| {
            repo.load_commit_diff(commit_sha)
        });

        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(commit_diff)) = rx.await {
                this.update(cx, |this, cx| {
                    this.commit_files.insert(sha.clone(), commit_diff.files);
                    this.rebuild_entries(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn update_visible_entries(&mut self, cx: &mut Context<Self>) {
        self.status_entries.clear();
        self.history_entries.clear();
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
            self.status_entries.push(VsGitListEntry::GroupHeader {
                group: ChangeGroup::MergeConflicts,
                count: conflicts.len(),
            });
            for entry in conflicts {
                self.status_entries.push(VsGitListEntry::FileEntry {
                    repo_path: entry.repo_path,
                    status: entry.status,
                    staging: entry.status.staging(),
                    group: ChangeGroup::MergeConflicts,
                });
            }
        }

        if !staged.is_empty() {
            self.staged_count = staged.len();
            self.status_entries.push(VsGitListEntry::GroupHeader {
                group: ChangeGroup::StagedChanges,
                count: staged.len(),
            });
            for entry in staged {
                self.status_entries.push(VsGitListEntry::FileEntry {
                    repo_path: entry.repo_path,
                    status: entry.status,
                    staging: StageStatus::Staged,
                    group: ChangeGroup::StagedChanges,
                });
            }
        }

        if !unstaged.is_empty() {
            self.unstaged_count = unstaged.len();
            self.status_entries.push(VsGitListEntry::GroupHeader {
                group: ChangeGroup::Changes,
                count: unstaged.len(),
            });
            for entry in unstaged {
                self.status_entries.push(VsGitListEntry::FileEntry {
                    repo_path: entry.repo_path,
                    status: entry.status,
                    staging: StageStatus::Unstaged,
                    group: ChangeGroup::Changes,
                });
            }
        }

        // Build history entries separately
        if !self.commit_entries.is_empty() {
            self.history_entries.push(VsGitListEntry::HistoryHeader);
            if !self.history_collapsed {
                for commit in &self.commit_entries {
                    let expanded = self.expanded_commits.contains(&commit.sha);
                    self.history_entries.push(VsGitListEntry::CommitEntry {
                        sha: commit.sha.clone(),
                        subject: commit.subject.clone(),
                        author: commit.author_name.clone(),
                        timestamp: commit.commit_timestamp,
                        expanded,
                    });
                    if expanded {
                        if let Some(files) = self.commit_files.get(&commit.sha) {
                            for file in files {
                                self.history_entries.push(VsGitListEntry::CommitFileEntry {
                                    sha: commit.sha.clone(),
                                    path: file.path.clone(),
                                    status: file.status(),
                                });
                            }
                        }
                    }
                }
                if self.has_more_commits {
                    self.history_entries.push(VsGitListEntry::LoadMoreButton);
                }
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

    fn find_existing_file_diff(
        &self,
        project_path: &ProjectPath,
        diff_kind: crate::vs_file_diff_view::VsDiffKind,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(workspace) = self.workspace.upgrade() else {
            return false;
        };
        let pane = workspace.read(cx).active_pane().clone();
        let found = pane.read(cx).items().enumerate().find(|(_, item)| {
            if let Some(diff_view) = item.act_as::<crate::vs_file_diff_view::VsFileDiffView>(cx) {
                let dv = diff_view.read(cx);
                dv.project_path.as_ref() == Some(project_path) && dv.diff_kind == diff_kind
            } else {
                false
            }
        }).map(|(ix, _)| ix);

        if let Some(ix) = found {
            pane.update(cx, |pane, cx| {
                pane.activate_item(ix, true, true, window, cx);
            });
            true
        } else {
            false
        }
    }

    fn find_existing_commit_diff(
        &self,
        full_path: &str,
        sha_short: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(workspace) = self.workspace.upgrade() else {
            return false;
        };
        let pane = workspace.read(cx).active_pane().clone();
        let found = pane.read(cx).items().enumerate().find(|(_, item)| {
            if let Some(diff_view) = item.act_as::<crate::vs_commit_diff_view::VsCommitDiffView>(cx) {
                let dv = diff_view.read(cx);
                dv.full_path.as_ref() == full_path && dv.sha_short.as_ref() == sha_short
            } else {
                false
            }
        }).map(|(ix, _)| ix);

        if let Some(ix) = found {
            pane.update(cx, |pane, cx| {
                pane.activate_item(ix, true, true, window, cx);
            });
            true
        } else {
            false
        }
    }

    fn repo_display_name(&self, cx: &App) -> Option<SharedString> {
        self.active_repository
            .as_ref()
            .map(|repo| repo.read(cx).display_name())
    }

    fn all_repositories(&self, cx: &App) -> Vec<(SharedString, Entity<Repository>)> {
        let git_store = self.project.read(cx).git_store();
        let repos = git_store.read(cx).repositories();
        let mut result: Vec<_> = repos
            .values()
            .map(|repo| (repo.read(cx).display_name(), repo.clone()))
            .collect();
        result.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        result
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
            .status_entries
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
            .status_entries
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
        if self.status_entries.is_empty() {
            return;
        }
        let next = match self.selected_entry {
            Some(ix) => (ix + 1).min(self.status_entries.len() - 1),
            None => 0,
        };
        self.selected_entry = Some(next);
        self.status_scroll_handle.scroll_to_item(next, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, _window: &mut Window, cx: &mut Context<Self>) {
        if self.status_entries.is_empty() {
            return;
        }
        let prev = match self.selected_entry {
            Some(ix) => ix.saturating_sub(1),
            None => 0,
        };
        self.selected_entry = Some(prev);
        self.status_scroll_handle.scroll_to_item(prev, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm_selection(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_entry else {
            return;
        };
        let Some(entry) = self.status_entries.get(ix) else {
            return;
        };
        if let VsGitListEntry::FileEntry {
            repo_path, status, group, ..
        } = entry
        {
            self.open_file_diff(repo_path.clone(), *status, *group, window, cx);
        }
    }

    fn open_file(
        &mut self,
        repo_path: RepoPath,
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
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace
                    .open_path_preview(project_path, None, true, false, true, window, cx)
                    .detach_and_log_err(cx);
            });
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

        // Check if this diff is already open
        let diff_kind = match group {
            ChangeGroup::StagedChanges => crate::vs_file_diff_view::VsDiffKind::Staged,
            _ => crate::vs_file_diff_view::VsDiffKind::Unstaged,
        };
        if self.find_existing_file_diff(&project_path, diff_kind, window, cx) {
            return;
        }

        let workspace = self.workspace.clone();
        let project = self.project.clone();

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

    fn render_branch_indicator(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let branch = self.branch_name(cx);
        let all_repos = self.all_repositories(cx);
        let has_multiple_repos = all_repos.len() > 1;
        let repo_name = self.repo_display_name(cx).unwrap_or("repo".into());
        let has_worktrees = self.git_worktrees.len() > 1;

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
            .when(has_multiple_repos, |el| {
                let menu = ContextMenu::build(window, cx, {
                    let all_repos = all_repos.clone();
                    let active_name = repo_name.clone();
                    move |mut menu, _window, _cx| {
                        for (name, repo) in all_repos.iter() {
                            let name = name.clone();
                            let repo = repo.clone();
                            let is_active = name == active_name;
                            let render_name = name.clone();
                            menu = menu.custom_entry(
                                move |_window, _cx| {
                                    h_flex()
                                        .gap_1()
                                        .when(is_active, |el| {
                                            el.child(
                                                Icon::new(IconName::Check)
                                                    .size(IconSize::XSmall)
                                                    .color(Color::Accent),
                                            )
                                        })
                                        .when(!is_active, |el| {
                                            el.child(div().w(px(14.)))
                                        })
                                        .child(
                                            Label::new(render_name.clone())
                                                .size(LabelSize::Small)
                                                .color(if is_active { Color::Accent } else { Color::Default }),
                                        )
                                        .into_any_element()
                                },
                                {
                                    let repo = repo.clone();
                                    move |_window, cx| {
                                        repo.update(cx, |repo, cx| {
                                            repo.set_as_active_repository(cx);
                                        });
                                    }
                                },
                            );
                        }
                        menu
                    }
                });
                el.child(
                    DropdownMenu::new("repo-selector", repo_name, menu)
                        .style(DropdownStyle::Ghost)
                        .trigger_size(ui::ButtonSize::Compact)
                        .attach(gpui::Corner::BottomLeft)
                )
                .child(
                    Label::new("/")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .when(has_worktrees, |el| {
                let worktrees = self.git_worktrees.clone();
                let current_branch = branch.clone();
                let workspace = self.workspace.clone();
                let menu = ContextMenu::build(window, cx, {
                    move |mut menu, _window, _cx| {
                        for wt in worktrees.iter() {
                            let ref_name: SharedString = wt.ref_name.clone()
                                .unwrap_or_else(|| "(detached)".into());
                            let is_current = ref_name == current_branch;
                            let display_name = ref_name.clone();
                            let wt_path = wt.path.clone();
                            let workspace = workspace.clone();
                            menu = menu.custom_entry(
                                move |_window, _cx| {
                                    h_flex()
                                        .gap_1()
                                        .when(is_current, |el| {
                                            el.child(
                                                Icon::new(IconName::Check)
                                                    .size(IconSize::XSmall)
                                                    .color(Color::Accent),
                                            )
                                        })
                                        .when(!is_current, |el| {
                                            el.child(div().w(px(14.)))
                                        })
                                        .child(
                                            Label::new(display_name.clone())
                                                .size(LabelSize::Small)
                                                .color(if is_current { Color::Accent } else { Color::Default }),
                                        )
                                        .into_any_element()
                                },
                                {
                                    let wt_path = wt_path.clone();
                                    let workspace = workspace.clone();
                                    move |window, cx| {
                                        if let Some(workspace) = workspace.upgrade() {
                                            let is_local = workspace.update(cx, |workspace, cx| {
                                                workspace.project().read(cx).is_local()
                                            });
                                            if is_local {
                                                let open_task = workspace.update(cx, |workspace, cx| {
                                                    workspace.open_workspace_for_paths(
                                                        workspace::OpenMode::NewWindow,
                                                        vec![wt_path.clone()],
                                                        window,
                                                        cx,
                                                    )
                                                });
                                                window.spawn(cx, async move |_cx| {
                                                    open_task.await?;
                                                    anyhow::Ok(())
                                                }).detach_and_log_err(cx);
                                            } else {
                                                let connection_and_state = workspace.update(cx, |workspace, cx| {
                                                    let connection_options = workspace.project().read(cx).remote_connection_options(cx);
                                                    let app_state = workspace.app_state().clone();
                                                    (connection_options, app_state)
                                                });
                                                if let Some(connection_options) = connection_and_state.0 {
                                                    let app_state = connection_and_state.1;
                                                    let wt_path = wt_path.clone();
                                                    window.spawn(cx, async move |mut cx| {
                                                        recent_projects::open_remote_project(
                                                            connection_options,
                                                            vec![wt_path],
                                                            app_state,
                                                            workspace::OpenOptions {
                                                                open_mode: workspace::OpenMode::NewWindow,
                                                                ..Default::default()
                                                            },
                                                            &mut cx,
                                                        ).await
                                                    }).detach_and_log_err(cx);
                                                }
                                            }
                                        }
                                    }
                                },
                            );
                        }
                        menu
                    }
                });
                el.child(
                    DropdownMenu::new("worktree-selector", branch.clone(), menu)
                        .style(DropdownStyle::Ghost)
                        .trigger_size(ui::ButtonSize::Compact)
                        .attach(gpui::Corner::BottomLeft)
                )
            })
            .when(!has_worktrees, |el| {
                el.child(
                    Label::new(branch)
                        .size(LabelSize::Small)
                        .color(Color::Default),
                )
            })
    }

    fn render_entries(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entry_count = self.status_entries.len();

        uniform_list(
            "vs-git-entries",
            entry_count,
            cx.processor(
                move |this: &mut Self, range: std::ops::Range<usize>, window, cx| {
                    range
                        .map(|ix| {
                            let entry = &this.status_entries[ix];
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
                                _ => gpui::Empty.into_any_element(),
                            }
                        })
                        .collect()
                },
            ),
        )
        .flex_shrink()
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .track_scroll(&self.status_scroll_handle)
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
                    if group == ChangeGroup::MergeConflicts {
                        this.open_file(click_path.clone(), window, cx);
                    } else {
                        this.open_file_diff(click_path.clone(), status, group, window, cx);
                    }
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

        // Open file button (for all groups)
        {
            let open_path = repo_path.clone();
            row = row.child(
                IconButton::new(
                    ElementId::NamedInteger("open-file".into(), ix_u64),
                    IconName::File,
                )
                .icon_size(IconSize::XSmall)
                .tooltip(Tooltip::text("Open File"))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_file(open_path.clone(), window, cx);
                })),
            );
        }

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
            ChangeGroup::MergeConflicts => {
                let stage_path = repo_path.clone();
                row = row.child(
                    IconButton::new(
                        ElementId::NamedInteger("stage-conflict".into(), ix_u64),
                        IconName::Plus,
                    )
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Stage (Mark as Resolved)"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.stage_file(stage_path.clone(), cx);
                    })),
                );
            }
        }

        row
    }

    fn open_commit_file_diff(
        &mut self,
        sha: SharedString,
        path: RepoPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.commit_files.get(&sha) else {
            return;
        };
        let Some(file) = files.iter().find(|f| f.path == path) else {
            return;
        };
        let full_path: SharedString = path.as_unix_str().to_string().into();
        let filename: SharedString = path
            .as_ref()
            .file_name()
            .unwrap_or("")
            .to_string()
            .into();
        let sha_short: SharedString = sha[..7.min(sha.len())].to_string().into();

        // Check if this diff is already open
        if self.find_existing_commit_diff(&full_path, &sha_short, window, cx) {
            return;
        }

        let old_text = file.old_text.clone().unwrap_or_default();
        let new_text = file.new_text.clone().unwrap_or_default();
        let workspace = self.workspace.clone();
        let project_path = self.active_repository.as_ref()
            .and_then(|repo| repo.read(cx).repo_path_to_project_path(&path, cx));

        let diff_view = cx.new(|cx| {
            crate::vs_commit_diff_view::VsCommitDiffView::new(
                old_text,
                new_text,
                full_path,
                filename,
                sha_short,
                project_path,
                window,
                cx,
            )
        });

        if let Some(workspace) = workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                let pane = workspace.active_pane();
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(diff_view), true, true, None, window, cx);
                });
            });
        }
    }

    fn render_compared_section(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base_branch = self.base_branch.clone().unwrap_or("?".into());
        let file_count = self.compared_files.len();
        let collapsed = self.compared_collapsed;

        v_flex()
            .w_full()
            .child(
                h_flex()
                    .id("compared-header")
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_1()
                    .bg(cx.theme().colors().surface_background)
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.compared_collapsed = !this.compared_collapsed;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_grow()
                            .child(
                                Label::new(format!("Compared to {}", base_branch))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .weight(gpui::FontWeight::BOLD),
                            )
                            .child(
                                Label::new(format!("({} files)", file_count))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    ),
            )
            .when(!collapsed, |el| {
                let file_elements: Vec<_> = self.compared_files.iter().enumerate().map(|(ix, (path, status))| {
                    self.render_compared_file_entry(ix, path.clone(), *status, cx).into_any_element()
                }).collect();
                el.children(file_elements)
            })
    }

    fn render_compared_file_entry(
        &self,
        ix: usize,
        path: RepoPath,
        status: CommitFileStatus,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path_ref: &std::sync::Arc<util::rel_path::RelPath> = path.as_ref();
        let filename = path_ref.file_name().unwrap_or("").to_string();
        let parent = path_ref
            .parent()
            .map(|p: &util::rel_path::RelPath| {
                p.display(util::paths::PathStyle::Posix).to_string()
            })
            .filter(|p: &String| !p.is_empty());

        let (status_letter, status_color) = match status {
            CommitFileStatus::Added => ("A", Color::Created),
            CommitFileStatus::Modified => ("M", Color::Modified),
            CommitFileStatus::Deleted => ("D", Color::Deleted),
        };

        let base_branch = self.base_branch.clone().unwrap_or("main".into());

        h_flex()
            .id(ElementId::NamedInteger("compared-file".into(), ix as u64))
            .w_full()
            .pl(px(20.))
            .pr_2()
            .py_0p5()
            .gap_1()
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .cursor_pointer()
            .on_click({
                let click_path = path.clone();
                let click_base = base_branch.clone();
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_compared_file_diff(click_base.clone(), click_path.clone(), window, cx);
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
                Label::new(status_letter)
                    .size(LabelSize::Small)
                    .color(status_color),
            )
    }

    fn open_compared_file_diff(
        &mut self,
        base_ref: SharedString,
        path: RepoPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        let full_path: SharedString = path.as_unix_str().to_string().into();
        let filename: SharedString = path.as_ref().file_name().unwrap_or("").to_string().into();
        let base_display: SharedString = base_ref.split('/').last()
            .unwrap_or(base_ref.as_ref()).to_string().into();

        // Check if this diff is already open
        if self.find_existing_commit_diff(&full_path, &base_display, window, cx) {
            return;
        }

        let workspace = self.workspace.clone();
        let project_path = repo.read(cx).repo_path_to_project_path(&path, cx);
        let rx = repo.update(cx, |repo, _cx| {
            repo.diff_file_text(base_ref.to_string(), path)
        });

        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok((old_text, new_text))) = rx.await {
                this.update_in(cx, |_this, window, cx| {
                    let diff_view = cx.new(|cx| {
                        crate::vs_commit_diff_view::VsCommitDiffView::new(
                            old_text,
                            new_text,
                            full_path,
                            filename,
                            base_display,
                            project_path,
                            window,
                            cx,
                        )
                    });
                    if let Some(workspace) = workspace.upgrade() {
                        workspace.update(cx, |workspace, cx| {
                            let pane = workspace.active_pane();
                            pane.update(cx, |pane, cx| {
                                pane.add_item(Box::new(diff_view), true, true, None, window, cx);
                            });
                        });
                    }
                }).ok();
            }
        })
        .detach();
    }

    fn render_history_header_standalone(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.commit_entries.len();
        let collapsed = self.history_collapsed;
        h_flex()
            .id("history-header-standalone")
            .w_full()
            .px_2()
            .py_0p5()
            .gap_1()
            .bg(cx.theme().colors().surface_background)
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.history_collapsed = !this.history_collapsed;
                this.rebuild_entries(cx);
            }))
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_grow()
                    .child(
                        Label::new("Commit History")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .weight(gpui::FontWeight::BOLD),
                    )
                    .child(
                        Label::new(format!("({})", count))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
    }

    fn render_history_list(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Build a flat list of just commits and their files (no header)
        let mut list_entries: Vec<VsGitListEntry> = Vec::new();
        for commit in &self.commit_entries {
            let expanded = self.expanded_commits.contains(&commit.sha);
            list_entries.push(VsGitListEntry::CommitEntry {
                sha: commit.sha.clone(),
                subject: commit.subject.clone(),
                author: commit.author_name.clone(),
                timestamp: commit.commit_timestamp,
                expanded,
            });
            if expanded {
                if let Some(files) = self.commit_files.get(&commit.sha) {
                    for file in files {
                        list_entries.push(VsGitListEntry::CommitFileEntry {
                            sha: commit.sha.clone(),
                            path: file.path.clone(),
                            status: file.status(),
                        });
                    }
                }
            }
        }
        if self.has_more_commits {
            list_entries.push(VsGitListEntry::LoadMoreButton);
        }

        let entry_count = list_entries.len();

        // Store in a shared ref for the closure
        let list_entries = std::sync::Arc::new(list_entries);

        uniform_list(
            "vs-git-history-list",
            entry_count,
            cx.processor({
                let list_entries = list_entries.clone();
                move |this: &mut Self, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .map(|ix| {
                            let entry = &list_entries[ix];
                            match entry {
                                VsGitListEntry::CommitEntry {
                                    sha,
                                    subject,
                                    author,
                                    timestamp,
                                    expanded,
                                } => this
                                    .render_commit_entry(
                                        ix,
                                        sha.clone(),
                                        subject.clone(),
                                        author.clone(),
                                        *timestamp,
                                        *expanded,
                                        cx,
                                    )
                                    .into_any_element(),
                                VsGitListEntry::CommitFileEntry { sha, path, status } => this
                                    .render_commit_file_entry(
                                        ix,
                                        sha.clone(),
                                        path.clone(),
                                        *status,
                                        cx,
                                    )
                                    .into_any_element(),
                                VsGitListEntry::LoadMoreButton => this
                                    .render_load_more(cx)
                                    .into_any_element(),
                                _ => gpui::Empty.into_any_element(),
                            }
                        })
                        .collect()
                }
            }),
        )
        .flex_grow()
        .with_sizing_behavior(ListSizingBehavior::Auto)
        .track_scroll(&self.history_scroll_handle)
    }

    fn render_commit_entry(
        &self,
        ix: usize,
        sha: SharedString,
        subject: SharedString,
        author: SharedString,
        timestamp: i64,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = self.selected_entry == Some(ix);
        let time_str = format_relative_time(timestamp);
        let sha_short: SharedString = sha[..7.min(sha.len())].to_string().into();

        h_flex()
            .id(ElementId::NamedInteger("commit-entry".into(), ix as u64))
            .w_full()
            .px_2()
            .py_0p5()
            .gap_1()
            .when(is_selected, |el| {
                el.bg(cx.theme().colors().ghost_element_selected)
            })
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .on_click({
                let click_sha = sha.clone();
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.selected_entry = Some(ix);
                    this.toggle_commit(click_sha.clone(), window, cx);
                })
            })
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                h_flex()
                    .flex_grow()
                    .overflow_x_hidden()
                    .gap_1()
                    .child(
                        Label::new(subject)
                            .size(LabelSize::Small)
                            .color(Color::Default)
                            .single_line(),
                    ),
            )
            .child(
                Label::new(sha_short)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .child(
                Label::new(author)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .single_line(),
            )
            .child(
                Label::new(time_str)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
    }

    fn render_commit_file_entry(
        &self,
        ix: usize,
        _sha: SharedString,
        path: RepoPath,
        status: CommitFileStatus,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = self.selected_entry == Some(ix);
        let path_ref: &std::sync::Arc<util::rel_path::RelPath> = path.as_ref();
        let filename = path_ref.file_name().unwrap_or("").to_string();
        let parent = path_ref
            .parent()
            .map(|p: &util::rel_path::RelPath| {
                p.display(util::paths::PathStyle::Posix).to_string()
            })
            .filter(|p: &String| !p.is_empty());

        let (status_letter, status_color) = match status {
            CommitFileStatus::Added => ("A", Color::Created),
            CommitFileStatus::Modified => ("M", Color::Modified),
            CommitFileStatus::Deleted => ("D", Color::Deleted),
        };

        h_flex()
            .id(ElementId::NamedInteger("commit-file".into(), ix as u64))
            .w_full()
            .pl(px(24.))
            .pr_2()
            .py_0p5()
            .gap_1()
            .when(is_selected, |el| {
                el.bg(cx.theme().colors().ghost_element_selected)
            })
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .on_click({
                let click_sha = _sha.clone();
                let click_path = path.clone();
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.selected_entry = Some(ix);
                    this.open_commit_file_diff(click_sha.clone(), click_path.clone(), window, cx);
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
                Label::new(status_letter)
                    .size(LabelSize::Small)
                    .color(status_color),
            )
    }

    fn render_load_more(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("load-more-commits")
            .w_full()
            .px_2()
            .py_1()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                let skip = this.commit_entries.len();
                this.load_history(skip, window, cx);
            }))
            .child(
                Label::new("Load More...")
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
    }

    fn render_empty_state(&self, _cx: &App) -> impl IntoElement {
        v_flex().size_full().justify_center().items_center().child(
            Label::new("No changes")
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
    }
}

fn format_relative_time(timestamp: i64) -> SharedString {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - timestamp;
    if diff < 60 {
        "just now".into()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60).into()
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600).into()
    } else if diff < 604800 {
        format!("{}d ago", diff / 86400).into()
    } else if diff < 2592000 {
        format!("{}w ago", diff / 604800).into()
    } else {
        format!("{}mo ago", diff / 2592000).into()
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
        let has_status = !self.status_entries.is_empty();
        let has_history = !self.history_entries.is_empty();
        let has_compared = !self.compared_files.is_empty();
        let has_bottom_section = has_history || has_compared;

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
            .child(self.render_branch_indicator(window, cx))
            .relative()
            .when(has_status, |el| {
                el.child(self.render_entries(window, cx))
            })
            .when(!has_status && !has_bottom_section, |el| {
                el.child(self.render_empty_state(cx))
            })
            .on_drag_move::<DraggedHistoryHandle>(
                cx.listener(|this, event: &gpui::DragMoveEvent<DraggedHistoryHandle>, _window, _cx| {
                    let bounds = event.bounds;
                    let drag_y = event.event.position.y;
                    let bounds_height = bounds.bottom() - bounds.top();
                    if bounds_height > px(0.) {
                        // drag_y is from top; we want ratio from bottom
                        let ratio_from_bottom = ((bounds.bottom() - drag_y) / bounds_height).clamp(0.1, 0.8);
                        this.history_height = ratio_from_bottom;
                    }
                }),
            )
            .when(has_bottom_section, |el| {
                let history_collapsed = self.history_collapsed;
                let history_height = self.history_height;
                el.child(
                    v_flex()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .when(history_collapsed && !has_compared, |el| el)
                        .when(!history_collapsed || has_compared, |el| el.h(relative(history_height)))
                        .bg(cx.theme().colors().panel_background)
                        // Drag handle border (only when expanded)
                        .when(!history_collapsed || has_compared, |el| {
                            el.child(
                                div()
                                    .id("history-resize-handle")
                                    .w_full()
                                    .h(px(4.))
                                    .mt(px(-2.))
                                    .cursor_row_resize()
                                    .on_drag(DraggedHistoryHandle, |_, _, _, cx| cx.new(|_| gpui::Empty))
                                    .hover(|style| style.bg(cx.theme().colors().border))
                            )
                        })
                        // Compared to section
                        .when(has_compared, |el| {
                            el.child(self.render_compared_section(window, cx))
                        })
                        // History header
                        .when(has_history, |el| {
                            el.child(self.render_history_header_standalone(cx))
                        })
                        // History content (hidden when collapsed)
                        .when(has_history && !history_collapsed, |el| {
                            el.child(
                                v_flex()
                                    .flex_grow()
                                    .min_h_0()
                                    .overflow_hidden()
                                    .child(self.render_history_list(window, cx)),
                            )
                        }),
                )
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
