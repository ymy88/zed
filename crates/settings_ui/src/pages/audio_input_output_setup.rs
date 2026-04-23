use gpui::{AnyElement, App, IntoElement, Window};
use settings::{AudioInputDeviceName, AudioOutputDeviceName};
use ui::prelude::*;

use crate::{SettingField, SettingsFieldMetadata, SettingsUiFile};

pub fn render_input_audio_device_dropdown(
    _field: SettingField<AudioInputDeviceName>,
    _file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    _cx: &mut App,
) -> AnyElement {
    Label::new("Audio devices not available")
        .color(Color::Muted)
        .into_any_element()
}

pub fn render_output_audio_device_dropdown(
    _field: SettingField<AudioOutputDeviceName>,
    _file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    _cx: &mut App,
) -> AnyElement {
    Label::new("Audio devices not available")
        .color(Color::Muted)
        .into_any_element()
}
