mod vs_git_panel;
mod vs_git_panel_settings;

use gpui::App;
use workspace::Workspace;

pub use vs_git_panel::VsGitPanel;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _cx| {
        vs_git_panel::register(workspace);
    })
    .detach();
}
