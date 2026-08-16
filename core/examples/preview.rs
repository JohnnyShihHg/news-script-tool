use news_script_core::config::Config;
use news_script_core::{format, import_files};

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
    let cfg = Config::default();
    let summary = import_files(&files, &cfg);
    let out = format::format_batch(&summary.passed, &cfg.output);
    println!("{}", out);
}
