// SPDX-License-Identifier: MPL-2.0

//! Fit subtitle captions to the rendered width without losing track qualifiers.

use cosmic::Renderer;
use cosmic::iced::advanced::text::{self, Paragraph as _};
use cosmic::iced::{Font, Size, alignment};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const TEXT_SIZE: f32 = 12.0;
const MAX_CACHED_LABELS: usize = 256;

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    label: String,
    suffix: Option<String>,
    width: u32,
    font: Font,
}

pub(super) fn compact(label: &str, max_width: f32) -> String {
    cached_fit(label, None, max_width)
}

/// Fit a caption while reserving its disambiguating suffix. The suffix is
/// normalized and separated from the caption by one space.
pub(super) fn fit_with_suffix(label: &str, suffix: &str, max_width: f32) -> String {
    cached_fit(label, Some(suffix), max_width)
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cached_fit(label: &str, suffix: Option<&str>, max_width: f32) -> String {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, String>>> = OnceLock::new();
    let max_width = if max_width.is_nan() {
        0.0
    } else {
        max_width.max(0.0)
    };
    let key = CacheKey {
        label: normalize(label),
        suffix: suffix.map(normalize),
        width: max_width.to_bits(),
        font: cosmic::font::default(),
    };
    let cache = CACHE.get_or_init(Mutex::default);
    if let Ok(cache) = cache.lock() {
        if let Some(fitted) = cache.get(&key) {
            return fitted.clone();
        }
    }
    // Font shaping can take its own lock; never hold the caption cache lock
    // while shaping. Rebuilds with unchanged labels reuse the completed result.
    let fitted = if let Some(suffix) = &key.suffix {
        fit(&key.label, suffix, max_width, key.font)
    } else if width(&key.label, key.font) <= max_width {
        key.label.clone()
    } else if let Some((language, qualifier)) = semantic_label(&key.label) {
        let full = joined(language, &qualifier);
        let plain = joined(language, &qualifier.replace("· ", ""));
        if width(&full, key.font) <= max_width {
            full
        } else if width(&plain, key.font) <= max_width {
            // Remove decorative separators before shortening the language.
            plain
        } else {
            fit(language, &qualifier, max_width, key.font)
        }
    } else {
        fit(&key.label, "", max_width, key.font)
    };
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_CACHED_LABELS {
            cache.clear();
        }
        cache.insert(key, fitted.clone());
    }
    fitted
}

fn width(label: &str, font: Font) -> f32 {
    if label.is_empty() {
        return 0.0;
    }
    <Renderer as text::Renderer>::Paragraph::with_text(text::Text {
        content: label,
        bounds: Size::new(f32::INFINITY, TEXT_SIZE * 1.2),
        size: TEXT_SIZE.into(),
        line_height: text::LineHeight::Relative(1.2),
        font,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
        ellipsize: text::Ellipsize::None,
    })
    .min_width()
}

fn joined(label: &str, suffix: &str) -> String {
    match (label.is_empty(), suffix.is_empty()) {
        (true, _) => suffix.to_owned(),
        (_, true) => label.to_owned(),
        _ => format!("{label} {suffix}"),
    }
}

fn fit(label: &str, suffix: &str, max_width: f32, font: Font) -> String {
    let full = joined(label, suffix);
    if width(&full, font) <= max_width {
        return full;
    }
    let marker = joined("…", suffix);
    if width(&marker, font) > max_width {
        // Extremely narrow bounds may fit only the identifying suffix. Never
        // overrun the arrow or attempt an invalid UTF-8 byte truncation.
        return if suffix.is_empty() {
            String::new()
        } else if width(suffix, font) <= max_width {
            suffix.to_owned()
        } else {
            fit(suffix, "", max_width, font)
        };
    }
    let label = label.trim_end_matches('…').trim_end();
    let boundaries: Vec<_> = label
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(label.len()))
        .collect();
    let mut low = 0;
    let mut high = boundaries.len() - 1;
    while low < high {
        let middle = (low + high + 1) / 2;
        let candidate = joined(
            &format!("{}…", label[..boundaries[middle]].trim_end()),
            suffix,
        );
        if width(&candidate, font) <= max_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    joined(&format!("{}…", label[..boundaries[low]].trim_end()), suffix)
}

fn semantic_label(label: &str) -> Option<(&str, String)> {
    let lower = label.to_ascii_lowercase();
    let words: Vec<_> = lower
        .split(|character: char| !character.is_ascii_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    let mut qualifiers = Vec::new();
    let mut markers = Vec::new();
    if words.contains(&"forced") {
        qualifiers.push("Forced");
        markers.push("forced");
    }
    if words.contains(&"sdh") || lower.contains("hearing impaired") {
        qualifiers.push("SDH");
        markers.extend(["sdh", "hearing impaired"]);
    } else if words.contains(&"cc") {
        qualifiers.push("CC");
        markers.push("cc");
    }
    if qualifiers.is_empty() {
        return None;
    }
    let qualifier_start = markers
        .iter()
        .filter_map(|marker| {
            lower
                .match_indices(marker)
                .find(|(index, _)| {
                    let before = lower[..*index].chars().next_back();
                    let after = lower[*index + marker.len()..].chars().next();
                    before.is_none_or(|character| !character.is_ascii_alphabetic())
                        && after.is_none_or(|character| !character.is_ascii_alphabetic())
                })
                .map(|(index, _)| index)
        })
        .min()?;
    let prefix = &label[..qualifier_start];
    let language_end = [" · ", " - ", " — "]
        .iter()
        .filter_map(|separator| prefix.find(separator))
        .min()
        .unwrap_or(prefix.len());
    let language = prefix[..language_end].trim_end_matches(|character: char| {
        character.is_whitespace() || matches!(character, '·' | '-' | '—' | '(' | '[')
    });
    Some((language, format!("· {}", qualifiers.join(" · "))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_language_and_codec_labels_that_fit_the_selector() {
        for label in ["English - SRT", "English - PGS", "Japanese", "Portuguese"] {
            assert_eq!(compact(label, 84.0), label);
        }
        assert_eq!(compact("  English  -  SRT  ", 84.0), "English - SRT");
    }

    #[test]
    fn uses_glyph_width_instead_of_character_count() {
        let font = cosmic::font::default();
        let narrow = "iiiiiiiiii";
        let wide = "WWWWWWWWWW";
        let available = (width(narrow, font) + width(wide, font)) / 2.0;
        assert_eq!(compact(narrow, available), narrow);
        let fitted = compact(wide, available);
        assert_ne!(fitted, wide);
        assert!(fitted.ends_with('…'));
        assert!(width(&fitted, font) <= available);
    }

    #[test]
    fn retains_qualifiers_without_abbreviating_language_unnecessarily() {
        let label = "English · Forced · A very long subtitle title";
        let semantic = "English · Forced";
        let font = cosmic::font::default();
        let available = width(semantic, font) + 0.1;
        assert_eq!(compact(label, available), semantic);
        let plain = "English Forced";
        assert_eq!(compact(label, width(plain, font) + 0.1), plain);
        let long = compact(
            "An exceptionally long language name · SDH · Extra information",
            84.0,
        );
        assert!(long.ends_with("· SDH"));
        assert!(long.contains('…'));
        assert!(width(&long, font) <= 84.0);
    }

    #[test]
    fn reserves_suffix_and_handles_unicode_and_tiny_widths() {
        let font = cosmic::font::default();
        for label in [
            "日本語のとても長い字幕の名前",
            "Žluťoučký kůň · české titulky",
            "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301} subtitle title",
            "English already shortened…",
        ] {
            let fitted = fit_with_suffix(label, " 12", 84.0);
            assert!(fitted.ends_with("12"));
            assert!(width(&fitted, font) <= 84.0);
            assert!(!fitted.contains("……"));
        }
        assert_eq!(compact("English", 0.0), "");
        assert_eq!(compact("English", f32::NAN), "");
        assert_eq!(fit_with_suffix("English", "12", 0.0), "");
    }
}
