//! UI-facing names for the shared per-user host preferences.
pub use wayline_host::{
    DEFAULT_AUTO_OPEN_DOWNLOAD_DIR, DEFAULT_DRAG_ACTIVATE, DEFAULT_NOTIFICATION_DURATION_SECONDS,
    DEFAULT_PINNED, MAX_NAME_CHARS, NOTIFICATION_DURATION_OPTIONS, NotificationPosition,
    default_name, load_auto_open_download_dir, load_desktop_notifications_enabled,
    load_drag_activate, load_language, load_name as load, load_notification_duration_seconds,
    load_notification_position, load_pinned, save_auto_open_download_dir,
    save_desktop_notifications_enabled_to, save_drag_activate, save_language_to,
    save_name_to as save_to, save_notification_duration_seconds_to, save_notification_position_to,
    save_pinned, settings_path as path,
};

/// Commit the persisted value to the UI only after the save succeeds.
pub fn toggle(
    current: &mut bool,
    save: impl FnOnce(bool) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    *current = save(!*current)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_preference_save_preserves_ui_and_disk_and_can_be_retried() {
        for save in [
            wayline_host::save_drag_activate_to
                as fn(&std::path::Path, bool) -> anyhow::Result<bool>,
            wayline_host::save_auto_open_download_dir_to,
            wayline_host::save_pinned_to,
        ] {
            for initial in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("desktop.json");
                std::fs::write(&path, b"{").unwrap();
                let mut current = initial;
                assert!(toggle(&mut current, |value| save(&path, value)).is_err());
                assert_eq!(current, initial);
                assert_eq!(std::fs::read(&path).unwrap(), b"{");

                std::fs::write(&path, b"{}").unwrap();
                toggle(&mut current, |value| save(&path, value)).unwrap();
                assert_eq!(current, !initial);
                let settings: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                assert_eq!(
                    settings.as_object().unwrap().values().next().unwrap(),
                    !initial
                );
            }
        }
    }
}
