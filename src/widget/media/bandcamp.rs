// SPDX-License-Identifier: MPL-2.0

//! Resolve album artwork when Bandcamp's browser media session omits it.

use reqwest::{Url, header::CONTENT_TYPE};
use scraper::{Html, Selector};

const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;

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

pub(super) async fn resolve_artwork_url(client: &reqwest::Client, page: &Url) -> Option<Url> {
    let page = page_url(page.as_str())?;
    let mut response = client
        .get(page)
        .header(reqwest::header::ACCEPT, "text/html")
        .send()
        .await
        .map_err(|error| {
            log::debug!("Unable to fetch Bandcamp artwork metadata: {error:?}");
        })
        .ok()?;
    if !response.status().is_success() {
        log::debug!("Bandcamp artwork metadata returned {}", response.status());
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
    artwork_url_from_html(&String::from_utf8_lossy(&bytes), &final_page)
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

fn artwork_url_from_html(html: &str, page: &Url) -> Option<Url> {
    let document = Html::parse_document(html);
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
mod tests {
    use super::*;

    fn page() -> Url {
        page_url("https://artist.bandcamp.com/album/example").unwrap()
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
