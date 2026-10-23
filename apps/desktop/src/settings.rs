//! UI-facing names for the shared per-user host preferences.
pub use omdrop_host::{
    MAX_NAME_CHARS, default_name, load_language, load_name as load, save_language_to,
    save_name_to as save_to, settings_path as path,
};
