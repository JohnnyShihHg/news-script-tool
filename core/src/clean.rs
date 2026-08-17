use crate::config::{AnnotationsConfig, CleanConfig, FilterConfig};
use crate::model::StyleClass;

/// Symbols producers leave at the start of a line as iNews control runs (`.^_^`).
/// `-` is deliberately excluded: a leading dash can be legitimate prose.
const LEADING_MARKERS: &[char] = &['.', '^', '_', '~', '*', '=', '+', '|', '\\', '/', '#'];

/// Trailing junk to shave off the end of the body. CJK punctuation is absent on
/// purpose so a closing `。` survives.
const TRAILING_MARKERS: &[char] =
    &['.', '^', '_', '~', '*', '=', '+', '|', '\\', '/', '#', '-', ' ', '\t'];

fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        let n = c as u32;
        (0x4E00..=0x9FFF).contains(&n) || (0x3400..=0x4DBF).contains(&n)
    })
}

/// Remove the hand-typed markers producers leave in script bodies.
///
/// Scope is deliberately narrow: the START of a line (`..早安你好`), the very END of
/// the body (`。##`), and lines that are nothing but a note (`##`, `OK`). Punctuation
/// inside a sentence is never touched — a regex cannot tell a stray dot from a real
/// one mid-prose, and a wrong guess edits a script that goes to air.
///
/// Runs BEFORE punctuation normalization on purpose: that pass rewrites `.` to `、`,
/// so a leading `..` would no longer be recognisable by the time it got here.
/// URL spans are masked out using the same detection the punctuation pass uses.
pub fn strip_body_markers(body: &str, cfg: &CleanConfig) -> String {
    let mut out_lines: Vec<String> = Vec::new();

    for line in body.lines() {
        let was_blank = line.trim().is_empty();
        let has_url = !crate::punctuation::url_mask(line).iter().all(|b| !b);

        let mut current = line.to_string();
        if cfg.strip_marker_symbols {
            current = strip_line_markers(&current);
        }

        // Blank lines are paragraph structure and survive as-is. A line that became
        // blank only because it was nothing but markers (`##`) is dropped instead --
        // keeping it would leave a stray gap where the note used to be.
        if was_blank {
            out_lines.push(current);
            continue;
        }
        if current.trim().is_empty() {
            continue;
        }

        // A line carrying a URL is content even with no Chinese in it, so the
        // non-CJK rule must not swallow it.
        if cfg.drop_non_cjk_lines && !has_cjk(&current) && !has_url {
            continue;
        }

        out_lines.push(current);
    }

    let mut text = out_lines.join("\n");

    if cfg.strip_trailing_symbols {
        text = text.trim_end_matches(|c: char| TRAILING_MARKERS.contains(&c) || c == '\n').to_string();
    }

    text.trim().to_string()
}

fn strip_line_markers(line: &str) -> String {
    let url = crate::punctuation::url_mask(line);

    // Leading control run: only strip while outside a URL, so a line that *starts*
    // with a URL is left completely alone.
    let mut start = 0usize;
    for (i, c) in line.char_indices() {
        if url.get(i).copied().unwrap_or(false) {
            break;
        }
        if LEADING_MARKERS.contains(&c) || c == ' ' || c == '\t' {
            start = i + c.len_utf8();
        } else {
            break;
        }
    }

    // Only the leading run is removed. Punctuation sitting inside prose is left
    // exactly as typed: there is no reliable way to tell a stray dot from a real one
    // mid-sentence, and guessing wrong rewrites a script that goes to air.
    line[start..].to_string()
}

/// Marker for the slug line, derived from the 編輯備註 note.
///
/// Returns the label only — the slug string itself is never modified, because the
/// slug is what gets matched against the shared doc. Prefixing it would make every
/// comparison miss.
///
/// Most restrictive wins when a note somehow hits more than one group: a story that
/// reads as both blocked and cleared must be treated as blocked.
pub fn slug_marker(editor_note: &str, cfg: &AnnotationsConfig) -> String {
    let note = editor_note.trim();
    if note.is_empty() {
        return String::new();
    }
    let hits = |terms: &[String]| terms.iter().any(|t| !t.is_empty() && note.contains(t.as_str()));

    if hits(&cfg.no_upload_terms) {
        cfg.no_upload_label.clone()
    } else if hits(&cfg.copyright_terms) {
        cfg.copyright_label.clone()
    } else if hits(&cfg.allowed_upload_terms) {
        cfg.allowed_upload_label.clone()
    } else {
        String::new()
    }
}

/// Prefix for the on-screen title: 獨家 when the note marks an exclusive, otherwise
/// 最新 when the style is a breaking-news format. Exclusive wins — it is the stronger
/// claim, and per the user only one prefix should appear.
///
/// Returns an empty string when the title already carries the prefix, so a headline a
/// producer prefixed by hand never ends up as 「最新》最新》…」.
pub fn title_prefix(editor_note: &str, style: &str, title: &str, cfg: &AnnotationsConfig) -> String {
    let note = editor_note.trim();
    let is_exclusive = cfg
        .exclusive_terms
        .iter()
        .any(|t| !t.is_empty() && note.contains(t.as_str()));

    let chosen = if is_exclusive {
        &cfg.exclusive_prefix
    } else if cfg.latest_styles.iter().any(|s| eq_ci(s, style)) {
        &cfg.latest_prefix
    } else {
        return String::new();
    };

    let trimmed = title.trim_start();
    if trimmed.starts_with(chosen.as_str())
        || trimmed.starts_with(cfg.exclusive_prefix.as_str())
        || trimmed.starts_with(cfg.latest_prefix.as_str())
    {
        return String::new();
    }
    chosen.clone()
}

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

/// Style inferred from the slug, for rows that leave 樣式 blank and write the format
/// into the news name instead (`心喻14推播` → `推播`).
///
/// Only consulted when 樣式 is actually empty — a real 樣式 always wins, so this can
/// never override what the producer typed. Returns the configured term itself (not the
/// slug text) so it lines up with `allowed_styles`/`latest_styles` exactly.
pub fn style_from_slug(slug: &str, cfg: &FilterConfig) -> Option<String> {
    cfg.slug_style_terms
        .iter()
        .find(|t| !t.trim().is_empty() && slug.contains(t.trim()))
        .map(|t| t.trim().to_string())
}

/// slug ends with one of the excluded suffixes (e.g. `SOU`), case-insensitive.
pub fn is_excluded_slug(slug: &str, cfg: &FilterConfig) -> bool {
    let slug_lower = slug.trim().to_lowercase();
    cfg.excluded_slug_suffixes
        .iter()
        .any(|suf| slug_lower.ends_with(&suf.to_lowercase()))
}

#[cfg(test)]
mod slug_style_tests {
    use super::*;

    fn f() -> FilterConfig {
        FilterConfig::default()
    }

    #[test]
    fn anchor_hour_push_slug_yields_the_push_style() {
        assert_eq!(style_from_slug("心喻14推播", &f()).as_deref(), Some("推播"));
    }

    #[test]
    fn the_configured_term_is_returned_not_the_slug_text() {
        // Must line up with allowed_styles/latest_styles exactly, or classification
        // and the 最新》 prefix would both miss.
        let style = style_from_slug("心喻14推播", &f()).unwrap();
        assert!(f().allowed_styles.iter().any(|s| s == &style));
    }

    /// Real slug forms collected from the newsroom. The term can sit anywhere —
    /// leading, trailing, or buried mid-string — so this is a plain substring test,
    /// deliberately not a position or shape rule.
    #[test]
    fn every_real_world_push_slug_form_is_recognized() {
        for slug in [
            "欣怡推播14稿標",
            "禕呈推播稿標",
            "生活推播1020",
            "XX14推播",
            "XX推播預錄",
            "心喻14推播",
        ] {
            assert_eq!(style_from_slug(slug, &f()).as_deref(), Some("推播"), "missed {slug}");
        }
    }

    #[test]
    fn a_bare_push_character_does_not_match() {
        // 鬼月撿便宜14推 is a different kind of row; matching the single character 推
        // would sweep it in. Whole terms only.
        assert_eq!(style_from_slug("鬼月撿便宜14推", &f()), None);
    }

    #[test]
    fn an_ordinary_slug_infers_nothing() {
        assert_eq!(style_from_slug("含冰排吃冰1800", &f()), None);
    }

    #[test]
    fn an_empty_configured_term_never_matches_everything() {
        let mut cfg = f();
        cfg.slug_style_terms = vec!["  ".into()];
        assert_eq!(style_from_slug("含冰排吃冰1800", &cfg), None);
    }
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    fn cfg() -> CleanConfig {
        CleanConfig::default()
    }

    #[test]
    fn leading_control_run_is_stripped_but_the_sentence_survives() {
        let out = strip_body_markers(".^_^昨天早上6點多，一輛砂石車疑似掉落。", &cfg());
        assert_eq!(out, "昨天早上6點多，一輛砂石車疑似掉落。");
    }

    #[test]
    fn trailing_hash_goes_but_the_full_stop_stays() {
        let out = strip_body_markers("連線記者 李允薰。##", &cfg());
        assert_eq!(out, "連線記者 李允薰。");
    }

    #[test]
    fn real_content_with_english_and_digits_is_untouched() {
        // The one thing this pass must never do is eat script content.
        let out = strip_body_markers("花蓮和平林道2K處的邊坡，PM2.5濃度偏高，AI分析顯示。", &cfg());
        assert_eq!(out, "花蓮和平林道2K處的邊坡，PM2.5濃度偏高，AI分析顯示。");
    }

    #[test]
    fn lines_made_only_of_symbols_or_english_notes_are_dropped_whole() {
        let out = strip_body_markers("第一段內文。\n##\nOK\n第二段內文。", &cfg());
        assert_eq!(out, "第一段內文。\n第二段內文。");
    }

    #[test]
    fn blank_lines_between_paragraphs_are_preserved() {
        let out = strip_body_markers("第一段。\n\n第二段。", &cfg());
        assert_eq!(out, "第一段。\n\n第二段。");
    }

    #[test]
    fn urls_keep_their_hash_and_dots() {
        let out = strip_body_markers("詳見 https://example.com/a.b#section 謝謝收看", &cfg());
        assert_eq!(out, "詳見 https://example.com/a.b#section 謝謝收看");
    }

    #[test]
    fn a_line_that_is_only_a_url_survives_despite_having_no_chinese() {
        // drop_non_cjk_lines would otherwise eat it; the URL guard has to win.
        let out = strip_body_markers("第一段。\nhttps://example.com/watch?v=abc\n第二段。", &cfg());
        assert!(out.contains("https://example.com/watch?v=abc"), "got: {out}");
    }

    #[test]
    fn leading_stray_punctuation_goes_but_prose_punctuation_stays() {
        // The reported case: producers open a paragraph with junk punctuation.
        assert_eq!(strip_body_markers("..早安你好", &cfg()), "早安你好");
        // Mid-sentence punctuation is untouchable -- no regex can tell a stray dot
        // from a real one, and editing prose that goes to air is worse than leaving
        // a stray mark in.
        assert_eq!(strip_body_markers("文.字之間的點不要動。", &cfg()), "文.字之間的點不要動。");
        assert_eq!(strip_body_markers("溫度1.5度今天很冷。", &cfg()), "溫度1.5度今天很冷。");
    }

    #[test]
    fn each_switch_can_be_turned_off_independently() {
        let off = CleanConfig {
            strip_marker_symbols: false,
            drop_non_cjk_lines: false,
            strip_trailing_symbols: false,
        };
        let input = ".^_^內文。\n##\n結尾。##";
        assert_eq!(strip_body_markers(input, &off), input);
    }
}

#[cfg(test)]
mod annotation_tests {
    use super::*;
    use crate::config::AnnotationsConfig;

    fn ann() -> AnnotationsConfig {
        AnnotationsConfig::default()
    }

    #[test]
    fn every_listed_do_not_publish_wording_collapses_to_one_label() {
        for note in [
            "勿上網", "勿上YT", "勿YT", "不上YT", "不上網", "不po網", "版權勿上",
            "不要上網", "勿網", "網勿",
        ] {
            assert_eq!(slug_marker(note, &ann()), "【勿上網】", "note was {note}");
        }
    }

    #[test]
    fn copyright_and_cleared_wordings_collapse_to_their_own_labels() {
        for note in ["未授權", "不授權", "版權問題"] {
            assert_eq!(slug_marker(note, &ann()), "【版權問題】", "note was {note}");
        }
        for note in ["已授權", "授權可上", "可上YT", "可上網"] {
            assert_eq!(slug_marker(note, &ann()), "【可上網】", "note was {note}");
        }
    }

    #[test]
    fn a_note_about_a_black_screen_is_not_mistaken_for_do_not_publish() {
        // 「切勿黑畫面」 appears in real scripts. Matching on the bare character 勿
        // would wrongly block it, so only the listed full terms count.
        assert_eq!(slug_marker("切勿黑畫面", &ann()), "");
    }

    #[test]
    fn notes_with_no_publishing_instruction_produce_no_marker() {
        for note in ["", "即時訊10", "柯鳳儀 +說明框", "交12前"] {
            assert_eq!(slug_marker(note, &ann()), "", "note was {note:?}");
        }
    }

    #[test]
    fn a_marker_is_found_inside_a_longer_note() {
        assert_eq!(slug_marker("版權問題 待確認", &ann()), "【版權問題】");
    }

    #[test]
    fn exclusive_notes_seen_in_real_scripts_all_produce_the_exclusive_prefix() {
        for note in ["獨", "C獨主", "獨家 修標7千人"] {
            assert_eq!(title_prefix(note, "SOT", "標題", &ann()), "獨家》", "note was {note}");
        }
    }

    #[test]
    fn breaking_styles_produce_the_latest_prefix_case_insensitively() {
        for style in ["live", "LIVE", "slive", "SLIVE", "sl", "SL", "4G", "SNG", "推播", "連線", "旋風", "閃電", "海神"] {
            assert_eq!(title_prefix("", style, "標題", &ann()), "最新》", "style was {style}");
        }
    }

    #[test]
    fn exclusive_wins_when_a_breaking_style_is_also_exclusive() {
        assert_eq!(title_prefix("獨", "LIVE", "標題", &ann()), "獨家》");
    }

    #[test]
    fn an_ordinary_style_with_an_ordinary_note_gets_no_prefix() {
        assert_eq!(title_prefix("即時訊10", "SOT", "標題", &ann()), "");
    }

    #[test]
    fn a_title_a_producer_already_prefixed_is_left_alone() {
        // Otherwise the headline reads 「最新》最新》…」.
        assert_eq!(title_prefix("", "LIVE", "最新》已經加過了", &ann()), "");
        assert_eq!(title_prefix("獨", "SOT", "獨家》已經加過了", &ann()), "");
        // Cross-case too: an exclusive whose title already says 最新》 gains nothing.
        assert_eq!(title_prefix("獨", "SOT", "最新》人工加的", &ann()), "");
    }
}
