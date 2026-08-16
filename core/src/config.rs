use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterConfig {
    pub allowed_styles: Vec<String>,
    pub blocked_styles: Vec<String>,
    pub excluded_slug_suffixes: Vec<String>,
    pub flag_styles: Vec<String>,
    pub title_tag_pattern: String,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            allowed_styles: vec![
                "SOT".into(), "短sot".into(), "LIVE".into(), "海神".into(),
                "閃電".into(), "旋風".into(), "4G".into(), "TEL".into(), "電連".into(),
            ],
            blocked_styles: vec!["BS".into(), "SO".into()],
            excluded_slug_suffixes: vec!["SOU".into()],
            flag_styles: vec!["TEL".into(), "電連".into()],
            title_tag_pattern: r"^\[BAR_.*大\]$".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkersConfig {
    pub refresh_keywords: Vec<String>,
}

impl Default for MarkersConfig {
    fn default() -> Self {
        Self { refresh_keywords: vec!["抓新".into()] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunctuationConfig {
    pub quotes_to_corner: bool,
    pub dot_to_enumeration: bool,
    pub protect_urls: bool,
    pub preserve_halfwidth_space: bool,
    pub map: BTreeMap<String, String>,
}

impl Default for PunctuationConfig {
    fn default() -> Self {
        let mut map = BTreeMap::new();
        map.insert(",".into(), "，".into());
        map.insert(":".into(), "：".into());
        map.insert(";".into(), "；".into());
        map.insert("?".into(), "？".into());
        map.insert("!".into(), "！".into());
        map.insert("(".into(), "（".into());
        map.insert(")".into(), "）".into());
        Self {
            quotes_to_corner: true,
            dot_to_enumeration: true,
            protect_urls: true,
            preserve_halfwidth_space: true,
            map,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub keyword_count: usize,
    pub keyword_separator: String,
    pub entry_blank_lines: usize,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { keyword_count: 4, keyword_separator: " ".into(), entry_blank_lines: 1 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiSettingsConfig {
    pub model: String,
}

impl Default for GeminiSettingsConfig {
    fn default() -> Self {
        Self { model: "gemini-3.5-flash-lite".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportConfig {
    /// Empty string means "no default set" — the user must pick a folder each time.
    pub default_folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Colour theme id, matching a `[data-theme]` block in styles.css. Persisted here
    /// (not in browser storage) so a chosen theme survives restarts and stays put
    /// until someone explicitly saves a different one.
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { theme: "light".into() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub filter: FilterConfig,
    #[serde(default)]
    pub markers: MarkersConfig,
    #[serde(default)]
    pub punctuation: PunctuationConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub gemini: GeminiSettingsConfig,
    #[serde(default)]
    pub import: ImportConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

pub fn load_from_str(s: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(s)
}

pub fn load_from_path(path: &std::path::Path) -> std::io::Result<Config> {
    let s = std::fs::read_to_string(path)?;
    load_from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn save_to_path(cfg: &Config, path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let s = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_toml() {
        let cfg = Config::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = load_from_str(&s).unwrap();
        assert_eq!(back.filter.allowed_styles, cfg.filter.allowed_styles);
        assert_eq!(back.gemini.model, cfg.gemini.model);
        assert_eq!(back.punctuation.map.get(","), Some(&"，".to_string()));
    }
}
