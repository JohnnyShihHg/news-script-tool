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
    /// Style is blocked (e.g. BS/SO) — filtered out.
    FilteredByStyle { file_name: String, slug: String, style: String },
    /// Style is neither on the allow list nor the block list — needs a human call.
    UnknownStyle(NewsEntry),
    /// TEL entry with no production block / no content — kept but needs manual script.
    NeedsManualContent(NewsEntry),
    /// Structurally broken (missing `>]`, title tag present but no T2 line while body is non-empty, etc.)
    ParseFailed { file_name: String, reason: String },
    /// Empty rundown placeholder (CM break, promo slot, blank template) or `*SOU` filler — not real news.
    Skipped,
}
