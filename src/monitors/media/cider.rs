// SPDX-License-Identifier: MPL-2.0

//! Native client for Cider's local playback API.

use super::{MediaInfo, PlaybackStatus};
use reqwest::blocking::{Client as HttpClient, RequestBuilder};
use serde::Deserialize;
use std::time::Duration;

const API_BASE_URL: &str = "http://localhost:10767/api/v1/playback";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(super) struct Client {
    http: HttpClient,
    base_url: String,
}

impl Client {
    pub(super) fn new() -> Self {
        let http = HttpClient::builder()
            .user_agent("cosmic-widget-applet/0.1")
            .build()
            .unwrap_or_else(|error| {
                log::warn!("Failed to configure the Cider HTTP client: {error}");
                HttpClient::new()
            });
        Self {
            http,
            base_url: API_BASE_URL.into(),
        }
    }

    pub(super) fn now_playing(&self, token: Option<&str>) -> Option<MediaInfo> {
        let response = self
            .authenticated(self.http.get(self.url("now-playing")), token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json::<NowPlayingResponse>()
            .ok()?;
        if response.status != "ok" {
            return None;
        }

        let is_playing = self.is_playing(token).unwrap_or(true);
        let mut info = response.info?.into_media_info(is_playing)?;
        info.volume = self.volume(token);
        Some(info)
    }

    pub(super) fn command(&self, endpoint: &str, token: Option<&str>) -> bool {
        self.authenticated(self.http.post(self.url(endpoint)), token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .is_ok()
    }

    pub(super) fn seek(&self, position_seconds: u64, token: Option<&str>) -> bool {
        self.authenticated(self.http.post(self.url("seek")), token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "position": position_seconds }))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .is_ok()
    }

    pub(super) fn volume(&self, token: Option<&str>) -> Option<f64> {
        let response = self
            .authenticated(self.http.get(self.url("volume")), token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json::<VolumeResponse>()
            .ok()?;
        response.level()
    }

    pub(super) fn set_volume(&self, volume: f64, token: Option<&str>) -> bool {
        if !super::volume::valid_level(volume) {
            return false;
        }
        self.authenticated(self.http.post(self.url("volume")), token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "volume": volume }))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .is_ok()
    }

    fn is_playing(&self, token: Option<&str>) -> Option<bool> {
        let response = self
            .authenticated(self.http.get(self.url("is-playing")), token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json::<PlaybackStateResponse>()
            .ok()?;
        (response.status == "ok").then_some(response.is_playing)
    }

    fn authenticated(&self, request: RequestBuilder, token: Option<&str>) -> RequestBuilder {
        if let Some(token) = token {
            request.header("apptoken", token)
        } else {
            request
        }
    }

    fn url(&self, endpoint: &str) -> String {
        format!("{}/{endpoint}", self.base_url)
    }
}

#[derive(Deserialize)]
struct VolumeResponse {
    status: String,
    #[serde(default, deserialize_with = "super::volume::deserialize_number")]
    volume: Option<f64>,
}

impl VolumeResponse {
    fn level(self) -> Option<f64> {
        (self.status == "ok")
            .then_some(self.volume)
            .flatten()
            .filter(|level| super::volume::valid_level(*level))
    }
}

#[derive(Deserialize)]
struct NowPlayingResponse {
    status: String,
    info: Option<TrackInfo>,
}

#[derive(Deserialize)]
struct PlaybackStateResponse {
    status: String,
    is_playing: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackInfo {
    name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    album_name: String,
    artwork: Option<Artwork>,
    #[serde(default)]
    duration_in_millis: u64,
    #[serde(default)]
    current_playback_time: f64,
}

impl TrackInfo {
    fn into_media_info(self, is_playing: bool) -> Option<MediaInfo> {
        if self.name.is_empty() {
            return None;
        }

        Some(MediaInfo {
            player_name: "Cider".to_string(),
            title: self.name,
            artist: self.artist_name,
            album: self.album_name,
            art_url: self
                .artwork
                .map(|artwork| artwork.url.replace("{w}", "300").replace("{h}", "300")),
            status: if is_playing {
                PlaybackStatus::Playing
            } else {
                PlaybackStatus::Paused
            },
            position: (self.current_playback_time.max(0.0) * 1000.0) as u64,
            duration: self.duration_in_millis,
            can_play: true,
            can_pause: true,
            can_go_next: true,
            can_go_previous: true,
            can_seek: true,
            ..Default::default()
        })
    }
}

#[derive(Deserialize)]
struct Artwork {
    url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cider_track_json_without_string_scanning() {
        let response: NowPlayingResponse = serde_json::from_str(
            r#"{
                "status":"ok",
                "info":{
                    "albumName":"Album",
                    "artistName":"Artist",
                    "artwork":{"url":"https://example.test/{w}x{h}.png"},
                    "durationInMillis":357187,
                    "name":"A \"quoted\" title",
                    "currentPlaybackTime":141.020301
                }
            }"#,
        )
        .unwrap();

        let info = response.info.unwrap().into_media_info(true).unwrap();
        assert_eq!(info.title, "A \"quoted\" title");
        assert_eq!(info.artist, "Artist");
        assert_eq!(info.album, "Album");
        assert_eq!(info.position, 141_020);
        assert_eq!(info.duration, 357_187);
        assert_eq!(
            info.art_url.as_deref(),
            Some("https://example.test/300x300.png")
        );
        assert_eq!(info.status, PlaybackStatus::Playing);
    }

    #[test]
    fn parses_cider_playback_state() {
        let response: PlaybackStateResponse =
            serde_json::from_str(r#"{"status":"ok","is_playing":false}"#).unwrap();
        assert_eq!(response.status, "ok");
        assert!(!response.is_playing);
    }

    #[test]
    fn volume_readback_requires_a_valid_successful_response() {
        for (json, expected) in [
            (r#"{"status":"ok","volume":0.37}"#, Some(0.37)),
            (r#"{"status":"ok","volume":0}"#, Some(0.0)),
            (r#"{"status":"ok","volume":1}"#, Some(1.0)),
            (r#"{"status":"error","volume":0.5}"#, None),
            (r#"{"status":"ok","volume":1.01}"#, None),
            (r#"{"status":"ok","volume":-1}"#, None),
            (r#"{"status":"ok","volume":"0.5"}"#, None),
            (r#"{"status":"ok","volume":null}"#, None),
            (r#"{"status":"ok"}"#, None),
        ] {
            assert_eq!(
                serde_json::from_str::<VolumeResponse>(json)
                    .unwrap()
                    .level(),
                expected
            );
        }
    }

    #[test]
    fn volume_api_uses_authenticated_normalized_get_and_post() {
        use super::super::volume::tests::http_fixture;
        let (url, request) = http_fixture(200, r#"{"status":"ok","volume":0.42}"#);
        let client = Client {
            http: HttpClient::new(),
            base_url: format!("{url}/api/v1/playback"),
        };
        assert_eq!(client.volume(Some("fixture-token")), Some(0.42));
        let (headers, body) = request.join().unwrap();
        assert!(headers.starts_with("GET /api/v1/playback/volume HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("apptoken: fixture-token")
        );
        assert!(body.is_empty());

        let (url, request) = http_fixture(200, r#"{"status":"ok"}"#);
        let client = Client {
            http: HttpClient::new(),
            base_url: format!("{url}/api/v1/playback"),
        };
        for invalid in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(!client.set_volume(invalid, Some("fixture-token")));
        }
        assert!(client.set_volume(0.27, Some("fixture-token")));
        let (headers, body) = request.join().unwrap();
        assert!(headers.starts_with("POST /api/v1/playback/volume HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("apptoken: fixture-token")
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"volume":0.27})
        );
    }

    #[test]
    #[ignore = "requires a running authenticated Cider instance"]
    fn reads_live_cider_state() {
        let token = live_token();
        let info = Client::new().now_playing(Some(&token)).unwrap();

        eprintln!(
            "Cider: {:?}, {} / {} ms, {} - {}",
            info.status, info.position, info.duration, info.artist, info.title
        );
        assert!(!info.title.is_empty());
        assert!(info.duration > 0);
    }

    #[test]
    #[ignore = "requires a running authenticated Cider instance"]
    fn seeks_live_cider_to_its_current_position() {
        let token = live_token();
        let client = Client::new();
        let info = client.now_playing(Some(&token)).unwrap();
        assert!(client.seek(info.position / 1000, Some(&token)));
    }

    fn live_token() -> String {
        let token_path = dirs::config_dir()
            .unwrap()
            .join("cosmic/com.github.zoliviragh.CosmicWidget/v1/cider_api_token");
        let raw = std::fs::read_to_string(token_path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }
}
