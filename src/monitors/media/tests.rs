// SPDX-License-Identifier: MPL-2.0

use super::{
    AlbumArt, ArtworkCache, ArtworkRequest, ArtworkSource, EmbyCredentialDiscovery, HashMap,
    Instant, MediaInfo, MediaMonitor, MultiPlayerState, PlaybackStatus, PlayerId, PositionTracker,
    TrackArtwork, TrackSignature, merge_mpris_proxy_timeline, preferred_player_id,
    update_tracked_position,
};
use std::time::Duration;

fn media(status: PlaybackStatus, position: u64, title: &str) -> MediaInfo {
    MediaInfo {
        player_name: "Firefox".to_string(),
        title: title.to_string(),
        artist: "YouTube".to_string(),
        status,
        position,
        duration: 600_000,
        ..Default::default()
    }
}

#[test]
fn recognizes_playerctld_as_an_mpris_proxy() {
    assert!(MediaMonitor::is_playerctld_mpris_player(
        "org.mpris.MediaPlayer2.playerctld"
    ));
    assert!(!MediaMonitor::is_playerctld_mpris_player(
        "org.mpris.MediaPlayer2.firefox.instance_1_560"
    ));
}

#[test]
fn proxy_timeline_augments_the_real_player() {
    let mut firefox = media(PlaybackStatus::Playing, 15_000, "Video");
    firefox.player_name = "Firefox".to_string();
    firefox.duration = 617_000;
    firefox.can_seek = false;
    let mut proxy = media(PlaybackStatus::Playing, 356_000, "Video");
    proxy.player_name = "Playerctld".to_string();
    proxy.duration = 617_000;
    proxy.can_seek = true;

    merge_mpris_proxy_timeline(&mut firefox, &proxy);

    assert_eq!(firefox.player_name, "Firefox");
    assert_eq!(firefox.position, 356_000);
    assert_eq!(firefox.duration, 617_000);
    assert!(firefox.can_seek);
}

fn artwork(source_width: u32, source_height: u32) -> AlbumArt {
    AlbumArt {
        source_width,
        source_height,
        iced_handle: cosmic::iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
        encoded_bytes: 4,
    }
}

fn weighted_artwork(source_width: u32, source_height: u32, encoded_bytes: usize) -> AlbumArt {
    AlbumArt {
        encoded_bytes,
        ..artwork(source_width, source_height)
    }
}

#[test]
fn artwork_cache_evicts_the_least_recently_used_entry() {
    let mut cache = ArtworkCache::with_limits(2, 100, 10_000);
    cache.insert("first".to_string(), weighted_artwork(10, 10, 4));
    cache.insert("second".to_string(), weighted_artwork(10, 10, 4));
    assert!(cache.get("first").is_some());

    cache.insert("third".to_string(), weighted_artwork(10, 10, 4));

    assert!(cache.get("first").is_some());
    assert!(cache.get("second").is_none());
    assert!(cache.get("third").is_some());
}

#[test]
fn artwork_cache_enforces_encoded_byte_and_pixel_budgets() {
    let mut cache = ArtworkCache::with_limits(4, 7, 150);
    cache.insert("first".to_string(), weighted_artwork(10, 10, 4));
    cache.insert("second".to_string(), weighted_artwork(10, 10, 4));

    assert!(cache.get("first").is_none());
    assert!(cache.get("second").is_some());
    assert_eq!(cache.encoded_bytes, 4);
    assert_eq!(cache.source_pixels, 100);

    cache.insert("oversized".to_string(), weighted_artwork(20, 20, 4));
    assert!(cache.get("oversized").is_none());
    assert!(cache.get("second").is_some());
}

#[test]
fn decoded_artwork_uses_an_encoded_iced_handle() {
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([1, 2, 3, 255]),
    ))
    .write_to(&mut encoded, image::ImageFormat::Png)
    .unwrap();

    let artwork = MediaMonitor::decode_artwork(encoded.into_inner()).unwrap();
    let cosmic::iced::widget::image::Handle::Bytes(_, handle_bytes) = &artwork.iced_handle else {
        panic!("decoded artwork should use an encoded Iced handle");
    };

    assert!(!handle_bytes.is_empty());
}

#[test]
fn completed_artwork_is_cached_but_only_applied_to_its_original_track() {
    let player_id = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let old_track = TrackSignature::from(&media(PlaybackStatus::Playing, 0, "Previous video"));
    let current_track = TrackSignature::from(&media(PlaybackStatus::Playing, 0, "Current video"));
    let mut selections =
        HashMap::from([(player_id.clone(), TrackArtwork::new(current_track.clone()))]);
    let mut cache = ArtworkCache::new(4);

    MediaMonitor::accept_completed_artwork(
        ArtworkRequest {
            player_id: player_id.clone(),
            track: old_track,
            source: ArtworkSource::Image("https://example.test/old.jpg".to_string()),
        },
        artwork(1280, 720),
        &mut cache,
        &mut selections,
    );

    assert!(cache.get("https://example.test/old.jpg").is_some());
    assert!(selections[&player_id].best.is_none());

    MediaMonitor::accept_completed_artwork(
        ArtworkRequest {
            player_id: player_id.clone(),
            track: current_track,
            source: ArtworkSource::Image("https://example.test/current.jpg".to_string()),
        },
        artwork(640, 360),
        &mut cache,
        &mut selections,
    );

    assert_eq!(
        selections[&player_id]
            .best
            .as_ref()
            .map(AlbumArt::source_pixel_count),
        Some(640 * 360),
    );
}

#[test]
fn seeds_and_advances_a_playing_position_from_the_initial_mpris_sample() {
    let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let mut trackers: HashMap<PlayerId, PositionTracker> = HashMap::new();
    let started = Instant::now();
    let mut info = media(PlaybackStatus::Playing, 120_000, "Video");

    assert!(update_tracked_position(
        &mut trackers,
        &player,
        &mut info,
        started
    ));
    assert_eq!(info.position, 120_000);

    let mut unchanged = media(PlaybackStatus::Playing, 120_000, "Video");
    assert!(!update_tracked_position(
        &mut trackers,
        &player,
        &mut unchanged,
        started + Duration::from_secs(2),
    ));
    assert_eq!(unchanged.position, 122_000);

    let mut refreshed = media(PlaybackStatus::Playing, 125_000, "Video");
    update_tracked_position(
        &mut trackers,
        &player,
        &mut refreshed,
        started + Duration::from_secs(3),
    );
    assert_eq!(refreshed.position, 125_000);
}

#[test]
fn freezes_on_pause_and_accepts_a_real_paused_seek() {
    let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let mut trackers: HashMap<PlayerId, PositionTracker> = HashMap::new();
    let started = Instant::now();
    let mut playing = media(PlaybackStatus::Playing, 120_000, "Video");
    update_tracked_position(&mut trackers, &player, &mut playing, started);

    let mut paused = media(PlaybackStatus::Paused, 120_000, "Video");
    update_tracked_position(
        &mut trackers,
        &player,
        &mut paused,
        started + Duration::from_secs(1),
    );
    assert_eq!(paused.position, 121_000);

    let mut drifting = media(PlaybackStatus::Paused, 121_000, "Video");
    update_tracked_position(
        &mut trackers,
        &player,
        &mut drifting,
        started + Duration::from_secs(2),
    );
    assert_eq!(drifting.position, 121_000);

    let mut seeked = media(PlaybackStatus::Paused, 90_000, "Video");
    update_tracked_position(
        &mut trackers,
        &player,
        &mut seeked,
        started + Duration::from_secs(3),
    );
    assert_eq!(seeked.position, 90_000);
}

#[test]
fn resumes_from_the_frozen_position_and_resets_for_a_new_track() {
    let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let mut trackers: HashMap<PlayerId, PositionTracker> = HashMap::new();
    let started = Instant::now();
    let mut paused = media(PlaybackStatus::Paused, 90_000, "First video");
    update_tracked_position(&mut trackers, &player, &mut paused, started);

    let mut resumed = media(PlaybackStatus::Playing, 90_000, "First video");
    update_tracked_position(
        &mut trackers,
        &player,
        &mut resumed,
        started + Duration::from_secs(1),
    );
    assert_eq!(resumed.position, 90_000);

    let mut progressing = media(PlaybackStatus::Playing, 90_000, "First video");
    update_tracked_position(
        &mut trackers,
        &player,
        &mut progressing,
        started + Duration::from_secs(2),
    );
    assert_eq!(progressing.position, 91_000);

    let mut next = media(PlaybackStatus::Playing, 5_000, "Second video");
    assert!(update_tracked_position(
        &mut trackers,
        &player,
        &mut next,
        started + Duration::from_secs(3),
    ));
    assert_eq!(next.position, 5_000);
}

#[test]
fn preserves_valid_timeline_across_incomplete_firefox_samples() {
    let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let mut trackers: HashMap<PlayerId, PositionTracker> = HashMap::new();
    let started = Instant::now();
    let mut valid = media(PlaybackStatus::Playing, 368_000, "Video");
    update_tracked_position(&mut trackers, &player, &mut valid, started);

    let mut incomplete = media(PlaybackStatus::Playing, 0, "Video");
    incomplete.duration = 0;
    update_tracked_position(
        &mut trackers,
        &player,
        &mut incomplete,
        started + Duration::from_secs(1),
    );

    assert_eq!(incomplete.position, 369_000);
    assert_eq!(incomplete.duration, 600_000);
}

#[test]
fn identifies_only_cider_mpris_sources_as_api_duplicates() {
    assert!(MediaMonitor::is_cider_mpris_player(
        "org.mpris.MediaPlayer2.cider"
    ));
    assert!(MediaMonitor::is_cider_mpris_player(
        "org.mpris.MediaPlayer2.Cider.instance_1"
    ));
    assert!(!MediaMonitor::is_cider_mpris_player(
        "org.mpris.MediaPlayer2.firefox.instance_1"
    ));
}

#[test]
fn parses_youtube_urls_and_orders_thumbnail_candidates() {
    let urls = [
        "https://www.youtube.com/watch?v=testVideo_1&list=WL",
        "https://youtu.be/testVideo_1?t=30",
        "https://music.youtube.com/watch?v=testVideo_1",
        "https://www.youtube.com/shorts/testVideo_1",
        "https://www.youtube.com/live/testVideo_1",
        "https://www.youtube.com/embed/testVideo_1",
    ];
    for url in urls {
        assert_eq!(
            MediaMonitor::extract_youtube_video_id(url).as_deref(),
            Some("testVideo_1")
        );
    }

    let info = MediaInfo {
        media_url: Some(urls[0].to_string()),
        art_url: Some("file:///tmp/firefox-art.jpg".to_string()),
        ..Default::default()
    };
    assert_eq!(
        MediaMonitor::artwork_candidate_urls(&info),
        vec![
            "https://i.ytimg.com/vi/testVideo_1/maxresdefault.jpg",
            "https://i.ytimg.com/vi/testVideo_1/hqdefault.jpg",
            "https://i.ytimg.com/vi/testVideo_1/mqdefault.jpg",
            "file:///tmp/firefox-art.jpg",
        ]
    );
}

#[test]
fn artwork_selection_never_downgrades_the_current_track() {
    let track = TrackSignature {
        title: "Video".to_string(),
        artist: "Channel".to_string(),
        media_url: Some("https://youtube.com/watch?v=testVideo_1".to_string()),
    };
    let mut selection = TrackArtwork::new(track);
    let source = ArtworkSource::Image("https://example.test/artwork.jpg".to_string());
    selection.accept(&source, artwork(1280, 720));
    selection.accept(&source, artwork(60, 60));

    let selected = selection.best.expect("artwork should be selected");
    assert_eq!((selected.source_width, selected.source_height), (1280, 720));
}

#[test]
fn bandcamp_cover_candidates_use_the_canonical_album_page() {
    let info = MediaInfo {
        media_url: Some("https://artist.bandcamp.com/album/example/?from=discover#track".into()),
        art_url: Some("file:///tmp/browser-placeholder.png".into()),
        ..Default::default()
    };
    assert_eq!(
        MediaMonitor::artwork_candidate_sources(&info),
        vec![
            ArtworkSource::BandcampPage("https://artist.bandcamp.com/album/example".into()),
            ArtworkSource::Image("file:///tmp/browser-placeholder.png".into()),
        ]
    );
    let unrelated = MediaInfo {
        media_url: Some("https://example.test/album/example".into()),
        ..Default::default()
    };
    assert!(MediaMonitor::artwork_candidate_sources(&unrelated).is_empty());
}

fn bandcamp_track_fixture(tracks: &[(&str, u64)]) -> (MediaInfo, super::BandcampPageCache) {
    let page = "https://artist.bandcamp.com/album/example";
    let info = MediaInfo {
        player_name: "Zen".into(),
        title: "▶︎ Example | Artist".into(),
        media_url: Some(page.into()),
        status: PlaybackStatus::Playing,
        duration: 239_000,
        position: 20_000,
        ..Default::default()
    };
    let mut cache = super::BandcampPageCache::default();
    cache.insert(
        page.into(),
        super::bandcamp::PageMetadata {
            artwork_url: None,
            album: "Example".into(),
            artist: "Artist".into(),
            tracks_complete: true,
            tracks: tracks
                .iter()
                .enumerate()
                .map(
                    |(index, (title, duration_ms))| super::bandcamp::TrackMetadata {
                        title: (*title).into(),
                        duration_ms: *duration_ms,
                        url: reqwest::Url::parse(&format!(
                            "https://artist.bandcamp.com/track/song-{index}"
                        ))
                        .unwrap(),
                    },
                )
                .collect(),
        },
    );
    (info, cache)
}

#[test]
fn bandcamp_track_title_uses_unique_firefox_whole_second_duration() {
    let (mut info, mut cache) = bandcamp_track_fixture(&[
        ("First Song", 239_920),
        ("Second Song", 168_990),
        ("Third Song", 169_000),
    ]);
    MediaMonitor::apply_bandcamp_metadata(
        "org.mpris.MediaPlayer2.firefox.instance",
        &mut info,
        &mut cache,
    );
    assert_eq!(
        (
            info.title.as_str(),
            info.artist.as_str(),
            info.album.as_str()
        ),
        ("First Song", "Artist", "Example")
    );
    assert_eq!(
        info.media_url.as_deref(),
        Some("https://artist.bandcamp.com/album/example")
    );
    assert_eq!((info.position, info.duration), (20_000, 239_000));
}

#[test]
fn bandcamp_keeps_caption_when_duration_is_ambiguous_unknown_or_not_firefox() {
    for (tracks, duration, browser) in [
        (
            vec![("First", 239_100), ("Second", 239_990)],
            239_000,
            "firefox",
        ),
        (vec![("First", 239_920)], 0, "firefox"),
        (vec![("First", 239_920)], 240_000, "firefox"),
        (vec![("First", 239_920)], 239_920, "firefox"),
        (vec![("First", 239_920)], 239_000, "chromium"),
    ] {
        let (mut info, mut cache) = bandcamp_track_fixture(&tracks);
        info.duration = duration;
        let original = info.title.clone();
        MediaMonitor::apply_bandcamp_metadata(
            &format!("org.mpris.MediaPlayer2.{browser}"),
            &mut info,
            &mut cache,
        );
        assert_eq!(info.title, original);
        assert!(info.artist.is_empty());
    }
    let (mut info, mut cache) = bandcamp_track_fixture(&[("First", 239_920)]);
    cache.pages.values_mut().next().unwrap().0.tracks_complete = false;
    MediaMonitor::apply_bandcamp_metadata("org.mpris.MediaPlayer2.firefox", &mut info, &mut cache);
    assert_eq!(info.title, "▶︎ Example | Artist");
}

#[test]
fn bandcamp_preserves_supplied_titles_and_only_uses_the_current_page() {
    let (mut info, mut cache) = bandcamp_track_fixture(&[("First", 239_920)]);
    info.title = "Actual browser title".into();
    MediaMonitor::apply_bandcamp_metadata("org.mpris.MediaPlayer2.firefox", &mut info, &mut cache);
    assert_eq!(info.title, "Actual browser title");
    info.title = "▶︎ Example | Artist".into();
    info.media_url = Some("https://artist.bandcamp.com/album/different".into());
    MediaMonitor::apply_bandcamp_metadata("org.mpris.MediaPlayer2.firefox", &mut info, &mut cache);
    assert_eq!(info.title, "▶︎ Example | Artist");
}

#[test]
fn bandcamp_direct_track_url_does_not_require_duration_inference() {
    let (mut info, mut cache) = bandcamp_track_fixture(&[("First", 239_100), ("Second", 239_920)]);
    let track_page = "https://artist.bandcamp.com/track/song-1";
    let metadata = cache.pages.values().next().unwrap().0.clone();
    cache.insert(track_page.into(), metadata);
    info.media_url = Some(track_page.into());
    info.title = "Second | Artist".into();
    info.duration = 0;
    MediaMonitor::apply_bandcamp_metadata("org.mpris.MediaPlayer2.chromium", &mut info, &mut cache);
    assert_eq!(info.title, "Second");
    assert_eq!(info.artist, "Artist");
    assert!(info.album.is_empty());
}

#[test]
fn enriched_bandcamp_player_still_merges_its_raw_mpris_proxy() {
    let (raw, mut cache) = bandcamp_track_fixture(&[("First", 239_920)]);
    let bus = "org.mpris.MediaPlayer2.firefox";
    let player_id = PlayerId::Mpris(bus.into());
    let raw_track = TrackSignature::from(&raw);
    let raw_tracks = HashMap::from([(player_id.clone(), raw_track.clone())]);
    let mut enriched = raw.clone();
    MediaMonitor::apply_bandcamp_metadata(bus, &mut enriched, &mut cache);
    let mut players = vec![(player_id, enriched)];
    let mut proxy = raw;
    proxy.position = 50_000;
    proxy.can_seek = true;
    assert!(super::merge_mpris_proxy_player(
        &mut players,
        &raw_tracks,
        &proxy,
        &raw_track
    ));
    assert_eq!(players[0].1.title, "First");
    assert_eq!(players[0].1.position, 50_000);
    assert!(players[0].1.can_seek);
}

#[test]
fn evicted_bandcamp_metadata_can_reload_while_respecting_failure_backoff() {
    for retry_pending in [false, true] {
        let (mut info, _) = bandcamp_track_fixture(&[("First", 239_920)]);
        let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".into());
        let source = ArtworkSource::BandcampPage(info.media_url.clone().unwrap());
        let mut selection = TrackArtwork::new(TrackSignature::from(&info));
        selection.attempted_sources.insert(source.clone());
        if retry_pending {
            selection
                .retry_after
                .insert(source.clone(), Instant::now() + Duration::from_secs(30));
        }
        let mut selections = HashMap::from([(player.clone(), selection)]);
        let cache = std::sync::Arc::new(std::sync::Mutex::new(ArtworkCache::new(4)));
        let (requests, mut receiver) = tokio::sync::mpsc::channel(4);
        let (_completed_tx, completed) = std::sync::mpsc::channel();
        let mut loader = super::ArtworkLoader {
            requests,
            completed,
            pending_sources: Default::default(),
            bandcamp_pages: Default::default(),
            available: true,
        };
        MediaMonitor::apply_best_artwork(&player, &mut info, &cache, &mut selections, &mut loader);
        assert_eq!(receiver.try_recv().is_ok(), !retry_pending);
        assert_eq!(loader.pending_sources.contains(&source), !retry_pending);
    }
}

#[test]
fn failed_cover_download_does_not_discard_successful_track_metadata() {
    let (mut info, cache) = bandcamp_track_fixture(&[("First", 239_920)]);
    let source = ArtworkSource::BandcampPage(info.media_url.clone().unwrap());
    let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".into());
    let request = ArtworkRequest {
        player_id: player.clone(),
        track: TrackSignature::from(&info),
        source: source.clone(),
    };
    let (requests, _requests_rx) = tokio::sync::mpsc::channel(4);
    let (completed_tx, completed) = std::sync::mpsc::channel();
    let mut loader = super::ArtworkLoader {
        requests,
        completed,
        pending_sources: std::collections::HashSet::from([source]),
        bandcamp_pages: Default::default(),
        available: true,
    };
    completed_tx
        .send(super::ArtworkResult {
            request,
            artwork: None,
            bandcamp_metadata: Some(cache.pages.into_values().next().unwrap().0),
        })
        .unwrap();
    let artwork_cache = std::sync::Arc::new(std::sync::Mutex::new(ArtworkCache::new(4)));
    let mut selections = HashMap::from([(player, TrackArtwork::new(TrackSignature::from(&info)))]);
    MediaMonitor::collect_artwork_results(&mut loader, &artwork_cache, &mut selections);
    MediaMonitor::apply_bandcamp_metadata(
        "org.mpris.MediaPlayer2.firefox",
        &mut info,
        &mut loader.bandcamp_pages,
    );
    assert_eq!(info.title, "First");
}

#[test]
fn bandcamp_track_change_resets_timeline_even_when_album_url_is_unchanged() {
    let (raw, mut cache) = bandcamp_track_fixture(&[("First", 239_920), ("Second", 169_000)]);
    let bus_name = "org.mpris.MediaPlayer2.firefox";
    let player = PlayerId::Mpris(bus_name.into());
    let mut trackers = HashMap::new();
    let now = Instant::now();
    let mut first = raw.clone();
    MediaMonitor::apply_bandcamp_metadata(bus_name, &mut first, &mut cache);
    update_tracked_position(&mut trackers, &player, &mut first, now);
    let mut second = raw;
    second.duration = 169_000;
    second.position = 0;
    MediaMonitor::apply_bandcamp_metadata(bus_name, &mut second, &mut cache);
    assert!(update_tracked_position(
        &mut trackers,
        &player,
        &mut second,
        now + Duration::from_secs(1)
    ));
    assert_eq!(second.title, "Second");
    assert_eq!(second.position, 0);
}

#[test]
fn bandcamp_page_cache_is_bounded_and_retains_recently_used_metadata() {
    let (_, mut cache) = bandcamp_track_fixture(&[("First", 239_920)]);
    let original_url = "https://artist.bandcamp.com/album/example";
    let metadata = cache.get(original_url).unwrap().clone();
    for index in 1..super::MAX_CACHED_ARTWORKS {
        cache.insert(
            format!("https://artist.bandcamp.com/album/{index}"),
            metadata.clone(),
        );
    }
    cache.get(original_url).unwrap();
    cache.insert("https://artist.bandcamp.com/album/new".into(), metadata);
    assert_eq!(cache.pages.len(), super::MAX_CACHED_ARTWORKS);
    assert!(cache.get(original_url).is_some());
    assert!(cache.get("https://artist.bandcamp.com/album/1").is_none());
}

#[test]
fn bandcamp_album_cover_takes_precedence_over_browser_placeholder() {
    let info = media(PlaybackStatus::Playing, 0, "Album");
    let mut selection = TrackArtwork::new(TrackSignature::from(&info));
    let placeholder = ArtworkSource::Image("file:///tmp/browser-placeholder.png".into());
    let cover = ArtworkSource::BandcampPage("https://artist.bandcamp.com/album/example".into());
    selection.accept(&placeholder, artwork(512, 512));
    selection.accept(&cover, artwork(350, 350));
    selection.accept(&placeholder, artwork(1024, 1024));
    let best = selection.best.unwrap();
    assert_eq!((best.source_width, best.source_height), (350, 350));
}

#[test]
fn failed_album_lookup_can_retry_for_every_track_waiting_on_the_shared_page() {
    let source = ArtworkSource::BandcampPage("https://artist.bandcamp.com/album/example".into());
    let mut selections = HashMap::new();
    for index in 0..2 {
        let info = media(PlaybackStatus::Playing, 0, &format!("Track {index}"));
        let mut selection = TrackArtwork::new(TrackSignature::from(&info));
        selection.attempted_sources.insert(source.clone());
        selections.insert(
            PlayerId::Mpris(format!("org.mpris.MediaPlayer2.firefox.{index}")),
            selection,
        );
    }
    let now = Instant::now();
    MediaMonitor::retry_failed_bandcamp_artwork(&source, &mut selections, now);
    for selection in selections.values() {
        assert!(!selection.attempted_sources.contains(&source));
        assert_eq!(
            selection.retry_after[&source],
            now + super::BANDCAMP_ARTWORK_RETRY_DELAY
        );
    }
}

#[test]
fn cover_from_previous_album_cannot_replace_current_track_artwork() {
    let player = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".into());
    let mut info = media(PlaybackStatus::Playing, 0, "Shared title");
    info.media_url = Some("https://artist.bandcamp.com/album/previous".into());
    let request = ArtworkRequest {
        player_id: player.clone(),
        track: TrackSignature::from(&info),
        source: ArtworkSource::BandcampPage(info.media_url.clone().unwrap()),
    };
    info.media_url = Some("https://artist.bandcamp.com/album/current".into());
    let mut selections = HashMap::from([(
        player.clone(),
        TrackArtwork::new(TrackSignature::from(&info)),
    )]);
    let mut cache = ArtworkCache::new(4);
    MediaMonitor::accept_completed_artwork(request, artwork(350, 350), &mut cache, &mut selections);
    assert!(selections[&player].best.is_none());
    assert!(
        cache
            .get("https://artist.bandcamp.com/album/previous")
            .is_some()
    );
}

#[tokio::test]
#[ignore = "downloads artwork from the public album in BANDCAMP_TEST_URL"]
async fn live_bandcamp_page_loads_decoded_album_artwork() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Info)
        .try_init();
    let raw = std::env::var("BANDCAMP_TEST_URL")
        .expect("set BANDCAMP_TEST_URL to a public album or track");
    let page = super::bandcamp::page_url(&raw).expect("valid Bandcamp page URL");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("cosmic-widget-applet/0.1")
        .build()
        .unwrap();
    let (cover, metadata) = ArtworkSource::BandcampPage(page.to_string())
        .load(&client)
        .await;
    let cover = cover.expect("album cover must download and decode");
    if let Ok(expected_title) = std::env::var("BANDCAMP_TEST_EXPECTED_TITLE") {
        let metadata = metadata.expect("album must provide track metadata");
        let mut info = MediaInfo {
            title: format!("{} | {}", metadata.album, metadata.artist),
            media_url: Some(page.to_string()),
            duration: std::env::var("BANDCAMP_TEST_DURATION_MS")
                .expect("set the captured MPRIS duration in milliseconds")
                .parse()
                .unwrap(),
            ..Default::default()
        };
        let mut cache = super::BandcampPageCache::default();
        cache.insert(page.to_string(), metadata);
        MediaMonitor::apply_bandcamp_metadata(
            "org.mpris.MediaPlayer2.firefox",
            &mut info,
            &mut cache,
        );
        assert_eq!(info.title, expected_title);
        println!("Bandcamp track resolved: {} — {}", info.title, info.artist);
    }
    assert!(cover.source_width > 0 && cover.source_height > 0);
    if let Ok(path) = std::env::var("BANDCAMP_TEST_ARTWORK_PATH") {
        let cosmic::iced::widget::image::Handle::Bytes(_, bytes) = &cover.iced_handle else {
            panic!("artwork must retain encoded bytes");
        };
        std::fs::write(path, bytes).unwrap();
    }
    println!(
        "Bandcamp cover decoded: {}x{}",
        cover.source_width, cover.source_height
    );
}

#[test]
fn selects_the_newest_saved_emby_credentials() {
    let mut leveldb_bytes = vec![0, 0xff, b'n', b'o', b'i', b's', b'e', 0];
    leveldb_bytes.extend_from_slice(
        br#"{"Servers":[{"LocalAddress":"http://old:8096/","RemoteAddress":null,"ManualAddress":null,"UserId":"user-1","Users":[{"UserId":"user-1","AccessToken":"old-token"}],"DateLastAccessed":10}]}"#,
    );
    leveldb_bytes.extend_from_slice(&[0, 0x80]);
    leveldb_bytes.extend_from_slice(
        br#"prefix:{"Servers":[{"LocalAddress":"http://nas:8096","RemoteAddress":"https://remote.example","ManualAddress":"http://nas:8096","UserId":"user-2","Users":[{"UserId":"user-2","AccessToken":"new-token"}],"DateLastAccessed":20}]}:suffix"#,
    );
    leveldb_bytes.push(0xff);

    let credentials =
        MediaMonitor::parse_emby_credentials(&leveldb_bytes).expect("credentials should be parsed");
    assert_eq!(credentials.user_id, "user-2");
    assert_eq!(credentials.access_token, "new-token");
    assert_eq!(
        credentials.server_urls,
        vec!["http://nas:8096", "https://remote.example"]
    );
}

#[test]
fn skips_incomplete_emby_records_while_scanning_leveldb_bytes() {
    let mut leveldb_bytes = br#"{"Servers":[{"LocalAddress":"partial"}"#.to_vec();
    leveldb_bytes.extend_from_slice(&[0, 0xff, 0]);
    leveldb_bytes.extend_from_slice(
        br#"{"Servers":[{"LocalAddress":"http://nas:8096","RemoteAddress":null,"ManualAddress":null,"UserId":"user-1","Users":[{"UserId":"user-1","AccessToken":"token"}],"DateLastAccessed":30}]}"#,
    );

    let credentials = MediaMonitor::parse_emby_credentials(&leveldb_bytes)
        .expect("complete credentials after a partial record should be parsed");
    assert_eq!(credentials.user_id, "user-1");
    assert_eq!(credentials.server_urls, vec!["http://nas:8096"]);
}

#[test]
fn unchanged_emby_leveldb_files_are_not_rescanned() {
    let directory = std::env::temp_dir().join(format!(
        "cosmic-widget-emby-discovery-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let leveldb = directory.join("000001.log");
    std::fs::write(
        &leveldb,
        br#"{"Servers":[{"LocalAddress":"http://nas:8096","RemoteAddress":null,"ManualAddress":null,"UserId":"user-1","Users":[{"UserId":"user-1","AccessToken":"token"}],"DateLastAccessed":30}]}"#,
    )
    .unwrap();
    let mut discovery = EmbyCredentialDiscovery::default();

    let first = discovery.refresh_from(&directory).unwrap();
    let second = discovery.refresh_from(&directory).unwrap();

    assert_eq!(first.access_token, "token");
    assert_eq!(second.access_token, "token");
    assert_eq!(discovery.scan_count, 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
#[ignore = "requires an Emby Theater profile with a saved server"]
fn discovers_live_emby_credentials() {
    let credentials = MediaMonitor::discover_emby_credentials()
        .expect("saved Emby credentials should be discovered");
    assert!(!credentials.server_urls.is_empty());
    assert!(!credentials.user_id.is_empty());
    assert!(!credentials.access_token.is_empty());
}

#[test]
fn maps_an_emby_episode_to_media_state() {
    let session = serde_json::from_value(serde_json::json!({
        "Id": "session-1",
        "Client": "Emby Theater",
        "DeviceName": "test-desktop",
        "SupportsRemoteControl": true,
        "PlaylistIndex": 8,
        "PlaylistLength": 20,
        "NowPlayingItem": {
            "Id": "30705",
            "Name": "Henry Deaver",
            "SeriesName": "Castle Rock",
            "SeriesId": "30696",
            "IndexNumber": 9,
            "ParentIndexNumber": 1,
            "RunTimeTicks": 26986000000_u64,
            "ImageTags": {"Primary": "image-tag"},
            "PrimaryImageAspectRatio": 1.7777777778
        },
        "PlayState": {
            "PositionTicks": 8668548674_u64,
            "IsPaused": false,
            "CanSeek": true
        }
    }))
    .expect("session fixture should deserialize");

    let (player_id, info) = MediaMonitor::media_info_from_emby_session("http://nas:8096", &session)
        .expect("active session should map to media");
    assert_eq!(player_id, PlayerId::Emby("session-1".to_string()));
    assert_eq!(info.player_name, "Emby");
    assert_eq!(info.title, "Henry Deaver");
    assert_eq!(info.artist, "Castle Rock");
    assert_eq!(info.album, "Season 1, Episode 9");
    assert_eq!(info.position, 866_854);
    assert_eq!(info.duration, 2_698_600);
    assert_eq!(
        info.art_url.as_deref(),
        Some(
            "http://nas:8096/emby/Items/30705/Images/Primary?maxWidth=640&quality=90&tag=image-tag"
        )
    );
    assert_eq!(info.status, PlaybackStatus::Playing);
    assert!(info.can_play && info.can_pause && info.can_seek);
    assert!(info.can_go_previous && info.can_go_next);
}

#[test]
fn accepts_emby_inactive_session_sentinels() {
    let session = serde_json::from_value::<super::EmbySession>(serde_json::json!({
        "Id": "inactive-session",
        "Client": "Emby Web",
        "DeviceName": "Browser",
        "SupportsRemoteControl": false,
        "PlaylistIndex": -1,
        "PlaylistLength": 0,
        "NowPlayingItem": null,
        "PlayState": {
            "PositionTicks": null,
            "IsPaused": false,
            "CanSeek": false
        }
    }))
    .expect("inactive session sentinel values should deserialize");

    assert_eq!(session.playlist_index, Some(-1));
    assert!(
        session
            .play_state
            .is_some_and(|state| state.position_ticks.is_none())
    );
}

#[test]
fn newly_playing_source_becomes_preferred() {
    let firefox = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let previous = MultiPlayerState {
        players: vec![
            (
                PlayerId::Cider,
                media(PlaybackStatus::Playing, 20_000, "Music"),
            ),
            (
                firefox.clone(),
                media(PlaybackStatus::Paused, 40_000, "Video"),
            ),
        ],
        current_index: 0,
    };
    let players = vec![
        (
            PlayerId::Cider,
            media(PlaybackStatus::Playing, 21_000, "Music"),
        ),
        (
            firefox.clone(),
            media(PlaybackStatus::Playing, 40_000, "Video"),
        ),
    ];

    assert_eq!(
        preferred_player_id(&previous, &players, Some(&PlayerId::Cider)),
        Some(firefox)
    );
}

#[test]
fn stopping_selected_source_falls_back_to_playing_source() {
    let firefox = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let previous = MultiPlayerState {
        players: vec![
            (
                PlayerId::Cider,
                media(PlaybackStatus::Playing, 20_000, "Music"),
            ),
            (
                firefox.clone(),
                media(PlaybackStatus::Playing, 40_000, "Video"),
            ),
        ],
        current_index: 0,
    };
    let players = vec![
        (
            firefox.clone(),
            media(PlaybackStatus::Playing, 41_000, "Video"),
        ),
        (
            PlayerId::Cider,
            media(PlaybackStatus::Paused, 21_000, "Music"),
        ),
    ];

    assert_eq!(
        preferred_player_id(&previous, &players, Some(&PlayerId::Cider)),
        Some(firefox)
    );
}

#[test]
fn manual_source_selection_persists_without_playback_transition() {
    let firefox = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let previous = MultiPlayerState {
        players: vec![
            (
                PlayerId::Cider,
                media(PlaybackStatus::Playing, 20_000, "Music"),
            ),
            (
                firefox.clone(),
                media(PlaybackStatus::Paused, 40_000, "Video"),
            ),
        ],
        current_index: 1,
    };
    let players = previous.players.clone();

    assert_eq!(
        preferred_player_id(&previous, &players, Some(&firefox)),
        Some(firefox)
    );
}
