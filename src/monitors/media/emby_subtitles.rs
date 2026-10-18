// SPDX-License-Identifier: MPL-2.0

//! Subtitle metadata and commands for the native Emby session backend.
//! Uses the existing media polling/control workers; item details are cached.

use std::collections::HashSet;
use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer};

use super::{
    EmbyControl, EmbyCredentials, EmbySession, MediaMonitor, MultiPlayerState, PlaybackStatus,
    PlayerId, SubtitleState, SubtitleTarget, SubtitleTrack,
};

const COMMAND: &str = "SetSubtitleStreamIndex";
const NEGATIVE_RETRY: Duration = Duration::from_secs(30);
const DETAIL_REFRESH: Duration = Duration::from_secs(300);
const MAX_DETAIL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SUBTITLE_TRACKS: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct MediaStream {
    #[serde(rename = "Type")]
    kind: Option<String>,
    index: Option<i32>,
    display_title: Option<String>,
    display_language: Option<String>,
    language: Option<String>,
    title: Option<String>,
    codec: Option<String>,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    is_forced: bool,
    #[serde(default)]
    is_hearing_impaired: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct MediaSource {
    id: Option<String>,
    default_subtitle_stream_index: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_streams")]
    media_streams: Option<Vec<MediaStream>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemDetails {
    id: String,
    #[serde(default, deserialize_with = "deserialize_streams")]
    media_streams: Option<Vec<MediaStream>>,
    #[serde(default, deserialize_with = "deserialize_sources")]
    media_sources: Option<Vec<MediaSource>>,
}

// Optional subtitle metadata must never make the entire playing session vanish
// when a client reports a missing or malformed stream field.
pub(super) fn deserialize_streams<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<MediaStream>>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_array().map(|values| {
        values
            .iter()
            .filter_map(|value| serde_json::from_value(value.clone()).ok())
            .collect()
    }))
}

pub(super) fn deserialize_sources<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<MediaSource>>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_array().map(|values| {
        values
            .iter()
            .filter_map(|value| serde_json::from_value(value.clone()).ok())
            .collect()
    }))
}

pub(super) fn deserialize_source<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<MediaSource>, D::Error> {
    Ok(serde_json::from_value(serde_json::Value::deserialize(deserializer)?).ok())
}

pub(super) fn deserialize_index<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<i32>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_i64()
        .and_then(|index| i32::try_from(index).ok())
        .filter(|index| *index >= -1))
}

pub(super) fn deserialize_commands<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

fn target(session: &EmbySession) -> Option<SubtitleTarget> {
    if !session.supports_remote_control
        || !session
            .supported_commands
            .iter()
            .any(|command| command.eq_ignore_ascii_case(COMMAND))
    {
        return None;
    }
    let item = session.now_playing_item.as_ref()?;
    let play = session.play_state.as_ref()?;
    if session.id.is_empty() || item.id.is_empty() {
        return None;
    }
    Some(SubtitleTarget {
        session_id: session.id.clone(),
        item_id: item.id.clone(),
        media_source_id: play
            .media_source_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .cloned()
            .or_else(|| {
                play.media_source
                    .as_ref()?
                    .id
                    .as_ref()
                    .filter(|id| !id.is_empty())
                    .cloned()
            }),
    })
}

fn source_matches(source: &MediaSource, target: &SubtitleTarget) -> bool {
    target
        .media_source_id
        .as_ref()
        .is_none_or(|id| source.id.as_ref() == Some(id))
}

fn matching_source<'a>(
    sources: &'a [MediaSource],
    target: &SubtitleTarget,
) -> Option<&'a MediaSource> {
    match target.media_source_id.as_deref() {
        Some(id) => sources
            .iter()
            .find(|source| source.id.as_deref() == Some(id)),
        None if sources.len() == 1 => sources.first(),
        None => None,
    }
}

fn inline_tracks(session: &EmbySession, target: &SubtitleTarget) -> Option<Vec<SubtitleTrack>> {
    let play = session.play_state.as_ref()?;
    if let Some(source) = play
        .media_source
        .as_ref()
        .filter(|source| source_matches(source, target))
    {
        if let Some(streams) = source
            .media_streams
            .as_deref()
            .filter(|streams| !streams.is_empty())
        {
            return Some(tracks(streams, source.default_subtitle_stream_index));
        }
    }
    let item = session.now_playing_item.as_ref()?;
    if let Some(source) = item
        .media_sources
        .as_deref()
        .and_then(|sources| matching_source(sources, target))
    {
        if let Some(streams) = source
            .media_streams
            .as_deref()
            .filter(|streams| !streams.is_empty())
        {
            return Some(tracks(streams, source.default_subtitle_stream_index));
        }
    }
    // Generic item streams are not tied to a media source. If the session
    // reports another version or multiple unresolved variants, let the detail
    // cache find the exact source instead of sending another version's indices.
    let play_source_mismatch = play
        .media_source
        .as_ref()
        .and_then(|source| source.id.as_deref())
        .filter(|id| !id.is_empty())
        .zip(target.media_source_id.as_deref())
        .is_some_and(|(reported, active)| reported != active);
    let item_sources_ambiguous = item.media_sources.as_ref().is_some_and(|sources| {
        sources.len() > 1
            || sources.iter().any(|source| {
                source
                    .id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .zip(target.media_source_id.as_deref())
                    .is_some_and(|(reported, active)| reported != active)
            })
    });
    if play_source_mismatch || item_sources_ambiguous {
        return None;
    }
    item.media_streams
        .as_deref()
        .filter(|streams| !streams.is_empty())
        .map(|streams| tracks(streams, None))
}

fn state(
    session: &EmbySession,
    target: SubtitleTarget,
    tracks: Vec<SubtitleTrack>,
) -> Option<SubtitleState> {
    if tracks.is_empty() {
        return None;
    }
    let selected_index = session
        .play_state
        .as_ref()?
        .subtitle_stream_index
        .filter(|index| *index == -1 || tracks.iter().any(|track| track.index == *index));
    Some(SubtitleState {
        target,
        tracks,
        selected_index,
    })
}

pub(super) fn from_session(session: &EmbySession) -> Option<SubtitleState> {
    let target = target(session)?;
    let tracks = inline_tracks(session, &target)?;
    state(session, target, tracks)
}

fn clean_label(value: Option<&str>) -> Option<String> {
    let value = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then(|| value.chars().take(240).collect())
}

fn language_label(value: &str) -> &str {
    match value {
        "eng" | "en" => "English",
        "jpn" | "ja" => "Japanese",
        "slk" | "slo" | "sk" => "Slovak",
        "ces" | "cze" | "cs" => "Czech",
        "deu" | "ger" | "de" => "German",
        "fra" | "fre" | "fr" => "French",
        "spa" | "es" => "Spanish",
        "ita" | "it" => "Italian",
        "pol" | "pl" => "Polish",
        "por" | "pt" => "Portuguese",
        "hun" | "hu" => "Hungarian",
        "ukr" | "uk" => "Ukrainian",
        "rus" | "ru" => "Russian",
        "kor" | "ko" => "Korean",
        "zho" | "chi" | "zh" => "Chinese",
        _ => value,
    }
}

fn tracks(streams: &[MediaStream], default_index: Option<i32>) -> Vec<SubtitleTrack> {
    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    for stream in streams {
        if !stream
            .kind
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("Subtitle"))
        {
            continue;
        }
        let Some(index) = stream.index.filter(|index| *index >= 0) else {
            continue;
        };
        if !seen.insert(index) {
            continue;
        }
        let label = clean_label(stream.display_title.as_deref()).unwrap_or_else(|| {
            let language = clean_label(stream.display_language.as_deref()).or_else(|| {
                clean_label(stream.language.as_deref())
                    .filter(|language| language != "und")
                    .map(|language| language_label(&language.to_ascii_lowercase()).to_string())
            });
            let mut parts = vec![
                language.unwrap_or_else(|| format!("Subtitle track {}", i64::from(index) + 1)),
            ];
            if let Some(title) = clean_label(stream.title.as_deref()) {
                parts.push(title);
            }
            if stream.is_forced {
                parts.push("Forced".into());
            }
            if stream.is_hearing_impaired {
                parts.push("SDH".into());
            }
            if let Some(codec) = clean_label(stream.codec.as_deref()) {
                parts.push(codec.to_uppercase());
            }
            parts.join(" · ")
        });
        tracks.push((
            default_index == Some(index) || (default_index.is_none() && stream.is_default),
            SubtitleTrack { index, label },
        ));
        if tracks.len() >= MAX_SUBTITLE_TRACKS {
            break;
        }
    }
    // A CC toggle can choose the source's default; other languages retain the
    // source order and original stream indices (never dropdown positions).
    tracks.sort_by_key(|(default, _)| !*default);
    tracks.into_iter().map(|(_, track)| track).collect()
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    server_url: String,
    user_id: String,
    target: SubtitleTarget,
}

struct CachedTracks {
    key: CacheKey,
    checked_at: Instant,
    tracks: Option<Vec<SubtitleTrack>>,
}

#[derive(Default)]
pub(super) struct Cache {
    entry: Option<CachedTracks>,
}

impl Cache {
    pub(super) fn resolve(
        &mut self,
        client: &reqwest::blocking::Client,
        credentials: &EmbyCredentials,
        server_url: &str,
        session: &EmbySession,
        now: Instant,
    ) -> Option<SubtitleState> {
        self.resolve_with(credentials, server_url, session, now, |target| {
            fetch_tracks(client, credentials, server_url, target)
        })
    }

    fn resolve_with(
        &mut self,
        credentials: &EmbyCredentials,
        server_url: &str,
        session: &EmbySession,
        now: Instant,
        fetch: impl FnOnce(&SubtitleTarget) -> Option<Vec<SubtitleTrack>>,
    ) -> Option<SubtitleState> {
        let Some(target) = target(session) else {
            self.entry = None;
            return None;
        };
        if let Some(tracks) = inline_tracks(session, &target) {
            self.entry = None;
            return state(session, target, tracks);
        }
        let key = CacheKey {
            server_url: server_url.to_string(),
            user_id: credentials.user_id.clone(),
            target: target.clone(),
        };
        let refresh = self.entry.as_ref().is_none_or(|entry| {
            entry.key != key
                || now.saturating_duration_since(entry.checked_at)
                    >= if entry.tracks.is_some() {
                        DETAIL_REFRESH
                    } else {
                        NEGATIVE_RETRY
                    }
        });
        if refresh {
            let tracks = fetch(&target);
            self.entry = Some(CachedTracks {
                key,
                checked_at: now,
                tracks,
            });
        }
        let tracks = self.entry.as_ref()?.tracks.as_ref()?.clone();
        state(session, target, tracks)
    }
}

fn route_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn fetch_tracks(
    client: &reqwest::blocking::Client,
    credentials: &EmbyCredentials,
    server_url: &str,
    target: &SubtitleTarget,
) -> Option<Vec<SubtitleTrack>> {
    if !route_id(&credentials.user_id) || !route_id(&target.item_id) {
        return None;
    }
    let response = client
        .get(MediaMonitor::emby_api_url(
            server_url,
            &format!("Users/{}/Items/{}", credentials.user_id, target.item_id),
        ))
        .header("X-Emby-Token", &credentials.access_token)
        .query(&[("Fields", "MediaSources,MediaStreams")])
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    let mut bytes = Vec::new();
    response
        .take(MAX_DETAIL_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_DETAIL_BYTES {
        return None;
    }
    let details: ItemDetails = serde_json::from_slice(&bytes).ok()?;
    detail_tracks(&details, target)
}

fn detail_tracks(details: &ItemDetails, target: &SubtitleTarget) -> Option<Vec<SubtitleTrack>> {
    if details.id != target.item_id {
        return None;
    }
    if let Some(source) = details
        .media_sources
        .as_deref()
        .and_then(|sources| matching_source(sources, target))
    {
        return source
            .media_streams
            .as_deref()
            .map(|streams| tracks(streams, source.default_subtitle_stream_index));
    }
    // Library-level streams can describe another version. Only fall back when
    // no version is selected, or the default source is the requested item itself.
    if details
        .media_sources
        .as_ref()
        .is_some_and(|sources| !sources.is_empty())
    {
        return None;
    }
    if target
        .media_source_id
        .as_ref()
        .is_some_and(|id| id != &target.item_id)
    {
        return None;
    }
    details
        .media_streams
        .as_deref()
        .map(|streams| tracks(streams, None))
}

pub(super) fn valid_request(
    state: &MultiPlayerState,
    control: &EmbyControl,
    target: &SubtitleTarget,
    index: i32,
) -> bool {
    let Some((PlayerId::Emby(session_id), info)) = state.current_player() else {
        return false;
    };
    let Some(subtitles) = info.subtitles.as_ref() else {
        return false;
    };
    session_id == &target.session_id
        && info.status != PlaybackStatus::Stopped
        && control.session_id == target.session_id
        && control.subtitle_target.as_ref() == Some(target)
        && subtitles.target == *target
        && (index == -1 || subtitles.tracks.iter().any(|track| track.index == index))
}

pub(super) fn send(client: &reqwest::blocking::Client, control: &EmbyControl, index: i32) -> bool {
    if index < -1 || !route_id(&control.session_id) {
        return false;
    }
    client
        .post(MediaMonitor::emby_api_url(
            &control.server_url,
            &format!("Sessions/{}/Command/{COMMAND}", control.session_id),
        ))
        .header("X-Emby-Token", &control.access_token)
        .json(&serde_json::json!({"Arguments": {"Index": index.to_string()}}))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(value: serde_json::Value) -> EmbySession {
        serde_json::from_value(value).unwrap()
    }

    fn fixture() -> serde_json::Value {
        json!({
            "Id": "session-1", "Client": "Emby Theater", "DeviceName": "desktop",
            "SupportsRemoteControl": true, "SupportedCommands": ["SetSubtitleStreamIndex"],
            "NowPlayingItem": {"Id": "item-1", "Name": "Episode", "MediaStreams": [
                {"Type": "Video", "Index": 0}, {"Type": "Audio", "Index": 1},
                {"Type": "Subtitle", "Index": 4, "Language": "eng", "Title": "Full", "Codec": "ass"},
                {"Type": "Subtitle", "Index": 7, "DisplayTitle": "Japanese · SDH", "IsDefault": true}
            ]},
            "PlayState": {"MediaSourceId": "source-1", "SubtitleStreamIndex": 4}
        })
    }

    fn credentials() -> EmbyCredentials {
        EmbyCredentials {
            server_urls: vec![],
            user_id: "user-1".into(),
            access_token: "fixture-token".into(),
            last_accessed: 0,
        }
    }

    #[test]
    fn exposes_actual_subtitle_indices_and_prefers_default_language() {
        let state = from_session(&session(fixture())).unwrap();
        assert_eq!(
            state
                .tracks
                .iter()
                .map(|track| track.index)
                .collect::<Vec<_>>(),
            vec![7, 4]
        );
        assert_eq!(state.tracks[0].label, "Japanese · SDH");
        assert_eq!(state.tracks[1].label, "English · Full · ASS");
        assert_eq!(state.selected_index, Some(4));
        assert_eq!(state.target.media_source_id.as_deref(), Some("source-1"));
    }

    #[test]
    fn current_source_wins_over_other_versions_and_generic_item_streams() {
        let mut value = fixture();
        value["PlayState"]["MediaSource"] = json!({"Id": "source-1", "DefaultSubtitleStreamIndex": 9,
            "MediaStreams": [{"Type": "Subtitle", "Index": 8, "Language": "eng"}, {"Type": "Subtitle", "Index": 9, "Language": "slk"}]});
        value["NowPlayingItem"]["MediaSources"] = json!([
            {"Id": "other-source", "MediaStreams": [{"Type": "Subtitle", "Index": 2, "Language": "jpn"}]}
        ]);
        value["PlayState"]["SubtitleStreamIndex"] = json!(9);
        let state = from_session(&session(value)).unwrap();
        assert_eq!(
            state
                .tracks
                .iter()
                .map(|track| track.index)
                .collect::<Vec<_>>(),
            vec![9, 8]
        );
        assert_eq!(state.tracks[0].label, "Slovak");
        assert_eq!(state.selected_index, Some(9));
    }

    #[test]
    fn mismatched_source_metadata_uses_exact_source_details_not_generic_indices() {
        for source_location in ["item", "play"] {
            let mut value = fixture();
            let wrong_source = json!({"Id": "other-source", "MediaStreams": [
                {"Type": "Subtitle", "Index": 4, "Language": "eng"}
            ]});
            if source_location == "item" {
                value["NowPlayingItem"]["MediaSources"] = json!([wrong_source]);
            } else {
                value["PlayState"]["MediaSource"] = wrong_source;
            }
            let session = session(value);
            assert!(from_session(&session).is_none(), "{source_location}");
            let details: ItemDetails = serde_json::from_value(json!({
                "Id": "item-1", "MediaSources": [
                    {"Id": "other-source", "MediaStreams": [{"Type": "Subtitle", "Index": 4}]},
                    {"Id": "source-1", "MediaStreams": [{"Type": "Subtitle", "Index": 9, "Language": "slk"}]}
                ]
            })).unwrap();
            let mut fetched = false;
            let state = Cache::default()
                .resolve_with(
                    &credentials(),
                    "http://server",
                    &session,
                    Instant::now(),
                    |target| {
                        fetched = true;
                        assert_eq!(target.media_source_id.as_deref(), Some("source-1"));
                        detail_tracks(&details, target)
                    },
                )
                .unwrap();
            assert!(fetched);
            assert_eq!(
                state.tracks,
                vec![SubtitleTrack {
                    index: 9,
                    label: "Slovak".into()
                }]
            );
            assert_eq!(state.selected_index, None);
        }
    }

    #[test]
    fn unresolved_multiple_versions_do_not_use_generic_item_streams() {
        for active_source in [json!(null), json!("source-1")] {
            let mut value = fixture();
            value["PlayState"]["MediaSourceId"] = active_source;
            value["NowPlayingItem"]["MediaSources"] = json!([
                {"Id": "source-1"},
                {"Id": "other-source", "MediaStreams": [{"Type": "Subtitle", "Index": 4}]}
            ]);
            let session = session(value);
            assert!(from_session(&session).is_none());
            let mut fetched = false;
            assert!(
                Cache::default()
                    .resolve_with(
                        &credentials(),
                        "http://server",
                        &session,
                        Instant::now(),
                        |_| {
                            fetched = true;
                            None
                        }
                    )
                    .is_none()
            );
            assert!(fetched);
        }
    }

    #[test]
    fn generic_streams_remain_available_without_conflicting_source_metadata() {
        let mut value = fixture();
        value["PlayState"]["MediaSource"] = json!({"Id": "source-1"});
        value["NowPlayingItem"]["MediaSources"] = json!([{"Id": "source-1"}]);
        let state = from_session(&session(value)).unwrap();
        assert_eq!(
            state
                .tracks
                .iter()
                .map(|track| track.index)
                .collect::<Vec<_>>(),
            vec![7, 4]
        );
    }

    #[test]
    fn hidden_without_capability_or_tracks_and_unknown_is_distinct_from_off() {
        for (key, value) in [
            ("SupportsRemoteControl", json!(false)),
            ("SupportedCommands", json!([])),
            ("SupportedCommands", json!(null)),
        ] {
            let mut fixture = fixture();
            fixture[key] = value;
            assert!(from_session(&session(fixture)).is_none());
        }
        for (index, expected) in [
            (json!(null), None),
            (json!(-2), None),
            (json!(-1), Some(-1)),
            (json!(99), None),
        ] {
            let mut value = fixture();
            value["PlayState"]["SubtitleStreamIndex"] = index;
            assert_eq!(
                from_session(&session(value)).unwrap().selected_index,
                expected
            );
        }
        let mut value = fixture();
        value["NowPlayingItem"]["MediaStreams"] = json!([{"Type": "Video", "Index": 0}]);
        assert!(from_session(&session(value)).is_none());
    }

    #[test]
    fn item_detail_cache_retries_missing_data_without_polling_every_second() {
        let credentials = credentials();
        let mut value = fixture();
        value["NowPlayingItem"]
            .as_object_mut()
            .unwrap()
            .remove("MediaStreams");
        let session = session(value.clone());
        let mut cache = Cache::default();
        let now = Instant::now();
        assert!(
            cache
                .resolve_with(&credentials, "http://server", &session, now, |_| None)
                .is_none()
        );
        assert!(
            cache
                .resolve_with(
                    &credentials,
                    "http://server",
                    &session,
                    now + Duration::from_secs(1),
                    |_| panic!("negative cache must not poll again")
                )
                .is_none()
        );
        let tracks = vec![SubtitleTrack {
            index: 4,
            label: "English".into(),
        }];
        assert!(
            cache
                .resolve_with(
                    &credentials,
                    "http://server",
                    &session,
                    now + NEGATIVE_RETRY,
                    |_| Some(tracks)
                )
                .is_some()
        );
        value["PlayState"]["SubtitleStreamIndex"] = json!(-1);
        let off = self::session(value.clone());
        let state = cache
            .resolve_with(
                &credentials,
                "http://server",
                &off,
                now + NEGATIVE_RETRY + Duration::from_secs(1),
                |_| panic!("positive cache must be reused"),
            )
            .unwrap();
        assert_eq!(state.selected_index, Some(-1));
        value["PlayState"]["MediaSourceId"] = json!("new-source");
        assert!(
            cache
                .resolve_with(
                    &credentials,
                    "http://server",
                    &self::session(value),
                    now + NEGATIVE_RETRY + Duration::from_secs(2),
                    |_| None
                )
                .is_none()
        );
    }

    #[test]
    fn unsupported_sessions_do_not_fetch_item_details() {
        let mut value = fixture();
        value["SupportedCommands"] = json!([]);
        assert!(
            Cache::default()
                .resolve_with(
                    &credentials(),
                    "http://server",
                    &session(value),
                    Instant::now(),
                    |_| panic!("unsupported player must not fetch details")
                )
                .is_none()
        );
    }

    #[test]
    fn library_detail_fallback_requires_the_current_version() {
        let details: ItemDetails = serde_json::from_value(json!({
            "Id": "item-1",
            "MediaStreams": [{"Type": "Subtitle", "Index": 2, "Language": "jpn"}],
            "MediaSources": [
                {"Id": "wrong-source", "MediaStreams": [{"Type": "Subtitle", "Index": 2, "Language": "jpn"}]},
                {"Id": "source-1", "MediaStreams": [{"Type": "Subtitle", "Index": 5, "Language": "eng"}]}
            ]
        })).unwrap();
        let mut target = target(&session(fixture())).unwrap();
        let tracks = detail_tracks(&details, &target).unwrap();
        assert_eq!(tracks[0].index, 5);
        assert_eq!(tracks[0].label, "English");
        target.media_source_id = Some("missing-source".into());
        assert!(detail_tracks(&details, &target).is_none());
        target.media_source_id = None;
        assert!(detail_tracks(&details, &target).is_none());
        target.item_id = "other-item".into();
        assert!(detail_tracks(&details, &target).is_none());
    }

    #[test]
    fn malformed_or_duplicate_streams_do_not_become_dropdown_positions() {
        let mut value = fixture();
        value["NowPlayingItem"]["MediaStreams"] = json!([
            {"Type": "Subtitle", "Index": -1, "DisplayTitle": "Invalid"},
            {"Type": "Subtitle", "Index": "bad"},
            {"Type": "Audio", "Index": 1, "Language": "eng"},
            {"Type": "Subtitle", "Index": 12, "Language": "eng", "IsForced": true},
            {"Type": "Subtitle", "Index": 12, "Language": "fra"}
        ]);
        let state = from_session(&session(value)).unwrap();
        assert_eq!(
            state.tracks,
            vec![SubtitleTrack {
                index: 12,
                label: "English · Forced".into()
            }]
        );
        assert_eq!(state.selected_index, None);
    }

    #[test]
    fn subtitle_command_uses_general_command_json_and_off_sentinel() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 1024];
            loop {
                let count = socket.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let end = bytes
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .unwrap();
            let headers = String::from_utf8_lossy(&bytes[..end]);
            assert!(headers.starts_with(
                "POST /emby/Sessions/session-1/Command/SetSubtitleStreamIndex HTTP/1.1\r\n"
            ));
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("content-type: application/json")
            );
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("x-emby-token: fixture-token")
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[end + 4..]).unwrap(),
                json!({"Arguments": {"Index": "-1"}})
            );
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let control = EmbyControl {
            server_url: format!("http://{address}"),
            access_token: "fixture-token".into(),
            session_id: "session-1".into(),
            subtitle_target: None,
        };
        assert!(send(&client, &control, -1));
        server.join().unwrap();
    }
}
