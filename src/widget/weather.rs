// SPDX-License-Identifier: MPL-2.0

//! # Weather Monitoring Module
//!
//! This module integrates with the OpenWeatherMap API to display current weather
//! conditions in the widget. It includes custom icon rendering using the
//! Weather Icons font.
//!
//! ## API Integration
//!
//! Uses the OpenWeatherMap "Current Weather Data" API:
//! ```text
//! https://api.openweathermap.org/data/2.5/weather?q={location}&appid={key}&units=metric
//! ```
//!
//! Requires a free API key from https://openweathermap.org/api
//!
//! ## Update Frequency
//!
//! - Minimum interval: 10 minutes (600 seconds)
//! - Background thread polls for requests every 10 seconds
//! - First update triggers immediately on startup
//!
//! ## Icon System
//!
//! OpenWeatherMap returns icon codes like "01d" (clear day) or "10n" (rain night).
//! These are mapped to Weather Icons font characters for visual display.
//!
//! ## Error Handling
//!
//! - Missing API key: Silently skips updates
//! - Missing location: Silently skips updates
//! - API failure: Keeps previous data, logs error
//! - Network timeout: 5 second limit to prevent blocking

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ============================================================================
// Embedded Font Resource
// ============================================================================

/// Weather Icons font embedded directly in the binary.
///
/// This TTF file contains glyphs for weather conditions (sun, clouds, rain, etc.)
/// from the Weather Icons project: https://erikflowers.github.io/weather-icons/
const WEATHER_ICONS_FONT: &[u8] = include_bytes!("../../resources/weathericons.ttf");

/// Load the Weather Icons font into the system font cache.
///
/// Pango/Cairo require fonts to be accessible via filesystem, so we extract
/// the embedded font to the user's cache directory on first use.
///
/// # Font Location
///
/// Written to: `$XDG_CACHE_HOME/cosmic-widget-weathericons.ttf`
/// (typically `~/.cache/cosmic-widget-weathericons.ttf`)
pub fn load_weather_font() {
    use std::io::Write;
    use std::fs;
    
    // Create a temporary file for the font (Pango needs a file path)
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let font_path = cache_dir.join("cosmic-widget-weathericons.ttf");
    
    // Write font to cache if it doesn't exist or size doesn't match (updated binary)
    if !font_path.exists() || fs::metadata(&font_path).map(|m| m.len()).unwrap_or(0) != WEATHER_ICONS_FONT.len() as u64 {
        if let Ok(mut file) = fs::File::create(&font_path) {
            let _ = file.write_all(WEATHER_ICONS_FONT);
            log::info!("Weather Icons font loaded from embedded data to {:?}", font_path);
        }
    }
}

// ============================================================================
// OpenWeatherMap API Response Structures
// ============================================================================

/// Root response from OpenWeatherMap "Current Weather" API.
#[derive(Debug, Deserialize)]
struct OpenWeatherResponse {
    /// Main weather measurements (temp, humidity)
    main: MainWeather,
    /// Array of weather conditions (usually one element)
    weather: Vec<WeatherCondition>,
    /// City name from API (may differ from input location)
    name: String,
}

/// Temperature and humidity data from API.
#[derive(Debug, Deserialize)]
struct MainWeather {
    /// Current temperature in Celsius (with units=metric)
    temp: f32,
    /// "Feels like" temperature accounting for wind/humidity
    feels_like: f32,
    /// Minimum temperature (at the moment, not forecast)
    temp_min: f32,
    /// Maximum temperature (at the moment, not forecast)
    temp_max: f32,
    /// Humidity percentage (0-100)
    humidity: u8,
}

/// Weather condition details from API.
#[derive(Debug, Deserialize)]
struct WeatherCondition {
    /// Human-readable description (e.g., "light rain", "clear sky")
    description: String,
    /// Icon code for weather visualization (e.g., "01d", "10n")
    /// Format: 2-digit condition + day(d)/night(n) suffix
    icon: String,
}

// ============================================================================
// Public Weather Data Struct
// ============================================================================

/// Processed weather data for display in the widget.
///
/// This struct contains all weather information needed for rendering,
/// extracted and normalized from the OpenWeatherMap API response.
///
/// # Serialization
///
/// Implements Serialize/Deserialize for potential caching (not currently used).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherData {
    /// Current temperature in Celsius
    pub temperature: f32,
    /// "Feels like" temperature (wind chill / heat index)
    pub feels_like: f32,
    /// Current minimum temperature
    pub temp_min: f32,
    /// Current maximum temperature
    pub temp_max: f32,
    /// Humidity percentage (0-100)
    pub humidity: u8,
    /// Capitalized weather description (e.g., "Light rain")
    pub description: String,
    /// OpenWeatherMap icon code (e.g., "01d", "10n")
    pub icon: String,
    /// City name returned by API
    pub location: String,
}

impl Default for WeatherData {
    /// Default weather data for display before first API response.
    fn default() -> Self {
        Self {
            temperature: 0.0,
            feels_like: 0.0,
            temp_min: 0.0,
            temp_max: 0.0,
            humidity: 0,
            description: String::from("N/A"),
            icon: String::from("01d"),  // Clear day as default icon
            location: String::from("Unknown"),
        }
    }
}

// ============================================================================
// Weather Monitor Struct
// ============================================================================

/// Monitors weather conditions via OpenWeatherMap API.
///
/// Fetches weather data in a background thread to avoid blocking the render loop.
/// Updates are rate-limited to once every 10 minutes to respect API quotas.
///
/// # Threading Model
///
/// - `weather_data`: Shared state with latest weather info
/// - `api_key` / `location`: Shared config, can be updated from settings
/// - `update_requested`: Flag to trigger background fetch
/// - Background thread checks for requests every 10 seconds
///
/// # Configuration
///
/// Requires both an API key and location to be set. Without these, updates
/// are silently skipped.
pub struct WeatherMonitor {
    /// Shared weather data, updated by background thread
    pub weather_data: Arc<Mutex<Option<WeatherData>>>,
    /// Timestamp of last update (for rate limiting)
    pub last_update: Instant,
    /// OpenWeatherMap API key (shared for background thread)
    api_key: Arc<Mutex<String>>,
    /// Location query string (city name or "city,country")
    location: Arc<Mutex<String>>,
    /// Flag to signal background thread that an update is needed
    update_requested: Arc<Mutex<bool>>,
}

impl WeatherMonitor {
    /// Create a new weather monitor with background update thread.
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenWeatherMap API key (from settings)
    /// * `location` - Location query (e.g., "London", "New York,US")
    ///
    /// # Initialization
    ///
    /// 1. Sets `last_update` to 11 minutes ago to trigger immediate first update
    /// 2. Spawns background thread for API requests
    /// 3. Background thread polls for update requests every 10 seconds
    pub fn new(api_key: String, location: String) -> Self {
        // Initialize last_update to 11 minutes ago to force immediate first update
        // (Rate limit is 10 minutes, so 11 minutes ensures first update triggers)
        let last_update = Instant::now() - std::time::Duration::from_secs(660);
        
        let api_key = Arc::new(Mutex::new(api_key));
        let location = Arc::new(Mutex::new(location));
        let update_requested = Arc::new(Mutex::new(false));
        let weather_data = Arc::new(Mutex::new(None));
        
        // Spawn background thread for weather updates
        // This avoids blocking the main render loop on network requests
        let api_key_clone = Arc::clone(&api_key);
        let location_clone = Arc::clone(&location);
        let update_requested_clone = Arc::clone(&update_requested);
        let weather_data_clone = Arc::clone(&weather_data);
        
        std::thread::spawn(move || {
            loop {
                // Poll for update requests every 10 seconds
                std::thread::sleep(std::time::Duration::from_secs(10));
                
                // Check if update is needed (atomic check-and-clear)
                let requested = {
                    let mut req = update_requested_clone.lock().unwrap();
                    if *req {
                        *req = false;
                        true
                    } else {
                        false
                    }
                };
                
                if requested {
                    let api_key = api_key_clone.lock().unwrap().clone();
                    let location = location_clone.lock().unwrap().clone();
                    
                    if !api_key.is_empty() && !location.is_empty() {
                        log::info!("Background: Fetching weather data for location: {}", location);
                        match Self::fetch_weather_static(&api_key, &location) {
                            Ok(data) => {
                                log::info!("Background: Weather data fetched: {}°C, {} (icon: {})", 
                                    data.temperature, data.description, data.icon);
                                *weather_data_clone.lock().unwrap() = Some(data);
                            }
                            Err(e) => {
                                log::error!("Background: Failed to fetch weather: {}", e);
                            }
                        }
                    }
                }
            }
        });
        
        Self {
            weather_data,
            last_update,
            api_key,
            location,
            update_requested,
        }
    }

    /// Request a weather update if rate limit has elapsed.
    ///
    /// Rate-limited to once every 10 minutes (600 seconds) to respect
    /// OpenWeatherMap API quotas. The actual API call runs in the background
    /// thread - this just sets a flag.
    ///
    /// # Skipped When
    ///
    /// - API key is empty or not configured
    /// - Location is empty or not configured
    /// - Less than 10 minutes since last update
    pub fn update(&mut self) {
        // Only update if we have an API key and location
        {
            let api_key = self.api_key.lock().unwrap();
            let location = self.location.lock().unwrap();
            
            if api_key.is_empty() || location.is_empty() {
                log::trace!("Weather update skipped: API key or location not configured");
                return;
            }
        }
        
        // Don't update more than once every 10 minutes (API rate limiting)
        let elapsed = self.last_update.elapsed().as_secs();
        if elapsed < 600 {
            log::trace!("Weather update skipped: too soon ({}s since last update, need 600s)", elapsed);
            return;
        }
        
        log::info!("Requesting weather update from background thread");
        *self.update_requested.lock().unwrap() = true;
        self.last_update = Instant::now();
    }
    
    /// Fetch weather data from OpenWeatherMap API (blocking).
    ///
    /// This is a static method called from the background thread.
    ///
    /// # API Request
    ///
    /// ```text
    /// GET https://api.openweathermap.org/data/2.5/weather?q={location}&appid={key}&units=metric
    /// ```
    ///
    /// # Processing
    ///
    /// 1. Strip quotes from config values (cosmic_config quirk)
    /// 2. Build API URL with metric units
    /// 3. Make HTTP request with 5-second timeout
    /// 4. Parse JSON response
    /// 5. Capitalize weather description
    /// 6. Return processed WeatherData
    fn fetch_weather_static(api_key: &str, location: &str) -> Result<WeatherData, Box<dyn std::error::Error>> {
        // Strip quotes from location and API key (cosmic_config may store them with quotes)
        let location = location.trim_matches('"');
        let api_key = api_key.trim_matches('"');
        
        log::debug!("Making API request for location: {}", location);
        
        let url = format!(
            "https://api.openweathermap.org/data/2.5/weather?q={}&appid={}&units=metric",
            location, api_key
        );

        // Use a client with timeout to prevent blocking indefinitely
        // 5 seconds is generous for a simple API call
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
            
        let response: OpenWeatherResponse = client.get(&url).send()?.json()?;
        
        log::debug!("Weather API response received for: {}", response.name);

        // Capitalize first letter of description
        let description = response
            .weather
            .first()
            .map(|w| {
                let mut desc = w.description.clone();
                if let Some(first_char) = desc.chars().next() {
                    desc = first_char.to_uppercase().collect::<String>() + &desc[1..];
                }
                desc
            })
            .unwrap_or_else(|| String::from("Unknown"));

        // Extract icon code (e.g., "01d", "10n")
        let icon = response
            .weather
            .first()
            .map(|w| w.icon.clone())
            .unwrap_or_else(|| String::from("01d"));

        Ok(WeatherData {
            temperature: response.main.temp,
            feels_like: response.main.feels_like,
            temp_min: response.main.temp_min,
            temp_max: response.main.temp_max,
            humidity: response.main.humidity,
            description,
            icon,
            location: response.name,
        })
    }
    
    /// Update the API key (called when settings change).
    pub fn set_api_key(&mut self, api_key: String) {
        *self.api_key.lock().unwrap() = api_key;
    }
    
    /// Update the location query (called when settings change).
    pub fn set_location(&mut self, location: String) {
        *self.location.lock().unwrap() = location;
    }
}

// ============================================================================
// Weather Icon Drawing
// ============================================================================

/// Draw a weather icon using the Weather Icons font.
///
/// Maps OpenWeatherMap icon codes to Weather Icons Unicode characters
/// and renders them using Pango/Cairo.
///
/// # Arguments
///
/// * `cr` - Cairo context for drawing
/// * `x` - Left edge X coordinate
/// * `y` - Top edge Y coordinate
/// * `size` - Icon size in pixels (width and height)
/// * `icon_code` - OpenWeatherMap icon code (e.g., "01d", "10n")
///
/// # Icon Code Format
///
/// OpenWeatherMap uses codes like "01d" or "10n":
/// - First 2 chars: Weather condition (01-50)
/// - Last char: Day (d) or Night (n)
///
/// # Weather Condition Mapping
///
/// | Code | Day Icon | Night Icon | Condition |
/// |------|----------|------------|-----------|
/// | 01   | sunny    | clear      | Clear sky |
/// | 02   | cloudy   | cloudy     | Few clouds |
/// | 03   | overcast | partly     | Scattered clouds |
/// | 04   | cloudy   | cloudy     | Broken clouds |
/// | 09   | showers  | showers    | Shower rain |
/// | 10   | rain     | rain       | Rain |
/// | 11   | storm    | storm      | Thunderstorm |
/// | 13   | snow     | snow       | Snow |
/// | 50   | fog      | fog        | Mist/Fog |
pub fn draw_weather_icon(cr: &cairo::Context, x: f64, y: f64, size: f64, icon_code: &str) {
    // Parse icon code: first 2 chars are condition, last char is day(d) or night(n)
    let condition = if icon_code.len() >= 2 { &icon_code[0..2] } else { "01" };
    let is_day = icon_code.ends_with('d');
    
    // Map OpenWeatherMap icon codes to Weather Icons font Unicode characters
    // Reference: https://erikflowers.github.io/weather-icons/
    let icon_char = match condition {
        "01" => if is_day { "\u{f00d}" } else { "\u{f02e}" },  // wi-day-sunny / wi-night-clear
        "02" => if is_day { "\u{f002}" } else { "\u{f086}" },  // wi-day-cloudy / wi-night-alt-cloudy
        "03" => if is_day { "\u{f013}" } else { "\u{f031}" },  // wi-day-sunny-overcast / wi-night-partly-cloudy
        "04" => "\u{f041}",                                     // wi-cloudy (same day/night)
        "09" => if is_day { "\u{f009}" } else { "\u{f029}" },  // wi-day-showers / wi-night-alt-showers
        "10" => if is_day { "\u{f008}" } else { "\u{f028}" },  // wi-day-rain / wi-night-alt-rain
        "11" => if is_day { "\u{f010}" } else { "\u{f02d}" },  // wi-day-thunderstorm / wi-night-alt-thunderstorm
        "13" => if is_day { "\u{f00a}" } else { "\u{f02a}" },  // wi-day-snow / wi-night-alt-snow
        "50" => if is_day { "\u{f003}" } else { "\u{f04a}" },  // wi-day-fog / wi-night-fog
        _ => "\u{f041}",                                        // Default to wi-cloudy
    };
    
    // Create pango layout for text/icon rendering
    let layout = pangocairo::functions::create_layout(cr);
    
    // Use the Weather Icons font at slightly smaller than requested size
    // (0.9 factor for visual balance)
    let font_desc = pango::FontDescription::from_string(&format!("Weather Icons {}", (size * 0.9) as i32));
    layout.set_font_description(Some(&font_desc));
    layout.set_text(icon_char);
    
    // Get text dimensions for centering
    let (text_width, text_height) = layout.pixel_size();
    
    // Center the icon within the requested size box
    let text_x = x + (size - text_width as f64) / 2.0;
    let text_y = y + (size - text_height as f64) / 2.0;
    
    cr.move_to(text_x, text_y);
    
    // Draw with black outline and white fill for visibility on any background
    pangocairo::functions::layout_path(cr, &layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(3.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
}

