use crate::config::FilterConfig;
use crate::model::StyleClass;

fn eq_ci(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim()) || a.trim() == b.trim()
}

pub fn classify_style(style: &str, cfg: &FilterConfig) -> StyleClass {
    if cfg.allowed_styles.iter().any(|s| eq_ci(s, style)) {
        StyleClass::Allowed
    } else if cfg.blocked_styles.iter().any(|s| eq_ci(s, style)) {
        StyleClass::Blocked
    } else {
        StyleClass::Unknown
    }
}

pub fn is_flagged_style(style: &str, cfg: &FilterConfig) -> bool {
    cfg.flag_styles.iter().any(|s| eq_ci(s, style))
}

/// slug ends with one of the excluded suffixes (e.g. `SOU`), case-insensitive.
pub fn is_excluded_slug(slug: &str, cfg: &FilterConfig) -> bool {
    let slug_lower = slug.trim().to_lowercase();
    cfg.excluded_slug_suffixes
        .iter()
        .any(|suf| slug_lower.ends_with(&suf.to_lowercase()))
}
