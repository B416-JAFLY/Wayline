use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    process::ExitStatus,
};

pub const LOCALES_DIR: &str = "crates/wayline-tools/locales";
pub const ACTION_COUNT: usize = 7;
pub const FIELD_COUNT: usize = 13;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Chinese,
    English,
}

impl Language {
    pub fn toggle(&mut self) {
        *self = match self {
            Self::Chinese => Self::English,
            Self::English => Self::Chinese,
        };
    }

    fn code(self) -> &'static str {
        match self {
            Self::Chinese => "zh-CN",
            Self::English => "en-US",
        }
    }
}

#[derive(Debug)]
pub struct Catalogs {
    chinese: Strings,
    english: Strings,
}

impl Catalogs {
    pub fn load(directory: &Path) -> Result<Self> {
        let load = |language: Language| {
            let path = directory.join(format!("{}.toml", language.code()));
            let source = fs::read_to_string(&path)
                .with_context(|| format!("read TUI language file {}", path.display()))?;
            toml::from_str(&source)
                .with_context(|| format!("parse TUI language file {}", path.display()))
        };
        // Validate both languages before entering raw mode. Missing files or keys
        // and incorrect array lengths are errors, never partial translations.
        Ok(Self {
            chinese: load(Language::Chinese)?,
            english: load(Language::English)?,
        })
    }

    pub fn get(&self, language: Language) -> &Strings {
        match language {
            Language::Chinese => &self.chinese,
            Language::English => &self.english,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strings {
    pub actions: [String; ACTION_COUNT],
    pub fields: [String; FIELD_COUNT],
    pub hints: [String; ACTION_COUNT],
    pub help: String,
    pub ui: UiStrings,
    pub status: StatusStrings,
    pub notice: NoticeStrings,
    pub error: ErrorStrings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiStrings {
    pub title: String,
    pub host: String,
    pub tasks: String,
    pub settings: String,
    pub command: String,
    pub logs: String,
    pub following: String,
    pub browsing: String,
    pub retained: String,
    pub omitted: String,
    pub footer: String,
    pub editor: String,
    pub resize: String,
    pub small_footer: String,
    pub enabled: String,
    pub disabled: String,
    pub automatic: String,
    pub default_value: String,
    pub log_snapshot: String,
    pub older_lines: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusStrings {
    pub idle: String,
    pub running: String,
    pub cancelling_quit: String,
    pub cancelling: String,
    pub cancelled: String,
    pub success: String,
    pub failed: String,
    pub error: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeStrings {
    pub navigation: String,
    pub running: String,
    pub edit_cancelled: String,
    pub updated: String,
    pub edit_help: String,
    pub locked: String,
    pub busy: String,
    pub log_saved: String,
    pub preview: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorStrings {
    pub invalid_text: String,
    pub required: String,
    pub terminal_required: String,
    pub resolve_root: String,
    pub read_target: String,
    pub read_target_failed: String,
    pub missing_target: String,
    pub initialize_terminal: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    InvalidText,
    Required,
}

impl InputError {
    pub fn text(self, strings: &Strings) -> &str {
        match self {
            Self::InvalidText => &strings.error.invalid_text,
            Self::Required => &strings.error.required,
        }
    }
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for InputError {}

pub fn validate_text(text: &str) -> Result<(), InputError> {
    if text.len() > 4096 || text.chars().any(char::is_control) {
        Err(InputError::InvalidText)
    } else {
        Ok(())
    }
}

// Store semantic state so switching language also updates messages already on screen.
pub enum Status {
    Idle,
    Running(usize),
    Cancelling(bool),
    Cancelled,
    Success,
    Failed(ExitStatus),
    Error,
}

impl Status {
    pub fn text(&self, strings: &Strings) -> String {
        let status = &strings.status;
        match self {
            Self::Idle => status.idle.clone(),
            Self::Running(action) => format!("{} · {}", status.running, strings.actions[*action]),
            Self::Cancelling(true) => status.cancelling_quit.clone(),
            Self::Cancelling(false) => status.cancelling.clone(),
            Self::Cancelled => status.cancelled.clone(),
            Self::Success => status.success.clone(),
            Self::Failed(code) => format!("{} · {code}", status.failed),
            Self::Error => status.error.clone(),
        }
    }
}

pub enum Notice {
    Navigation,
    Running,
    EditCancelled,
    Updated,
    EditHelp,
    Locked,
    Busy,
    LogSaved(PathBuf),
    InputError(InputError),
    Error(String),
    Preview,
}

impl Notice {
    pub fn text(&self, strings: &Strings) -> String {
        let notice = &strings.notice;
        match self {
            Self::Navigation => notice.navigation.clone(),
            Self::Running => notice.running.clone(),
            Self::EditCancelled => notice.edit_cancelled.clone(),
            Self::Updated => notice.updated.clone(),
            Self::EditHelp => notice.edit_help.clone(),
            Self::Locked => notice.locked.clone(),
            Self::Busy => notice.busy.clone(),
            Self::LogSaved(path) => format!("{}: {}", notice.log_saved, path.display()),
            Self::InputError(error) => error.text(strings).into(),
            Self::Error(error) => error.clone(),
            Self::Preview => notice.preview.clone(),
        }
    }
}

#[cfg(test)]
pub fn test_catalogs() -> Catalogs {
    Catalogs::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("locales")).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> (String, String) {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        (
            fs::read_to_string(directory.join("zh-CN.toml")).unwrap(),
            fs::read_to_string(directory.join("en-US.toml")).unwrap(),
        )
    }

    #[test]
    fn disk_edits_are_loaded_on_the_next_start() {
        let directory = tempfile::tempdir().unwrap();
        let (zh, en) = sources();
        fs::write(directory.path().join("zh-CN.toml"), zh).unwrap();
        let path = directory.path().join("en-US.toml");
        fs::write(&path, &en).unwrap();
        let old = Catalogs::load(directory.path()).unwrap();
        let mut edited: toml::Value = toml::from_str(&en).unwrap();
        edited["ui"]["title"] = toml::Value::String("Custom title from disk".into());
        fs::write(&path, toml::to_string(&edited).unwrap()).unwrap();
        let new = Catalogs::load(directory.path()).unwrap();
        assert_ne!(
            old.get(Language::English).ui.title,
            new.get(Language::English).ui.title
        );
        assert_eq!(
            new.get(Language::English).ui.title,
            "Custom title from disk"
        );
    }

    #[test]
    fn missing_malformed_or_incomplete_language_files_report_the_path() {
        let directory = tempfile::tempdir().unwrap();
        let (zh, en) = sources();
        assert!(
            format!("{:#}", Catalogs::load(directory.path()).unwrap_err()).contains("zh-CN.toml")
        );
        fs::write(directory.path().join("zh-CN.toml"), zh).unwrap();
        assert!(
            format!("{:#}", Catalogs::load(directory.path()).unwrap_err()).contains("en-US.toml")
        );
        let original: toml::Value = toml::from_str(&en).unwrap();
        let mut missing = original.clone();
        missing["ui"].as_table_mut().unwrap().remove("title");
        let mut unknown = original.clone();
        unknown["ui"]
            .as_table_mut()
            .unwrap()
            .insert("unknown_key".into(), toml::Value::String("unknown".into()));
        let mut short_array = original;
        short_array["actions"].as_array_mut().unwrap().pop();
        for invalid in [
            "invalid = [".into(),
            toml::to_string(&missing).unwrap(),
            toml::to_string(&unknown).unwrap(),
            toml::to_string(&short_array).unwrap(),
        ] {
            fs::write(directory.path().join("en-US.toml"), invalid).unwrap();
            let error = Catalogs::load(directory.path()).unwrap_err();
            assert!(format!("{error:#}").contains("en-US.toml"), "{error:#}");
        }
    }
}
