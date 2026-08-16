use crate::config::{CleanConfig, FilterConfig};
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
