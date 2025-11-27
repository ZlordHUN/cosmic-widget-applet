// SPDX-License-Identifier: MPL-2.0

//! # Media Player Monitoring Module
//!
//! This module monitors and controls media playback via the Cider API.
//! Cider is an Apple Music client that exposes a REST API for control.
//!
//! ## API Integration
//!
//! Connects to Cider's local API at `http://localhost:10767/api/v1/`:
//! - `playback/now-playing` - Get current track info
//! - `playback/is-playing` - Check if playing
//! - `playback/playpause` - Toggle play/pause
//! - `playback/next` - Skip to next track
//! - `playback/previous` - Go to previous track
//! - `playback/seek` - Seek to position
//!
//! ## Authentication
//!
//! Some Cider installations require an API token. If provided in settings,
//! it's sent as an `apptoken` HTTP header.
//!
//! ## Polling Architecture
//!
//! A background thread polls the API every second to get current track
//! info. This provides real-time progress updates for the progress bar.
//!
//! ## Album Art
//!
//! Album artwork is downloaded from Apple Music CDN and cached in memory.
//! Images are decoded to RGBA format for Cairo rendering. The cache is
//! keyed by URL to avoid re-downloading when tracks repeat.
//!
//! ## Error Handling
//!
//! - Cider not running → Empty MediaInfo (no media section displayed)
//! - API errors → Silent fallback to empty state
//! - Network timeout → 1 second limit to prevent blocking
//! - Artwork errors → Falls back to no artwork display

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::collections::HashMap;

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
// Media Monitor Struct
// ============================================================================

/// Monitors media playback and provides control via Cider API.
///
/// Spawns a background thread that polls the Cider API every second
/// to get current track information. Provides methods for playback
/// control (play/pause, next, previous).
///
/// # Thread Safety
///
/// - `media_info`: Shared state for current track (Arc<Mutex>)
/// - `cider_token`: Shared API token, can be updated from settings
/// - `artwork_cache`: Shared cache for decoded album artwork
pub struct MediaMonitor {
    /// Current media info, updated by background thread
    media_info: Arc<Mutex<MediaInfo>>,
    /// Cider API token for authentication (optional)
    cider_token: Arc<Mutex<Option<String>>>,
    /// Cache for downloaded album artwork
    artwork_cache: Arc<Mutex<ArtworkCache>>,
}

impl MediaMonitor {
    /// Create a new media monitor with optional API token.
    ///
    /// # Arguments
    ///
    /// * `api_token` - Optional Cider API token for authenticated endpoints
    ///
    /// # Background Thread
    ///
    /// Immediately spawns a background thread that:
    /// 1. Polls `now-playing` endpoint every second
    /// 2. Checks `is-playing` for accurate status
    /// 3. Downloads and caches album artwork
    /// 4. Updates shared MediaInfo state
    pub fn new(api_token: Option<String>) -> Self {
        let media_info = Arc::new(Mutex::new(MediaInfo::default()));
        // Use provided token or None if empty
        let token = api_token.filter(|t| !t.is_empty());
        let cider_token = Arc::new(Mutex::new(token));
        let artwork_cache = Arc::new(Mutex::new(ArtworkCache::new(10)));
        
        // Spawn background thread to monitor media
        let media_info_clone = Arc::clone(&media_info);
        let cider_token_clone = Arc::clone(&cider_token);
        let artwork_cache_clone = Arc::clone(&artwork_cache);
        
        std::thread::spawn(move || {
            Self::monitor_loop(media_info_clone, cider_token_clone, artwork_cache_clone);
        });
        
        Self {
            media_info,
            cider_token,
            artwork_cache,
        }
    }
    
    /// Main background monitoring loop.
    ///
    /// Runs forever, polling the Cider API every second and updating
    /// the shared MediaInfo state. Also downloads artwork when track changes.
    fn monitor_loop(
        media_info: Arc<Mutex<MediaInfo>>,
        cider_token: Arc<Mutex<Option<String>>>,
        artwork_cache: Arc<Mutex<ArtworkCache>>,
    ) {
        log::info!("Starting Cider media monitor");
        let mut last_art_url: Option<String> = None;
        
        loop {
            // Try Cider API with current token
            let token = cider_token.lock().unwrap().clone();
            if let Some(mut info) = Self::try_cider_api(token.as_deref()) {
                // Check if we need to load new artwork
                if info.art_url.is_some() && info.art_url != last_art_url {
                    let url = info.art_url.clone().unwrap();
                    last_art_url = info.art_url.clone();
                    
                    // Check cache first
                    let cached = artwork_cache.lock().unwrap().get(&url);
                    if let Some(art) = cached {
                        info.album_art = Some(art);
                    } else {
                        // Download and cache artwork
                        if let Some(art) = Self::download_artwork(&url) {
                            artwork_cache.lock().unwrap().insert(url, art.clone());
                            info.album_art = Some(art);
                        }
                    }
                } else if info.art_url.is_some() {
                    // Same URL, use cached
                    let url = info.art_url.as_ref().unwrap();
                    let cached = artwork_cache.lock().unwrap().get(url);
                    info.album_art = cached;
                }
                
                let mut stored = media_info.lock().unwrap();
                *stored = info;
            } else {
                // No media playing or Cider not running
                last_art_url = None;
                let mut stored = media_info.lock().unwrap();
                *stored = MediaInfo::default();
            }
            
            // Poll every second for responsive progress updates
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    
    /// Download and decode album artwork from URL.
    ///
    /// Downloads the image using curl, then decodes it using the `image` crate.
    /// Resizes to a reasonable size for the widget display.
    ///
    /// # Arguments
    ///
    /// * `url` - URL to download artwork from (typically Apple Music CDN)
    ///
    /// # Returns
    ///
    /// Decoded `AlbumArt` if successful, `None` on any error
    fn download_artwork(url: &str) -> Option<AlbumArt> {
        use std::process::Command;
        use image::GenericImageView;
        
        log::info!("Downloading album art from: {}", url);
        
        // Download image data using curl
        let output = Command::new("curl")
            .args(&["-s", "--max-time", "5", "-L"])  // Silent, 5 second timeout, follow redirects
            .arg(url)
            .output()
            .ok()?;
        
        if !output.status.success() || output.stdout.is_empty() {
            log::warn!("Failed to download album art");
            return None;
        }
        
        // Decode image
        let img = image::load_from_memory(&output.stdout).ok()?;
        
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
    
    /// Get current media info snapshot.
    pub fn get_media_info(&self) -> MediaInfo {
        self.media_info.lock().unwrap().clone()
    }
    
    /// Update the Cider API token.
    ///
    /// Called when user changes the token in settings.
    pub fn set_cider_token(&self, token: Option<String>) {
        let mut stored = self.cider_token.lock().unwrap();
        *stored = token;
        log::info!("Cider API token updated");
    }
    
    // ========================================================================
    // Playback Control Methods
    // ========================================================================
    
    /// Send a playback control command to Cider.
    ///
    /// Uses POST request to the specified endpoint.
    fn send_cider_command(&self, endpoint: &str) -> bool {
        use std::process::Command;
        
        let token = self.cider_token.lock().unwrap().clone();
        
        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "-X", "POST", "--max-time", "1"]);
        
        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }
        
        cmd.arg(&format!("http://localhost:10767/api/v1/playback/{}", endpoint));
        
        if let Ok(output) = cmd.output() {
            return output.status.success();
        }
        false
    }
    
    /// Toggle play/pause state.
    ///
    /// Sends `playpause` command and immediately updates local state
    /// for responsive UI (before next poll confirms change).
    pub fn play_pause(&self) {
        // Use the playpause toggle endpoint
        if self.send_cider_command("playpause") {
            // Immediately toggle local state for responsive UI
            let mut info = self.media_info.lock().unwrap();
            info.status = match info.status {
                PlaybackStatus::Playing => PlaybackStatus::Paused,
                PlaybackStatus::Paused => PlaybackStatus::Playing,
                PlaybackStatus::Stopped => PlaybackStatus::Playing,
            };
        }
    }
    
    /// Skip to next track.
    pub fn next(&self) {
        self.send_cider_command("next");
        // Set to playing since next usually starts playback
        let mut info = self.media_info.lock().unwrap();
        info.status = PlaybackStatus::Playing;
    }
    
    /// Go to previous track.
    pub fn previous(&self) {
        self.send_cider_command("previous");
        // Set to playing since previous usually starts playback
        let mut info = self.media_info.lock().unwrap();
        info.status = PlaybackStatus::Playing;
    }
    
    /// Seek to a specific position in the current track.
    ///
    /// # Arguments
    ///
    /// * `position_seconds` - Target position in seconds from start of track
    ///
    /// # Returns
    ///
    /// `true` if seek command was sent successfully
    pub fn seek(&self, position_seconds: f64) -> bool {
        use std::process::Command;
        
        let token = self.cider_token.lock().unwrap().clone();
        
        let mut cmd = Command::new("curl");
        cmd.args(&["-s", "-X", "POST", "--max-time", "1"]);
        cmd.args(&["-H", "Content-Type: application/json"]);
        
        if let Some(t) = token {
            cmd.args(&["-H", &format!("apptoken: {}", t)]);
        }
        
        // Send position as JSON body
        cmd.args(&["-d", &format!("{{\"position\": {}}}", position_seconds as u64)]);
        cmd.arg("http://localhost:10767/api/v1/playback/seek");
        
        log::info!("Seeking to {} seconds", position_seconds);
        
        if let Ok(output) = cmd.output() {
            if output.status.success() {
                // Update local position for responsive UI
                let mut info = self.media_info.lock().unwrap();
                info.position = (position_seconds * 1000.0) as u64;
                return true;
            }
        }
        false
    }
    
    /// Seek to a position based on progress percentage.
    ///
    /// # Arguments
    ///
    /// * `progress` - Value between 0.0 and 1.0 representing position in track
    ///
    /// # Returns
    ///
    /// `true` if seek command was sent successfully
    pub fn seek_to_progress(&self, progress: f64) -> bool {
        let info = self.media_info.lock().unwrap();
        let duration_seconds = info.duration as f64 / 1000.0;
        drop(info); // Release lock before calling seek
        
        let target_seconds = duration_seconds * progress.clamp(0.0, 1.0);
        self.seek(target_seconds)
    }
}
