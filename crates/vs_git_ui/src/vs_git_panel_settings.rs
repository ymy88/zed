use gpui::Pixels;
use ui::px;
use workspace::dock::DockPosition;

pub struct VsGitPanelSettings {
    pub dock: DockPosition,
    pub default_width: Pixels,
}

impl Default for VsGitPanelSettings {
    fn default() -> Self {
        Self {
            dock: DockPosition::Left,
            default_width: px(360.0),
        }
    }
}

impl VsGitPanelSettings {
    pub fn get_global() -> Self {
        Self::default()
    }
}
