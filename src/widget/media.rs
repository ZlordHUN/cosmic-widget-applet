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
//! ## Polling Architecture
//!
//! A background thread polls every second:
//! 1. Query Cider API for track info
//! 2. Enumerate MPRIS players via D-Bus
//! 3. Query each player's metadata and status
//! 4. Update shared state with all players

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_ARTWORK_BYTES: usize = 12 * 1024 * 1024;
const MAX_ARTWORK_DIMENSION: u32 = 4096;
const LEGACY_ARTWORK_DIMENSION: u32 = 512;
const MIN_YOUTUBE_THUMBNAIL_WIDTH: u32 = 320;
const MIN_YOUTUBE_THUMBNAIL_HEIGHT: u32 = 180;

// ============================================================================
// Album Art Cache
// ============================================================================

/// Decoded album art ready for rendering.
///
/// Stores RGBA pixel data along with dimensions for Cairo rendering.
#[derive(Clone)]
pub struct AlbumArt {
    /// Premultiplied BGRA pixels retained for the legacy Cairo renderer.
    pub data: Arc<[u8]>,
    /// Pixel width of the legacy render buffer.
    pub width: u32,
    /// Pixel height of the legacy render buffer.
    pub height: u32,
    /// Decoded width before any legacy-renderer downscaling.
    pub source_width: u32,
    /// Decoded height before any legacy-renderer downscaling.
    pub source_height: u32,
    /// Stable Iced handle backed by the original encoded image bytes.
    pub iced_handle: cosmic::iced::widget::image::Handle,
}

impl AlbumArt {
    fn source_pixel_count(&self) -> u64 {
        u64::from(self.source_width) * u64::from(self.source_height)
    }
}

impl std::fmt::Debug for AlbumArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlbumArt")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source_width", &self.source_width)
            .field("source_height", &self.source_height)
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// Cache for downloaded and decoded album artwork.
///
/// Keyed by artwork URL to avoid re-downloading the same image.
/// Limited to prevent unbounded memory growth.
struct ArtworkCache {
    /// URL → decoded artwork mapping
    cache: HashMap<String, AlbumArt>,
    /// Maximum number of cached artworks
    max_size: usize,
}

impl ArtworkCache {
    fn new(max_size: usize) -> Self {
        Self {
            cache: HashMap::new(),
            max_size,
        }
    }

    fn get(&self, url: &str) -> Option<AlbumArt> {
        self.cache.get(url).cloned()
    }

    fn insert(&mut self, url: String, art: AlbumArt) {
        // Simple eviction: clear cache if at capacity
        if self.cache.len() >= self.max_size {
            self.cache.clear();
        }
        self.cache.insert(url, art);
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
    attempted_urls: HashSet<String>,
}

impl TrackArtwork {
    fn new(track: TrackSignature) -> Self {
        Self {
            track,
            best: None,
            attempted_urls: HashSet::new(),
        }
    }

    fn accept(&mut self, candidate: AlbumArt) {
        let is_better = self
            .best
            .as_ref()
            .is_none_or(|current| candidate.source_pixel_count() > current.source_pixel_count());
        if is_better {
            self.best = Some(candidate);
        }
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
/// - `artwork_cache`: Shared cache for decoded album artwork
/// - `selected_player`: User's player selection
#[derive(Clone)]
pub struct MediaMonitor {
    /// All players' state
    player_state: Arc<Mutex<MultiPlayerState>>,
    /// Cider API token for authentication (optional)
    cider_token: Arc<Mutex<Option<String>>>,
    /// Cache for downloaded album artwork
    artwork_cache: Arc<Mutex<ArtworkCache>>,
    /// Currently selected player ID (persists across updates)
    selected_player: Arc<Mutex<Option<PlayerId>>>,
    /// Connection details for the active local Emby Theater session.
    emby_control: Arc<Mutex<Option<EmbyControl>>>,
}

impl MediaMonitor {
    /// Create a new media monitor with optional Cider API token.
    pub fn new(api_token: Option<String>) -> Self {
        let player_state = Arc::new(Mutex::new(MultiPlayerState::default()));
        let token = api_token.filter(|t| !t.is_empty());
        let cider_token = Arc::new(Mutex::new(token));
        let artwork_cache = Arc::new(Mutex::new(ArtworkCache::new(20)));
        let selected_player = Arc::new(Mutex::new(None));
        let emby_control = Arc::new(Mutex::new(None));

        // Spawn background thread to monitor all players
        let state_clone = Arc::clone(&player_state);
        let token_clone = Arc::clone(&cider_token);
        let cache_clone = Arc::clone(&artwork_cache);
        let selected_clone = Arc::clone(&selected_player);
        let emby_control_clone = Arc::clone(&emby_control);

        std::thread::spawn(move || {
            Self::monitor_loop(
                state_clone,
                token_clone,
                cache_clone,
                selected_clone,
                emby_control_clone,
            );
        });

        Self {
            player_state,
            cider_token,
            artwork_cache,
            selected_player,
            emby_control,
        }
    }

    /// Main background monitoring loop.
    fn monitor_loop(
        player_state: Arc<Mutex<MultiPlayerState>>,
        cider_token: Arc<Mutex<Option<String>>>,
        artwork_cache: Arc<Mutex<ArtworkCache>>,
        selected_player: Arc<Mutex<Option<PlayerId>>>,
        emby_control: Arc<Mutex<Option<EmbyControl>>>,
    ) {
        log::info!("Starting multi-player media monitor");
        let mut artwork_by_player: HashMap<PlayerId, TrackArtwork> = HashMap::new();
        let mut position_trackers: HashMap<PlayerId, PositionTracker> = HashMap::new();
        let mut timeline_refreshes: HashMap<PlayerId, TrackSignature> = HashMap::new();
        let emby_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .user_agent("cosmic-widget-applet/0.1")
            .build()
            .ok();
        let mut emby_credentials = Self::discover_emby_credentials();
        let mut last_emby_discovery = Instant::now();

        loop {
            let mut players: Vec<(PlayerId, MediaInfo)> = Vec::new();

            // 1. Query the native Emby Theater session through Emby Server.
            if last_emby_discovery.elapsed() >= Duration::from_secs(30) {
                emby_credentials = Self::discover_emby_credentials().or(emby_credentials);
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
                );
                *emby_control.lock().unwrap() = Some(control);
                players.push((player_id, info));
            } else {
                *emby_control.lock().unwrap() = None;
            }

            // 2. Try Cider API
            let token = cider_token.lock().unwrap().clone();
            let mut has_cider_api_player = false;
            if let Some(mut info) = Self::try_cider_api(token.as_deref()) {
                Self::apply_best_artwork(
                    &PlayerId::Cider,
                    &mut info,
                    &artwork_cache,
                    &mut artwork_by_player,
                );
                players.push((PlayerId::Cider, info));
                has_cider_api_player = true;
            }

            // 3. Enumerate MPRIS players
            if let Some(mpris_players) = Self::get_mpris_players() {
                for bus_name in mpris_players {
                    if has_cider_api_player && Self::is_cider_mpris_player(&bus_name) {
                        continue;
                    }

                    if let Some(mut info) = Self::try_mpris_player(&bus_name) {
                        let player_id = PlayerId::Mpris(bus_name.clone());
                        let track = TrackSignature::from(&info);

                        if Self::is_firefox_mpris_player(&bus_name)
                            && info.status == PlaybackStatus::Playing
                            && info.position == 0
                            && info.duration == 0
                            && timeline_refreshes.get(&player_id) != Some(&track)
                        {
                            timeline_refreshes.insert(player_id.clone(), track);
                            if let Some(refreshed) = Self::refresh_mpris_timeline(&bus_name) {
                                info = refreshed;
                            }
                        }

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

                        players.push((player_id, info));
                    }
                }
            }

            position_trackers.retain(|id, _| players.iter().any(|(player_id, _)| player_id == id));
            artwork_by_player.retain(|id, _| players.iter().any(|(player_id, _)| player_id == id));

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
        let leveldb_dir = dirs::config_dir()?
            .join("Emby Theater")
            .join("Local Storage")
            .join("leveldb");
        let mut files = std::fs::read_dir(leveldb_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("log" | "ldb")
                )
            })
            .collect::<Vec<PathBuf>>();
        files.sort();

        files
            .into_iter()
            .filter_map(|path| Command::new("strings").arg(path).output().ok())
            .filter(|output| output.status.success())
            .filter_map(|output| {
                Self::parse_emby_credentials(&String::from_utf8_lossy(&output.stdout))
            })
            .max_by_key(|credentials| credentials.last_accessed)
    }

    fn parse_emby_credentials(output: &str) -> Option<EmbyCredentials> {
        output
            .lines()
            .filter_map(|line| {
                let json = &line[line.find("{\"Servers\":[")?..];
                let mut deserializer = serde_json::Deserializer::from_str(json);
                SavedEmbyServers::deserialize(&mut deserializer).ok()
            })
            .flat_map(|saved| saved.servers)
            .filter_map(|server| {
                let configured_user_id = server.user_id.as_deref();
                let user = configured_user_id
                    .and_then(|user_id| {
                        server.users.iter().find(|user| user.user_id == user_id)
                    })
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
            Self::emby_api_url(
                server_url,
                &format!("Items/{item_id}/Images/{image_type}")
            ),
            urlencoding::encode(tag),
        ))
    }

    // ========================================================================
    // MPRIS D-Bus Methods
    // ========================================================================

    /// Get list of all MPRIS player bus names.
    fn get_mpris_players() -> Option<Vec<String>> {
        let output = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.ListNames",
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut players = Vec::new();

        for line in stdout.lines() {
            if let Some(start) = line.find("\"org.mpris.MediaPlayer2.") {
                if let Some(end) = line[start + 1..].find('"') {
                    let name = &line[start + 1..start + 1 + end];
                    players.push(name.to_string());
                }
            }
        }

        Some(players)
    }

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

    fn refresh_mpris_timeline(bus_name: &str) -> Option<MediaInfo> {
        log::info!("Refreshing incomplete Firefox MPRIS timeline");
        if !Self::send_mpris_method(bus_name, "Pause") {
            return None;
        }

        std::thread::sleep(Duration::from_millis(200));
        let mut refreshed = Self::try_mpris_player(bus_name);
        let resumed = Self::send_mpris_method(bus_name, "Play");
        if !resumed {
            log::warn!("Failed to resume Firefox after refreshing its MPRIS timeline");
        }
        if resumed && let Some(info) = refreshed.as_mut() {
            info.status = PlaybackStatus::Playing;
        }
        refreshed
    }

    fn send_mpris_method(bus_name: &str, method: &str) -> bool {
        let method = format!("org.mpris.MediaPlayer2.Player.{method}");
        Command::new("dbus-send")
            .args([
                "--session",
                "--type=method_call",
                &format!("--dest={bus_name}"),
                "/org/mpris/MediaPlayer2",
                &method,
            ])
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Query an MPRIS player for its current state.
    fn try_mpris_player(bus_name: &str) -> Option<MediaInfo> {
        let metadata = Self::query_mpris_property(bus_name, "Metadata")?;
        let metadata = metadata.get("data")?;
        let playback_status = Self::query_mpris_property(bus_name, "PlaybackStatus")?;
        let position = Self::query_mpris_property(bus_name, "Position")?;

        // Parse player name from bus name
        let player_name = PlayerId::Mpris(bus_name.to_string()).display_name();

        // Parse playback status
        let status = match playback_status.get("data").and_then(serde_json::Value::as_str) {
            Some("Playing") => PlaybackStatus::Playing,
            Some("Paused") => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        };

        // Parse position (microseconds to milliseconds)
        let position = position
            .get("data")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            / 1000;

        // Parse metadata
        let title = Self::mpris_metadata_string(metadata, "xesam:title").unwrap_or_default();
        let artist = Self::mpris_metadata_array_string(metadata, "xesam:artist").unwrap_or_default();
        let album = Self::mpris_metadata_string(metadata, "xesam:album").unwrap_or_default();
        let duration = Self::mpris_metadata_i64(metadata, "mpris:length").unwrap_or(0) / 1000;

        let media_url = Self::mpris_metadata_string(metadata, "xesam:url");
        let art_url = Self::mpris_metadata_string(metadata, "mpris:artUrl");

        // Skip if no title (nothing playing)
        if title.is_empty() {
            return None;
        }

        Some(MediaInfo {
            player_name,
            title,
            artist,
            album,
            art_url,
            media_url,
            album_art: None,
            status,
            position: position as u64,
            duration: duration as u64,
            can_play: true,
            can_pause: true,
            can_go_next: true,
            can_go_previous: true,
            can_seek: true,
        })
    }

    fn query_mpris_property(bus_name: &str, property: &str) -> Option<serde_json::Value> {
        let output = Command::new("busctl")
            .args([
                "--user",
                "--json=short",
                "get-property",
                bus_name,
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player",
                property,
            ])
            .output()
            .ok()?;

        output
            .status
            .success()
            .then(|| serde_json::from_slice(&output.stdout).ok())
            .flatten()
    }

    fn mpris_metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
        metadata
            .get(key)?
            .get("data")?
            .as_str()
            .map(ToOwned::to_owned)
    }

    fn mpris_metadata_array_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
        metadata
            .get(key)?
            .get("data")?
            .as_array()?
            .first()?
            .as_str()
            .map(ToOwned::to_owned)
    }

    fn mpris_metadata_i64(metadata: &serde_json::Value, key: &str) -> Option<i64> {
        metadata.get(key)?.get("data")?.as_i64()
    }

    fn apply_best_artwork(
        player_id: &PlayerId,
        info: &mut MediaInfo,
        artwork_cache: &Arc<Mutex<ArtworkCache>>,
        artwork_by_player: &mut HashMap<PlayerId, TrackArtwork>,
    ) {
        let track = TrackSignature::from(&*info);
        let selection = artwork_by_player
            .entry(player_id.clone())
            .or_insert_with(|| TrackArtwork::new(track.clone()));
        if selection.track != track {
            *selection = TrackArtwork::new(track);
        }

        let is_youtube = info
            .media_url
            .as_deref()
            .and_then(Self::extract_youtube_video_id)
            .is_some();
        for url in Self::artwork_candidate_urls(info) {
            if !selection.attempted_urls.insert(url.clone()) {
                continue;
            }

            let cached = { artwork_cache.lock().unwrap().get(&url) };
            let candidate = if cached.is_some() {
                cached
            } else {
                Self::download_artwork(&url).inspect(|art| {
                    artwork_cache
                        .lock()
                        .unwrap()
                        .insert(url.clone(), art.clone());
                })
            };

            if let Some(candidate) = candidate {
                let adequate_youtube_thumbnail = candidate.source_width
                    >= MIN_YOUTUBE_THUMBNAIL_WIDTH
                    && candidate.source_height >= MIN_YOUTUBE_THUMBNAIL_HEIGHT;
                selection.accept(candidate);
                if is_youtube && adequate_youtube_thumbnail {
                    break;
                }
            }
        }

        info.album_art = selection.best.clone();
    }

    fn artwork_candidate_urls(info: &MediaInfo) -> Vec<String> {
        let mut candidates = info
            .media_url
            .as_deref()
            .and_then(Self::extract_youtube_video_id)
            .map(|video_id| {
                ["maxresdefault", "hqdefault", "mqdefault"]
                    .into_iter()
                    .map(|variant| {
                        format!("https://i.ytimg.com/vi/{video_id}/{variant}.jpg")
                    })
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
    /// Reads the encoded image and creates a stable Iced handle plus a bounded
    /// compatibility buffer for the legacy Cairo renderer.
    /// Handles both http(s):// and file:// URLs.
    fn download_artwork(url: &str) -> Option<AlbumArt> {
        log::info!("Downloading album art from: {}", url);

        let uri = reqwest::Url::parse(url).ok()?;
        let image_data = match uri.scheme() {
            "file" => {
                let path = uri.to_file_path().ok()?;
                std::fs::read(path).ok()?
            }
            "http" | "https" => {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .user_agent("cosmic-widget-applet/0.1")
                    .build()
                    .ok()?;
                let response = client.get(uri).send().ok()?.error_for_status().ok()?;
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_ARTWORK_BYTES as u64)
                {
                    log::warn!("Artwork response exceeds size limit: {url}");
                    return None;
                }
                response.bytes().ok()?.to_vec()
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

        let image = image::load_from_memory(&image_data).ok()?;
        let source_width = image.width();
        let source_height = image.height();
        if source_width == 0
            || source_height == 0
            || source_width > MAX_ARTWORK_DIMENSION
            || source_height > MAX_ARTWORK_DIMENSION
        {
            log::warn!("Rejected artwork dimensions: {source_width}x{source_height}");
            return None;
        }

        let resized = image.resize(
            LEGACY_ARTWORK_DIMENSION,
            LEGACY_ARTWORK_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        );

        let rgba = resized.to_rgba8();
        let (width, height) = rgba.dimensions();

        let mut bgra_data = Vec::with_capacity((width * height * 4) as usize);
        for pixel in rgba.pixels() {
            let [r, g, b, a] = pixel.0;
            let alpha = a as f32 / 255.0;
            bgra_data.push((b as f32 * alpha) as u8);
            bgra_data.push((g as f32 * alpha) as u8);
            bgra_data.push((r as f32 * alpha) as u8);
            bgra_data.push(a);
        }

        log::info!("Album art loaded: {source_width}x{source_height}");

        Some(AlbumArt {
            data: bgra_data.into(),
            width,
            height,
            source_width,
            source_height,
            iced_handle: cosmic::iced::widget::image::Handle::from_bytes(image_data),
        })
    }

    /// Query Cider API for current track info.
    ///
    /// Uses `curl` for HTTP requests to avoid pulling in reqwest for
    /// a simple local API call.
    ///
    /// # Returns
    ///
    /// `Some(MediaInfo)` if Cider is running and playing
    /// `None` if Cider is not running or no track is loaded
    fn try_cider_api(token: Option<&str>) -> Option<MediaInfo> {
        use std::process::Command;

        // Build curl command for now-playing endpoint
        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "--max-time", "1"]); // Silent, 1 second timeout

        // Add authentication header if token provided
        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }

        cmd.arg("http://localhost:10767/api/v1/playback/now-playing");

        let output = cmd.output().ok()?;

        if !output.status.success() {
            return None;
        }

        let json_str = String::from_utf8_lossy(&output.stdout);

        // Check for error response
        if json_str.contains("\"error\"") {
            return None;
        }

        // Also query the is-playing endpoint for accurate playback status
        let is_playing = Self::check_is_playing(token);

        // Parse JSON response
        Self::parse_cider_response(&json_str, is_playing)
    }

    /// Check if media is currently playing via is-playing endpoint.
    fn check_is_playing(token: Option<&str>) -> bool {
        use std::process::Command;

        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "--max-time", "1"]);

        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }

        cmd.arg("http://localhost:10767/api/v1/playback/is-playing");

        if let Ok(output) = cmd.output() {
            if output.status.success() {
                let json_str = String::from_utf8_lossy(&output.stdout);
                return json_str.contains("\"is_playing\":true");
            }
        }

        // Default to true if we can't determine (optimistic)
        true
    }

    /// Parse Cider API JSON response into MediaInfo.
    ///
    /// Uses simple string parsing to avoid JSON dependency overhead.
    /// Extracts: name, artistName, albumName, artwork.url, durationInMillis,
    /// currentPlaybackTime.
    fn parse_cider_response(json: &str, is_playing: bool) -> Option<MediaInfo> {
        // Check if status is ok
        if !json.contains("\"status\":\"ok\"") {
            return None;
        }

        // Determine playback status from is_playing parameter
        let playback_status = if is_playing {
            PlaybackStatus::Playing
        } else {
            PlaybackStatus::Paused
        };

        let mut info = MediaInfo {
            player_name: "Cider".to_string(),
            can_play: true,
            can_pause: true,
            can_go_next: true,
            can_go_previous: true,
            can_seek: true,
            status: playback_status,
            ..Default::default()
        };

        // Extract title (name field in Cider API)
        if let Some(name) = Self::extract_json_string(json, "\"name\":\"") {
            info.title = name;
        }

        // Extract artist
        if let Some(artist) = Self::extract_json_string(json, "\"artistName\":\"") {
            info.artist = artist;
        }

        // Extract album
        if let Some(album) = Self::extract_json_string(json, "\"albumName\":\"") {
            info.album = album;
        }

        // Extract artwork URL from within the artwork object
        // The response has: "artwork":{"width":...,"height":...,"url":"https://..."}
        if let Some(artwork_start) = json.find("\"artwork\":{") {
            let artwork_section = &json[artwork_start..];
            // Find url within the artwork object
            if let Some(url) = Self::extract_json_string(artwork_section, "\"url\":\"") {
                // Replace {w}x{h} placeholders with actual size
                let artwork_url = url.replace("{w}", "300").replace("{h}", "300");
                info.art_url = Some(artwork_url);
            }
        }

        // Extract duration in milliseconds
        if let Some(duration_str) = Self::extract_json_number(json, "\"durationInMillis\":") {
            if let Ok(duration) = duration_str.parse::<u64>() {
                info.duration = duration;
            }
        }

        // Extract current playback time (seconds → milliseconds)
        if let Some(pos_str) = Self::extract_json_number(json, "\"currentPlaybackTime\":") {
            if let Ok(pos) = pos_str.parse::<f64>() {
                info.position = (pos * 1000.0) as u64;
            }
        }

        // Check if we got meaningful data
        if info.title.is_empty() {
            return None;
        }

        Some(info)
    }

    /// Extract a string value from JSON by key.
    ///
    /// Simple parsing: finds key, then extracts until next quote.
    fn extract_json_string(json: &str, key: &str) -> Option<String> {
        let start = json.find(key)? + key.len();
        let rest = &json[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    /// Extract a numeric value from JSON by key.
    ///
    /// Simple parsing: finds key, then extracts until delimiter.
    fn extract_json_number(json: &str, key: &str) -> Option<String> {
        let start = json.find(key)? + key.len();
        let rest = &json[start..];
        let end = rest.find(|c: char| c == ',' || c == '}' || c == ']')?;
        Some(rest[..end].trim().to_string())
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
        command: &str,
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
        let command = command.to_string();
        std::thread::spawn(move || {
            let Ok(client) = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(3))
                .user_agent("cosmic-widget-applet/0.1")
                .build()
            else {
                return;
            };
            let url = Self::emby_api_url(
                &control.server_url,
                &format!("Sessions/{}/Playing/{command}", control.session_id),
            );
            let mut request = client
                .post(url)
                .header("X-Emby-Token", control.access_token);
            if let Some((key, value)) = query {
                request = request.query(&[(key, value)]);
            }
            if let Err(error) = request.send().and_then(reqwest::blocking::Response::error_for_status)
            {
                log::warn!("Emby {command} command failed: {error}");
            }
        });
        true
    }

    fn send_cider_command(&self, endpoint: &str) -> bool {
        let token = self.cider_token.lock().unwrap().clone();

        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "-X", "POST", "--max-time", "1"]);

        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }

        cmd.arg(&format!(
            "http://localhost:10767/api/v1/playback/{}",
            endpoint
        ));

        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn cider_play_pause(&self) {
        // State is already toggled by play_pause() caller
        // Just send command in background to avoid blocking
        self.send_cider_command_async("playpause");
    }

    fn send_cider_command_async(&self, endpoint: &str) {
        let token = self.cider_token.lock().unwrap().clone();
        let url = format!("http://localhost:10767/api/v1/playback/{}", endpoint);

        std::thread::spawn(move || {
            let mut cmd = Command::new("curl");
            cmd.args(&["-s", "-X", "POST", "--max-time", "1"]);

            if let Some(t) = token {
                cmd.args(&["-H", &format!("apptoken: {}", t)]);
            }

            cmd.arg(&url);
            let _ = cmd.output();
        });
    }

    fn cider_next(&self) {
        self.send_cider_command("next");
    }

    fn cider_previous(&self) {
        self.send_cider_command("previous");
    }

    fn cider_seek(&self, position_seconds: f64) -> bool {
        let token = self.cider_token.lock().unwrap().clone();

        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "-X", "POST", "--max-time", "1"]);
        cmd.args(&["-H", "Content-Type: application/json"]);

        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }

        cmd.args(&[
            "-d",
            &format!("{{\"position\": {}}}", position_seconds as u64),
        ]);
        cmd.arg("http://localhost:10767/api/v1/playback/seek");

        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    // ========================================================================
    // MPRIS Control Methods
    // ========================================================================

    fn mpris_play_pause(&self, bus_name: &str) {
        log::info!("Sending PlayPause to MPRIS player: {}", bus_name);
        let result = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player.PlayPause",
            ])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    log::info!("PlayPause command succeeded for {}", bus_name);
                } else {
                    log::error!(
                        "PlayPause command failed for {}: {:?}",
                        bus_name,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            Err(e) => {
                log::error!("Failed to execute dbus-send for PlayPause: {}", e);
            }
        }
    }

    fn mpris_next(&self, bus_name: &str) {
        let _ = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player.Next",
            ])
            .output();
    }

    fn mpris_previous(&self, bus_name: &str) {
        let _ = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player.Previous",
            ])
            .output();
    }

    fn mpris_seek(&self, bus_name: &str, position_us: u64) -> bool {
        let current_pos = Self::query_mpris_property(bus_name, "Position")
            .and_then(|position| position.get("data").and_then(serde_json::Value::as_i64))
            .unwrap_or(0);

        let offset = position_us as i64 - current_pos;

        Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player.Seek",
                &format!("int64:{}", offset),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AlbumArt, HashMap, Instant, MediaInfo, MediaMonitor, MultiPlayerState, PlaybackStatus,
        PlayerId, PositionTracker, TrackArtwork, TrackSignature, preferred_player_id,
        update_tracked_position,
    };
    use std::sync::Arc;
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

    fn artwork(source_width: u32, source_height: u32) -> AlbumArt {
        AlbumArt {
            data: Arc::from(vec![0, 0, 0, 255]),
            width: 1,
            height: 1,
            source_width,
            source_height,
            iced_handle: cosmic::iced::widget::image::Handle::from_rgba(
                1,
                1,
                vec![0, 0, 0, 255],
            ),
        }
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
    fn preserves_quotes_in_structured_mpris_metadata() {
        let metadata = serde_json::json!({
            "xesam:title": {
                "type": "s",
                "data": "Intel: \"Ya, we're cooked\""
            },
            "xesam:artist": {
                "type": "as",
                "data": ["TechLinked"]
            },
            "mpris:length": {
                "type": "x",
                "data": 559_000_000_i64
            }
        });

        assert_eq!(
            MediaMonitor::mpris_metadata_string(&metadata, "xesam:title").as_deref(),
            Some("Intel: \"Ya, we're cooked\"")
        );
        assert_eq!(
            MediaMonitor::mpris_metadata_array_string(&metadata, "xesam:artist").as_deref(),
            Some("TechLinked")
        );
        assert_eq!(
            MediaMonitor::mpris_metadata_i64(&metadata, "mpris:length"),
            Some(559_000_000)
        );
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
        selection.accept(artwork(1280, 720));
        selection.accept(artwork(60, 60));

        let selected = selection.best.expect("artwork should be selected");
        assert_eq!((selected.source_width, selected.source_height), (1280, 720));
    }

    #[test]
    fn selects_the_newest_saved_emby_credentials() {
        let leveldb_strings = r#"
noise
{"Servers":[{"LocalAddress":"http://old:8096/","RemoteAddress":null,"ManualAddress":null,"UserId":"user-1","Users":[{"UserId":"user-1","AccessToken":"old-token"}],"DateLastAccessed":10}]}
prefix:{"Servers":[{"LocalAddress":"http://nas:8096","RemoteAddress":"https://remote.example","ManualAddress":"http://nas:8096","UserId":"user-2","Users":[{"UserId":"user-2","AccessToken":"new-token"}],"DateLastAccessed":20}]}:suffix
"#;

        let credentials = MediaMonitor::parse_emby_credentials(leveldb_strings)
            .expect("credentials should be parsed");
        assert_eq!(credentials.user_id, "user-2");
        assert_eq!(credentials.access_token, "new-token");
        assert_eq!(
            credentials.server_urls,
            vec!["http://nas:8096", "https://remote.example"]
        );
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

        let (player_id, info) =
            MediaMonitor::media_info_from_emby_session("http://nas:8096", &session)
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
        assert!(session
            .play_state
            .is_some_and(|state| state.position_ticks.is_none()));
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
}
