//! Davy Labs: display-name filter.
//!
//! The Davy Labs apps are built from open-source engines whose upstream product names
//! (`PhotoCraft`, `ArtCraft`, …) can still turn up in text that the app shows, for example
//! in a message coming from a shared library. This module rewrites those names to the
//! Davy Labs names in text that is laid out *for display only*. egui calls it from
//! [`WidgetText`](https://docs.rs/egui/latest/egui/enum.WidgetText.html) layout and from
//! the `Painter::layout*` helpers. It is never applied to the editable buffer of a text
//! edit, and it never changes stored data: it only changes what gets drawn.
//!
//! Text without any of the needles (`Craft`, `craft`, `CRAFT`, `Deck Lab`, `DECK LAB`) is
//! returned unchanged without allocating. Results for matching strings are cached.

use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use core::cell::RefCell;

use super::{ByteIndex, LayoutJob};

/// `(prefix as written upstream, Davy Labs name)`. `Art` is the suite (`ArtCraft`).
const ENGINES: &[(&[&str], &str)] = &[
    (&["Photo"], "Photo Lab"),
    (&["Vector"], "Vector Lab"),
    (&["Pdf", "PDF"], "PDF Lab"),
    (&["Word"], "Word Lab"),
    (&["Deck"], "Power Lab"),
    (&["Grid"], "Grid Lab"),
    (&["Film"], "Film Lab"),
    (&["Sound"], "Sound Lab"),
    (&["Light"], "Light Lab"),
    (&["Effect"], "Effect Lab"),
    (&["Design"], "Design Lab"),
    (&["Fig"], "Fig Lab"),
    (&["CAD", "Cad"], "CAD Lab"),
    (&["Art"], "Davy Labs"),
];

/// Replacements that are not `<Engine>Craft`.
const EXTRA: &[(&str, &str)] = &[
    ("getartcraft.com", "davylabs.dev"),
    ("getartcraft", "davylabs"),
    ("Crafting Apps", "Davy Labs apps"),
    ("Crafting apps", "Davy Labs apps"),
    ("Craft Apps", "Davy Labs apps"),
    ("Craft apps", "Davy Labs apps"),
    ("CRAFTING APPS", "DAVY LABS APPS"),
    ("CRAFT APPS", "DAVY LABS APPS"),
    ("Deck Lab", "Power Lab"),
    ("DECK LAB", "POWER LAB"),
];

/// Spellings that are left alone on purpose.
///
/// `Soundcraft` is a real audio hardware brand (mixers, USB audio interfaces), so a device
/// name like "Soundcraft Signature 12" must keep its name. Upstream writes its app
/// `SoundCraft`, which is still replaced. All-lowercase spellings (`photocraft`) are never
/// replaced either: they are file extensions, ids and URLs.
const KEEP: &[&str] = &["Soundcraft"];

/// Cheap test: can `text` contain anything we replace?
#[inline]
fn has_needle(text: &str) -> bool {
    text.contains("Craft")
        || text.contains("craft")
        || text.contains("CRAFT")
        || text.contains("Deck Lab")
        || text.contains("DECK LAB")
}

/// All `(from, to)` pairs, longest `from` first so that a longer name wins at a position.
fn patterns() -> &'static [(String, String)] {
    static PATTERNS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        let mut out: Vec<(String, String)> = Vec::new();
        for (prefixes, name) in ENGINES {
            for prefix in *prefixes {
                for (craft, upper) in [("Craft", false), ("craft", false), ("CRAFT", true)] {
                    let from = if upper {
                        alloc::format!("{}{craft}", prefix.to_uppercase())
                    } else {
                        alloc::format!("{prefix}{craft}")
                    };
                    if KEEP.contains(&from.as_str()) {
                        continue;
                    }
                    let to = if upper {
                        name.to_uppercase()
                    } else {
                        (*name).to_owned()
                    };
                    if !out.iter().any(|(f, _)| *f == from) {
                        out.push((from, to));
                    }
                }
            }
        }
        for (from, to) in EXTRA {
            out.push(((*from).to_owned(), (*to).to_owned()));
        }
        // Stable sort: longest first, ties keep table order.
        out.sort_by_key(|(from, _)| core::cmp::Reverse(from.len()));
        out
    })
}

/// One replaced piece: `original[orig.0..orig.1]` became `text[new.0..new.1]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    orig: (usize, usize),
    new: (usize, usize),
}

/// The result of filtering a string that contained at least one upstream name.
#[derive(Debug)]
struct Filtered {
    text: String,
    spans: Vec<Span>,
}

impl Filtered {
    /// Map a byte offset in the original string to one in the filtered string.
    /// An offset inside a replaced name maps to the end of its replacement, so
    /// contiguous ranges stay contiguous.
    fn map_offset(&self, offset: usize) -> usize {
        let (mut last_orig, mut last_new) = (0, 0);
        for span in &self.spans {
            if offset <= span.orig.0 {
                break;
            }
            if offset < span.orig.1 {
                return span.new.1;
            }
            last_orig = span.orig.1;
            last_new = span.new.1;
        }
        last_new + (offset - last_orig)
    }
}

/// Scan `text` once, replacing names left to right. `None` if nothing was replaced.
fn compute(text: &str) -> Option<Filtered> {
    let patterns = patterns();
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut spans = Vec::new();
    let mut copied = 0; // bytes of `text` already handled
    let mut i = 0;
    'scan: while i < bytes.len() {
        let b = bytes[i];
        // Every pattern is ASCII, so a match can only start on an ASCII byte, which is
        // always a char boundary.
        if b.is_ascii_alphabetic() {
            // Skip `KEEP` words as a whole.
            for keep in KEEP {
                if bytes[i..].starts_with(keep.as_bytes()) {
                    i += keep.len();
                    continue 'scan;
                }
            }
            for (from, to) in patterns {
                if from.as_bytes()[0] == b && bytes[i..].starts_with(from.as_bytes()) {
                    out.push_str(&text[copied..i]);
                    let start = out.len();
                    out.push_str(to);
                    spans.push(Span {
                        orig: (i, i + from.len()),
                        new: (start, out.len()),
                    });
                    i += from.len();
                    copied = i;
                    continue 'scan;
                }
            }
        }
        i += 1;
    }
    if spans.is_empty() {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(Filtered { text: out, spans })
}

const CACHE_LIMIT: usize = 1024;

std::thread_local! {
    static CACHE: RefCell<ahash::HashMap<String, Option<Arc<Filtered>>>> =
        RefCell::new(ahash::HashMap::default());
}

/// Filter with the per-thread cache. `None` = unchanged.
fn lookup(text: &str) -> Option<Arc<Filtered>> {
    if !has_needle(text) {
        return None;
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(hit) = cache.get(text) {
            return hit.clone();
        }
        let result = compute(text).map(Arc::new);
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(text.to_owned(), result.clone());
        result
    })
}

/// Would [`display_str`] change `text`?
pub fn would_change(text: &str) -> bool {
    lookup(text).is_some()
}

/// The display form of `text`: upstream product names replaced by Davy Labs names.
///
/// Borrows (no allocation) when nothing needs replacing.
pub fn display_str(text: &str) -> Cow<'_, str> {
    match lookup(text) {
        None => Cow::Borrowed(text),
        Some(filtered) => Cow::Owned(filtered.text.clone()),
    }
}

/// Like [`display_str`], for an owned string. Returns `text` itself when unchanged.
pub fn display_string(text: String) -> String {
    match lookup(&text) {
        None => text,
        Some(filtered) => filtered.text.clone(),
    }
}

/// Apply [`display_str`] to a [`LayoutJob`], keeping its sections on the same text.
pub fn display_job(job: &mut LayoutJob) {
    let Some(filtered) = lookup(&job.text) else {
        return;
    };
    for section in &mut job.sections {
        let start = filtered.map_offset(section.byte_range.start.0);
        let end = filtered.map_offset(section.byte_range.end.0).max(start);
        section.byte_range = ByteIndex(start)..ByteIndex(end);
    }
    job.text.clone_from(&filtered.text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextFormat;
    use ecolor::Color32;

    /// Distinct formats so `LayoutJob::append` doesn't merge the sections.
    fn fmt(color: Color32) -> TextFormat {
        TextFormat {
            color,
            ..Default::default()
        }
    }

    #[test]
    fn unchanged_text_is_borrowed() {
        assert!(matches!(display_str("Hello world"), Cow::Borrowed(_)));
        assert!(matches!(
            display_str("aircraft and witchcraft"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(display_str(""), Cow::Borrowed(_)));
    }

    #[test]
    fn names_are_replaced() {
        assert_eq!(display_str("About PhotoCraft"), "About Photo Lab");
        assert_eq!(display_str("Photocraft 0.18"), "Photo Lab 0.18");
        assert_eq!(display_str("PdfCraft and PDFCraft"), "PDF Lab and PDF Lab");
        assert_eq!(display_str("DeckCraft"), "Power Lab");
        assert_eq!(display_str("Deck Lab"), "Power Lab");
        assert_eq!(display_str("CADCraft / CadCraft"), "CAD Lab / CAD Lab");
        assert_eq!(display_str("PHOTOCRAFT"), "PHOTO LAB");
        assert_eq!(
            display_str("Part of ArtCraft, see getartcraft.com"),
            "Part of Davy Labs, see davylabs.dev"
        );
        assert_eq!(
            display_str("https://www.getartcraft.com/apps"),
            "https://www.davylabs.dev/apps"
        );
        assert_eq!(display_str("All Crafting Apps"), "All Davy Labs apps");
        assert_eq!(display_str("ArtCraft apps"), "Davy Labs apps");
        assert_eq!(display_str("Open in VectorCraft…"), "Open in Vector Lab…");
        for (from, to) in [
            ("WordCraft", "Word Lab"),
            ("GridCraft", "Grid Lab"),
            ("FilmCraft", "Film Lab"),
            ("SoundCraft", "Sound Lab"),
            ("LightCraft", "Light Lab"),
            ("EffectCraft", "Effect Lab"),
            ("DesignCraft", "Design Lab"),
            ("FigCraft", "Fig Lab"),
        ] {
            assert_eq!(display_str(from), to);
        }
    }

    #[test]
    fn kept_spellings() {
        // File extensions, ids and URLs are lowercase: untouched.
        assert_eq!(display_str("file.photocraft"), "file.photocraft");
        assert_eq!(display_str("photocraft://open"), "photocraft://open");
        // A real hardware brand.
        assert_eq!(
            display_str("Soundcraft Signature 12"),
            "Soundcraft Signature 12"
        );
        // Already ours: idempotent.
        assert_eq!(
            display_str("Photo Lab by Davy Labs"),
            "Photo Lab by Davy Labs"
        );
    }

    #[test]
    fn non_ascii_around_names() {
        assert_eq!(display_str("«PhotoCraft» — ✓"), "«Photo Lab» — ✓");
        assert_eq!(display_str("日本語ArtCraft日本語"), "日本語Davy Labs日本語");
    }

    #[test]
    fn owned_string_unchanged_is_same_allocation() {
        let s = String::from("Nothing to see");
        let ptr = s.as_ptr();
        let out = display_string(s);
        assert_eq!(out.as_ptr(), ptr);
    }

    #[test]
    fn layout_job_sections_follow_the_text() {
        let mut job = LayoutJob::default();
        job.append("Welcome to ", 0.0, fmt(Color32::RED));
        job.append("PhotoCraft", 0.0, fmt(Color32::GREEN));
        job.append(" by ArtCraft!", 0.0, fmt(Color32::BLUE));
        display_job(&mut job);
        assert_eq!(job.text, "Welcome to Photo Lab by Davy Labs!");
        let parts: Vec<&str> = job
            .sections
            .iter()
            .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(parts, ["Welcome to ", "Photo Lab", " by Davy Labs!"]);
    }

    #[test]
    fn layout_job_name_split_across_sections() {
        let mut job = LayoutJob::default();
        job.append("Photo", 0.0, fmt(Color32::RED));
        job.append("Craft rocks", 0.0, fmt(Color32::GREEN));
        display_job(&mut job);
        assert_eq!(job.text, "Photo Lab rocks");
        let ranges: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.start.0, s.byte_range.end.0))
            .collect();
        // Contiguous and covering the whole text.
        assert_eq!(ranges, [(0, 9), (9, 15)]);
    }

    #[test]
    fn cache_returns_same_result() {
        for _ in 0..3 {
            assert_eq!(display_str("GridCraft"), "Grid Lab");
        }
    }
}
