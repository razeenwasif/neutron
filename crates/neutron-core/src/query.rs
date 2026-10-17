//! Searches by what files are rather than where: name, kind, when last
//! changed, tag, and where to look — a Smart Folder, or a search typed with
//! `kind:` and `date:` in it.
//!
//! Pure: a [`Query`] is written to and read from a short string, so it can
//! live in a location and a saved file, and turned into a Spotlight
//! predicate. Running it is the shell's business.

use std::path::PathBuf;

/// What kind of file to find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Document,
    Text,
    Pdf,
    Image,
    Movie,
    Music,
    Code,
    Application,
    Archive,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Folder,
        Kind::Document,
        Kind::Text,
        Kind::Pdf,
        Kind::Image,
        Kind::Movie,
        Kind::Music,
        Kind::Code,
        Kind::Application,
        Kind::Archive,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => "Folder",
            Kind::Document => "Document",
            Kind::Text => "Text",
            Kind::Pdf => "PDF",
            Kind::Image => "Image",
            Kind::Movie => "Movie",
            Kind::Music => "Music",
            Kind::Code => "Code",
            Kind::Application => "Application",
            Kind::Archive => "Archive",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Kind::Folder => "folder",
            Kind::Document => "document",
            Kind::Text => "text",
            Kind::Pdf => "pdf",
            Kind::Image => "image",
            Kind::Movie => "movie",
            Kind::Music => "music",
            Kind::Code => "code",
            Kind::Application => "app",
            Kind::Archive => "archive",
        }
    }

    fn from_word(word: &str) -> Option<Kind> {
        let word = word.to_ascii_lowercase();
        Kind::ALL.into_iter().find(|k| k.word() == word).or(match word.as_str() {
            "folders" | "dir" | "directory" => Some(Kind::Folder),
            "doc" | "docs" | "documents" => Some(Kind::Document),
            "images" | "photo" | "photos" | "picture" | "pictures" => Some(Kind::Image),
            "video" | "videos" | "movies" => Some(Kind::Movie),
            "audio" | "song" | "songs" => Some(Kind::Music),
            "source" => Some(Kind::Code),
            "application" | "apps" => Some(Kind::Application),
            "zip" | "archives" => Some(Kind::Archive),
            _ => None,
        })
    }

    /// The Spotlight test for this kind.
    fn predicate(self) -> &'static str {
        match self {
            Kind::Folder => "kMDItemContentType == \"public.folder\"",
            Kind::Document => {
                "(kMDItemContentTypeTree == \"public.composite-content\" || kMDItemContentTypeTree == \"public.text\" || kMDItemContentType == \"com.adobe.pdf\")"
            }
            Kind::Text => "kMDItemContentTypeTree == \"public.text\"",
            Kind::Pdf => "kMDItemContentType == \"com.adobe.pdf\"",
            Kind::Image => "kMDItemContentTypeTree == \"public.image\"",
            Kind::Movie => "kMDItemContentTypeTree == \"public.movie\"",
            Kind::Music => "kMDItemContentTypeTree == \"public.audio\"",
            Kind::Code => "kMDItemContentTypeTree == \"public.source-code\"",
            Kind::Application => "kMDItemContentType == \"com.apple.application-bundle\"",
            Kind::Archive => "kMDItemContentTypeTree == \"public.archive\"",
        }
    }
}

/// How recently it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Within {
    Today,
    Week,
    Month,
    Year,
}

impl Within {
    pub const ALL: [Within; 4] = [Within::Today, Within::Week, Within::Month, Within::Year];

    pub fn label(self) -> &'static str {
        match self {
            Within::Today => "Today",
            Within::Week => "Past week",
            Within::Month => "Past month",
            Within::Year => "Past year",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Within::Today => "today",
            Within::Week => "week",
            Within::Month => "month",
            Within::Year => "year",
        }
    }

    fn from_word(word: &str) -> Option<Within> {
        Within::ALL.into_iter().find(|w| w.word().eq_ignore_ascii_case(word))
    }

    /// Spotlight's own clock: midnight today, so many days back.
    fn since(self) -> &'static str {
        match self {
            Within::Today => "$time.today",
            Within::Week => "$time.today(-7)",
            Within::Month => "$time.today(-30)",
            Within::Year => "$time.today(-365)",
        }
    }
}

/// A search.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// Text the name holds — and, with `contents`, the text inside.
    pub text: String,
    pub contents: bool,
    pub kind: Option<Kind>,
    pub within: Option<Within>,
    pub tag: Option<String>,
    /// Only under this folder; everywhere Spotlight looks otherwise.
    pub scope: Option<PathBuf>,
}

impl Query {
    /// A search typed into a box: words are the name, and `kind:pdf`,
    /// `date:week` and `tag:Red` narrow it, as the Finder's tokens do.
    pub fn typed(input: &str) -> Query {
        let mut query = Query::default();
        let mut words = Vec::new();
        for word in input.split_whitespace() {
            match word.split_once(':') {
                Some(("kind", k)) if Kind::from_word(k).is_some() => query.kind = Kind::from_word(k),
                Some(("date", d)) | Some(("modified", d)) if Within::from_word(d).is_some() => {
                    query.within = Within::from_word(d)
                }
                Some(("tag", t)) if !t.is_empty() => query.tag = Some(t.to_owned()),
                _ => words.push(word),
            }
        }
        query.text = words.join(" ");
        query
    }

    /// Whether it narrows by anything but the name.
    pub fn is_filtered(&self) -> bool {
        self.kind.is_some() || self.within.is_some() || self.tag.is_some() || self.contents
    }

    /// Whether there is anything to look for at all.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.kind.is_none() && self.within.is_none() && self.tag.is_none()
    }

    /// The Spotlight predicate: every condition, all of them true.
    pub fn predicate(&self) -> String {
        let mut parts = Vec::new();
        let text = self.text.trim();
        if !text.is_empty() {
            let t = escape(text);
            if self.contents {
                parts.push(format!("(kMDItemFSName == \"*{t}*\"cd || kMDItemTextContent == \"*{t}*\"cd)"));
            } else {
                parts.push(format!("kMDItemFSName == \"*{t}*\"cd"));
            }
        }
        if let Some(kind) = self.kind {
            parts.push(kind.predicate().to_owned());
        }
        if let Some(within) = self.within {
            parts.push(format!("kMDItemFSContentChangeDate >= {}", within.since()));
        }
        if let Some(tag) = &self.tag {
            parts.push(format!("kMDItemUserTags == \"{}\"cd", escape(tag)));
        }
        if parts.is_empty() {
            // Nothing asked for finds nothing, rather than every file there is.
            return "kMDItemFSName == \"\"".into();
        }
        parts.join(" && ")
    }

    /// What it is called where no one has named it: the text and filters.
    pub fn describe(&self) -> String {
        let mut words = Vec::new();
        if !self.text.trim().is_empty() {
            words.push(format!("“{}”", self.text.trim()));
        }
        if let Some(kind) = self.kind {
            words.push(kind.label().to_owned());
        }
        if let Some(within) = self.within {
            words.push(within.label().to_lowercase());
        }
        if let Some(tag) = &self.tag {
            words.push(format!("tagged {tag}"));
        }
        if words.is_empty() { "Search".into() } else { words.join(", ") }
    }

    /// Written as one line, for a location or a saved file: fields separated
    /// by `\u{1f}`, which no one types.
    pub fn encode(&self) -> String {
        [
            self.text.clone(),
            if self.contents { "1".into() } else { String::new() },
            self.kind.map(|k| k.word().to_owned()).unwrap_or_default(),
            self.within.map(|w| w.word().to_owned()).unwrap_or_default(),
            self.tag.clone().unwrap_or_default(),
            self.scope.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        ]
        .join("\u{1f}")
    }

    pub fn decode(text: &str) -> Query {
        let mut fields = text.split('\u{1f}');
        let mut next = || fields.next().unwrap_or("").to_owned();
        let (text, contents, kind, within, tag, scope) = (next(), next(), next(), next(), next(), next());
        Query {
            text,
            contents: contents == "1",
            kind: Kind::from_word(&kind),
            within: Within::from_word(&within),
            tag: (!tag.is_empty()).then_some(tag),
            scope: (!scope.is_empty()).then(|| PathBuf::from(scope)),
        }
    }
}

/// Spotlight's query syntax, escaped.
fn escape(text: &str) -> String {
    text.chars().flat_map(|c| if matches!(c, '\\' | '"' | '*' | '?') { vec!['\\', c] } else { vec![c] }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_tokens_narrow_and_the_rest_is_the_name() {
        let q = Query::typed("tax kind:pdf date:year tag:Red 2025");
        assert_eq!(q.text, "tax 2025");
        assert_eq!(q.kind, Some(Kind::Pdf));
        assert_eq!(q.within, Some(Within::Year));
        assert_eq!(q.tag.as_deref(), Some("Red"));
        assert!(q.is_filtered());
        // An unknown kind is left as text rather than dropped.
        assert_eq!(Query::typed("kind:spaceship").text, "kind:spaceship");
        assert_eq!(Query::typed("kind:photos").kind, Some(Kind::Image));
    }

    #[test]
    fn the_predicate_holds_every_condition() {
        let q = Query { text: "a\"b".into(), kind: Some(Kind::Image), within: Some(Within::Week), ..Default::default() };
        assert_eq!(
            q.predicate(),
            "kMDItemFSName == \"*a\\\"b*\"cd && kMDItemContentTypeTree == \"public.image\" && kMDItemFSContentChangeDate >= $time.today(-7)"
        );
        let contents = Query { text: "x".into(), contents: true, ..Default::default() };
        assert!(contents.predicate().contains("kMDItemTextContent"));
        assert!(Query::default().is_empty());
    }

    #[test]
    fn a_query_survives_being_written_down() {
        let q = Query {
            text: "notes".into(),
            contents: true,
            kind: Some(Kind::Text),
            within: Some(Within::Month),
            tag: Some("Work".into()),
            scope: Some(PathBuf::from("/Users/me/Documents")),
        };
        assert_eq!(Query::decode(&q.encode()), q);
        assert_eq!(Query::decode(&Query::default().encode()), Query::default());
        assert_eq!(q.describe(), "“notes”, Text, past month, tagged Work");
    }
}
