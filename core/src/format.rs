use crate::config::OutputConfig;
use crate::model::NewsEntry;

/// Render one entry as the four-line block from spec §5:
/// `{slug} {style} {time} {group}` / title / body / keywords.
/// An empty group is omitted rather than leaving a trailing space.
pub fn format_entry(entry: &NewsEntry, cfg: &OutputConfig) -> String {
    let mut head_parts = vec![entry.slug.as_str(), entry.style.as_str(), entry.time.as_str()];
    if !entry.group.trim().is_empty() {
        head_parts.push(entry.group.as_str());
    }
    let head = head_parts.join(" ");
    let keywords = entry.keywords.join(&cfg.keyword_separator);
    format!("{}\n{}\n{}\n{}", head, entry.title, entry.body, keywords)
}

pub fn format_batch(entries: &[NewsEntry], cfg: &OutputConfig) -> String {
    let blank = "\n".repeat(cfg.entry_blank_lines);
    entries
        .iter()
        .map(|e| format_entry(e, cfg))
        .collect::<Vec<_>>()
        .join(&format!("\n{}", blank))
}
