use gpui::{
    AnyElement, ScrollHandle,
    prelude::*,
};
use ui::prelude::*;

use crate::SettingsWindow;

pub(crate) fn render_tool_permissions_setup_page(
    _settings_window: &SettingsWindow,
    _scroll_handle: &ScrollHandle,
    _window: &mut Window,
    _cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    div().into_any_element()
}

macro_rules! tool_config_page_fn {
    ($fn_name:ident, $tool_id:literal) => {
        pub fn $fn_name(
            _settings_window: &SettingsWindow,
            _scroll_handle: &ScrollHandle,
            _window: &mut Window,
            _cx: &mut Context<SettingsWindow>,
        ) -> AnyElement {
            div().into_any_element()
        }
    };
}

tool_config_page_fn!(render_terminal_tool_config, "terminal");
tool_config_page_fn!(render_edit_file_tool_config, "edit_file");
tool_config_page_fn!(render_delete_path_tool_config, "delete_path");
tool_config_page_fn!(render_copy_path_tool_config, "copy_path");
tool_config_page_fn!(render_move_path_tool_config, "move_path");
tool_config_page_fn!(render_create_directory_tool_config, "create_directory");
tool_config_page_fn!(render_save_file_tool_config, "save_file");
tool_config_page_fn!(render_fetch_tool_config, "fetch");
tool_config_page_fn!(render_web_search_tool_config, "web_search");
tool_config_page_fn!(
    render_restore_file_from_disk_tool_config,
    "restore_file_from_disk"
);
