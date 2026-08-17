/// Raw `key: value` header fields from the iNews txt, in file order, all preserved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    pub fields: Vec<(String, String)>,
}

impl Header {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewsEntry {
    pub file_name: String,
    pub header: Header,
    pub slug: String,
    pub style: String,
    pub time: String,
    pub group: String,
    pub title: String,
    /// Label for the slug line (e.g. `【勿上網】`), composed at output time. Kept out
    /// of `slug` itself because `slug` is what gets matched against the shared doc.
    pub slug_marker: String,
    pub body: String,
    pub raw_title: String,
    pub raw_body: String,
    pub keywords: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StyleClass {
    Allowed,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Passed style filter, has title + body, ready for output.
    Passed(NewsEntry),
    /// Style is blocked (e.g. BS/SO) — filtered out of the output by default.
    ///
    /// Carries the fully parsed entry rather than just its name: a blocked style is a
    /// default, not a verdict, and a BS story does occasionally turn out to be needed.
    /// Keeping the parsed slug/title/body means it can be ticked back on from the
    /// 已濾除 tab there and then, instead of the user having to edit the blocklist in
    /// settings and re-import.
    FilteredByStyle(NewsEntry),
    /// Style is neither on the allow list nor the block list — needs a human call.
    UnknownStyle(NewsEntry),
    /// TEL entry with no production block / no content — kept but needs manual script.
    NeedsManualContent(NewsEntry),
    /// Structurally broken (missing `>]`, title tag present but no T2 line while body is non-empty, etc.)
    ParseFailed { file_name: String, reason: String },
    /// Empty rundown placeholder (CM break, promo slot, blank template) or `*SOU` filler — not real news.
    Skipped,
}
