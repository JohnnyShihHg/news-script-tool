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

/// An entry that still needs its script filled in by hand -- unless its style is on
/// the blocklist. Without this check only fully parsed entries were ever filtered, so
/// a blocked style with no 稿頭內文 slipped past the blocklist into 待補稿.
fn needs_manual(entry: NewsEntry, cfg: &Config) -> Outcome {
    match clean::classify_style(&entry.style, &cfg.filter) {
        StyleClass::Blocked => Outcome::FilteredByStyle(entry),
        _ => Outcome::NeedsManualContent(entry),
    }
}

fn header_value(header: &Header, key: &str) -> String {
    header.get(key).unwrap_or("").trim().to_string()
}

/// Run one decoded txt file through the full Phase-1 pipeline: header parse ->
/// slug-suffix exclusion -> production-block/body/title parse -> style
/// classification -> punctuation normalization.
pub fn process_text(file_name: &str, text: &str, cfg: &Config) -> Outcome {
    let outcome = classify_text(file_name, text, cfg);
    // A blocked 編輯備註 term (`預告`) overrides whichever bucket the style chose.
    let blocked = |e: &NewsEntry| {
        clean::blocked_note_term(&header_value(&e.header, "編輯備註"), &cfg.filter).map(str::to_string)
    };
    match outcome {
        Outcome::Passed(mut e) | Outcome::UnknownStyle(mut e) | Outcome::NeedsManualContent(mut e)
            if blocked(&e).is_some() =>
        {
            let term = blocked(&e).unwrap();
            e.warnings.push(format!("編輯備註含「{}」，已濾除", term));
            Outcome::FilteredByStyle(e)
        }
        other => other,
    }
}

/// Turn any outcome into a 解析失敗 that still carries its entry, so a row the
/// tool judged not to be news can be ticked back on by hand.
///
/// These rows used to vanish without a trace (Outcome::Skipped). That hid real
/// stories whenever a rule guessed wrong -- `孟潔連宜蘭21SL` left 樣式 blank and wrote
/// SL into the slug, which nothing recognised, so it never showed up anywhere.
fn demote(outcome: Outcome, reason: String) -> Outcome {
    match outcome {
        Outcome::Passed(mut e)
        | Outcome::UnknownStyle(mut e)
        | Outcome::NeedsManualContent(mut e)
        | Outcome::FilteredByStyle(mut e) => {
            e.warnings.insert(0, reason.clone());
            Outcome::ParseFailed { file_name: e.file_name.clone(), reason, entry: Box::new(e) }
        }
        Outcome::ParseFailed { file_name, reason: inner, mut entry } => {
            let reason = format!("{reason}；{inner}");
            entry.warnings[0] = reason.clone();
            Outcome::ParseFailed { file_name, reason, entry }
        }
    }
}

/// A 解析失敗 for a file whose production block could not be read: whatever follows
/// the header goes into the body as-is, so the user can trim it into a script by
/// hand instead of opening the txt.
fn parse_failed(file_name: &str, header: Header, rest: &str, reason: String, cfg: &Config) -> Outcome {
    let editor_note = header_value(&header, "編輯備註");
    let entry = NewsEntry {
        file_name: file_name.to_string(),
        slug: header_value(&header, "新聞名稱(標題)"),
        style: header_value(&header, "樣式"),
        time: header_value(&header, "累積時間"),
        group: header_value(&header, "組"),
        title: String::new(),
        slug_marker: clean::slug_marker(&editor_note, &cfg.annotations),
        body: rest.trim().to_string(),
        raw_title: String::new(),
        raw_body: String::new(),
        keywords: Vec::new(),
        warnings: vec![reason.clone()],
        header,
    };
    Outcome::ParseFailed { file_name: file_name.to_string(), reason, entry: Box::new(entry) }
}

/// The entry for a file that blew up mid-parse: only its name is trustworthy.
fn bare_failure(file_name: &str, reason: String) -> Outcome {
    let entry = NewsEntry {
        file_name: file_name.to_string(),
        header: Header::default(),
        slug: file_name.trim_end_matches(".txt").to_string(),
        style: String::new(),
        time: String::new(),
        group: String::new(),
        title: String::new(),
        slug_marker: String::new(),
        body: String::new(),
        raw_title: String::new(),
        raw_body: String::new(),
        keywords: Vec::new(),
        warnings: vec![reason.clone()],
    };
    Outcome::ParseFailed { file_name: file_name.to_string(), reason, entry: Box::new(entry) }
}

fn classify_text(file_name: &str, text: &str, cfg: &Config) -> Outcome {
    let text = parse::decode_and_normalize(text.as_bytes());
    let (header, header_lines) = parse::parse_header(&text);

    let slug = header_value(&header, "新聞名稱(標題)");
    let mut style = header_value(&header, "樣式");
    let editor_note = header_value(&header, "編輯備註");

    // Rows these rules call "not news" are still parsed in full and land in 解析失敗
    // with the reason attached, instead of being dropped (see `demote`).
    let mut skip_reason = None;
    if clean::is_excluded_slug(&slug, &cfg.filter) {
        skip_reason = Some("slug 結尾是排除字尾（如 SOU），判定為非新聞稿".to_string());
    }

    // Some rows leave 樣式 blank and write the format into the slug instead
    // (`心喻14推播`). Recover it before the blank-樣式 check below.
    let mut inferred_style = None;
    if style.is_empty() {
        if let Some(from_slug) = clean::style_from_slug(&slug, &cfg.filter) {
            inferred_style = Some(format!("樣式空白，依 slug 判定為「{from_slug}」"));
            style = from_slug;
        } else {
            let video_id = header_value(&header, "影像編號");
            if let Some(from_id) = clean::style_from_video_id(&video_id, &cfg.filter) {
                inferred_style = Some(format!("樣式空白，依影像編號 {video_id} 判定為「{from_id}」"));
                style = from_id;
            }
        }
    }

    // A blank 樣式 with nothing in the slug usually means this row is rundown
    // structure (bumper/sponsor-spot/producer note), not a news script -- unless
    // 編輯備註 names a format. The remark is prose, not a field, so it is not trusted
    // to set the style (「推播預告」 and 「不推播」 both contain the term); it only
    // keeps the row in 未知樣式 for a human to call. A slug keep term (`專`) does the
    // same from the slug side.
    let mut note_style_hint = None;
    if style.is_empty() {
        let hint = clean::slug_keep_term(&slug, &cfg.filter)
            .map(|t| format!("樣式空白，slug 含「{t}」，保留但未判定樣式，請人工確認"))
            .or_else(|| {
                clean::note_style_term(&editor_note, &cfg.filter)
                    .map(|t| format!("樣式空白，編輯備註含「{t}」，未自動判定樣式，請人工確認"))
            });
        match hint {
            Some(h) => note_style_hint = Some(h),
            None if skip_reason.is_none() => {
                skip_reason = Some("樣式空白，slug 與編輯備註也判斷不出樣式，判定為節目結構".to_string())
            }
            None => {}
        }
    }

    let outcome = classify_body(
        file_name,
        &text,
        header,
        header_lines,
        style,
        inferred_style,
        note_style_hint,
        cfg,
    );
    match skip_reason {
        Some(reason) => demote(outcome, reason),
        None => outcome,
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_body(
    file_name: &str,
    text: &str,
    header: Header,
    header_lines: usize,
    style: String,
    inferred_style: Option<String>,
    note_style_hint: Option<String>,
    cfg: &Config,
) -> Outcome {
    let slug = header_value(&header, "新聞名稱(標題)");
    let time = header_value(&header, "累積時間");
    let group = header_value(&header, "組");
    // 註記 is deliberately not read: in practice it holds a camera operator's name,
    // not a publishing instruction.
    let editor_note = header_value(&header, "編輯備註");

    let rest: String = text.lines().skip(header_lines).collect::<Vec<_>>().join("\n");
    let body_parse = parse::parse_body(
        &rest,
        &cfg.filter.title_tag_pattern,
        &cfg.filter.title_tag_fallback_pattern,
    );

    let (title_raw, body_raw, has_content, title_tag_found, fallback_tag) = match body_parse {
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
                return needs_manual(entry, cfg);
            }
            return parse_failed(file_name, header, &rest, "找不到製作區（[< ... >]）".to_string(), cfg);
        }
        parse::BodyParse::MissingClose => {
            return parse_failed(file_name, header, &rest, "製作區缺少結尾 >]".to_string(), cfg);
        }
        parse::BodyParse::Extracted { title, body, has_content, title_tag_found, fallback_tag } => {
            (title, body, has_content, title_tag_found, fallback_tag)
        }
    };

    // Judge emptiness on the cleaned body: a body that is nothing but producer
    // markers or non-Chinese format lines has no real script in it.
    let body_cleaned = clean::strip_body_markers(&body_raw, &cfg.clean);

    // Both ways a blank 樣式 can be resolved leave the user with something to check,
    // so every bucket below carries the reason it ended up where it did.
    let push_style_notes = |warnings: &mut Vec<String>| {
        if let Some(ref s) = inferred_style {
            warnings.push(s.clone());
        }
        if let Some(ref h) = note_style_hint {
            warnings.push(h.clone());
        }
        // Plain [BAR] is the long-standing weather-rundown case and needs no nagging;
        // a [BAR_xxx] segment card standing in for the headline is a guess to check.
        if let Some(tag) = fallback_tag.as_deref().filter(|t| !t.eq_ignore_ascii_case("[BAR]")) {
            warnings.push(format!("找不到 [BAR_..大] 標題標記，標題取自第一張 {tag} 字卡，請確認"));
        }
    };

    match (title_raw, body_cleaned.is_empty()) {
        // A genuinely empty rundown placeholder (`[< >]`, no cards at all) is
        // structure, not a news script -- but it goes to 解析失敗 rather than
        // vanishing, in case a real story was left that empty.
        (None, true) if !has_content => {
            return parse_failed(
                file_name,
                header,
                "",
                "製作區是空的、也沒有稿頭內文，判定為節目結構".to_string(),
                cfg,
            );
        }
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
                warnings: {
                    let mut w = vec!["找不到標題且無稿頭內文，需人工補稿".to_string()];
                    push_style_notes(&mut w);
                    w
                },
            };
            return needs_manual(entry, cfg);
        }
        // The title card is there but its T2 was never filled in, while the script
        // itself is real: keep the body and let a human supply the title.
        (None, false) if title_tag_found => {
            let (body_norm, warnings) = {
                let r = punctuation::normalize(&body_cleaned, &cfg.punctuation);
                (clean::join_lines(&r.text), r.warnings)
            };
            let mut warnings = warnings;
            warnings.push("有標題標記但 T2 空白，需人工補標題".to_string());
            push_style_notes(&mut warnings);
            let entry = NewsEntry {
                file_name: file_name.to_string(),
                header,
                slug,
                style: style.clone(),
                time,
                group,
                title: String::new(),
                slug_marker: clean::slug_marker(&editor_note, &cfg.annotations),
                body: body_norm,
                raw_title: String::new(),
                raw_body: body_raw,
                keywords: Vec::new(),
                warnings,
            };
            return needs_manual(entry, cfg);
        }
        (None, false) => {
            return parse_failed(file_name, header, &body_cleaned, "找不到標題標記".to_string(), cfg);
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
            push_style_notes(&mut warnings);

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
            return needs_manual(entry, cfg);
        }
        (Some(title), false) => {
            let (title_norm, mut warnings) = {
                let r = punctuation::normalize(&title, &cfg.punctuation);
                (r.text, r.warnings)
            };
            // Marker stripping (above) must precede punctuation normalization: that
            // pass rewrites `.` to `、`, which would make `..` markers unrecognisable.
            let (body_norm, body_warnings) = {
                let r = punctuation::normalize(&body_cleaned, &cfg.punctuation);
                (clean::join_lines(&r.text), r.warnings)
            };
            warnings.extend(body_warnings);
            if clean::is_flagged_style(&style, &cfg.filter) {
                warnings.push(format!("樣式「{}」可回寫但請確認", style));
            }
            push_style_notes(&mut warnings);

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

/// `process_text` behind a panic boundary, so one malformed script cannot take the
/// whole import -- and with `windows_subsystem = "windows"`, the whole app -- down with
/// it. This is a last line of defence, not error handling: expected format problems
/// still come back as a normal `Outcome::ParseFailed` from `process_text` itself. What
/// this catches is a parser bug we have not foreseen, and it reports the file name so
/// the offending script can actually be found next time.
fn process_text_isolated(file_name: &str, text: &str, cfg: &Config) -> Outcome {
    isolate(file_name, || process_text(file_name, text, cfg))
}

/// The panic boundary itself, split out from `process_text_isolated` so a test can
/// drive it with a closure that really panics -- once the known panics are fixed there
/// is no input left that would exercise it through `process_text`.
fn isolate<F: FnOnce() -> Outcome>(file_name: &str, f: F) -> Outcome {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(outcome) => outcome,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "未知錯誤".to_string());
            bare_failure(file_name, format!("解析時發生未預期錯誤：{detail}"))
        }
    }
}

pub fn import_files(files: &[(String, String)], cfg: &Config) -> ImportSummary {
    let mut summary = ImportSummary::default();
    for (name, text) in files {
        let outcome = process_text_isolated(name, text, cfg);
        match outcome {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_parse_becomes_a_named_parse_failure() {
        // Silence the default hook: this panic is the thing under test, and its
        // backtrace in the test output would just look like a failure.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = isolate("合成爆炸稿件.txt", || panic!("byte index 2 is not a char boundary"));
        std::panic::set_hook(previous);

        match outcome {
            Outcome::ParseFailed { file_name, reason, .. } => {
                assert_eq!(file_name, "合成爆炸稿件.txt");
                // The reason has to carry the cause, or the next crash is just as hard
                // to trace back to a script as this one was.
                assert!(reason.contains("char boundary"), "reason was {reason:?}");
            }
            other => panic!("expected ParseFailed, got {other:?}"),
        }
    }
}
