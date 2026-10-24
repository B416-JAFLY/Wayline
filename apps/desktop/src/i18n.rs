//! Desktop UI copy and the persisted language preference.
use omdrop_host::NameError;
use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// The desktop and firmware expose the same three interface languages.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    ZhCn,
    ZhTw,
    EnUs,
    JaJp,
    KoKr,
    FrFr,
    DeDe,
    EsEs,
}

impl Language {
    pub const ALL: [Self; 8] = [
        Self::ZhCn,
        Self::ZhTw,
        Self::EnUs,
        Self::JaJp,
        Self::KoKr,
        Self::FrFr,
        Self::DeDe,
        Self::EsEs,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::ZhTw => "zh-TW",
            Self::EnUs => "en-US",
            Self::JaJp => "ja-JP",
            Self::KoKr => "ko-KR",
            Self::FrFr => "fr-FR",
            Self::DeDe => "de-DE",
            Self::EsEs => "es-ES",
        }
    }

    pub const fn label(self) -> &'static str {
        self.strings().language_name
    }

    pub const fn font_family(self) -> &'static str {
        match self {
            Self::ZhCn => "Microsoft YaHei UI",
            Self::ZhTw => "Microsoft JhengHei UI",
            Self::JaJp => "Yu Gothic UI",
            Self::KoKr => "Malgun Gothic",
            Self::EnUs | Self::FrFr | Self::DeDe | Self::EsEs => "Segoe UI",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" | "zh-hans" | "zh-sg" => Some(Self::ZhCn),
            "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" => Some(Self::ZhTw),
            "en" | "en-us" | "en-gb" | "en-au" | "en-ca" => Some(Self::EnUs),
            "ja" | "ja-jp" => Some(Self::JaJp),
            "ko" | "ko-kr" => Some(Self::KoKr),
            "fr" | "fr-fr" | "fr-be" | "fr-ca" | "fr-ch" => Some(Self::FrFr),
            "de" | "de-de" | "de-at" | "de-ch" | "de-lu" => Some(Self::DeDe),
            "es" | "es-es" | "es-mx" | "es-419" => Some(Self::EsEs),
            _ => None,
        }
    }

    /// Windows UI language, with unsupported locales falling back to English.
    pub fn detect() -> Self {
        // SAFETY: GetUserDefaultUILanguage reads process-independent system data
        // and has no pointer or lifetime requirements.
        let language_id = unsafe { GetUserDefaultUILanguage() };
        let primary = language_id & 0x03ff;
        let sublanguage = (language_id >> 10) & 0x3f;
        match (primary, sublanguage) {
            (0x04, 0x01) | (0x04, 0x03) => Self::ZhTw,
            (0x04, _) => Self::ZhCn,
            (0x07, _) => Self::DeDe,
            (0x09, _) => Self::EnUs,
            (0x0a, _) => Self::EsEs,
            (0x0c, _) => Self::FrFr,
            (0x11, _) => Self::JaJp,
            (0x12, _) => Self::KoKr,
            _ => Self::EnUs,
        }
    }

    /// Read the saved preference; a missing value means the system language applies.
    pub fn load_preference() -> Result<Option<Self>, String> {
        match crate::settings::load_language().map_err(|error| format!("{error:#}"))? {
            Some(code) => Self::from_code(&code)
                .map(Some)
                .ok_or_else(|| format!("unsupported UI language code: {code}")),
            None => Ok(None),
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Language::ZhCn as u8);

pub fn set_current(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}

pub fn current() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        value if value == Language::ZhTw as u8 => Language::ZhTw,
        value if value == Language::EnUs as u8 => Language::EnUs,
        _ => Language::ZhCn,
    }
}

// The table is generated from locales/*.toml by build.rs.
include!(concat!(env!("OUT_DIR"), "/i18n_strings.rs"));

/// Replace named `{placeholders}` in a single pass so inserted values are never
/// interpreted as template syntax themselves.
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(relative_end) = rest[start + 1..].find('}') else {
            break;
        };
        let end = start + 1 + relative_end;
        output.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        if let Some((_, value)) = values.iter().find(|(key, _)| *key == name) {
            output.push_str(value);
        } else {
            output.push_str(&rest[start..=end]);
        }
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    output
}

impl Language {
    pub fn file_count(self, count: usize) -> String {
        fill(self.strings().file_count, &[("count", count.to_string())])
    }

    pub fn name_char_count(self, count: usize, max: usize) -> String {
        fill(
            self.strings().name_char_count,
            &[("count", count.to_string()), ("max", max.to_string())],
        )
    }

    pub fn transfer_files(self, sender: &str, files: usize) -> String {
        fill(
            self.strings().transfer_files,
            &[("sender", sender.into()), ("files", files.to_string())],
        )
    }

    pub fn saved_to(self, path: &str) -> String {
        format!("{}{path}", self.strings().saved_to_prefix)
    }

    pub fn received_size(self, size: &str) -> String {
        format!("{}{size}", self.strings().received_prefix)
    }

    pub fn history_summary(
        self,
        count: usize,
        file_size: &str,
        transferred_size: &str,
        duration: &str,
    ) -> String {
        fill(
            self.strings().history_summary,
            &[
                ("count", count.to_string()),
                ("file_size", file_size.into()),
                ("transferred_size", transferred_size.into()),
                ("duration", duration.into()),
            ],
        )
    }

    pub fn transfer_speed(self, speed: &str) -> String {
        fill(self.strings().transfer_speed, &[("speed", speed.into())])
    }

    pub fn transfer_elapsed(self, duration_ms: u64) -> String {
        fill(
            self.strings().transfer_elapsed,
            &[("duration", self.history_duration(duration_ms / 1000 * 1000))],
        )
    }

    pub fn history_average_speed(self, speed: &str) -> String {
        fill(
            self.strings().history_average_speed,
            &[("speed", speed.into())],
        )
    }

    pub fn history_duration(self, duration_ms: u64) -> String {
        let total_seconds = duration_ms.saturating_add(999) / 1000;
        if total_seconds < 60 {
            fill(
                self.strings().history_duration_seconds,
                &[("count", total_seconds.to_string())],
            )
        } else {
            fill(
                self.strings().history_duration_minutes,
                &[
                    ("minutes", (total_seconds / 60).to_string()),
                    ("seconds", (total_seconds % 60).to_string()),
                ],
            )
        }
    }

    pub fn incoming_ask(self, sender: &str, files: usize) -> String {
        fill(
            self.strings().incoming_ask,
            &[("sender", sender.into()), ("files", files.to_string())],
        )
    }

    pub fn accepted_save_to(self, path: &str) -> String {
        fill(self.strings().accepted_save_to, &[("path", path.into())])
    }

    pub fn received_notice(self, sender: &str, files: usize, path: &str) -> String {
        fill(
            self.strings().received_notice,
            &[
                ("sender", sender.into()),
                ("files", files.to_string()),
                ("path", path.into()),
            ],
        )
    }

    pub fn firmware_package_version(self, version: Option<&str>) -> String {
        let version = version
            .unwrap_or(self.strings().firmware_not_found)
            .to_owned();
        fill(
            self.strings().firmware_package_version,
            &[("version", version)],
        )
    }

    pub fn cannot_open_file(self, name: &str) -> String {
        fill(self.strings().cannot_open_file, &[("name", name.into())])
    }

    pub fn name_error(self, error: NameError) -> String {
        let text = self.strings();
        match error {
            NameError::ControlCharacter => text.name_error_control.into(),
            NameError::Empty => text.name_error_empty.into(),
            NameError::TooLong => fill(
                text.name_error_too_long,
                &[("max", crate::settings::MAX_NAME_CHARS.to_string())],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_round_trip_and_unknown_codes_fail() {
        for language in Language::ALL {
            assert_eq!(Language::from_code(language.code()), Some(language));
        }
        assert_eq!(Language::from_code(" zh-hant "), Some(Language::ZhTw));
        assert_eq!(Language::from_code("en-GB"), Some(Language::EnUs));
        assert_eq!(Language::from_code("ja"), Some(Language::JaJp));
        assert_eq!(Language::from_code("ko-KR"), Some(Language::KoKr));
        assert_eq!(Language::from_code("fr-CA"), Some(Language::FrFr));
        assert_eq!(Language::from_code("de-AT"), Some(Language::DeDe));
        assert_eq!(Language::from_code("es-419"), Some(Language::EsEs));
        assert_eq!(Language::from_code("it-IT"), None);
    }

    #[test]
    fn every_language_has_a_unique_code_and_native_label() {
        let mut codes = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeSet::new();
        for language in Language::ALL {
            assert!(codes.insert(language.code()));
            assert!(labels.insert(language.label()));
        }
        assert_eq!(codes.len(), 8);
        assert_eq!(labels.len(), 8);
        assert!(labels.contains("日本語"));
        assert!(labels.contains("한국어"));
        assert!(labels.contains("Français"));
        assert!(labels.contains("Deutsch"));
        assert!(labels.contains("Español"));
    }

    #[test]
    fn named_templates_fill_without_reinterpreting_inserted_values() {
        assert_eq!(Language::ZhCn.label(), "简体中文");
        assert_eq!(Language::EnUs.file_count(3), "3 files");
        assert_eq!(Language::ZhCn.name_char_count(2, 64), "2 / 64 字符");
        assert_eq!(
            Language::EnUs.received_notice("{files}", 2, "C:\\{path}"),
            "Received 2 files from {files} and saved them to C:\\{path}"
        );
    }

    #[test]
    fn translated_copy_is_non_empty_and_complete() {
        for language in Language::ALL {
            let strings = language.strings();
            assert!(!strings.settings_title.is_empty());
            assert!(!strings.firmware_start_install.is_empty());
            assert!(!language.file_count(2).is_empty());
            assert!(language.incoming_ask("A", 2).contains('A'));
            assert!(
                language
                    .received_notice("A", 2, "C:\\Downloads")
                    .contains("Downloads")
            );
        }
    }
}
