// SPDX-License-Identifier: MPL-2.0

//! Readable names for local videos whose player exposes audio-stream tags.
//!
//! This only cleans information already present in the filename. In particular,
//! an episode number never causes us to guess an episode name or series.

use std::path::PathBuf;

pub(super) fn local_video_path(media_url: &str) -> Option<PathBuf> {
    let path = reqwest::Url::parse(media_url).ok()?.to_file_path().ok()?;
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "mkv"
            | "mp4"
            | "m4v"
            | "webm"
            | "avi"
            | "mov"
            | "mpg"
            | "mpeg"
            | "ogv"
            | "wmv"
            | "ts"
            | "m2ts"
            | "mts"
            | "vob"
            | "3gp"
            | "3g2"
    )
    .then_some(path)
}

pub(super) fn from_url(media_url: &str) -> Option<String> {
    let path = local_video_path(media_url)?;
    let stem = path.file_stem()?.to_str()?.trim();
    (!stem.is_empty()).then(|| readable_title(stem))
}

fn readable_title(stem: &str) -> String {
    let suffix = technical_suffix(stem);
    let mut title = &stem[..suffix.unwrap_or(stem.len())];
    // A leading bracketed group is a release convention, but ordinary names
    // such as "[Director's Cut] Film" should survive without release evidence.
    if suffix.is_some() || episode_marker(title).is_some() {
        while let Some(rest) = title.trim_start().strip_prefix('[') {
            let Some((_, remainder)) = rest.split_once(']') else {
                break;
            };
            if remainder.trim().is_empty() {
                break;
            }
            title = remainder.trim_start();
        }
    }
    title = if suffix.is_some() {
        title.trim_matches(separator)
    } else {
        title.trim()
    };
    if title.is_empty() {
        return stem.to_owned();
    }

    if let Some((start, end)) = episode_marker(title) {
        let series = clean_separators(title[..start].trim_matches(separator));
        let episode = title[start..end].to_ascii_uppercase();
        let name = clean_separators(title[end..].trim_start_matches(separator).trim_end());
        [series, episode, name]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" — ")
    } else {
        clean_separators(title)
    }
}

fn separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '.' | '_' | '-' | '—')
}

fn token_boundary(character: char) -> bool {
    separator(character) || matches!(character, '[' | ']' | '(' | ')')
}

fn technical_suffix(title: &str) -> Option<usize> {
    // Only unambiguous video-release markers start a suffix. Words such as
    // "raw", "proper", "web", and "HD" can be part of an actual title.
    const MARKERS: &[&str] = &[
        "480p", "576p", "720p", "1080p", "1080i", "1440p", "2160p", "4320p", "web-dl", "webrip",
        "bluray", "bdrip", "brrip", "dvdrip", "hdtv", "x264", "x265", "h264", "h265", "h.264",
        "h.265", "hevc",
    ];
    let lower = title.to_ascii_lowercase();
    for (offset, character) in title.char_indices() {
        if offset == 0 {
            continue;
        }
        if matches!(character, '[' | '(') {
            let closer = if character == '[' { ']' } else { ')' };
            if let Some(end) = title[offset + 1..].find(closer) {
                let contents = &title[offset + 1..offset + 1 + end];
                // Fansub CRC tags have exactly eight hexadecimal digits.
                if contents.len() == 8 && contents.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Some(offset);
                }
            }
        }
        if !title[..offset]
            .chars()
            .next_back()
            .is_some_and(token_boundary)
        {
            continue;
        }
        if MARKERS.iter().any(|marker| {
            lower[offset..]
                .strip_prefix(marker)
                .is_some_and(|rest| rest.chars().next().is_none_or(token_boundary))
        }) {
            // Include the enclosing technical tag rather than leaving a
            // dangling bracket (e.g. "[WEB 1080p HEVC]") in the title.
            let before = &title[..offset];
            let bracket = before.rfind(['[', '(']);
            return Some(
                bracket
                    .filter(|&start| !before[start + 1..].contains([']', ')']))
                    .unwrap_or(offset),
            );
        }
    }
    None
}

fn episode_marker(title: &str) -> Option<(usize, usize)> {
    let bytes = title.as_bytes();
    for (start, character) in title.char_indices() {
        if start > 0
            && !title[..start]
                .chars()
                .next_back()
                .is_some_and(token_boundary)
        {
            continue;
        }
        let mut end = start;
        if character.eq_ignore_ascii_case(&'s') {
            end += 1;
            if !digits(bytes, &mut end, 1, 3)
                || !consume(bytes, &mut end, b'e')
                || !digits(bytes, &mut end, 1, 3)
            {
                continue;
            }
            // Preserve multiple-episode releases and explicit ranges.
            loop {
                let mut range_end = end;
                let has_hyphen = consume(bytes, &mut range_end, b'-');
                if !(consume(bytes, &mut range_end, b'e') || has_hyphen)
                    || !digits(bytes, &mut range_end, 1, 3)
                    || !title[range_end..]
                        .chars()
                        .next()
                        .is_none_or(|c| token_boundary(c) || c.eq_ignore_ascii_case(&'e'))
                {
                    break;
                }
                end = range_end;
            }
        } else if character.is_ascii_digit() {
            if !digits(bytes, &mut end, 1, 2)
                || !consume(bytes, &mut end, b'x')
                || !digits(bytes, &mut end, 2, 3)
            {
                continue;
            }
        } else {
            continue;
        }
        if title[end..].chars().next().is_none_or(token_boundary) {
            return Some((start, end));
        }
    }
    None
}

fn consume(bytes: &[u8], position: &mut usize, expected: u8) -> bool {
    if bytes
        .get(*position)
        .is_some_and(|byte| byte.eq_ignore_ascii_case(&expected))
    {
        *position += 1;
        true
    } else {
        false
    }
}

fn digits(bytes: &[u8], position: &mut usize, min: usize, max: usize) -> bool {
    let start = *position;
    while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
        *position += 1;
    }
    (min..=max).contains(&(*position - start))
}

fn clean_separators(title: &str) -> String {
    let characters = title.chars().collect::<Vec<_>>();
    let mut cleaned = String::with_capacity(title.len());
    for (index, &character) in characters.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|i| characters.get(i));
        let next = characters.get(index + 1);
        if character == '_' {
            cleaned.push(' ');
        } else if character == '.'
            && previous.is_some_and(|c| c.is_alphanumeric())
            && next.is_some_and(|c| c.is_alphanumeric())
            && !(previous.is_some_and(char::is_ascii_digit)
                && next.is_some_and(char::is_ascii_digit))
        {
            // Keep punctuation in common abbreviations and uppercase initials.
            let word = cleaned.rsplit([' ', '.']).next().unwrap_or_default();
            let abbreviation = matches!(
                word.to_ascii_lowercase().as_str(),
                "mr" | "mrs" | "ms" | "dr" | "st" | "prof" | "vs"
            );
            let initials = previous.is_some_and(char::is_ascii_uppercase)
                && next.is_some_and(char::is_ascii_uppercase)
                && (index < 2 || characters[index - 2] == '.')
                && (index + 2 == characters.len() || characters[index + 2] == '.');
            if abbreviation || initials {
                cleaned.push('.');
            }
            if !initials {
                cleaned.push(' ');
            }
        } else {
            cleaned.push(character);
        }
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(name: &str) -> String {
        reqwest::Url::from_file_path(PathBuf::from("/videos").join(name))
            .unwrap()
            .into()
    }

    #[test]
    fn cosmic_player_episode_release_becomes_readable() {
        assert_eq!(from_url(&url("Dragon.Ball.Super.Beerus.S01E01.The.God.Who.Destroys.1080p.CR.HYBRiD.WEB-DL.AAC2.0.H.264-4kHdHub.Com.mkv")).as_deref(), Some("Dragon Ball Super Beerus — S01E01 — The God Who Destroys"));
    }

    #[test]
    fn fansub_release_tags_are_removed_without_inventing_episode_names() {
        assert_eq!(
            from_url(&url(
                "[SubsPlease] 葬送のフリーレン - 01 (1080p) [A1B2C3D4].mkv"
            ))
            .as_deref(),
            Some("葬送のフリーレン - 01")
        );
        assert_eq!(
            from_url(&url(
                "[Group] Show.Name.S02E03.Episode.Title.[WEB 1080p HEVC AAC].mkv"
            ))
            .as_deref(),
            Some("Show Name — S02E03 — Episode Title")
        );
    }

    #[test]
    fn common_episode_markers_and_multi_episode_releases_are_preserved() {
        for (name, expected) in [
            ("Show_s01e02_Name.mp4", "Show — S01E02 — Name"),
            ("Show.1x02.Name.720p.mkv", "Show — 1X02 — Name"),
            (
                "Show.S01E01E02.Double.Episode.mkv",
                "Show — S01E01E02 — Double Episode",
            ),
            (
                "Show.S01E01-E02.Double.Episode.mkv",
                "Show — S01E01-E02 — Double Episode",
            ),
            (
                "Show.S01E01-02.Double.Episode.mkv",
                "Show — S01E01-02 — Double Episode",
            ),
            ("S01E02.mkv", "S01E02"),
        ] {
            assert_eq!(from_url(&url(name)).as_deref(), Some(expected));
        }
    }

    #[test]
    fn meaningful_numbers_unicode_and_punctuation_survive() {
        for name in [
            "Evangelion: 3.0+1.0 Thrice Upon a Time",
            "Spider-Man: Across the Spider-Verse (2023)",
            "Dr. Strangelove",
            "Mr.",
            "U.S.A.",
            "2001: A Space Odyssey",
            "魔女の宅急便",
            "[Director's Cut] Film",
            "A Raw Deal",
            "The Web",
        ] {
            assert_eq!(
                from_url(&url(&format!("{name}.mkv"))).as_deref(),
                Some(name)
            );
        }
        assert_eq!(
            from_url(&url("Mr.Robot.S01E01.Hello.Friend.1080p.mkv")).as_deref(),
            Some("Mr. Robot — S01E01 — Hello Friend")
        );
        assert_eq!(
            from_url(&url("Born.in.the.U.S.A.mp4")).as_deref(),
            Some("Born in the U.S.A")
        );
    }

    #[test]
    fn only_local_video_urls_are_accepted() {
        for source in [
            "https://example.com/movie.mkv",
            "file:///music/song.flac",
            "file://remotehost/movie.mp4",
            "/videos/movie.mkv",
            "nonsense",
        ] {
            assert!(from_url(source).is_none());
            assert!(local_video_path(source).is_none());
        }
        assert_eq!(
            local_video_path("file:///videos/Name%20%23%201.MKV"),
            Some(PathBuf::from("/videos/Name # 1.MKV"))
        );
    }
}
