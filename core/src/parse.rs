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
    Extracted { title: Option<String>, body: String },
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

/// Parse the production block (`[<` ... `>]`) after the header divider: extract the title
/// (the line right after the first `^\[BAR_.*大\]$`-style tag, case-insensitive, that itself
/// starts with `T2`/`t2`) and the body (everything after `>]`, trimmed, with inline `===xxx===`
/// divider lines removed).
pub fn parse_body(text_after_header: &str, title_tag_pattern: &str) -> BodyParse {
    let title_tag_re = Regex::new(&format!("(?i){}", title_tag_pattern)).unwrap();

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

    let mut title = None;
    let block_lines: Vec<&str> = block.lines().collect();
    for (i, line) in block_lines.iter().enumerate() {
        if title_tag_re.is_match(line.trim()) {
            if let Some(next) = block_lines.get(i + 1) {
                let t = next.trim();
                if t.len() >= 2 && t[..2].eq_ignore_ascii_case("t2") {
                    title = Some(t[2..].trim().to_string());
                }
            }
            break;
        }
    }

    let body_raw = after.trim();
    let body = strip_inline_dividers(body_raw).trim().to_string();

    BodyParse::Extracted { title, body }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE_PATTERN: &str = r"^\[BAR_.*大\]$";

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
            BodyParse::Extracted { title, body } => {
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
            BodyParse::Extracted { title, body } => {
                assert_eq!(title, None);
                assert_eq!(body, "內文");
            }
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

    #[test]
    fn empty_template_has_no_title_and_empty_body() {
        let text = "[<\n[BAR_大]\n\n[BAR]\nT2\n>]\n\n";
        match parse_body(text, TITLE_PATTERN) {
            BodyParse::Extracted { title, body } => {
                assert_eq!(title, None);
                assert_eq!(body, "");
            }
            other => panic!("unexpected {:?}", other),
        }
    }
}
