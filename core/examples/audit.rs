use news_script_core::config::Config;
use news_script_core::import_files;

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("txt") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path).unwrap();
            let text = news_script_core::parse::decode_and_normalize(&bytes);
            files.push((name, text));
        }
    }
    let total = files.len();
    let cfg = Config::default();
    let summary = import_files(&files, &cfg);
    let skipped = total - summary.loaded;

    println!("total files: {}", total);
    println!("loaded: {}", summary.loaded);
    println!("skipped(silent): {}", skipped);
    println!("passed: {}", summary.passed.len());
    for e in &summary.passed { println!("  PASS  {} [{}]", e.slug, e.style); }
    println!("filtered(style blocked): {}", summary.filtered.len());
    for o in &summary.filtered {
        if let news_script_core::model::Outcome::FilteredByStyle(e) = o {
            println!("  FILT  {} [{}]", e.slug, e.style);
        }
    }
    println!("unknown style: {}", summary.unknown.len());
    for o in &summary.unknown {
        if let news_script_core::model::Outcome::UnknownStyle(e) = o {
            println!("  UNK   {} [{}]", e.slug, e.style);
        }
    }
    println!("needs manual: {}", summary.needs_manual.len());
    for o in &summary.needs_manual {
        if let news_script_core::model::Outcome::NeedsManualContent(e) = o {
            println!("  TEL   {} [{}]", e.slug, e.style);
        }
    }
    println!("failed: {}", summary.failed.len());
    for o in &summary.failed {
        if let news_script_core::model::Outcome::ParseFailed{file_name, reason} = o {
            println!("  FAIL  {} -- {}", file_name, reason);
        }
    }
}
