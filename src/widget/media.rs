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

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::collections::HashMap;
use std::process::Command;

// ============================================================================
// Album Art Cache
// ============================================================================

/// Decoded album art ready for rendering.
///
/// Stores RGBA pixel data along with dimensions for Cairo rendering.
#[derive(Clone)]
pub struct AlbumArt {
    /// RGBA pixel data (4 bytes per pixel)
    pub data: Vec<u8>,
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
}

impl std::fmt::Debug for AlbumArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlbumArt")
            .field("width", &self.width)
            .field("height", &self.height)
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
    /// MPRIS D-Bus player with bus name
    Mpris(String),
}

impl PlayerId {
    /// Get display name for the player.
    pub fn display_name(&self) -> String {
        match self {
            PlayerId::Cider => "Cider".to_string(),
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
pub struct MediaMonitor {
    /// All players' state
    player_state: Arc<Mutex<MultiPlayerState>>,
    /// Cider API token for authentication (optional)
    cider_token: Arc<Mutex<Option<String>>>,
    /// Cache for downloaded album artwork
    artwork_cache: Arc<Mutex<ArtworkCache>>,
    /// Currently selected player ID (persists across updates)
    selected_player: Arc<Mutex<Option<PlayerId>>>,
}

impl MediaMonitor {
    /// Create a new media monitor with optional Cider API token.
    pub fn new(api_token: Option<String>) -> Self {
        let player_state = Arc::new(Mutex::new(MultiPlayerState::default()));
        let token = api_token.filter(|t| !t.is_empty());
        let cider_token = Arc::new(Mutex::new(token));
        let artwork_cache = Arc::new(Mutex::new(ArtworkCache::new(20)));
        let selected_player = Arc::new(Mutex::new(None));
        
        // Spawn background thread to monitor all players
        let state_clone = Arc::clone(&player_state);
        let token_clone = Arc::clone(&cider_token);
        let cache_clone = Arc::clone(&artwork_cache);
        let selected_clone = Arc::clone(&selected_player);
        
        std::thread::spawn(move || {
            Self::monitor_loop(state_clone, token_clone, cache_clone, selected_clone);
        });
        
        Self {
            player_state,
            cider_token,
            artwork_cache,
            selected_player,
        }
    }
    
    /// Main background monitoring loop.
    fn monitor_loop(
        player_state: Arc<Mutex<MultiPlayerState>>,
        cider_token: Arc<Mutex<Option<String>>>,
        artwork_cache: Arc<Mutex<ArtworkCache>>,
        selected_player: Arc<Mutex<Option<PlayerId>>>,
    ) {
        log::info!("Starting multi-player media monitor");
        let mut last_art_urls: HashMap<PlayerId, String> = HashMap::new();
        // Track last known position when playing, to work around Firefox MPRIS bug
        // where position keeps incrementing even when paused
        let mut paused_positions: HashMap<PlayerId, u64> = HashMap::new();
        let mut last_status: HashMap<PlayerId, PlaybackStatus> = HashMap::new();
        
        loop {
            let mut players: Vec<(PlayerId, MediaInfo)> = Vec::new();
            
            // 1. Try Cider API
            let token = cider_token.lock().unwrap().clone();
            if let Some(mut info) = Self::try_cider_api(token.as_deref()) {
                // Load artwork if needed
                if let Some(ref url) = info.art_url {
                    let needs_load = last_art_urls.get(&PlayerId::Cider) != Some(url);
                    if needs_load {
                        last_art_urls.insert(PlayerId::Cider, url.clone());
                        let cached = artwork_cache.lock().unwrap().get(url);
                        if let Some(art) = cached {
                            info.album_art = Some(art);
                        } else if let Some(art) = Self::download_artwork(url) {
                            artwork_cache.lock().unwrap().insert(url.clone(), art.clone());
                            info.album_art = Some(art);
                        }
                    } else {
                        info.album_art = artwork_cache.lock().unwrap().get(url);
                    }
                }
                players.push((PlayerId::Cider, info));
            }
            
            // 2. Enumerate MPRIS players
            if let Some(mpris_players) = Self::get_mpris_players() {
                for bus_name in mpris_players {
                    if let Some(mut info) = Self::try_mpris_player(&bus_name) {
                        let player_id = PlayerId::Mpris(bus_name.clone());
                        
                        // Workaround for Firefox MPRIS bug: position keeps incrementing when paused
                        // Track when player transitions from Playing to Paused and freeze position
                        let prev_status = last_status.get(&player_id).cloned();
                        if info.status == PlaybackStatus::Playing {
                            // Playing: update our cached position and clear any frozen position
                            paused_positions.remove(&player_id);
                        } else if info.status == PlaybackStatus::Paused {
                            // Just transitioned to Paused? Save the current position
                            if prev_status == Some(PlaybackStatus::Playing) {
                                paused_positions.insert(player_id.clone(), info.position);
                            }
                            // Use the frozen position if we have one
                            if let Some(&frozen_pos) = paused_positions.get(&player_id) {
                                info.position = frozen_pos;
                            }
                        } else {
                            // Stopped: clear cached position
                            paused_positions.remove(&player_id);
                        }
                        last_status.insert(player_id.clone(), info.status.clone());
                        
                        // Load artwork if available
                        if let Some(ref url) = info.art_url {
                            let needs_load = last_art_urls.get(&player_id) != Some(url);
                            if needs_load {
                                last_art_urls.insert(player_id.clone(), url.clone());
                                let cached = artwork_cache.lock().unwrap().get(url);
                                if let Some(art) = cached {
                                    info.album_art = Some(art);
                                } else if let Some(art) = Self::download_artwork(url) {
                                    artwork_cache.lock().unwrap().insert(url.clone(), art.clone());
                                    info.album_art = Some(art);
                                }
                            } else {
                                info.album_art = artwork_cache.lock().unwrap().get(url);
                            }
                        }
                        
                        // Fallback to app icon if no album art
                        if info.album_art.is_none() {
                            let icon_cache_key = format!("__icon__{}", bus_name);
                            let cached = artwork_cache.lock().unwrap().get(&icon_cache_key);
                            if let Some(art) = cached {
                                info.album_art = Some(art);
                            } else if let Some(art) = Self::load_app_icon(&bus_name) {
                                artwork_cache.lock().unwrap().insert(icon_cache_key, art.clone());
                                info.album_art = Some(art);
                            }
                        }
                        
                        players.push((player_id, info));
                    }
                }
            }
            
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
                let selected = selected_player.lock().unwrap().clone();
                
                // Find index of previously selected player
                let new_index = if let Some(ref sel_id) = selected {
                    players.iter().position(|(id, _)| id == sel_id).unwrap_or(0)
                } else {
                    0
                };
                
                state.players = players;
                state.current_index = new_index.min(state.players.len().saturating_sub(1));
            }
            
            std::thread::sleep(Duration::from_secs(1));
        }
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
    
    /// Query an MPRIS player for its current state.
    fn try_mpris_player(bus_name: &str) -> Option<MediaInfo> {
        // Get metadata
        let metadata_output = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.freedesktop.DBus.Properties.Get",
                "string:org.mpris.MediaPlayer2.Player",
                "string:Metadata",
            ])
            .output()
            .ok()?;
        
        // Get playback status
        let status_output = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.freedesktop.DBus.Properties.Get",
                "string:org.mpris.MediaPlayer2.Player",
                "string:PlaybackStatus",
            ])
            .output()
            .ok()?;
        
        // Get position
        let position_output = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.freedesktop.DBus.Properties.Get",
                "string:org.mpris.MediaPlayer2.Player",
                "string:Position",
            ])
            .output()
            .ok()?;
        
        let metadata_str = String::from_utf8_lossy(&metadata_output.stdout);
        let status_str = String::from_utf8_lossy(&status_output.stdout);
        let position_str = String::from_utf8_lossy(&position_output.stdout);
        
        // Parse player name from bus name
        let player_name = PlayerId::Mpris(bus_name.to_string()).display_name();
        
        // Parse playback status
        let status = if status_str.contains("\"Playing\"") {
            PlaybackStatus::Playing
        } else if status_str.contains("\"Paused\"") {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Stopped
        };
        
        // Parse position (microseconds to milliseconds)
        let position = Self::extract_dbus_int64(&position_str).unwrap_or(0) / 1000;
        
        // Parse metadata
        let title = Self::extract_dbus_metadata_string(&metadata_str, "xesam:title")
            .unwrap_or_default();
        let artist = Self::extract_dbus_metadata_array_string(&metadata_str, "xesam:artist")
            .unwrap_or_default();
        let album = Self::extract_dbus_metadata_string(&metadata_str, "xesam:album")
            .unwrap_or_default();
        let duration = Self::extract_dbus_metadata_int64(&metadata_str, "mpris:length")
            .unwrap_or(0) / 1000; // microseconds to milliseconds
        
        // Try to get artwork URL - first from mpris:artUrl, then try to extract from xesam:url
        let art_url = Self::extract_dbus_metadata_string(&metadata_str, "mpris:artUrl")
            .or_else(|| {
                // Try to extract thumbnail from webpage URL (YouTube, etc.)
                let page_url = Self::extract_dbus_metadata_string(&metadata_str, "xesam:url")?;
                Self::extract_thumbnail_from_url(&page_url)
            });
        
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
    
    /// Extract string from D-Bus metadata by key.
    fn extract_dbus_metadata_string(output: &str, key: &str) -> Option<String> {
        let key_pattern = format!("\"{}\"", key);
        let key_pos = output.find(&key_pattern)?;
        let after_key = &output[key_pos + key_pattern.len()..];
        
        if let Some(variant_pos) = after_key.find("variant") {
            let after_variant = &after_key[variant_pos..];
            if let Some(string_pos) = after_variant.find("string \"") {
                let start = string_pos + 8;
                let rest = &after_variant[start..];
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
        None
    }
    
    /// Extract first string from D-Bus metadata array by key.
    fn extract_dbus_metadata_array_string(output: &str, key: &str) -> Option<String> {
        let key_pattern = format!("\"{}\"", key);
        let key_pos = output.find(&key_pattern)?;
        let after_key = &output[key_pos + key_pattern.len()..];
        
        if let Some(array_pos) = after_key.find("array [") {
            let after_array = &after_key[array_pos..];
            if let Some(string_pos) = after_array.find("string \"") {
                let start = string_pos + 8;
                let rest = &after_array[start..];
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
        None
    }
    
    /// Extract int64 from D-Bus metadata by key.
    fn extract_dbus_metadata_int64(output: &str, key: &str) -> Option<i64> {
        let key_pattern = format!("\"{}\"", key);
        let key_pos = output.find(&key_pattern)?;
        let after_key = &output[key_pos + key_pattern.len()..];
        
        if let Some(variant_pos) = after_key.find("variant") {
            let after_variant = &after_key[variant_pos..];
            if let Some(int_pos) = after_variant.find("int64 ") {
                let start = int_pos + 6;
                let rest = &after_variant[start..];
                let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
                return rest[..end].trim().parse().ok();
            }
        }
        None
    }
    
    /// Extract int64 from D-Bus property response.
    fn extract_dbus_int64(output: &str) -> Option<i64> {
        if let Some(int_pos) = output.find("int64 ") {
            let start = int_pos + 6;
            let rest = &output[start..];
            let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
            return rest[..end].trim().parse().ok();
        }
        None
    }
    
    /// Extract thumbnail URL from a webpage URL (e.g., YouTube video ID -> thumbnail).
    ///
    /// Supports:
    /// - YouTube: youtube.com/watch?v=ID, youtu.be/ID, youtube.com/embed/ID
    /// - Vimeo: vimeo.com/ID (requires API call, returns None for now)
    /// - Bandcamp: Could potentially scrape, but complex
    fn extract_thumbnail_from_url(url: &str) -> Option<String> {
        // YouTube patterns
        if url.contains("youtube.com") || url.contains("youtu.be") {
            if let Some(video_id) = Self::extract_youtube_video_id(url) {
                // Use maxresdefault for best quality, falls back gracefully
                return Some(format!("https://img.youtube.com/vi/{}/maxresdefault.jpg", video_id));
            }
        }
        
        // Add more services here as needed
        // Vimeo, Spotify (web player), SoundCloud, etc.
        
        None
    }
    
    /// Extract YouTube video ID from various URL formats.
    fn extract_youtube_video_id(url: &str) -> Option<String> {
        // youtube.com/watch?v=VIDEO_ID
        if let Some(pos) = url.find("v=") {
            let start = pos + 2;
            let rest = &url[start..];
            let end = rest.find(|c: char| c == '&' || c == '#' || c == '?' || c == '/').unwrap_or(rest.len());
            let id = &rest[..end];
            if !id.is_empty() && id.len() == 11 {
                return Some(id.to_string());
            }
        }
        
        // youtu.be/VIDEO_ID
        if url.contains("youtu.be/") {
            if let Some(pos) = url.find("youtu.be/") {
                let start = pos + 9;
                let rest = &url[start..];
                let end = rest.find(|c: char| c == '&' || c == '#' || c == '?' || c == '/').unwrap_or(rest.len());
                let id = &rest[..end];
                if !id.is_empty() && id.len() == 11 {
                    return Some(id.to_string());
                }
            }
        }
        
        // youtube.com/embed/VIDEO_ID
        if url.contains("/embed/") {
            if let Some(pos) = url.find("/embed/") {
                let start = pos + 7;
                let rest = &url[start..];
                let end = rest.find(|c: char| c == '&' || c == '#' || c == '?' || c == '/').unwrap_or(rest.len());
                let id = &rest[..end];
                if !id.is_empty() && id.len() == 11 {
                    return Some(id.to_string());
                }
            }
        }
        
        None
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
            &format!("{}/.local/share/icons/hicolor/256x256/apps", std::env::var("HOME").unwrap_or_default()),
            &format!("{}/.local/share/icons/hicolor/128x128/apps", std::env::var("HOME").unwrap_or_default()),
            &format!("{}/.local/share/icons/hicolor/64x64/apps", std::env::var("HOME").unwrap_or_default()),
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
            "firefox" => &["firefox", "firefox-esr", "org.mozilla.firefox", "firefox-developer-edition"],
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
        let img = image::load_from_memory(&image_data).ok()?;
        
        // Resize to target size
        let target_size = 64u32;
        let resized = img.resize(target_size, target_size, image::imageops::FilterType::Lanczos3);
        
        // Convert to RGBA
        let rgba = resized.to_rgba8();
        let (width, height) = rgba.dimensions();
        
        // Cairo expects BGRA with pre-multiplied alpha
        let mut bgra_data = Vec::with_capacity((width * height * 4) as usize);
        for pixel in rgba.pixels() {
            let [r, g, b, a] = pixel.0;
            let alpha = a as f32 / 255.0;
            bgra_data.push((b as f32 * alpha) as u8);
            bgra_data.push((g as f32 * alpha) as u8);
            bgra_data.push((r as f32 * alpha) as u8);
            bgra_data.push(a);
        }
        
        Some(AlbumArt {
            data: bgra_data,
            width,
            height,
        })
    }
    
    /// Download and decode album artwork from URL.
    ///
    /// Downloads the image using curl, then decodes it using the `image` crate.
    /// Resizes to a reasonable size for the widget display.
    /// Handles both http(s):// and file:// URLs.
    fn download_artwork(url: &str) -> Option<AlbumArt> {
        use image::GenericImageView;
        
        log::info!("Downloading album art from: {}", url);
        
        // Handle file:// URLs differently
        let image_data = if url.starts_with("file://") {
            let path = url.strip_prefix("file://")?;
            std::fs::read(path).ok()?
        } else {
            let output = Command::new("curl")
                .args(&["-s", "--max-time", "5", "-L"])
                .arg(url)
                .output()
                .ok()?;
            
            if !output.status.success() || output.stdout.is_empty() {
                log::warn!("Failed to download album art");
                return None;
            }
            output.stdout
        };
        
        // Decode image
        let img = image::load_from_memory(&image_data).ok()?;
        
        // Resize to target size (e.g., 64x64 for widget display)
        let target_size = 64u32;
        let resized = img.resize(target_size, target_size, image::imageops::FilterType::Lanczos3);
        
        // Convert to RGBA
        let rgba = resized.to_rgba8();
        let (width, height) = rgba.dimensions();
        
        // Cairo expects BGRA with pre-multiplied alpha (ARGB32 format)
        // Convert RGBA to BGRA
        let mut bgra_data: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
        for pixel in rgba.pixels() {
            let [r, g, b, a] = pixel.0;
            // Pre-multiply alpha and swap to BGRA
            let alpha = a as f32 / 255.0;
            bgra_data.push((b as f32 * alpha) as u8); // B
            bgra_data.push((g as f32 * alpha) as u8); // G
            bgra_data.push((r as f32 * alpha) as u8); // R
            bgra_data.push(a);                         // A
        }
        
        log::info!("Album art loaded: {}x{}", width, height);
        
        Some(AlbumArt {
            data: bgra_data,
            width,
            height,
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
        cmd.args(&["-s", "--max-time", "1"]);  // Silent, 1 second timeout
        
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
                let artwork_url = url
                    .replace("{w}", "300")
                    .replace("{h}", "300");
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
        state.current_player()
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
            drop(state);
            
            log::info!("play_pause called for player: {:?}", player_id);
            match &player_id {
                PlayerId::Cider => self.cider_play_pause(),
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
                PlayerId::Mpris(bus_name) => self.mpris_previous(bus_name),
            }
        }
    }
    
    /// Seek to position based on progress (0.0 to 1.0).
    pub fn seek_to_progress(&self, progress: f64) -> bool {
        let state = self.player_state.lock().unwrap();
        if let Some((player_id, info)) = state.current_player() {
            let player_id = player_id.clone();
            let duration = info.duration;
            drop(state);
            
            let target_ms = (duration as f64 * progress.clamp(0.0, 1.0)) as u64;
            
            match &player_id {
                PlayerId::Cider => self.cider_seek(target_ms as f64 / 1000.0),
                PlayerId::Mpris(bus_name) => self.mpris_seek(bus_name, target_ms * 1000),
            }
        } else {
            false
        }
    }
    
    // ========================================================================
    // Cider Control Methods
    // ========================================================================
    
    fn send_cider_command(&self, endpoint: &str) -> bool {
        let token = self.cider_token.lock().unwrap().clone();
        
        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "-X", "POST", "--max-time", "1"]);
        
        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }
        
        cmd.arg(&format!("http://localhost:10767/api/v1/playback/{}", endpoint));
        
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
        
        cmd.args(&["-d", &format!("{{\"position\": {}}}", position_seconds as u64)]);
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
                    log::error!("PlayPause command failed for {}: {:?}", bus_name, String::from_utf8_lossy(&output.stderr));
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
        // Get current position first
        let output = Command::new("dbus-send")
            .args(&[
                "--session",
                "--print-reply",
                &format!("--dest={}", bus_name),
                "/org/mpris/MediaPlayer2",
                "org.freedesktop.DBus.Properties.Get",
                "string:org.mpris.MediaPlayer2.Player",
                "string:Position",
            ])
            .output()
            .ok();
        
        let current_pos = output
            .and_then(|o| Self::extract_dbus_int64(&String::from_utf8_lossy(&o.stdout)))
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
