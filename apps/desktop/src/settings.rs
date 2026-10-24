//! UI-facing names for the shared per-user host preferences.
pub use omdrop_host::{
    DEFAULT_NOTIFICATION_DURATION_SECONDS, MAX_NAME_CHARS, NOTIFICATION_DURATION_OPTIONS,
    NotificationPosition, default_name, load_desktop_notifications_enabled, load_language,
    load_name as load, load_notification_duration_seconds, load_notification_position,
    save_desktop_notifications_enabled_to, save_language_to, save_name_to as save_to,
    save_notification_duration_seconds_to, save_notification_position_to, settings_path as path,
};
