// SPDX-License-Identifier: MPL-2.0

//! # Media Player Monitoring Module
//!
//! This module monitors and controls media playback from multiple sources:
//! - **Cider API**: Apple Music client with REST API (priority source)
//! - **MPRIS D-Bus**: Standard Linux media player interface (Firefox, Spotify, etc.)
//!
//! ## Multi-Player Architecture
//!
//! The monitor tracks all active media players and allows the user to switch
//! between them using pagination dots. Players are discovered via:
//! - Cider REST API at localhost:10767
//! - MPRIS D-Bus names matching `org.mpris.MediaPlayer2.*`
//!
//! ## Player Priority
//!
//! When multiple players are available:
//! 1. Currently playing players are shown first
//! 2. Cider is prioritized when actively playing
//! 3. User selection persists until that player stops
//!
//! ## Album Art
//!
//! Album artwork is downloaded and cached:
//! - Cider: From Apple Music CDN URLs
//! - MPRIS: From `mpris:artUrl` metadata (file:// or http://)
//!
//! ## Monitoring Architecture
//!
//! A background thread samples Cider and Emby once per second. MPRIS players
//! are maintained by a persistent native D-Bus connection and refreshed from
//! player, property, and seek signals, with a slow reconciliation fallback.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

mod bandcamp;
mod cider;
mod mpris;

const MAX_ARTWORK_BYTES: usize = 12 * 1024 * 1024;
const MAX_ARTWORK_DIMENSION: u32 = 4096;
const MIN_YOUTUBE_THUMBNAIL_WIDTH: u32 = 320;
const MIN_YOUTUBE_THUMBNAIL_HEIGHT: u32 = 180;
const TIMELINE_REFRESH_RETRY: Duration = Duration::from_secs(2);
const MAX_TIMELINE_REFRESH_ATTEMPTS: u8 = 3;
const ARTWORK_QUEUE_CAPACITY: usize = 32;
const MAX_CONCURRENT_ARTWORK_REQUESTS: usize = 4;
const MAX_CACHED_ARTWORKS: usize = 20;
const MAX_ARTWORK_CACHE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ARTWORK_CACHE_PIXELS: u64 = 32 * 1024 * 1024;
const MEDIA_CONTROL_QUEUE_CAPACITY: usize = 32;
const BANDCAMP_ARTWORK_RETRY_DELAY: Duration = Duration::from_secs(30);

// ============================================================================
// Album Art Cache
// ============================================================================

/// Encoded album art and its source dimensions, ready for Iced rendering.
#[derive(Clone)]
pub struct AlbumArt {
    /// Decoded source width.
    pub source_width: u32,
    /// Decoded source height.
    pub source_height: u32,
    /// Stable Iced handle backed by the original encoded image bytes.
    pub iced_handle: cosmic::iced::widget::image::Handle,
    /// Size of the encoded image retained by the Iced handle.
    encoded_bytes: usize,
}

impl AlbumArt {
    fn source_pixel_count(&self) -> u64 {
        u64::from(self.source_width) * u64::from(self.source_height)
    }
}

impl std::fmt::Debug for AlbumArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlbumArt")
            .field("source_width", &self.source_width)
            .field("source_height", &self.source_height)
            .finish()
    }
}

/// Cache for downloaded and decoded album artwork.
///
/// Keyed by artwork URL to avoid re-downloading the same image.
/// Limited to prevent unbounded memory growth.
struct ArtworkCache {
    cache: HashMap<String, CachedArtwork>,
    max_entries: usize,
    max_encoded_bytes: usize,
    max_source_pixels: u64,
    encoded_bytes: usize,
    source_pixels: u64,
    access_generation: u64,
}

struct CachedArtwork {
    artwork: AlbumArt,
    last_accessed: u64,
}

impl ArtworkCache {
    fn new(max_entries: usize) -> Self {
        Self::with_limits(
            max_entries,
            MAX_ARTWORK_CACHE_BYTES,
            MAX_ARTWORK_CACHE_PIXELS,
        )
    }

    fn with_limits(max_entries: usize, max_encoded_bytes: usize, max_source_pixels: u64) -> Self {
        Self {
            cache: HashMap::new(),
            max_entries,
            max_encoded_bytes,
            max_source_pixels,
            encoded_bytes: 0,
            source_pixels: 0,
            access_generation: 0,
        }
    }

    fn get(&mut self, url: &str) -> Option<AlbumArt> {
        let generation = self.next_generation();
        let cached = self.cache.get_mut(url)?;
        cached.last_accessed = generation;
        Some(cached.artwork.clone())
    }

    fn insert(&mut self, url: String, art: AlbumArt) {
        let encoded_bytes = art.encoded_bytes;
        let source_pixels = art.source_pixel_count();
        if self.max_entries == 0
            || encoded_bytes > self.max_encoded_bytes
            || source_pixels > self.max_source_pixels
        {
            return;
        }

        if let Some(previous) = self.cache.remove(&url) {
            self.remove_weight(&previous.artwork);
        }

        while !self.cache.is_empty()
            && (self.cache.len() >= self.max_entries
                || self.encoded_bytes.saturating_add(encoded_bytes) > self.max_encoded_bytes
                || self.source_pixels.saturating_add(source_pixels) > self.max_source_pixels)
        {
            self.evict_least_recently_used();
        }

        let generation = self.next_generation();
        self.encoded_bytes = self.encoded_bytes.saturating_add(encoded_bytes);
        self.source_pixels = self.source_pixels.saturating_add(source_pixels);
        self.cache.insert(
            url,
            CachedArtwork {
                artwork: art,
                last_accessed: generation,
            },
        );
    }

    fn next_generation(&mut self) -> u64 {
        self.access_generation = self.access_generation.wrapping_add(1);
        self.access_generation
    }

    fn evict_least_recently_used(&mut self) {
        let Some(url) = self
            .cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_accessed)
            .map(|(url, _)| url.clone())
        else {
            return;
        };
        if let Some(evicted) = self.cache.remove(&url) {
            self.remove_weight(&evicted.artwork);
        }
    }

    fn remove_weight(&mut self, artwork: &AlbumArt) {
        self.encoded_bytes = self.encoded_bytes.saturating_sub(artwork.encoded_bytes);
        self.source_pixels = self
            .source_pixels
            .saturating_sub(artwork.source_pixel_count());
    }
}

// ============================================================================
// Playback Status Enum
// ============================================================================

/// Media player playback state.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackStatus {
    /// Track is currently playing
    Playing,
    /// Track is paused (can resume)
    Paused,
    /// No track loaded or player stopped
    Stopped,
}

impl Default for PlaybackStatus {
    fn default() -> Self {
        PlaybackStatus::Stopped
    }
}

// ============================================================================
// Player Identity
// ============================================================================

/// Identifies a specific media player instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlayerId {
    /// Cider Apple Music client (REST API)
    Cider,
    /// Native Emby Theater session ID.
    Emby(String),
    /// MPRIS D-Bus player with bus name
    Mpris(String),
}

impl PlayerId {
    /// Get display name for the player.
    pub fn display_name(&self) -> String {
        match self {
            PlayerId::Cider => "Cider".to_string(),
            PlayerId::Emby(_) => "Emby".to_string(),
            PlayerId::Mpris(name) => {
                // Extract friendly name from D-Bus name
                // e.g., "org.mpris.MediaPlayer2.firefox.instance_1_278" -> "Firefox"
                let parts: Vec<&str> = name.split('.').collect();
                if parts.len() >= 4 {
                    let player_name = parts[3];
                    // Capitalize first letter
                    let mut chars = player_name.chars();
                    match chars.next() {
                        None => player_name.to_string(),
                        Some(first) => first.to_uppercase().chain(chars).collect(),
                    }
                } else {
                    name.clone()
                }
            }
        }
    }
}

#[derive(Clone)]
struct EmbyCredentials {
    server_urls: Vec<String>,
    user_id: String,
    access_token: String,
    last_accessed: u64,
}

#[derive(Clone)]
struct EmbyControl {
    server_url: String,
    access_token: String,
    session_id: String,
}

enum MediaControlCommand {
    CiderCommand {
        endpoint: &'static str,
        token: Option<String>,
    },
    CiderSeek {
        position_seconds: u64,
        token: Option<String>,
    },
    Emby {
        control: EmbyControl,
        command: &'static str,
        query: Option<(&'static str, u64)>,
    },
    Mpris {
        bus_name: String,
        action: MprisControlAction,
    },
}

enum MprisControlAction {
    PlayPause,
    Next,
    Previous,
    Seek(u64),
}

#[derive(Clone)]
struct MediaControlWorker {
    commands: std::sync::mpsc::SyncSender<MediaControlCommand>,
}

impl MediaControlWorker {
    fn new(cider: cider::Client, mpris: mpris::Monitor) -> Self {
        let (commands, receiver) = std::sync::mpsc::sync_channel(MEDIA_CONTROL_QUEUE_CAPACITY);
        if let Err(error) = std::thread::Builder::new()
            .name("media-controls".to_string())
            .spawn(move || Self::run(receiver, cider, mpris))
        {
            log::warn!("Failed to start media control worker: {error}");
        }
        Self { commands }
    }

    fn enqueue(&self, command: MediaControlCommand) -> bool {
        match self.commands.try_send(command) {
            Ok(()) => true,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                log::warn!("Media control queue is full; dropping command");
                false
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                log::warn!("Media control worker is unavailable");
                false
            }
        }
    }

    fn run(
        receiver: std::sync::mpsc::Receiver<MediaControlCommand>,
        cider: cider::Client,
        mpris: mpris::Monitor,
    ) {
        let emby_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .user_agent("cosmic-widget-applet/0.1")
            .build()
            .ok();

        while let Ok(command) = receiver.recv() {
            match command {
                MediaControlCommand::CiderCommand { endpoint, token } => {
                    if !cider.command(endpoint, token.as_deref()) {
                        log::debug!("Cider {endpoint} command failed");
                    }
                }
                MediaControlCommand::CiderSeek {
                    position_seconds,
                    token,
                } => {
                    if !cider.seek(position_seconds, token.as_deref()) {
                        log::debug!("Cider seek command failed");
                    }
                }
                MediaControlCommand::Emby {
                    control,
                    command,
                    query,
                } => {
                    let Some(client) = emby_client.as_ref() else {
                        log::warn!("Emby control client is unavailable");
                        continue;
                    };
                    let url = MediaMonitor::emby_api_url(
                        &control.server_url,
                        &format!("Sessions/{}/Playing/{command}", control.session_id),
                    );
                    let mut request = client
                        .post(url)
                        .header("X-Emby-Token", control.access_token);
                    if let Some((key, value)) = query {
                        request = request.query(&[(key, value)]);
                    }
                    if let Err(error) = request
                        .send()
                        .and_then(reqwest::blocking::Response::error_for_status)
                    {
                        log::warn!("Emby {command} command failed: {error}");
                    }
                }
                MediaControlCommand::Mpris { bus_name, action } => {
                    let succeeded = match action {
                        MprisControlAction::PlayPause => mpris.play_pause(&bus_name),
                        MprisControlAction::Next => mpris.next(&bus_name),
                        MprisControlAction::Previous => mpris.previous(&bus_name),
                        MprisControlAction::Seek(position_us) => mpris.seek(&bus_name, position_us),
                    };
                    if !succeeded {
                        log::warn!("Native MPRIS control failed for {bus_name}");
                    }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct EmbyLevelDbFile {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Default)]
struct EmbyCredentialDiscovery {
    files: Option<Vec<EmbyLevelDbFile>>,
    credentials: Option<EmbyCredentials>,
    #[cfg(test)]
    scan_count: usize,
}

impl EmbyCredentialDiscovery {
    fn refresh(&mut self) -> Option<EmbyCredentials> {
        let Some(leveldb_dir) = dirs::config_dir().map(|directory| {
            directory
                .join("Emby Theater")
                .join("Local Storage")
                .join("leveldb")
        }) else {
            return self.credentials.clone();
        };
        self.refresh_from(&leveldb_dir)
    }

    fn refresh_from(&mut self, leveldb_dir: &Path) -> Option<EmbyCredentials> {
        let Ok(files) = Self::leveldb_files(leveldb_dir) else {
            return self.credentials.clone();
        };
        if self.files.as_ref() == Some(&files) {
            return self.credentials.clone();
        }

        #[cfg(test)]
        {
            self.scan_count += 1;
        }
        if let Some(credentials) = MediaMonitor::scan_emby_credentials(&files) {
            self.credentials = Some(credentials);
        }
        self.files = Some(files);
        self.credentials.clone()
    }

    fn leveldb_files(leveldb_dir: &Path) -> std::io::Result<Vec<EmbyLevelDbFile>> {
        let mut files = std::fs::read_dir(leveldb_dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if !matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("log" | "ldb")
                ) {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                Some(EmbyLevelDbFile {
                    path,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                })
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SavedEmbyServers {
    servers: Vec<SavedEmbyServer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SavedEmbyServer {
    local_address: Option<String>,
    manual_address: Option<String>,
    remote_address: Option<String>,
    user_id: Option<String>,
    users: Vec<SavedEmbyUser>,
    #[serde(default)]
    date_last_accessed: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SavedEmbyUser {
    user_id: String,
    access_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EmbySession {
    id: String,
    client: String,
    device_name: String,
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    supports_remote_control: bool,
    now_playing_item: Option<EmbyItem>,
    play_state: Option<EmbyPlayState>,
    playlist_index: Option<i64>,
    playlist_length: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EmbyItem {
    id: String,
    name: String,
    series_name: Option<String>,
    series_id: Option<String>,
    artists: Option<Vec<String>>,
    album: Option<String>,
    index_number: Option<u32>,
    parent_index_number: Option<u32>,
    production_year: Option<u32>,
    run_time_ticks: Option<u64>,
    image_tags: Option<HashMap<String, String>>,
    primary_image_aspect_ratio: Option<f64>,
    parent_thumb_item_id: Option<String>,
    parent_thumb_image_tag: Option<String>,
    series_primary_image_tag: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EmbyPlayState {
    position_ticks: Option<u64>,
    is_paused: Option<bool>,
    can_seek: Option<bool>,
}

// ============================================================================
// Media Info Struct
// ============================================================================

/// Information about the currently playing media.
///
/// Contains track metadata, playback position, and capability flags
/// for the media controls.
#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    /// Name of the media player (e.g., "Cider")
    pub player_name: String,
    /// Track title
    pub title: String,
    /// Artist name
    pub artist: String,
    /// Album name
    pub album: String,
    /// Album art URL from API
    pub art_url: Option<String>,
    /// URI of the media itself, used to identify tracks and derive video art.
    pub media_url: Option<String>,
    /// Decoded album artwork ready for rendering
    pub album_art: Option<AlbumArt>,
    /// Current playback status
    pub status: PlaybackStatus,
    /// Current playback position in milliseconds
    pub position: u64,
    /// Total track duration in milliseconds
    pub duration: u64,
    /// Whether play command is available
    #[allow(dead_code)]
    pub can_play: bool,
    /// Whether pause command is available
    #[allow(dead_code)]
    pub can_pause: bool,
    /// Whether next track command is available
    #[allow(dead_code)]
    pub can_go_next: bool,
    /// Whether previous track command is available
    #[allow(dead_code)]
    pub can_go_previous: bool,
    /// Whether seeking is supported
    #[allow(dead_code)]
    pub can_seek: bool,
}

impl MediaInfo {
    /// Check if there's an active media session.
    ///
    /// Returns true if we have both a player name and track title,
    /// indicating media is actually playing or paused.
    pub fn is_active(&self) -> bool {
        !self.player_name.is_empty() && !self.title.is_empty()
    }

    /// Format current position as mm:ss string.
    pub fn position_str(&self) -> String {
        let secs = self.position / 1000;
        format!("{}:{:02}", secs / 60, secs % 60)
    }

    /// Format duration as mm:ss string.
    pub fn duration_str(&self) -> String {
        let secs = self.duration / 1000;
        format!("{}:{:02}", secs / 60, secs % 60)
    }

    /// Get playback progress as fraction (0.0 to 1.0).
    ///
    /// Used for rendering the progress bar.
    pub fn progress(&self) -> f64 {
        if self.duration > 0 {
            (self.position as f64) / (self.duration as f64)
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TrackSignature {
    title: String,
    artist: String,
    media_url: Option<String>,
}

impl From<&MediaInfo> for TrackSignature {
    fn from(info: &MediaInfo) -> Self {
        Self {
            title: info.title.clone(),
            artist: info.artist.clone(),
            media_url: info.media_url.clone(),
        }
    }
}

#[derive(Clone)]
struct TrackArtwork {
    track: TrackSignature,
    best: Option<AlbumArt>,
    best_priority: u8,
    attempted_sources: HashSet<ArtworkSource>,
    retry_after: HashMap<ArtworkSource, Instant>,
}

impl TrackArtwork {
    fn new(track: TrackSignature) -> Self {
        Self {
            track,
            best: None,
            best_priority: 0,
            attempted_sources: HashSet::new(),
            retry_after: HashMap::new(),
        }
    }

    fn accept(&mut self, source: &ArtworkSource, candidate: AlbumArt) {
        let priority = source.priority();
        let is_better = self.best.as_ref().is_none_or(|current| {
            priority > self.best_priority
                || (priority == self.best_priority
                    && candidate.source_pixel_count() > current.source_pixel_count())
        });
        if is_better {
            self.best = Some(candidate);
            self.best_priority = priority;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ArtworkSource {
    Image(String),
    BandcampPage(String),
}

impl ArtworkSource {
    fn url(&self) -> &str {
        match self {
            Self::Image(url) | Self::BandcampPage(url) => url,
        }
    }

    fn priority(&self) -> u8 {
        // A verified album cover takes precedence over browser placeholder art.
        u8::from(matches!(self, Self::BandcampPage(_)))
    }

    async fn load(
        &self,
        client: &reqwest::Client,
    ) -> (Option<AlbumArt>, Option<bandcamp::PageMetadata>) {
        match self {
            Self::Image(url) => (MediaMonitor::download_artwork(client, url).await, None),
            Self::BandcampPage(url) => {
                let Some(page) = bandcamp::page_url(url) else {
                    return (None, None);
                };
                let Some(metadata) = bandcamp::fetch_page_metadata(client, &page).await else {
                    return (None, None);
                };
                let artwork = match metadata.artwork_url.as_ref() {
                    Some(image) => MediaMonitor::download_artwork(client, image.as_str()).await,
                    None => None,
                };
                (artwork, Some(metadata))
            }
        }
    }
}

#[derive(Clone)]
struct ArtworkRequest {
    player_id: PlayerId,
    track: TrackSignature,
    source: ArtworkSource,
}

struct ArtworkResult {
    request: ArtworkRequest,
    artwork: Option<AlbumArt>,
    bandcamp_metadata: Option<bandcamp::PageMetadata>,
}

#[derive(Default)]
struct BandcampPageCache {
    pages: HashMap<String, (bandcamp::PageMetadata, Instant)>,
}

impl BandcampPageCache {
    fn get(&mut self, url: &str) -> Option<&bandcamp::PageMetadata> {
        let (metadata, used_at) = self.pages.get_mut(url)?;
        *used_at = Instant::now();
        Some(metadata)
    }

    fn insert(&mut self, url: String, metadata: bandcamp::PageMetadata) {
        if !self.pages.contains_key(&url) && self.pages.len() >= MAX_CACHED_ARTWORKS {
            let oldest = self
                .pages
                .iter()
                .min_by_key(|(_, (_, used_at))| *used_at)
                .map(|(url, _)| url.clone());
            if let Some(oldest) = oldest {
                self.pages.remove(&oldest);
            }
        }
        self.pages.insert(url, (metadata, Instant::now()));
    }
}

struct ArtworkLoader {
    requests: tokio::sync::mpsc::Sender<ArtworkRequest>,
    completed: std::sync::mpsc::Receiver<ArtworkResult>,
    pending_sources: HashSet<ArtworkSource>,
    bandcamp_pages: BandcampPageCache,
    available: bool,
}

impl ArtworkLoader {
    fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .user_agent("cosmic-widget-applet/0.1")
            .build()
            .unwrap_or_else(|error| {
                log::warn!("Failed to configure the artwork HTTP client: {error}");
                reqwest::Client::new()
            });
        let (request_tx, mut request_rx) =
            tokio::sync::mpsc::channel::<ArtworkRequest>(ARTWORK_QUEUE_CAPACITY);
        let (completed_tx, completed_rx) = std::sync::mpsc::channel::<ArtworkResult>();

        if let Err(error) = std::thread::Builder::new()
            .name("artwork-loader".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::warn!("Failed to start the artwork runtime: {error}");
                        return;
                    }
                };

                runtime.block_on(async move {
                    let permits =
                        Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_ARTWORK_REQUESTS));
                    while let Some(request) = request_rx.recv().await {
                        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                            break;
                        };
                        let client = client.clone();
                        let completed_tx = completed_tx.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let (artwork, bandcamp_metadata) = request.source.load(&client).await;
                            let _ = completed_tx.send(ArtworkResult {
                                request,
                                artwork,
                                bandcamp_metadata,
                            });
                        });
                    }
                });
            })
        {
            log::warn!("Failed to spawn the artwork loader: {error}");
        }

        Self {
            requests: request_tx,
            completed: completed_rx,
            pending_sources: HashSet::new(),
            bandcamp_pages: BandcampPageCache::default(),
            available: true,
        }
    }

    fn enqueue(&mut self, request: ArtworkRequest) -> bool {
        if self.pending_sources.contains(&request.source) {
            return true;
        }
        if !self.available {
            return false;
        }

        let source = request.source.clone();
        match self.requests.try_send(request) {
            Ok(()) => {
                self.pending_sources.insert(source);
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => false,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.available = false;
                log::warn!("Artwork loader stopped accepting requests");
                false
            }
        }
    }

    fn take_completed(&mut self) -> Vec<ArtworkResult> {
        let completed = self.completed.try_iter().collect::<Vec<_>>();
        for result in &completed {
            self.pending_sources.remove(&result.request.source);
        }
        completed
    }
}

#[derive(Debug, Clone)]
struct PositionTracker {
    track: TrackSignature,
    raw_position: u64,
    position: u64,
    duration: u64,
    sampled_at: Instant,
    status: PlaybackStatus,
}

#[derive(Debug, Clone)]
struct TimelineRefresh {
    track: TrackSignature,
    attempted_at: Instant,
    attempts: u8,
    complete: bool,
}

fn update_tracked_position(
    trackers: &mut HashMap<PlayerId, PositionTracker>,
    player_id: &PlayerId,
    info: &mut MediaInfo,
    now: Instant,
) -> bool {
    let track = TrackSignature::from(&*info);
    let raw_position = info.position;
    let reported_duration = info.duration;
    let mut first_sample = true;

    if let Some(previous) = trackers.get(player_id).filter(|state| state.track == track) {
        first_sample = false;
        let elapsed = now.saturating_duration_since(previous.sampled_at);
        let elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        let predicted = match previous.status {
            PlaybackStatus::Playing => previous.position.saturating_add(elapsed_ms),
            PlaybackStatus::Paused | PlaybackStatus::Stopped => previous.position,
        };
        let raw_changed = raw_position != previous.raw_position;
        let incomplete_timeline =
            reported_duration == 0 && raw_position == 0 && previous.duration > 0;

        if reported_duration == 0 {
            info.duration = previous.duration;
        }

        info.position = if incomplete_timeline {
            predicted
        } else {
            match (&previous.status, &info.status) {
                (PlaybackStatus::Stopped, PlaybackStatus::Playing) => raw_position,
                (_, PlaybackStatus::Playing) if raw_changed => raw_position,
                (_, PlaybackStatus::Playing) => predicted,
                (PlaybackStatus::Playing, PlaybackStatus::Paused) if raw_changed => raw_position,
                (PlaybackStatus::Playing, PlaybackStatus::Paused) => predicted,
                (PlaybackStatus::Paused, PlaybackStatus::Paused) if raw_changed => {
                    let raw_delta = raw_position.abs_diff(previous.raw_position);
                    let looks_like_paused_drift = raw_position > previous.raw_position
                        && raw_delta.abs_diff(elapsed_ms) <= 500;

                    if looks_like_paused_drift {
                        previous.position
                    } else {
                        raw_position
                    }
                }
                (PlaybackStatus::Paused, PlaybackStatus::Paused) => previous.position,
                (_, PlaybackStatus::Paused | PlaybackStatus::Stopped) => raw_position,
            }
        };
    }

    if info.duration > 0 {
        info.position = info.position.min(info.duration);
    }

    trackers.insert(
        player_id.clone(),
        PositionTracker {
            track,
            raw_position,
            position: info.position,
            duration: info.duration,
            sampled_at: now,
            status: info.status.clone(),
        },
    );

    first_sample
}

fn merge_mpris_proxy_timeline(primary: &mut MediaInfo, proxy: &MediaInfo) {
    if proxy.duration > 0 {
        primary.position = proxy.position.min(proxy.duration);
        primary.duration = proxy.duration;
    }
    primary.can_seek |= proxy.can_seek;
}

fn merge_mpris_proxy_player(
    players: &mut [(PlayerId, MediaInfo)],
    raw_tracks: &HashMap<PlayerId, TrackSignature>,
    proxy: &MediaInfo,
    proxy_track: &TrackSignature,
) -> bool {
    if let Some((_, primary)) = players.iter_mut().find(|(id, _)| {
        matches!(id, PlayerId::Mpris(bus) if !MediaMonitor::is_playerctld_mpris_player(bus))
            && raw_tracks.get(id) == Some(proxy_track)
    }) {
        merge_mpris_proxy_timeline(primary, proxy);
        true
    } else {
        false
    }
}

// ============================================================================
// Multi-Player State
// ============================================================================

/// State for all detected media players.
#[derive(Debug, Clone, Default)]
pub struct MultiPlayerState {
    /// All detected players with their current info
    pub players: Vec<(PlayerId, MediaInfo)>,
    /// Index of currently selected/displayed player
    pub current_index: usize,
}

impl MultiPlayerState {
    /// Get the currently selected player's info.
    pub fn current_player(&self) -> Option<&(PlayerId, MediaInfo)> {
        self.players.get(self.current_index)
    }

    /// Get number of players.
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Move to next player (wraps around).
    pub fn next_player(&mut self) {
        if !self.players.is_empty() {
            self.current_index = (self.current_index + 1) % self.players.len();
        }
    }

    /// Move to previous player (wraps around).
    pub fn prev_player(&mut self) {
        if !self.players.is_empty() {
            if self.current_index == 0 {
                self.current_index = self.players.len() - 1;
            } else {
                self.current_index -= 1;
            }
        }
    }

    /// Select player by index.
    pub fn select_player(&mut self, index: usize) {
        if index < self.players.len() {
            self.current_index = index;
        }
    }

    /// Toggle the playing state of the current player.
    /// Used for immediate UI feedback after play/pause commands.
    pub fn toggle_current_playing(&mut self) {
        if let Some((_, info)) = self.players.get_mut(self.current_index) {
            info.status = match info.status {
                PlaybackStatus::Playing => PlaybackStatus::Paused,
                _ => PlaybackStatus::Playing,
            };
        }
    }
}

fn preferred_player_id(
    previous: &MultiPlayerState,
    players: &[(PlayerId, MediaInfo)],
    selected: Option<&PlayerId>,
) -> Option<PlayerId> {
    if players.is_empty() {
        return selected.cloned();
    }

    if !previous.players.is_empty() {
        let newly_playing = players.iter().find(|(id, info)| {
            info.status == PlaybackStatus::Playing
                && previous
                    .players
                    .iter()
                    .find(|(previous_id, _)| previous_id == id)
                    .is_none_or(|(_, previous_info)| {
                        previous_info.status != PlaybackStatus::Playing
                    })
        });
        if let Some((id, _)) = newly_playing {
            return Some(id.clone());
        }
    }

    if let Some(selected) = selected {
        let selected_was_playing = previous
            .players
            .iter()
            .find(|(id, _)| id == selected)
            .is_some_and(|(_, info)| info.status == PlaybackStatus::Playing);
        let selected_is_playing = players
            .iter()
            .find(|(id, _)| id == selected)
            .is_some_and(|(_, info)| info.status == PlaybackStatus::Playing);

        if selected_was_playing
            && !selected_is_playing
            && let Some((id, _)) = players
                .iter()
                .find(|(_, info)| info.status == PlaybackStatus::Playing)
        {
            return Some(id.clone());
        }

        if players.iter().any(|(id, _)| id == selected) {
            return Some(selected.clone());
        }
    }

    players
        .iter()
        .find(|(_, info)| info.status == PlaybackStatus::Playing)
        .or_else(|| players.first())
        .map(|(id, _)| id.clone())
}

// ============================================================================
// Media Monitor Struct
// ============================================================================

/// Monitors media playback from multiple sources.
///
/// Tracks Cider (Apple Music) and all MPRIS D-Bus players.
/// Allows switching between players with pagination dots.
///
/// # Thread Safety
///
/// - `player_state`: All players' info (Arc<Mutex>)
/// - `cider_token`: Shared API token, can be updated from settings
/// - `selected_player`: User's player selection
#[derive(Clone)]
pub struct MediaMonitor {
    /// All players' state
    player_state: Arc<Mutex<MultiPlayerState>>,
    /// Cider API token for authentication (optional)
    cider_token: Arc<Mutex<Option<String>>>,
    /// Currently selected player ID (persists across updates)
    selected_player: Arc<Mutex<Option<PlayerId>>>,
    /// Connection details for the active local Emby Theater session.
    emby_control: Arc<Mutex<Option<EmbyControl>>>,
    /// Ordered, nonblocking playback command queue.
    controls: MediaControlWorker,
}

impl MediaMonitor {
    /// Create a new media monitor with optional Cider API token.
    pub fn new(api_token: Option<String>) -> Self {
        let player_state = Arc::new(Mutex::new(MultiPlayerState::default()));
        let token = api_token.filter(|t| !t.is_empty());
        let cider_token = Arc::new(Mutex::new(token));
        let artwork_cache = Arc::new(Mutex::new(ArtworkCache::new(MAX_CACHED_ARTWORKS)));
        let selected_player = Arc::new(Mutex::new(None));
        let emby_control = Arc::new(Mutex::new(None));
        let mpris = mpris::Monitor::new();
        let cider = cider::Client::new();
        let controls = MediaControlWorker::new(cider.clone(), mpris.clone());

        // Spawn background thread to monitor all players
        let state_clone = Arc::clone(&player_state);
        let token_clone = Arc::clone(&cider_token);
        let cache_clone = Arc::clone(&artwork_cache);
        let selected_clone = Arc::clone(&selected_player);
        let emby_control_clone = Arc::clone(&emby_control);
        let mpris_clone = mpris.clone();
        let cider_clone = cider.clone();

        std::thread::spawn(move || {
            Self::monitor_loop(
                state_clone,
                token_clone,
                cache_clone,
                selected_clone,
                emby_control_clone,
                mpris_clone,
                cider_clone,
            );
        });

        Self {
            player_state,
            cider_token,
            selected_player,
            emby_control,
            controls,
        }
    }

    /// Main background monitoring loop.
    fn monitor_loop(
        player_state: Arc<Mutex<MultiPlayerState>>,
        cider_token: Arc<Mutex<Option<String>>>,
        artwork_cache: Arc<Mutex<ArtworkCache>>,
        selected_player: Arc<Mutex<Option<PlayerId>>>,
        emby_control: Arc<Mutex<Option<EmbyControl>>>,
        mpris: mpris::Monitor,
        cider: cider::Client,
    ) {
        log::info!("Starting multi-player media monitor");
        let mut artwork_by_player: HashMap<PlayerId, TrackArtwork> = HashMap::new();
        let mut artwork_loader = ArtworkLoader::new();
        let mut position_trackers: HashMap<PlayerId, PositionTracker> = HashMap::new();
        let mut timeline_refreshes: HashMap<PlayerId, TimelineRefresh> = HashMap::new();
        let emby_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .user_agent("cosmic-widget-applet/0.1")
            .build()
            .ok();
        let mut emby_discovery = EmbyCredentialDiscovery::default();
        let mut emby_credentials = emby_discovery.refresh();
        let mut last_emby_discovery = Instant::now();

        loop {
            Self::collect_artwork_results(
                &mut artwork_loader,
                &artwork_cache,
                &mut artwork_by_player,
            );
            let mut players: Vec<(PlayerId, MediaInfo)> = Vec::new();

            // 1. Query the native Emby Theater session through Emby Server.
            if last_emby_discovery.elapsed() >= Duration::from_secs(30) {
                emby_credentials = emby_discovery.refresh();
                last_emby_discovery = Instant::now();
            }
            let emby_player = emby_client.as_ref().and_then(|client| {
                emby_credentials
                    .as_ref()
                    .and_then(|credentials| Self::try_emby_player(client, credentials))
            });
            if let Some((player_id, mut info, control)) = emby_player {
                update_tracked_position(
                    &mut position_trackers,
                    &player_id,
                    &mut info,
                    Instant::now(),
                );
                Self::apply_best_artwork(
                    &player_id,
                    &mut info,
                    &artwork_cache,
                    &mut artwork_by_player,
                    &mut artwork_loader,
                );
                *emby_control.lock().unwrap() = Some(control);
                players.push((player_id, info));
            } else {
                *emby_control.lock().unwrap() = None;
            }

            // 2. Try Cider API
            let token = cider_token.lock().unwrap().clone();
            let mut has_cider_api_player = false;
            if let Some(mut info) = cider.now_playing(token.as_deref()) {
                Self::apply_best_artwork(
                    &PlayerId::Cider,
                    &mut info,
                    &artwork_cache,
                    &mut artwork_by_player,
                    &mut artwork_loader,
                );
                players.push((PlayerId::Cider, info));
                has_cider_api_player = true;
            }

            // 3. Consume the signal-driven native MPRIS snapshot. Query the
            // playerctld proxy last so it can supplement the real player it mirrors.
            let mut mpris_players = mpris.players();
            let mut raw_mpris_tracks = HashMap::new();
            mpris_players.sort_by_key(|(name, _)| Self::is_playerctld_mpris_player(name));
            for (bus_name, mut info) in mpris_players {
                if has_cider_api_player && Self::is_cider_mpris_player(&bus_name) {
                    continue;
                }

                let player_id = PlayerId::Mpris(bus_name.clone());
                let track = TrackSignature::from(&info);

                let incomplete_firefox_timeline = Self::is_firefox_mpris_player(&bus_name)
                    && info.status == PlaybackStatus::Playing
                    && info.position == 0
                    && info.duration == 0;
                let should_refresh = incomplete_firefox_timeline
                    && timeline_refreshes.get(&player_id).is_none_or(|refresh| {
                        refresh.track != track
                            || (!refresh.complete
                                && refresh.attempts < MAX_TIMELINE_REFRESH_ATTEMPTS
                                && refresh.attempted_at.elapsed() >= TIMELINE_REFRESH_RETRY)
                    });

                if should_refresh {
                    let previous_attempts = timeline_refreshes
                        .get(&player_id)
                        .filter(|refresh| refresh.track == track)
                        .map_or(0, |refresh| refresh.attempts);
                    if let Some(refreshed) = mpris.refresh_timeline(&bus_name) {
                        let complete = refreshed.duration > 0;
                        info = refreshed;
                        timeline_refreshes.insert(
                            player_id.clone(),
                            TimelineRefresh {
                                track: TrackSignature::from(&info),
                                attempted_at: Instant::now(),
                                attempts: previous_attempts + 1,
                                complete,
                            },
                        );
                    } else {
                        timeline_refreshes.insert(
                            player_id.clone(),
                            TimelineRefresh {
                                track,
                                attempted_at: Instant::now(),
                                attempts: previous_attempts + 1,
                                complete: false,
                            },
                        );
                    }
                }

                let raw_track = TrackSignature::from(&info);
                raw_mpris_tracks.insert(player_id.clone(), raw_track.clone());
                Self::apply_bandcamp_metadata(
                    &bus_name,
                    &mut info,
                    &mut artwork_loader.bandcamp_pages,
                );

                if update_tracked_position(
                    &mut position_trackers,
                    &player_id,
                    &mut info,
                    Instant::now(),
                ) {
                    log::info!(
                        "Initial MPRIS state for {}: status={:?}, position={}ms, duration={}ms",
                        info.player_name,
                        info.status,
                        info.position,
                        info.duration,
                    );
                }

                Self::apply_best_artwork(
                    &player_id,
                    &mut info,
                    &artwork_cache,
                    &mut artwork_by_player,
                    &mut artwork_loader,
                );

                // Fallback to app icon if no album art
                if info.album_art.is_none() {
                    let icon_cache_key = format!("__icon__{}", bus_name);
                    let cached = artwork_cache.lock().unwrap().get(&icon_cache_key);
                    if let Some(art) = cached {
                        info.album_art = Some(art);
                    } else if let Some(art) = Self::load_app_icon(&bus_name) {
                        artwork_cache
                            .lock()
                            .unwrap()
                            .insert(icon_cache_key, art.clone());
                        info.album_art = Some(art);
                    }
                }

                if Self::is_playerctld_mpris_player(&bus_name)
                    && merge_mpris_proxy_player(&mut players, &raw_mpris_tracks, &info, &raw_track)
                {
                    continue;
                }

                players.push((player_id, info));
            }

            position_trackers.retain(|id, _| players.iter().any(|(player_id, _)| player_id == id));
            artwork_by_player.retain(|id, _| players.iter().any(|(player_id, _)| player_id == id));
            timeline_refreshes.retain(|id, _| players.iter().any(|(player_id, _)| player_id == id));

            // Sort: playing first, then by player name
            players.sort_by(|a, b| {
                let a_playing = a.1.status == PlaybackStatus::Playing;
                let b_playing = b.1.status == PlaybackStatus::Playing;
                match (a_playing, b_playing) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.1.player_name.cmp(&b.1.player_name),
                }
            });

            // Update state with proper index handling
            {
                let mut state = player_state.lock().unwrap();
                let mut selected = selected_player.lock().unwrap();
                let preferred = preferred_player_id(&state, &players, selected.as_ref());

                let new_index = if let Some(ref player_id) = preferred {
                    players
                        .iter()
                        .position(|(id, _)| id == player_id)
                        .unwrap_or(0)
                } else {
                    0
                };

                *selected = preferred;
                state.players = players;
                state.current_index = new_index.min(state.players.len().saturating_sub(1));
            }

            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn discover_emby_credentials() -> Option<EmbyCredentials> {
        EmbyCredentialDiscovery::default().refresh()
    }

    fn scan_emby_credentials(files: &[EmbyLevelDbFile]) -> Option<EmbyCredentials> {
        files
            .iter()
            .filter_map(|file| std::fs::read(&file.path).ok())
            .filter_map(|bytes| Self::parse_emby_credentials(&bytes))
            .max_by_key(|credentials| credentials.last_accessed)
    }

    fn parse_emby_credentials(bytes: &[u8]) -> Option<EmbyCredentials> {
        const SAVED_SERVERS_MARKER: &[u8] = b"{\"Servers\":[";

        let mut saved_servers = Vec::new();
        let mut search_start = 0;
        while let Some(offset) = bytes[search_start..]
            .windows(SAVED_SERVERS_MARKER.len())
            .position(|window| window == SAVED_SERVERS_MARKER)
        {
            let json_start = search_start + offset;
            let mut deserializer = serde_json::Deserializer::from_slice(&bytes[json_start..]);
            if let Ok(saved) = SavedEmbyServers::deserialize(&mut deserializer) {
                saved_servers.push(saved);
            }
            search_start = json_start + SAVED_SERVERS_MARKER.len();
        }

        saved_servers
            .into_iter()
            .flat_map(|saved| saved.servers)
            .filter_map(|server| {
                let configured_user_id = server.user_id.as_deref();
                let user = configured_user_id
                    .and_then(|user_id| server.users.iter().find(|user| user.user_id == user_id))
                    .or_else(|| server.users.first())?;
                if user.access_token.is_empty() {
                    return None;
                }

                let mut server_urls = Vec::new();
                for address in [
                    server.local_address,
                    server.manual_address,
                    server.remote_address,
                ]
                .into_iter()
                .flatten()
                {
                    let address = address.trim_end_matches('/').to_string();
                    if !address.is_empty() && !server_urls.contains(&address) {
                        server_urls.push(address);
                    }
                }
                (!server_urls.is_empty()).then(|| EmbyCredentials {
                    server_urls,
                    user_id: user.user_id.clone(),
                    access_token: user.access_token.clone(),
                    last_accessed: server.date_last_accessed,
                })
            })
            .max_by_key(|credentials| credentials.last_accessed)
    }

    fn try_emby_player(
        client: &reqwest::blocking::Client,
        credentials: &EmbyCredentials,
    ) -> Option<(PlayerId, MediaInfo, EmbyControl)> {
        let host_name = sysinfo::System::host_name().unwrap_or_default();

        for server_url in &credentials.server_urls {
            let Ok(response) = client
                .get(Self::emby_api_url(server_url, "Sessions"))
                .header("X-Emby-Token", &credentials.access_token)
                .query(&[("ControllableByUserId", credentials.user_id.as_str())])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
            else {
                continue;
            };
            let Ok(sessions) = response.json::<Vec<EmbySession>>() else {
                continue;
            };
            let active_theater = sessions
                .iter()
                .filter(|session| {
                    session.client.eq_ignore_ascii_case("Emby Theater")
                        && session.now_playing_item.is_some()
                })
                .collect::<Vec<_>>();
            let session = active_theater
                .iter()
                .copied()
                .find(|session| {
                    !host_name.is_empty()
                        && (session.device_name.eq_ignore_ascii_case(&host_name)
                            || session.device_id.eq_ignore_ascii_case(&host_name))
                })
                .or_else(|| (active_theater.len() == 1).then(|| active_theater[0]))?;
            let (player_id, info) = Self::media_info_from_emby_session(server_url, session)?;
            let control = EmbyControl {
                server_url: server_url.clone(),
                access_token: credentials.access_token.clone(),
                session_id: session.id.clone(),
            };
            return Some((player_id, info, control));
        }

        None
    }

    fn media_info_from_emby_session(
        server_url: &str,
        session: &EmbySession,
    ) -> Option<(PlayerId, MediaInfo)> {
        let item = session.now_playing_item.as_ref()?;
        let play_state = session.play_state.as_ref()?;
        let status = if play_state.is_paused.unwrap_or(false) {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        let artist = if let Some(series_name) = item.series_name.as_ref() {
            series_name.clone()
        } else {
            item.artists.as_deref().unwrap_or_default().join(", ")
        };
        let album = match (item.parent_index_number, item.index_number) {
            (Some(season), Some(episode)) => format!("Season {season}, Episode {episode}"),
            _ if item.album.as_ref().is_some_and(|album| !album.is_empty()) => {
                item.album.clone().unwrap_or_default()
            }
            _ => item
                .production_year
                .map(|year| year.to_string())
                .unwrap_or_default(),
        };
        let can_go_previous = session.playlist_index.is_some_and(|index| index > 0);
        let can_go_next = match (session.playlist_index, session.playlist_length) {
            (Some(index), Some(length)) => index + 1 < length,
            _ => false,
        };

        Some((
            PlayerId::Emby(session.id.clone()),
            MediaInfo {
                player_name: "Emby".to_string(),
                title: item.name.clone(),
                artist,
                album,
                art_url: Self::emby_artwork_url(server_url, item),
                media_url: Some(format!("emby://item/{}", item.id)),
                album_art: None,
                status,
                position: play_state.position_ticks.unwrap_or(0) / 10_000,
                duration: item.run_time_ticks.unwrap_or(0) / 10_000,
                can_play: session.supports_remote_control,
                can_pause: session.supports_remote_control,
                can_go_next,
                can_go_previous,
                can_seek: session.supports_remote_control && play_state.can_seek.unwrap_or(false),
            },
        ))
    }

    fn emby_api_url(server_url: &str, path: &str) -> String {
        let base = server_url.trim_end_matches('/');
        if base.ends_with("/emby") {
            format!("{base}/{}", path.trim_start_matches('/'))
        } else {
            format!("{base}/emby/{}", path.trim_start_matches('/'))
        }
    }

    fn emby_artwork_url(server_url: &str, item: &EmbyItem) -> Option<String> {
        let (item_id, image_type, tag) = if let Some(tag) = item
            .image_tags
            .as_ref()
            .and_then(|tags| tags.get("Primary"))
        {
            (item.id.as_str(), "Primary", tag.as_str())
        } else if let (Some(item_id), Some(tag)) = (
            item.parent_thumb_item_id.as_deref(),
            item.parent_thumb_image_tag.as_deref(),
        ) {
            (item_id, "Thumb", tag)
        } else if let Some(series_id) = item.series_id.as_deref() {
            (
                series_id,
                "Primary",
                item.series_primary_image_tag.as_deref()?,
            )
        } else {
            return None;
        };
        let max_width = if item.primary_image_aspect_ratio.unwrap_or(1.0) >= 1.25 {
            640
        } else {
            400
        };

        Some(format!(
            "{}?maxWidth={max_width}&quality=90&tag={}",
            Self::emby_api_url(server_url, &format!("Items/{item_id}/Images/{image_type}")),
            urlencoding::encode(tag),
        ))
    }

    // ========================================================================
    // MPRIS D-Bus Methods
    // ========================================================================

    fn is_cider_mpris_player(bus_name: &str) -> bool {
        bus_name
            .strip_prefix("org.mpris.MediaPlayer2.")
            .and_then(|identity| identity.split('.').next())
            .is_some_and(|identity| identity.eq_ignore_ascii_case("cider"))
    }

    fn is_firefox_mpris_player(bus_name: &str) -> bool {
        bus_name
            .strip_prefix("org.mpris.MediaPlayer2.")
            .and_then(|identity| identity.split('.').next())
            .is_some_and(|identity| identity.eq_ignore_ascii_case("firefox"))
    }

    fn is_playerctld_mpris_player(bus_name: &str) -> bool {
        bus_name
            .strip_prefix("org.mpris.MediaPlayer2.")
            .is_some_and(|identity| identity.eq_ignore_ascii_case("playerctld"))
    }

    fn apply_bandcamp_metadata(
        bus_name: &str,
        info: &mut MediaInfo,
        pages: &mut BandcampPageCache,
    ) {
        let Some(page) = info.media_url.as_deref().and_then(bandcamp::page_url) else {
            return;
        };
        let Some(metadata) = pages.get(page.as_str()) else {
            return;
        };
        let direct_track = metadata.tracks.iter().find(|track| track.url == page);
        let caption = direct_track.map_or(metadata.album.as_str(), |track| track.title.as_str());
        if !Self::is_bandcamp_page_caption(&info.title, caption, &metadata.artist) {
            return;
        }

        let matched = direct_track.or_else(|| {
            // Firefox (including Zen) truncates mDuration to whole seconds
            // before exposing mpris:length. Do not use a nearest-duration guess:
            // every track must be known, and exactly one must match that second.
            // See widget/gtk/MPRISServiceHandler.cpp::GetMetadataAsGVariant.
            if !Self::is_firefox_mpris_player(bus_name)
                || !metadata.tracks_complete
                || info.duration == 0
                || info.duration % 1000 != 0
            {
                return None;
            }
            let mut matches = metadata
                .tracks
                .iter()
                .filter(|track| track.duration_ms / 1000 == info.duration / 1000);
            let candidate = matches.next()?;
            matches.next().is_none().then_some(candidate)
        });
        if let Some(track) = matched {
            info.title.clone_from(&track.title);
            if info.artist.trim().is_empty() {
                info.artist.clone_from(&metadata.artist);
            }
            if info.album.trim().is_empty() && page.path().starts_with("/album/") {
                info.album.clone_from(&metadata.album);
            }
        }
    }

    fn is_bandcamp_page_caption(title: &str, page_title: &str, artist: &str) -> bool {
        if page_title.is_empty() {
            return false;
        }
        let title = title
            .trim_start_matches(|character: char| {
                character.is_whitespace()
                    || matches!(character, '▶' | '►' | '⏸' | '\u{fe0e}' | '\u{fe0f}')
            })
            .trim_end();
        title == page_title || (!artist.is_empty() && title == format!("{page_title} | {artist}"))
    }

    fn apply_best_artwork(
        player_id: &PlayerId,
        info: &mut MediaInfo,
        artwork_cache: &Arc<Mutex<ArtworkCache>>,
        artwork_by_player: &mut HashMap<PlayerId, TrackArtwork>,
        artwork_loader: &mut ArtworkLoader,
    ) {
        let track = TrackSignature::from(&*info);
        let selection = artwork_by_player
            .entry(player_id.clone())
            .or_insert_with(|| TrackArtwork::new(track.clone()));
        if selection.track != track {
            *selection = TrackArtwork::new(track.clone());
        }

        let is_youtube = info
            .media_url
            .as_deref()
            .and_then(Self::extract_youtube_video_id)
            .is_some();
        for source in Self::artwork_candidate_sources(info) {
            let needs_bandcamp_metadata = matches!(source, ArtworkSource::BandcampPage(_))
                && !artwork_loader
                    .bandcamp_pages
                    .pages
                    .contains_key(source.url());
            if needs_bandcamp_metadata && !artwork_loader.pending_sources.contains(&source) {
                // Either cache can evict a still-active page. Retain retry
                // deadlines, but allow its track list to be fetched again.
                selection.attempted_sources.remove(&source);
            }
            let cached = { artwork_cache.lock().unwrap().get(source.url()) };
            if let Some(candidate) = cached {
                let adequate_youtube_thumbnail = candidate.source_width
                    >= MIN_YOUTUBE_THUMBNAIL_WIDTH
                    && candidate.source_height >= MIN_YOUTUBE_THUMBNAIL_HEIGHT;
                selection.accept(&source, candidate);
                if is_youtube && adequate_youtube_thumbnail {
                    break;
                }
                if !needs_bandcamp_metadata {
                    selection.attempted_sources.insert(source.clone());
                    continue;
                }
            }

            if selection.attempted_sources.contains(&source)
                || selection
                    .retry_after
                    .get(&source)
                    .is_some_and(|retry| Instant::now() < *retry)
            {
                continue;
            }

            let request = ArtworkRequest {
                player_id: player_id.clone(),
                track: track.clone(),
                source: source.clone(),
            };
            if artwork_loader.enqueue(request) {
                selection.retry_after.remove(&source);
                selection.attempted_sources.insert(source);
            }
        }

        info.album_art = selection.best.clone();
    }

    fn collect_artwork_results(
        artwork_loader: &mut ArtworkLoader,
        artwork_cache: &Arc<Mutex<ArtworkCache>>,
        artwork_by_player: &mut HashMap<PlayerId, TrackArtwork>,
    ) {
        for result in artwork_loader.take_completed() {
            let ArtworkResult {
                request,
                artwork,
                bandcamp_metadata,
            } = result;
            if let Some(metadata) = bandcamp_metadata {
                artwork_loader
                    .bandcamp_pages
                    .insert(request.source.url().to_string(), metadata);
            }
            let Some(artwork) = artwork else {
                log::debug!("Unable to load artwork from {}", request.source.url());
                Self::retry_failed_bandcamp_artwork(
                    &request.source,
                    artwork_by_player,
                    Instant::now(),
                );
                continue;
            };

            Self::accept_completed_artwork(
                request,
                artwork,
                &mut artwork_cache.lock().unwrap(),
                artwork_by_player,
            );
        }
    }

    fn retry_failed_bandcamp_artwork(
        source: &ArtworkSource,
        artwork_by_player: &mut HashMap<PlayerId, TrackArtwork>,
        now: Instant,
    ) {
        if !matches!(source, ArtworkSource::BandcampPage(_)) {
            return;
        }
        // Requests for the same album are shared across tracks and players.
        // Every waiting selection must recover if that shared fetch fails.
        for selection in artwork_by_player.values_mut() {
            if selection.attempted_sources.remove(source) {
                selection
                    .retry_after
                    .insert(source.clone(), now + BANDCAMP_ARTWORK_RETRY_DELAY);
            }
        }
    }

    fn accept_completed_artwork(
        request: ArtworkRequest,
        artwork: AlbumArt,
        artwork_cache: &mut ArtworkCache,
        artwork_by_player: &mut HashMap<PlayerId, TrackArtwork>,
    ) {
        let ArtworkRequest {
            player_id,
            track,
            source,
        } = request;
        artwork_cache.insert(source.url().to_string(), artwork.clone());
        if let Some(selection) = artwork_by_player.get_mut(&player_id)
            && selection.track == track
        {
            selection.accept(&source, artwork);
        }
    }

    fn artwork_candidate_sources(info: &MediaInfo) -> Vec<ArtworkSource> {
        let mut sources = Vec::new();
        if let Some(page) = info.media_url.as_deref().and_then(bandcamp::page_url) {
            sources.push(ArtworkSource::BandcampPage(page.to_string()));
        }
        sources.extend(
            Self::artwork_candidate_urls(info)
                .into_iter()
                .map(ArtworkSource::Image),
        );
        sources
    }

    fn artwork_candidate_urls(info: &MediaInfo) -> Vec<String> {
        let mut candidates = info
            .media_url
            .as_deref()
            .and_then(Self::extract_youtube_video_id)
            .map(|video_id| {
                ["maxresdefault", "hqdefault", "mqdefault"]
                    .into_iter()
                    .map(|variant| format!("https://i.ytimg.com/vi/{video_id}/{variant}.jpg"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if let Some(art_url) = info.art_url.as_ref()
            && !art_url.is_empty()
            && !candidates.contains(art_url)
        {
            candidates.push(art_url.clone());
        }
        candidates
    }

    fn extract_youtube_video_id(url: &str) -> Option<String> {
        let url = reqwest::Url::parse(url).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        let candidate = if host == "youtu.be" || host == "www.youtu.be" {
            url.path_segments()?
                .find(|segment| !segment.is_empty())?
                .to_string()
        } else if host == "youtube.com" || host.ends_with(".youtube.com") {
            if url.path().trim_end_matches('/') == "/watch" {
                url.query_pairs()
                    .find(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())?
            } else {
                let mut segments = url.path_segments()?.filter(|segment| !segment.is_empty());
                match segments.next()? {
                    "shorts" | "embed" | "live" => segments.next()?.to_string(),
                    _ => return None,
                }
            }
        } else {
            return None;
        };

        (candidate.len() == 11
            && candidate
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
        .then_some(candidate)
    }

    /// Get the icon path for a player application.
    ///
    /// Searches common icon locations for the app's icon.
    fn get_player_icon_path(bus_name: &str) -> Option<String> {
        // Extract app name from bus name (e.g., "org.mpris.MediaPlayer2.firefox.instance_1_278" -> "firefox")
        let app_name = bus_name
            .strip_prefix("org.mpris.MediaPlayer2.")
            .unwrap_or(bus_name)
            .split('.')
            .next()
            .unwrap_or(bus_name)
            .to_lowercase();

        // Common icon directories to search
        let icon_dirs = [
            "/usr/share/icons/hicolor/256x256/apps",
            "/usr/share/icons/hicolor/128x128/apps",
            "/usr/share/icons/hicolor/96x96/apps",
            "/usr/share/icons/hicolor/64x64/apps",
            "/usr/share/icons/hicolor/48x48/apps",
            "/usr/share/icons/hicolor/scalable/apps",
            "/usr/share/pixmaps",
            "/usr/share/app-info/icons/pop-artful-extra/64x64",
            "/usr/share/app-info/icons/ubuntu-focal-universe/64x64",
            "/usr/share/app-install/icons",
            "/var/lib/flatpak/exports/share/icons/hicolor/256x256/apps",
            "/var/lib/flatpak/exports/share/icons/hicolor/128x128/apps",
            "/var/lib/flatpak/exports/share/icons/hicolor/64x64/apps",
            &format!(
                "{}/.local/share/icons/hicolor/256x256/apps",
                std::env::var("HOME").unwrap_or_default()
            ),
            &format!(
                "{}/.local/share/icons/hicolor/128x128/apps",
                std::env::var("HOME").unwrap_or_default()
            ),
            &format!(
                "{}/.local/share/icons/hicolor/64x64/apps",
                std::env::var("HOME").unwrap_or_default()
            ),
        ];

        // Extensions to try
        let extensions = ["png", "svg", "xpm"];

        // Try to find exact match first
        for dir in &icon_dirs {
            for ext in &extensions {
                let path = format!("{}/{}.{}", dir, app_name, ext);
                if std::path::Path::new(&path).exists() {
                    log::info!("Found app icon: {}", path);
                    return Some(path);
                }
            }
        }

        // Try common browser variations
        let variations: &[&str] = match app_name.as_str() {
            "firefox" => &[
                "firefox",
                "firefox-esr",
                "org.mozilla.firefox",
                "firefox-developer-edition",
            ],
            "chromium" => &["chromium", "chromium-browser", "org.chromium.Chromium"],
            "chrome" | "google-chrome" => &["google-chrome", "chrome", "google-chrome-stable"],
            "brave" => &["brave", "brave-browser", "com.brave.Browser"],
            "vivaldi" => &["vivaldi", "vivaldi-stable"],
            "opera" => &["opera", "opera-stable"],
            "edge" | "msedge" => &["microsoft-edge", "msedge"],
            _ => &[],
        };

        for variant in variations {
            for dir in &icon_dirs {
                for ext in &extensions {
                    let path = format!("{}/{}.{}", dir, variant, ext);
                    if std::path::Path::new(&path).exists() {
                        log::info!("Found app icon (variant): {}", path);
                        return Some(path);
                    }
                }
            }
        }

        None
    }

    /// Load an app icon as album art (fallback when no real album art).
    fn load_app_icon(bus_name: &str) -> Option<AlbumArt> {
        let icon_path = Self::get_player_icon_path(bus_name)?;

        log::info!("Loading app icon as fallback: {}", icon_path);

        let image_data = std::fs::read(&icon_path).ok()?;

        // Handle SVG separately
        if icon_path.ends_with(".svg") {
            // For SVG, we need to rasterize - but that requires additional deps
            // For now, skip SVG files
            log::info!("Skipping SVG icon (not supported for fallback)");
            return None;
        }

        // Decode image
        Self::decode_artwork(image_data)
    }

    /// Download and decode album artwork from URL.
    ///
    /// Reads the encoded image and creates a stable Iced handle.
    /// Handles both http(s):// and file:// URLs.
    async fn download_artwork(client: &reqwest::Client, url: &str) -> Option<AlbumArt> {
        log::info!("Downloading album art from: {}", url);

        let uri = reqwest::Url::parse(url).ok()?;
        let image_data = match uri.scheme() {
            "file" => {
                let path = uri.to_file_path().ok()?;
                let metadata = tokio::fs::metadata(&path).await.ok()?;
                if metadata.len() > MAX_ARTWORK_BYTES as u64 {
                    log::warn!("Artwork file exceeds size limit: {url}");
                    return None;
                }
                tokio::fs::read(path).await.ok()?
            }
            "http" | "https" => {
                let mut response = client.get(uri).send().await.ok()?.error_for_status().ok()?;
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_ARTWORK_BYTES as u64)
                {
                    log::warn!("Artwork response exceeds size limit: {url}");
                    return None;
                }

                let capacity = response
                    .content_length()
                    .and_then(|length| usize::try_from(length).ok())
                    .unwrap_or_default()
                    .min(MAX_ARTWORK_BYTES);
                let mut bytes = Vec::with_capacity(capacity);
                while let Some(chunk) = response.chunk().await.ok()? {
                    if bytes.len().saturating_add(chunk.len()) > MAX_ARTWORK_BYTES {
                        log::warn!("Artwork response exceeds size limit: {url}");
                        return None;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                bytes
            }
            scheme => {
                log::warn!("Unsupported artwork URI scheme {scheme}: {url}");
                return None;
            }
        };

        Self::decode_artwork(image_data)
    }

    fn decode_artwork(image_data: Vec<u8>) -> Option<AlbumArt> {
        if image_data.is_empty() || image_data.len() > MAX_ARTWORK_BYTES {
            return None;
        }

        let encoded_bytes = image_data.len();
        let (source_width, source_height) =
            image::ImageReader::new(std::io::Cursor::new(image_data.as_slice()))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()?;
        if source_width == 0
            || source_height == 0
            || source_width > MAX_ARTWORK_DIMENSION
            || source_height > MAX_ARTWORK_DIMENSION
        {
            log::warn!("Rejected artwork dimensions: {source_width}x{source_height}");
            return None;
        }

        log::info!("Album art loaded: {source_width}x{source_height}");

        Some(AlbumArt {
            source_width,
            source_height,
            iced_handle: cosmic::iced::widget::image::Handle::from_bytes(image_data),
            encoded_bytes,
        })
    }

    // ========================================================================
    // Public API
    // ========================================================================

    /// Get the multi-player state snapshot.
    pub fn get_player_state(&self) -> MultiPlayerState {
        self.player_state.lock().unwrap().clone()
    }

    /// Get current media info (for backward compatibility).
    pub fn get_media_info(&self) -> MediaInfo {
        let state = self.player_state.lock().unwrap();
        state
            .current_player()
            .map(|(_, info)| info.clone())
            .unwrap_or_default()
    }

    /// Select next player.
    pub fn next_player(&self) {
        let mut state = self.player_state.lock().unwrap();
        state.next_player();
        if let Some((id, _)) = state.current_player() {
            *self.selected_player.lock().unwrap() = Some(id.clone());
        }
    }

    /// Select previous player.
    pub fn prev_player(&self) {
        let mut state = self.player_state.lock().unwrap();
        state.prev_player();
        if let Some((id, _)) = state.current_player() {
            *self.selected_player.lock().unwrap() = Some(id.clone());
        }
    }

    /// Select player by index.
    pub fn select_player(&self, index: usize) {
        let mut state = self.player_state.lock().unwrap();
        state.select_player(index);
        if let Some((id, _)) = state.current_player() {
            *self.selected_player.lock().unwrap() = Some(id.clone());
        }
    }

    /// Select a player by its stable backend identity.
    pub fn select_player_by_id(&self, player_id: &PlayerId) -> bool {
        let mut state = self.player_state.lock().unwrap();
        let Some(index) = state.players.iter().position(|(id, _)| id == player_id) else {
            return false;
        };

        state.current_index = index;
        *self.selected_player.lock().unwrap() = Some(player_id.clone());
        true
    }

    /// Update Cider API token.
    #[allow(dead_code)]
    pub fn set_cider_token(&self, token: Option<String>) {
        *self.cider_token.lock().unwrap() = token;
        log::info!("Cider API token updated");
    }

    // ========================================================================
    // Playback Control
    // ========================================================================

    /// Toggle play/pause on the current player.
    pub fn play_pause(&self) {
        let mut state = self.player_state.lock().unwrap();
        if let Some((player_id, _)) = state.current_player() {
            let player_id = player_id.clone();

            // Toggle local state immediately for responsive UI
            state.toggle_current_playing();
            let fallback = if state
                .current_player()
                .is_some_and(|(_, info)| info.status == PlaybackStatus::Playing)
            {
                None
            } else {
                state
                    .players
                    .iter()
                    .enumerate()
                    .find(|(_, (id, info))| {
                        id != &player_id && info.status == PlaybackStatus::Playing
                    })
                    .map(|(index, (id, _))| (index, id.clone()))
            };
            if let Some((index, id)) = fallback {
                state.current_index = index;
                *self.selected_player.lock().unwrap() = Some(id);
            }
            drop(state);

            log::info!("play_pause called for player: {:?}", player_id);
            match &player_id {
                PlayerId::Cider => self.cider_play_pause(),
                PlayerId::Emby(session_id) => {
                    self.send_emby_playstate_command(session_id, "PlayPause", None);
                }
                PlayerId::Mpris(bus_name) => self.mpris_play_pause(bus_name),
            }
        } else {
            log::warn!("play_pause called but no current player available");
        }
    }

    /// Skip to next track on the current player.
    pub fn next(&self) {
        let state = self.player_state.lock().unwrap();
        if let Some((player_id, _)) = state.current_player() {
            let player_id = player_id.clone();
            drop(state);

            match &player_id {
                PlayerId::Cider => self.cider_next(),
                PlayerId::Emby(session_id) => {
                    self.send_emby_playstate_command(session_id, "NextTrack", None);
                }
                PlayerId::Mpris(bus_name) => self.mpris_next(bus_name),
            }
        }
    }

    /// Go to previous track on the current player.
    pub fn previous(&self) {
        let state = self.player_state.lock().unwrap();
        if let Some((player_id, _)) = state.current_player() {
            let player_id = player_id.clone();
            drop(state);

            match &player_id {
                PlayerId::Cider => self.cider_previous(),
                PlayerId::Emby(session_id) => {
                    self.send_emby_playstate_command(session_id, "PreviousTrack", None);
                }
                PlayerId::Mpris(bus_name) => self.mpris_previous(bus_name),
            }
        }
    }

    /// Seek to position based on progress (0.0 to 1.0).
    pub fn seek_to_progress(&self, progress: f64) -> bool {
        let mut state = self.player_state.lock().unwrap();
        let current_index = state.current_index;
        if let Some((player_id, info)) = state.players.get_mut(current_index) {
            let player_id = player_id.clone();
            let target_ms = (info.duration as f64 * progress.clamp(0.0, 1.0)) as u64;
            info.position = target_ms;
            drop(state);

            match &player_id {
                PlayerId::Cider => self.cider_seek(target_ms as f64 / 1000.0),
                PlayerId::Emby(session_id) => self.send_emby_playstate_command(
                    session_id,
                    "Seek",
                    Some(("SeekPositionTicks", target_ms.saturating_mul(10_000))),
                ),
                PlayerId::Mpris(bus_name) => self.mpris_seek(bus_name, target_ms * 1000),
            }
        } else {
            false
        }
    }

    // ========================================================================
    // Cider Control Methods
    // ========================================================================

    fn send_emby_playstate_command(
        &self,
        session_id: &str,
        command: &'static str,
        query: Option<(&'static str, u64)>,
    ) -> bool {
        let Some(control) = self
            .emby_control
            .lock()
            .unwrap()
            .clone()
            .filter(|control| control.session_id == session_id)
        else {
            return false;
        };
        self.controls.enqueue(MediaControlCommand::Emby {
            control,
            command,
            query,
        })
    }

    fn send_cider_command(&self, endpoint: &'static str) -> bool {
        let token = self.cider_token.lock().unwrap().clone();
        self.controls
            .enqueue(MediaControlCommand::CiderCommand { endpoint, token })
    }

    fn cider_play_pause(&self) {
        self.send_cider_command("playpause");
    }

    fn cider_next(&self) {
        self.send_cider_command("next");
    }

    fn cider_previous(&self) {
        self.send_cider_command("previous");
    }

    fn cider_seek(&self, position_seconds: f64) -> bool {
        let token = self.cider_token.lock().unwrap().clone();
        self.controls.enqueue(MediaControlCommand::CiderSeek {
            position_seconds: position_seconds.max(0.0) as u64,
            token,
        })
    }

    // ========================================================================
    // MPRIS Control Methods
    // ========================================================================

    fn mpris_play_pause(&self, bus_name: &str) {
        log::info!("Sending PlayPause to MPRIS player: {}", bus_name);
        self.controls.enqueue(MediaControlCommand::Mpris {
            bus_name: bus_name.to_string(),
            action: MprisControlAction::PlayPause,
        });
    }

    fn mpris_next(&self, bus_name: &str) {
        self.controls.enqueue(MediaControlCommand::Mpris {
            bus_name: bus_name.to_string(),
            action: MprisControlAction::Next,
        });
    }

    fn mpris_previous(&self, bus_name: &str) {
        self.controls.enqueue(MediaControlCommand::Mpris {
            bus_name: bus_name.to_string(),
            action: MprisControlAction::Previous,
        });
    }

    fn mpris_seek(&self, bus_name: &str, position_us: u64) -> bool {
        self.controls.enqueue(MediaControlCommand::Mpris {
            bus_name: bus_name.to_string(),
            action: MprisControlAction::Seek(position_us),
        })
    }
}

#[cfg(test)]
mod tests;
