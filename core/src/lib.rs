pub mod clean;
pub mod config;
pub mod doc_match;
pub mod format;
pub mod gemini;
pub mod model;
pub mod parse;
pub mod punctuation;

use config::Config;
use model::{Header, NewsEntry, Outcome, StyleClass};

fn header_value(header: &Header, key: &str) -> String {
    header.get(key).unwrap_or("").trim().to_string()
}

/// Run one decoded txt file through the full Phase-1 pipeline: header parse ->
/// slug-suffix exclusion -> production-block/body/title parse -> style
/// classification -> punctuation normalization.
pub fn process_text(file_name: &str, text: &str, cfg: &Config) -> Outcome {
    let text = parse::decode_and_normalize(text.as_bytes());
    let (header, header_lines) = parse::parse_header(&text);

    let slug = header_value(&header, "新聞名稱(標題)");
    let mut style = header_value(&header, "樣式");
    let time = header_value(&header, "累積時間");
    let group = header_value(&header, "組");
    // 註記 is deliberately not read: in practice it holds a camera operator's name,
    // not a publishing instruction.
    let editor_note = header_value(&header, "編輯備註");

    if clean::is_excluded_slug(&slug, &cfg.filter) {
        return Outcome::Skipped;
    }

    // Some rows leave 樣式 blank and write the format into the slug instead
    // (`心喻14推播`). Recover it before the blank-樣式 skip below, or those rows
    // silently never reach the output.
    let mut inferred_style = None;
    if style.is_empty() {
        if let Some(from_slug) = clean::style_from_slug(&slug, &cfg.filter) {
            inferred_style = Some(from_slug.clone());
            style = from_slug;
        }
    }

    // A blank 樣式 with nothing in the slug either means this row is rundown structure
    // (bumper/sponsor-spot/producer note), not a news script, no matter what text
    // happens to sit in the body.
    if style.is_empty() {
        return Outcome::Skipped;
    }

    let rest: String = text.lines().skip(header_lines).collect::<Vec<_>>().join("\n");
    let body_parse = parse::parse_body(
        &rest,
        &cfg.filter.title_tag_pattern,
        &cfg.filter.title_tag_fallback_pattern,
    );

    let (title_raw, body_raw, has_content) = match body_parse {
        parse::BodyParse::NoProductionBlock => {
            if style.eq_ignore_ascii_case("TEL") {
                let entry = NewsEntry {
                    file_name: file_name.to_string(),
                    header,
                    slug: format!("TEL{}", slug),
                    style,
                    time,
                    group,
                    title: String::new(),
                    slug_marker: clean::slug_marker(&editor_note, &cfg.annotations),
                    body: String::new(),
                    raw_title: String::new(),
                    raw_body: String::new(),
                    keywords: Vec::new(),
                    warnings: vec!["TEL 無稿頭內容，需人工補稿".to_string()],
                };
                return Outcome::NeedsManualContent(entry);
            }
            return Outcome::ParseFailed {
                file_name: file_name.to_string(),
                reason: "找不到製作區（[< ... >]）".to_string(),
            };
        }
        parse::BodyParse::MissingClose => {
            return Outcome::ParseFailed {
                file_name: file_name.to_string(),
                reason: "製作區缺少結尾 >]".to_string(),
            };
        }
        parse::BodyParse::Extracted { title, body, has_content } => (title, body, has_content),
    };

    match (title_raw, body_raw.is_empty()) {
        // A genuinely empty rundown placeholder (`[< >]`, no cards at all) is
        // structure, not a news script -- silently skipped, as before.
        (None, true) if !has_content => return Outcome::Skipped,
        // Cards exist (has_content) but no T2 ever resolved to a title, or the title
        // tag itself was never found -- and there's no稿頭內文 either. This is the
        // same shape as TEL-with-no-content: a real story the tool cannot finish
        // parsing, needing a human to fill in title and/or body by hand. Most common
        // case: weather rundowns, which carry only [BAR] cards and no 稿頭內文 at all.
        (None, true) => {
            let entry = NewsEntry {
                file_name: file_name.to_string(),
                header,
                slug,
                style: style.clone(),
                time,
                group,
                title: String::new(),
                slug_marker: clean::slug_marker(&editor_note, &cfg.annotations),
                body: String::new(),
                raw_title: String::new(),
                raw_body: String::new(),
                keywords: Vec::new(),
                warnings: vec!["找不到標題且無稿頭內文，需人工補稿".to_string()],
            };
            return Outcome::NeedsManualContent(entry);
        }
        (None, false) => {
            return Outcome::ParseFailed {
                file_name: file_name.to_string(),
                reason: "找不到標題（標題標記下一行不是 T2）".to_string(),
            };
        }
        (Some(title), true) => {
            // The title tag resolved fine -- only 稿頭內文 is missing. Normalize and
            // prefix the title exactly like the Passed path so it doesn't sit
            // unformatted until the body is filled in.
            let (title_norm, warnings_from_title) = {
                let r = punctuation::normalize(&title, &cfg.punctuation);
                (r.text, r.warnings)
            };
            let mut warnings = warnings_from_title;
            warnings.push("找到標題但無稿頭內文，需人工補稿".to_string());

            let entry = NewsEntry {
                file_name: file_name.to_string(),
                header,
                slug,
                style: style.clone(),
                time,
                group,
                title: format!(
                    "{}{}",
                    clean::title_prefix(&editor_note, &style, &title_norm, &cfg.annotations),
                    title_norm
                ),
                slug_marker: clean::slug_marker(&editor_note, &cfg.annotations),
                body: String::new(),
                raw_title: title,
                raw_body: String::new(),
                keywords: Vec::new(),
                warnings,
            };
            return Outcome::NeedsManualContent(entry);
        }
        (Some(title), false) => {
            let (title_norm, mut warnings) = {
                let r = punctuation::normalize(&title, &cfg.punctuation);
                (r.text, r.warnings)
            };
            // Marker stripping must precede punctuation normalization: that pass
            // rewrites `.` to `、`, which would make `..` markers unrecognisable.
            let body_cleaned = clean::strip_body_markers(&body_raw, &cfg.clean);
            let (body_norm, body_warnings) = {
                let r = punctuation::normalize(&body_cleaned, &cfg.punctuation);
                (r.text, r.warnings)
            };
            warnings.extend(body_warnings);
            if clean::is_flagged_style(&style, &cfg.filter) {
                warnings.push(format!("樣式「{}」可回寫但請確認", style));
            }
            if let Some(ref s) = inferred_style {
                warnings.push(format!("樣式空白，依 slug 判定為「{}」", s));
            }

            let entry = NewsEntry {
                file_name: file_name.to_string(),
                header,
                slug,
                style: style.clone(),
                time,
                group,
                title: format!(
                    "{}{}",
                    clean::title_prefix(&editor_note, &style, &title_norm, &cfg.annotations),
                    title_norm
                ),
                slug_marker: clean::slug_marker(&editor_note, &cfg.annotations),
                body: body_norm,
                raw_title: title,
                raw_body: body_raw,
                keywords: Vec::new(),
                warnings,
            };

            match clean::classify_style(&style, &cfg.filter) {
                StyleClass::Allowed => Outcome::Passed(entry),
                StyleClass::Blocked => Outcome::FilteredByStyle(entry),
                StyleClass::Unknown => Outcome::UnknownStyle(entry),
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub loaded: usize,
    pub passed: Vec<NewsEntry>,
    pub filtered: Vec<Outcome>,
    pub unknown: Vec<Outcome>,
    pub needs_manual: Vec<Outcome>,
    pub failed: Vec<Outcome>,
}

pub fn import_files(files: &[(String, String)], cfg: &Config) -> ImportSummary {
    let mut summary = ImportSummary::default();
    for (name, text) in files {
        let outcome = process_text(name, text, cfg);
        match outcome {
            Outcome::Skipped => {}
            Outcome::Passed(e) => {
                summary.loaded += 1;
                summary.passed.push(e);
            }
            o @ Outcome::FilteredByStyle { .. } => {
                summary.loaded += 1;
                summary.filtered.push(o);
            }
            o @ Outcome::UnknownStyle(_) => {
                summary.loaded += 1;
                summary.unknown.push(o);
            }
            o @ Outcome::NeedsManualContent(_) => {
                summary.loaded += 1;
                summary.needs_manual.push(o);
            }
            o @ Outcome::ParseFailed { .. } => {
                summary.loaded += 1;
                summary.failed.push(o);
            }
        }
    }
    summary
}
