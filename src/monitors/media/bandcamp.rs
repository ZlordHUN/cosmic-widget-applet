// SPDX-License-Identifier: MPL-2.0

//! Resolve public album metadata when Bandcamp's browser media session omits it.

use reqwest::{Url, header::CONTENT_TYPE};
use scraper::{Html, Selector};

const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TRACKS: usize = 512;
const MAX_TEXT_CHARS: usize = 1024;
const MAX_TRACK_DURATION_SECONDS: f64 = 24.0 * 60.0 * 60.0;

#[derive(Clone, Debug)]
pub(super) struct PageMetadata {
    pub(super) artwork_url: Option<Url>,
    pub(super) album: String,
    pub(super) artist: String,
    pub(super) tracks: Vec<TrackMetadata>,
    pub(super) tracks_complete: bool,
}

#[derive(Clone, Debug)]
pub(super) struct TrackMetadata {
    pub(super) title: String,
    pub(super) duration_ms: u64,
    pub(super) url: Url,
}

/// Only individual album/track pages identify artwork for a media session.
pub(super) fn page_url(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw).ok()?;
    if !is_public_http_url(&url) {
        return None;
    }
    let host = url.host_str()?;
    if host != "bandcamp.com" && !host.ends_with(".bandcamp.com") {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let mut segments = path.strip_prefix('/')?.split('/');
    let kind = segments.next()?;
    let slug = segments.next()?;
    if !matches!(kind, "album" | "track") || slug.is_empty() || segments.next().is_some() {
        return None;
    }
    let path = format!("/{kind}/{slug}");
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

#[cfg(test)]
pub(super) async fn resolve_artwork_url(client: &reqwest::Client, page: &Url) -> Option<Url> {
    fetch_page_metadata(client, page).await?.artwork_url
}

pub(super) async fn fetch_page_metadata(
    client: &reqwest::Client,
    page: &Url,
) -> Option<PageMetadata> {
    let page = page_url(page.as_str())?;
    let mut response = client
        .get(page)
        .header(reqwest::header::ACCEPT, "text/html")
        .send()
        .await
        .map_err(|error| {
            log::debug!("Unable to fetch Bandcamp page metadata: {error:?}");
        })
        .ok()?;
    if !response.status().is_success() {
        log::debug!("Bandcamp page metadata returned {}", response.status());
        return None;
    }
    // A redirect must still resolve to an album/track page on Bandcamp.
    let final_page = page_url(response.url().as_str())?;
    let content_type = response.headers().get(CONTENT_TYPE)?.to_str().ok()?;
    let content_type = content_type.split(';').next()?.trim();
    if !content_type.eq_ignore_ascii_case("text/html")
        && !content_type.eq_ignore_ascii_case("application/xhtml+xml")
    {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PAGE_BYTES as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > MAX_PAGE_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    page_metadata_from_html(&String::from_utf8_lossy(&bytes), &final_page)
}

fn is_public_http_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        // Url normalizes explicit default ports to None.
        && url.port().is_none()
}

fn trusted_image_url(raw: &str, page: &Url) -> Option<Url> {
    let mut url = page.join(raw.trim()).ok()?;
    if !is_public_http_url(&url) || !url.host_str()?.ends_with(".bcbits.com") {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

fn page_metadata_from_html(html: &str, page: &Url) -> Option<PageMetadata> {
    let document = Html::parse_document(html);
    let mut metadata = PageMetadata {
        artwork_url: artwork_url_from_document(&document, page),
        album: String::new(),
        artist: String::new(),
        tracks: Vec::new(),
        tracks_complete: false,
    };
    let selector = Selector::parse("[data-tralbum]").ok()?;
    for element in document.select(&selector) {
        let Some(data) = element
            .value()
            .attr("data-tralbum")
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .filter(serde_json::Value::is_object)
        else {
            continue;
        };
        metadata.album = bounded_text(&data["current"]["title"]);
        metadata.artist = bounded_text(&data["artist"]);
        if let Some(tracks) = data["trackinfo"].as_array() {
            metadata.tracks = tracks
                .iter()
                .take(MAX_TRACKS)
                .filter_map(|track| parse_track(track, page))
                .collect();
            metadata.tracks_complete = !tracks.is_empty() && metadata.tracks.len() == tracks.len();
        }
        if !metadata.album.is_empty() || !metadata.artist.is_empty() || !metadata.tracks.is_empty()
        {
            break;
        }
    }
    if metadata.artwork_url.is_none()
        && metadata.album.is_empty()
        && metadata.artist.is_empty()
        && metadata.tracks.is_empty()
    {
        return None;
    }
    Some(metadata)
}

fn bounded_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(MAX_TEXT_CHARS)
        .collect()
}

fn parse_track(data: &serde_json::Value, page: &Url) -> Option<TrackMetadata> {
    let title = bounded_text(&data["title"]);
    if title.is_empty() {
        return None;
    }
    let duration_seconds = data["duration"].as_f64()?;
    if !duration_seconds.is_finite()
        || duration_seconds <= 0.0
        || duration_seconds > MAX_TRACK_DURATION_SECONDS
    {
        return None;
    }
    let duration_ms = (duration_seconds * 1000.0) as u64;
    if duration_ms == 0 {
        return None;
    }
    let url = page.join(data["title_link"].as_str()?.trim()).ok()?;
    let url = page_url(url.as_str())?;
    if url.origin() != page.origin() || !url.path().starts_with("/track/") {
        return None;
    }
    Some(TrackMetadata {
        title,
        duration_ms,
        url,
    })
}

fn artwork_url_from_document(document: &Html, page: &Url) -> Option<Url> {
    let meta = Selector::parse("meta").ok()?;
    for property in ["og:image:secure_url", "og:image"] {
        for element in document.select(&meta) {
            let attributes = element.value();
            if attributes
                .attr("property")
                .or_else(|| attributes.attr("name"))
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(property))
                && let Some(url) = attributes
                    .attr("content")
                    .and_then(|content| trusted_image_url(content, page))
            {
                return Some(url);
            }
        }
    }
    let link = Selector::parse("link").ok()?;
    document.select(&link).find_map(|element| {
        let attributes = element.value();
        if attributes.attr("rel").is_some_and(|value| {
            value
                .split_ascii_whitespace()
                .any(|relation| relation.eq_ignore_ascii_case("image_src"))
        }) {
            attributes
                .attr("href")
                .and_then(|href| trusted_image_url(href, page))
        } else {
            None
        }
    })
}

#[cfg(test)]
fn artwork_url_from_html(html: &str, page: &Url) -> Option<Url> {
    artwork_url_from_document(&Html::parse_document(html), page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        page_url("https://artist.bandcamp.com/album/example").unwrap()
    }

    fn tralbum_html(data: serde_json::Value) -> String {
        let encoded = data
            .to_string()
            .replace('&', "&amp;")
            .replace('"', "&quot;");
        format!("<script data-tralbum=\"{encoded}\"></script>")
    }

    #[test]
    fn parses_public_album_track_metadata_and_artwork_from_one_document() {
        let html = format!(
            r#"<meta property="og:image" content="https://f4.bcbits.com/img/cover.jpg">{}"#,
            tralbum_html(serde_json::json!({
                "artist": " Artist & Friends ",
                "current": { "title": " Album \"Title\" ", "type": "album" },
                "featured_track_id": 2,
                "trackinfo": [
                    {
                        "track_id": 1,
                        "title": " First & Second ",
                        "duration": 183.125,
                        "title_link": "/track/first-and-second?from=album#lyrics",
                        "file": { "mp3-128": "https://example.invalid/audio" }
                    },
                    {
                        "track_id": 2,
                        "title": "Another Song",
                        "duration": 201.9999,
                        "title_link": "https://artist.bandcamp.com/track/another-song"
                    }
                ]
            }))
        );
        let metadata = page_metadata_from_html(&html, &page()).unwrap();
        assert_eq!(metadata.album, "Album \"Title\"");
        assert_eq!(metadata.artist, "Artist & Friends");
        assert_eq!(
            metadata.artwork_url.unwrap().as_str(),
            "https://f4.bcbits.com/img/cover.jpg"
        );
        assert_eq!(metadata.tracks.len(), 2);
        assert_eq!(metadata.tracks[0].title, "First & Second");
        assert_eq!(metadata.tracks[0].duration_ms, 183_125);
        assert_eq!(
            metadata.tracks[0].url.as_str(),
            "https://artist.bandcamp.com/track/first-and-second"
        );
        assert_eq!(metadata.tracks[1].duration_ms, 201_999);
        // A page's featured track is static metadata, never a playback signal.
        assert_eq!(metadata.tracks[1].title, "Another Song");
        assert!(metadata.tracks_complete);
    }

    #[test]
    fn preserves_artwork_when_track_data_is_missing_or_malformed() {
        for tralbum in [
            "",
            r#"<script data-tralbum="invalid JSON"></script>"#,
            r#"<script data-tralbum="[]"></script>"#,
            r#"<script data-tralbum='{"trackinfo": "invalid"}'></script>"#,
        ] {
            let html = format!(
                r#"<meta property="og:image" content="https://f4.bcbits.com/img/cover.jpg">{tralbum}"#
            );
            let metadata = page_metadata_from_html(&html, &page()).unwrap();
            assert!(metadata.artwork_url.is_some());
            assert!(metadata.tracks.is_empty());
            assert!(metadata.album.is_empty());
            assert!(metadata.artist.is_empty());
            assert!(!metadata.tracks_complete);
        }
        assert!(page_metadata_from_html("<html>Unavailable</html>", &page()).is_none());
    }

    #[test]
    fn skips_malformed_tracks_and_untrusted_links_without_losing_valid_tracks() {
        let valid_track = serde_json::json!({
            "title": "Song", "duration": 90.0, "title_link": "/track/song"
        });
        let mut tracks = vec![serde_json::Value::Null, valid_track.clone()];
        for invalid_url in [
            "/album/example",
            "/track/",
            "/track/song/extra",
            "https://other.bandcamp.com/track/song",
            "http://artist.bandcamp.com/track/song",
            "https://artist.bandcamp.com:8443/track/song",
            "https://artist.bandcamp.com.example.org/track/song",
            "https://user:password@artist.bandcamp.com/track/song",
            "file:///track/song",
        ] {
            let mut track = valid_track.clone();
            track["title_link"] = invalid_url.into();
            tracks.push(track);
        }
        for invalid_duration in [
            serde_json::Value::Null,
            serde_json::json!("90"),
            serde_json::json!(-1.0),
            serde_json::json!(0.0),
            serde_json::json!(0.0001),
            serde_json::json!(MAX_TRACK_DURATION_SECONDS + 1.0),
        ] {
            let mut track = valid_track.clone();
            track["duration"] = invalid_duration;
            tracks.push(track);
        }
        let mut missing_title = valid_track;
        missing_title["title"] = "  ".into();
        tracks.push(missing_title);
        let html = tralbum_html(serde_json::json!({"trackinfo": tracks}));
        let metadata = page_metadata_from_html(&html, &page()).unwrap();
        assert_eq!(metadata.tracks.len(), 1);
        assert_eq!(metadata.tracks[0].title, "Song");
        assert!(metadata.artwork_url.is_none());
        assert!(!metadata.tracks_complete);
    }

    #[test]
    fn skips_invalid_embedded_json_before_valid_metadata() {
        let html = format!(
            r#"<script data-tralbum="invalid"></script>{}"#,
            tralbum_html(serde_json::json!({
                "current": { "title": "Album" }, "artist": "Artist"
            }))
        );
        let metadata = page_metadata_from_html(&html, &page()).unwrap();
        assert_eq!(metadata.album, "Album");
        assert_eq!(metadata.artist, "Artist");
        assert!(metadata.tracks.is_empty());
    }

    #[test]
    fn bounds_track_count_and_unicode_text_lengths() {
        let long_title = "音".repeat(MAX_TEXT_CHARS + 10);
        let tracks = vec![
            serde_json::json!({
                "title": long_title, "duration": 90, "title_link": "/track/song"
            });
            MAX_TRACKS + 1
        ];
        let html = tralbum_html(serde_json::json!({
            "current": { "title": long_title },
            "artist": long_title,
            "trackinfo": tracks
        }));
        let metadata = page_metadata_from_html(&html, &page()).unwrap();
        assert_eq!(metadata.album.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(metadata.artist.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(metadata.tracks.len(), MAX_TRACKS);
        assert_eq!(metadata.tracks[0].title.chars().count(), MAX_TEXT_CHARS);
        assert!(!metadata.tracks_complete);
    }

    #[test]
    fn canonicalizes_only_bandcamp_album_and_track_pages() {
        assert_eq!(
            page_url("https://ARTIST.bandcamp.com:443/album/example/?from=discover#track-2")
                .unwrap()
                .as_str(),
            "https://artist.bandcamp.com/album/example"
        );
        assert_eq!(
            page_url("http://artist.bandcamp.com:80/track/example?autoplay=true")
                .unwrap()
                .as_str(),
            "http://artist.bandcamp.com/track/example"
        );
        assert!(page_url("https://bandcamp.com/album/example").is_some());
    }

    #[test]
    fn rejects_unrelated_pages_credentials_and_nonstandard_ports() {
        for raw in [
            "file:///album/example",
            "ftp://artist.bandcamp.com/album/example",
            "https://bandcamp.com.example.org/album/example",
            "https://notbandcamp.com/album/example",
            "https://artist.bandcamp.com@127.0.0.1/album/example",
            "https://user:password@artist.bandcamp.com/album/example",
            "https://artist.bandcamp.com:8443/album/example",
            "http://artist.bandcamp.com:443/album/example",
            "https://artist.bandcamp.com/music",
            "https://artist.bandcamp.com/album/",
            "https://artist.bandcamp.com/album/example/extra",
        ] {
            assert!(page_url(raw).is_none(), "accepted {raw}");
        }
    }

    #[test]
    fn parses_attribute_order_case_and_html_entities() {
        let html = r#"<META CONTENT='https://f4.bcbits.com/img/a123_5.jpg?x=1&amp;y=2'
            PROPERTY='OG:IMAGE'>"#;
        assert_eq!(
            artwork_url_from_html(html, &page()).unwrap().as_str(),
            "https://f4.bcbits.com/img/a123_5.jpg?x=1&y=2"
        );
    }

    #[test]
    fn prefers_secure_open_graph_image_over_image_source_link() {
        let html = r#"
            <link rel="image_src" href="https://f4.bcbits.com/img/link.jpg">
            <meta property="og:image" content="http://f4.bcbits.com/img/ordinary.jpg">
            <meta property="og:image:secure_url" content="https://f4.bcbits.com/img/secure.jpg">
        "#;
        assert_eq!(
            artwork_url_from_html(html, &page()).unwrap().as_str(),
            "https://f4.bcbits.com/img/secure.jpg"
        );
    }

    #[test]
    fn accepts_protocol_relative_cdn_urls_and_image_source_fallback() {
        let html = r#"<link href="//f4.bcbits.com/img/cover.jpg#unused" rel="IMAGE_SRC">"#;
        assert_eq!(
            artwork_url_from_html(html, &page()).unwrap().as_str(),
            "https://f4.bcbits.com/img/cover.jpg"
        );
        assert!(trusted_image_url("/img/cover.jpg", &page()).is_none());
    }

    #[test]
    fn ignores_untrusted_images_and_continues_to_a_valid_candidate() {
        for raw in [
            "file:///tmp/cover.jpg",
            "data:image/png;base64,AAAA",
            "https://f4.bcbits.com.example.org/img/cover.jpg",
            "https://notbcbits.com/img/cover.jpg",
            "https://user:password@f4.bcbits.com/img/cover.jpg",
            "https://f4.bcbits.com:8443/img/cover.jpg",
            "https://127.0.0.1/img/cover.jpg",
        ] {
            assert!(trusted_image_url(raw, &page()).is_none(), "accepted {raw}");
        }
        let html = r#"
            <meta property="og:image:secure_url" content="https://example.org/logo.jpg">
            <meta property="og:image" content="">
            <meta name="og:image" content="https://f4.bcbits.com/img/cover.jpg">
        "#;
        assert_eq!(
            artwork_url_from_html(html, &page()).unwrap().as_str(),
            "https://f4.bcbits.com/img/cover.jpg"
        );
    }

    #[tokio::test]
    #[ignore = "requires network and BANDCAMP_TEST_URL for an album/track page"]
    async fn resolves_live_bandcamp_artwork() {
        let raw = std::env::var("BANDCAMP_TEST_URL").expect("set BANDCAMP_TEST_URL");
        let page = page_url(&raw).expect("BANDCAMP_TEST_URL must identify a Bandcamp album/track");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .user_agent("cosmic-widget-applet/0.1")
            .build()
            .unwrap();
        let artwork = resolve_artwork_url(&client, &page)
            .await
            .expect("Bandcamp page did not provide trusted artwork");
        assert!(artwork.host_str().unwrap().ends_with(".bcbits.com"));
    }
}
