/// Interface language.
///
/// Controls system prompt language, CLI messages, and builtin skill language.
/// Does NOT affect LLM reply language — the LLM always replies in the user's message language.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Language {
    /// English (default, open-source friendly)
    #[default]
    English,
    /// Chinese
    Chinese,
}

impl Language {
    /// Parse from a config string value.
    /// Unknown values fall back to English.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "zh" | "zh-CN" | "zh-TW" | "zh-Hans" | "zh-Hant" => Self::Chinese,
            _ => Self::English,
        }
    }

    /// Infer from the OS `LANG` environment variable.
    pub fn from_locale() -> Self {
        let lang = std::env::var("LANG").unwrap_or_default();
        if lang.starts_with("zh") {
            Self::Chinese
        } else {
            Self::English
        }
    }

    /// Resolve language with priority: config value → LANG env var → English default.
    ///
    /// Pass the raw string from `config.toml [default].language`.
    /// Empty string means the field was absent → fall back to locale detection.
    pub fn detect(config_lang: &str) -> Self {
        if config_lang.is_empty() {
            Self::from_locale()
        } else {
            Self::from_str(config_lang)
        }
    }

    pub fn is_english(self) -> bool {
        self == Self::English
    }

    pub fn is_chinese(self) -> bool {
        self == Self::Chinese
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_english_variants() {
        assert_eq!(Language::from_str("en"), Language::English);
        assert_eq!(Language::from_str("en-US"), Language::English);
        assert_eq!(Language::from_str("fr"), Language::English); // unknown → English
        assert_eq!(Language::from_str(""), Language::English);
    }

    #[test]
    fn from_str_chinese_variants() {
        assert_eq!(Language::from_str("zh"), Language::Chinese);
        assert_eq!(Language::from_str("zh-CN"), Language::Chinese);
        assert_eq!(Language::from_str("zh-TW"), Language::Chinese);
        assert_eq!(Language::from_str("zh-Hans"), Language::Chinese);
        assert_eq!(Language::from_str("zh-Hant"), Language::Chinese);
    }

    #[test]
    fn detect_uses_config_when_set() {
        assert_eq!(Language::detect("zh"), Language::Chinese);
        assert_eq!(Language::detect("en"), Language::English);
    }

    #[test]
    fn detect_falls_back_to_locale_when_empty() {
        // We can't control LANG in tests reliably, so just check it doesn't panic
        // and returns a valid Language value.
        let lang = Language::detect("");
        assert!(lang == Language::English || lang == Language::Chinese);
    }

    #[test]
    fn default_is_english() {
        assert_eq!(Language::default(), Language::English);
    }
}
