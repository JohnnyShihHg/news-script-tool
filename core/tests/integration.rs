//! End-to-end tests over the fixture folder.
//!
//! The fixtures are SYNTHETIC: same iNews field layout and production-block markup as
//! the real newsroom exports, but entirely invented content. Real scripts carry
//! unpublished copy, colleagues' names and internal identifiers -- several are marked
//! 勿上網 -- so they stay on local machines and out of this public repo. Regenerate or
//! extend them by hand; each file below is named for the parse path it covers.

use news_script_core::config::Config;
use news_script_core::model::Outcome;
use news_script_core::{format, import_files, process_text};

fn load_fixtures() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("txt") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path).unwrap();
            let text = news_script_core::parse::decode_and_normalize(&bytes);
            files.push((name, text));
        }
    }
    files
}

fn fixture<'a>(files: &'a [(String, String)], name: &str) -> (&'a str, &'a str) {
    files
        .iter()
        .find(|(n, _)| n == name)
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .unwrap_or_else(|| panic!("fixture {name} present"))
}

#[test]
fn sample_folder_classifies_every_file_without_panicking() {
    let files = load_fixtures();
    assert!(files.len() >= 15, "expected the fixture set to cover every parse path");
    let cfg = Config::default();
    let summary = import_files(&files, &cfg);

    // Every file must land in exactly one bucket or be silently Skipped; running the
    // whole set through without panicking round-trips the entire pipeline.
    assert!(summary.passed.len() >= 5, "expected several SOT/短sot entries to pass");
    assert!(!summary.filtered.is_empty(), "expected BS/SO to be filtered");
    assert!(!summary.needs_manual.is_empty(), "expected the TEL fixture to need manual content");
    assert!(!summary.unknown.is_empty(), "expected the unknown-style fixture to be flagged");
}

#[test]
fn sot_sample_produces_expected_four_line_output() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成焦點報導1800.txt");
    let cfg = Config::default();
    let entry = match process_text(name, text, &cfg) {
        Outcome::Passed(e) => e,
        other => panic!("expected Passed, got {other:?}"),
    };
    assert_eq!(entry.slug, "合成焦點報導1800");
    assert_eq!(entry.style, "SOT");
    assert_eq!(entry.time, "10:57:44");
    assert_eq!(entry.group, "生");
    assert!(entry.title.contains("合成測試標題"), "title was {:?}", entry.title);
    assert!(entry.body.starts_with("這是合成測試用的內文第一段"), "body was {:?}", entry.body);

    let rendered = format::format_entry(&entry, &cfg.output);
    assert_eq!(rendered.lines().next().unwrap(), "合成焦點報導1800 SOT 10:57:44 生");
}

#[test]
fn lowercase_t2_and_blank_group_are_handled() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成短稿快訊0600.txt");
    let cfg = Config::default();
    let entry = match process_text(name, text, &cfg) {
        Outcome::Passed(e) => e,
        other => panic!("expected Passed, got {other:?}"),
    };
    assert_eq!(entry.style, "短sot", "style must not be upper-cased");
    assert_eq!(entry.group, "");
    assert!(entry.title.contains("合成短稿標題"), "title was {:?}", entry.title);

    // A blank group must not leave a dangling separator on the header line.
    let head = format::format_entry(&entry, &cfg.output).lines().next().unwrap().to_string();
    assert_eq!(head, "合成短稿快訊0600 短sot 06:48:32");
}

#[test]
fn bs_style_with_inline_divider_is_filtered() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成濾除稿件0600.txt");
    let cfg = Config::default();
    match process_text(name, text, &cfg) {
        Outcome::FilteredByStyle { style, .. } => assert_eq!(style, "bs"),
        other => panic!("expected FilteredByStyle, got {other:?}"),
    }
}

#[test]
fn so_style_is_filtered() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成濾除稿件SO0620.txt");
    let cfg = Config::default();
    match process_text(name, text, &cfg) {
        Outcome::FilteredByStyle { style, .. } => assert_eq!(style, "SO"),
        other => panic!("expected FilteredByStyle, got {other:?}"),
    }
}

#[test]
fn sou_suffix_files_are_silently_skipped() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成連線報導sou.txt");
    let cfg = Config::default();
    assert_eq!(process_text(name, text, &cfg), Outcome::Skipped);
}

#[test]
fn tel_file_with_no_content_needs_manual_content_and_gets_prefixed_slug() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成連線報導1100.txt");
    let cfg = Config::default();
    match process_text(name, text, &cfg) {
        Outcome::NeedsManualContent(entry) => assert_eq!(entry.slug, "TEL合成連線報導1100"),
        other => panic!("expected NeedsManualContent, got {other:?}"),
    }
}

#[test]
fn empty_rundown_placeholder_is_silently_skipped() {
    let files = load_fixtures();
    let cfg = Config::default();
    for placeholder in ["TMP.txt", "=====CM1=======.txt", "下節預告1.txt", "話題人物.txt"] {
        let (name, text) = fixture(&files, placeholder);
        assert_eq!(process_text(name, text, &cfg), Outcome::Skipped, "{placeholder} should be skipped");
    }
}

#[test]
fn blank_style_rows_are_silently_skipped_even_with_real_text_in_the_body() {
    let files = load_fixtures();
    // Carries body text but an empty 樣式 field — per user decision, a blank style
    // alone means "not a news script", regardless of what the body holds.
    let (name, text) = fixture(&files, "合成空白樣式0800.txt");
    let cfg = Config::default();
    assert_eq!(process_text(name, text, &cfg), Outcome::Skipped);
}

#[test]
fn non_blank_unrecognized_style_with_no_title_tag_still_surfaces_as_a_failure() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成無標記失敗0900.txt");
    let cfg = Config::default();
    match process_text(name, text, &cfg) {
        Outcome::ParseFailed { .. } => {}
        other => panic!("expected ParseFailed, got {other:?}"),
    }
}

#[test]
fn unknown_style_is_flagged_not_silently_dropped() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成未知樣式0700.txt");
    let cfg = Config::default();
    match process_text(name, text, &cfg) {
        Outcome::UnknownStyle(entry) => assert_eq!(entry.style, "未知樣式X"),
        other => panic!("expected UnknownStyle, got {other:?}"),
    }
}

#[test]
fn punctuation_normalization_survives_the_full_pipeline() {
    let files = load_fixtures();
    let (name, text) = fixture(&files, "合成標點測試0700.txt");
    let cfg = Config::default();
    let entry = match process_text(name, text, &cfg) {
        Outcome::Passed(e) => e,
        other => panic!("expected Passed, got {other:?}"),
    };
    assert!(entry.body.contains('，'), "halfwidth comma should become fullwidth");
    assert!(entry.body.contains("1.5"), "decimals must survive: {:?}", entry.body);
    assert!(
        entry.body.contains("https://example.com/a.b?c=1"),
        "URLs must be protected verbatim: {:?}",
        entry.body
    );
}
