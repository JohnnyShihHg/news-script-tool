use crate::model::Header;
use regex::Regex;

/// Decode raw bytes as UTF-8, falling back to Big5, and normalize line endings to `\n`.
pub fn decode_and_normalize(bytes: &[u8]) -> String {
    let text = if let Ok(s) = std::str::from_utf8(bytes) {
        s.to_string()
    } else {
        let (cow, _enc, had_errors) = encoding_rs::BIG5.decode(bytes);
        if had_errors {
            String::from_utf8_lossy(bytes).to_string()
        } else {
            cow.into_owned()
        }
    };
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Parse the `key: value` header block, preserving every field and its order.
/// Stops at the underscore divider line (10+ underscores).
pub fn parse_header(text: &str) -> (Header, usize) {
    let divider_re = Regex::new(r"^_{10,}\s*$").unwrap();
    let mut fields = Vec::new();
    let mut consumed_lines = 0;
    for (i, line) in text.lines().enumerate() {
        consumed_lines = i + 1;
        if divider_re.is_match(line.trim_end()) {
            break;
        }
        if let Some(idx) = line.find([':', '：']) {
            let key = line[..idx].trim().to_string();
            let value = line[idx + 1..].trim().to_string();
            if !key.is_empty() {
                fields.push((key, value));
            }
        }
    }
    (Header { fields }, consumed_lines)
}

#[derive(Debug, Clone, PartialEq)]
pub enum BodyParse {
    /// No `[<` production-block marker anywhere in the file at all.
    NoProductionBlock,
    /// `[<` found but no matching `>]` after it.
    MissingClose,
    /// Production block present; title (if any) and body extracted.
    Extracted {
        title: Option<String>,
        body: String,
        /// Whether the `[< ... >]` block itself held anything beyond the opening
        /// marker line -- distinguishes a genuinely empty rundown placeholder (`[<
        /// >]`, no cards at all) from a script whose cards exist but never resolved
        /// to a title (e.g. no T2 line anywhere). Both can have `title: None`, but
        /// only the former is safe to skip silently.
        has_content: bool,
    },
}

/// Strip `===xxx===`-style divider markers from a body. In real scripts these show up
/// either as their own line (`==BS==` on a line by itself) or tacked onto the end of a
/// content line with no separating whitespace (`...引發爭議，===bs===`) — either way they
/// are not part of the spoken content, so the marker (and any line left empty by removing
/// it) is dropped.
fn strip_inline_dividers(body: &str) -> String {
    let re = Regex::new(r"=+[^=\n]{0,40}=+").unwrap();
    let cleaned = re.replace_all(body, "");
    cleaned
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Index of the first line matching `tag_re`, if any.
fn find_tag_line(block_lines: &[&str], tag_re: &Regex) -> Option<usize> {
    block_lines.iter().position(|l| tag_re.is_match(l.trim()))
}

/// Scan forward from just after a title-tag line for the first `T2`/`t2` line, within
/// the window bounded by the next `[...]` tag line (exclusive) or the end of the
/// block. Blank lines and other noise (a source credit line, a `T1` line, etc.) are
/// skipped rather than treated as "no title" -- real scripts carry both. The window
/// stops at the next tag line so this can never wander into an unrelated card.
fn scan_window_for_t2(block_lines: &[&str], tag_idx: usize) -> Option<String> {
    for line in &block_lines[tag_idx + 1..] {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('[') {
            break;
        }
        if t.len() >= 2 && t[..2].eq_ignore_ascii_case("t2") {
            return Some(t[2..].trim().to_string());
        }
    }
    None
}

/// Parse the production block (`[<` ... `>]`) after the header divider: extract the
/// title and the body (everything after `>]`, trimmed, with inline `===xxx===`
/// divider lines removed).
///
/// Title lookup is two-pass. First `title_tag_pattern` (normally `[BAR_..大]`) is
/// searched for across the whole block; if found, its window is scanned for T2 and
/// that result stands even if no title turns up. Only when `title_tag_pattern`
/// matches nowhere at all does `title_tag_fallback_pattern` (`[BAR]`) get a turn --
/// some scripts (weather rundowns) carry only plain `[BAR]` cards and never a big BAR.
pub fn parse_body(
    text_after_header: &str,
    title_tag_pattern: &str,
    title_tag_fallback_pattern: &str,
) -> BodyParse {
    let title_tag_re = Regex::new(&format!("(?i){}", title_tag_pattern)).unwrap();
    let fallback_re = Regex::new(&format!("(?i){}", title_tag_fallback_pattern)).unwrap();

    let open_idx = match text_after_header.find("[<") {
        Some(i) => i,
        None => return BodyParse::NoProductionBlock,
    };
    let close_idx = match text_after_header[open_idx..].find(">]") {
        Some(i) => open_idx + i,
        None => return BodyParse::MissingClose,
    };

    let block = &text_after_header[open_idx..close_idx];
    let after = &text_after_header[close_idx + 2..];

    let block_lines: Vec<&str> = block.lines().collect();
    let title = match find_tag_line(&block_lines, &title_tag_re) {
        Some(idx) => scan_window_for_t2(&block_lines, idx),
        None => find_tag_line(&block_lines, &fallback_re)
            .and_then(|idx| scan_window_for_t2(&block_lines, idx)),
    };
    // block_lines[0] is always the "[<" marker line itself; anything real starts
    // after it.
    let has_content = block_lines[1..].iter().any(|l| !l.trim().is_empty());

    let body_raw = after.trim();
    let body = strip_inline_dividers(body_raw).trim().to_string();

    BodyParse::Extracted { title, body, has_content }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE_PATTERN: &str = r"^\[BAR_.*大\]$";
    const FALLBACK_PATTERN: &str = r"^\[BAR\]$";

    fn parse_body(text: &str, title_pattern: &str) -> BodyParse {
        super::parse_body(text, title_pattern, FALLBACK_PATTERN)
    }

    #[test]
    fn header_preserves_all_fields_in_order() {
        let text = "序號: *03\n新聞名稱(標題): 偷換漆把手1800\n樣式: SOT\n組: 生\n_______________________________________________________________\nbody here";
        let (header, consumed) = parse_header(text);
        assert_eq!(header.get("新聞名稱(標題)"), Some("偷換漆把手1800"));
        assert_eq!(header.get("樣式"), Some("SOT"));
        assert_eq!(header.get("組"), Some("生"));
        assert_eq!(header.fields.len(), 4);
        assert_eq!(text.lines().nth(consumed).unwrap(), "body here");
    }

    #[test]
    fn empty_group_field_parses_to_empty_string() {
        let text = "新聞名稱(標題): 氣象署講雨1100\n組: \n_______________________________________________________________\n";
        let (header, _) = parse_header(text);
        assert_eq!(header.get("組"), Some(""));
    }

    #[test]
    fn title_tag_variant_bar_da_is_recognized() {
        let simple = "[<\n[BAR_大]\nT2標題文字\n>]\n內文";
        match parse_body(simple, TITLE_PATTERN) {
            BodyParse::Extracted { title, body, .. } => {
                assert_eq!(title.as_deref(), Some("標題文字"));
                assert_eq!(body, "內文");
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn title_tag_variant_bar_zuida_is_recognized() {
        let text = "[<\n[BAR_獨大]\nT2看環景錄影才知! 車主控新車\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => {
                assert_eq!(title.as_deref(), Some("看環景錄影才知! 車主控新車"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn lowercase_t2_is_recognized() {
        let text = "[<\n[BAR_最新大]\nt2小寫標題\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => assert_eq!(title.as_deref(), Some("小寫標題")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn plain_bar_without_da_is_not_treated_as_title() {
        let text = "[<\n[BAR]\nT2分段字卡不是標題\n[BAR_最新大]\nT2真正標題\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => assert_eq!(title.as_deref(), Some("真正標題")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn t1_next_line_does_not_produce_a_title() {
        let text = "[<\n[BAR_大]\nT1不該被抓\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, body, .. } => {
                assert_eq!(title, None);
                assert_eq!(body, "內文");
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn blank_line_between_tag_and_t2_still_finds_the_title() {
        let text = "[<\n[BAR_獨大]\n\nT2天熱\"水蜜桃冰\"排翻\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => {
                assert_eq!(title.as_deref(), Some("天熱\"水蜜桃冰\"排翻"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn whitespace_only_lines_between_tag_and_t2_are_also_skipped() {
        let text = "[<\n[BAR_大]\n   \n\t\nT2標題\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => assert_eq!(title.as_deref(), Some("標題")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn blank_then_non_t2_line_still_yields_no_title() {
        let text = "[<\n[BAR_大]\n\nT1不該被抓\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => assert_eq!(title, None),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn inline_divider_lines_are_stripped_from_body() {
        let text = "[<\n[BAR_最新大]\nT2標題\n>]\n稿頭段落，===bs===\n後段內容";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { body, .. } => {
                assert_eq!(body, "稿頭段落，\n後段內容");
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn double_equals_inline_divider_is_also_stripped() {
        let text = "[<\n[BAR_最新大]\nT2標題\n>]\n前段\n==BS==\n後段";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { body, .. } => {
                assert_eq!(body, "前段\n後段");
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn no_open_bracket_at_all_is_no_production_block() {
        let text = "no production block here";
        assert_eq!(parse_body(text, TITLE_PATTERN), BodyParse::NoProductionBlock);
    }

    #[test]
    fn missing_close_bracket_is_detected() {
        let text = "[<\n[BAR_大]\nT2標題\nno close here";
        assert_eq!(parse_body(text, TITLE_PATTERN), BodyParse::MissingClose);
    }

    // --- Fallback pattern: plain [BAR] cards, used only when no big-BAR tag exists ---

    #[test]
    fn fallback_pattern_finds_the_title_when_no_big_bar_tag_exists() {
        // Mirrors a real weather rundown: only plain [BAR] cards, never [BAR_..大].
        let text = "[<\n[主播_合成]\n\n\n[BAR]\nT2本週天氣不穩! 低壓帶盤據 慎防強對流發展\n[BAR]\nT2注意! 本週低壓帶影響 中南.東南部注意大雨\n>]\n";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => {
                assert_eq!(title.as_deref(), Some("本週天氣不穩! 低壓帶盤據 慎防強對流發展"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn lowercase_fallback_tag_is_recognized() {
        let text = "[<\n[bar]\nT2小寫標記也要認得\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => {
                assert_eq!(title.as_deref(), Some("小寫標記也要認得"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn a_noise_line_before_t2_is_skipped_under_the_big_bar_pattern_too() {
        // The window scan is one shared function for both patterns, not a
        // fallback-only fix -- confirm a big-BAR tag also tolerates noise before T2.
        let text = "[<\n[BAR_獨大]\n#n合成通訊社\nT2大字標記下也要跳過雜訊行\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => {
                assert_eq!(title.as_deref(), Some("大字標記下也要跳過雜訊行"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn a_noise_line_between_the_fallback_tag_and_t2_is_skipped() {
        // Real example: a source credit line sits between [bar] and its T2.
        let text = "[<\n[bar]\n#n自由時報\nT2產銷平衡 北市蛋商公會:這周蛋價不調漲\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => {
                assert_eq!(title.as_deref(), Some("產銷平衡 北市蛋商公會:這周蛋價不調漲"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn big_bar_tag_present_anywhere_disables_the_fallback_entirely() {
        // Even a plain [BAR] card earlier in the block must not win once a big-BAR tag
        // exists anywhere -- the fallback only activates when the primary pattern
        // matches nowhere in the whole block.
        let text = "[<\n[BAR]\nT2不該被選中\n[BAR_最新大]\nT2真正標題\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => assert_eq!(title.as_deref(), Some("真正標題")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn anchor_name_block_is_never_mistaken_for_a_title_even_via_fallback() {
        // [雙框_記者右] carries a real T2 (the anchor's name), but it must never be
        // picked up -- neither pattern matches that tag, by design.
        let text = "[<\n[雙框_記者右]\nT1李郁莉\nT2呂心喻\n\n[BAR]\nT2真正的新聞標題\n>]\n內文";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, .. } => assert_eq!(title.as_deref(), Some("真正的新聞標題")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn empty_template_has_no_title_and_empty_body() {
        let text = "[<\n[BAR_大]\n\n[BAR]\nT2\n>]\n\n";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, body, .. } => {
                assert_eq!(title, None);
                assert_eq!(body, "");
            }
            other => panic!("unexpected {:?}", other),
        }
    }
}
