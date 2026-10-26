use crate::config::OutputConfig;
use crate::model::NewsEntry;

/// Render one entry as the block from spec §5:
/// `{slug} {style} {time} {group}` / title / blank line / body / keywords.
/// An empty group is omitted rather than leaving a trailing space; the blank line is
/// left out when there is no body. Must match `buildOutputText` in app/src/logic.js.
pub fn format_entry(entry: &NewsEntry, cfg: &OutputConfig) -> String {
    let mut head_parts = vec![entry.slug.as_str(), entry.style.as_str(), entry.time.as_str()];
    if !entry.group.trim().is_empty() {
        head_parts.push(entry.group.as_str());
    }
    let head = head_parts.join(" ");
    let keywords = entry.keywords.join(&cfg.keyword_separator);
    if entry.body.trim().is_empty() {
        format!("{}\n{}\n{}\n{}", head, entry.title, entry.body, keywords)
    } else {
        format!("{}\n{}\n\n{}\n{}", head, entry.title, entry.body, keywords)
    }
}

pub fn format_batch(entries: &[NewsEntry], cfg: &OutputConfig) -> String {
    let blank = "\n".repeat(cfg.entry_blank_lines);
    entries
        .iter()
        .map(|e| format_entry(e, cfg))
        .collect::<Vec<_>>()
        .join(&format!("\n{}", blank))
}
