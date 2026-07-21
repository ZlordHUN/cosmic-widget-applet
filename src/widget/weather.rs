// SPDX-License-Identifier: MPL-2.0

//! # Weather Monitoring Module
//!
//! This module integrates with the Open-Meteo API to display current weather
//! conditions in the widget. It includes custom icon rendering using the
//! Weather Icons font.
//!
//! ## API Integration
//!
//! Uses the Open-Meteo free API (no API key required):
//! - Geocoding: `https://geocoding-api.open-meteo.com/v1/search?name={city}`
//! - Weather: `https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=...`
//!
//! See https://open-meteo.com/en/docs for full documentation.
//!
//! ## Update Frequency
//!
//! - Minimum interval: 2 minutes (120 seconds)
//! - Update requests wake the background worker immediately
//! - First update triggers immediately on startup
//! - Cached data is displayed while the live refresh completes
//!
//! ## Icon System
//!
//! Open-Meteo returns WMO weather codes which are mapped to Weather Icons
//! font characters for visual display.
//!
//! ## Error Handling
//!
//! - Missing location: Silently skips updates
//! - API failure: Keeps previous data, logs error
//! - Network timeout: 5 second limit to prevent a stalled request

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
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
    use std::fs;
    use std::io::Write;

    // Create a temporary file for the font (Pango needs a file path)
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let font_path = cache_dir.join("cosmic-widget-weathericons.ttf");

    // Write font to cache if it doesn't exist or size doesn't match (updated binary)
    if !font_path.exists()
        || fs::metadata(&font_path).map(|m| m.len()).unwrap_or(0) != WEATHER_ICONS_FONT.len() as u64
    {
        if let Ok(mut file) = fs::File::create(&font_path) {
            let _ = file.write_all(WEATHER_ICONS_FONT);
            log::info!(
                "Weather Icons font loaded from embedded data to {:?}",
                font_path
            );
        }
    }
}

// ============================================================================
// Open-Meteo API Response Structures
// ============================================================================

/// Response from Open-Meteo Geocoding API.
#[derive(Debug, Deserialize)]
struct GeocodingResponse {
    results: Option<Vec<GeocodingResult>>,
}

/// Single result from geocoding search.
#[derive(Debug, Deserialize)]
struct GeocodingResult {
    name: String,
    latitude: f64,
    longitude: f64,
    country: Option<String>,
    #[serde(default)]
    admin1: Option<String>,
}

/// Response from Open-Meteo Weather Forecast API.
#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    current: CurrentWeather,
}

/// Current weather data from Open-Meteo API.
#[derive(Debug, Deserialize)]
struct CurrentWeather {
    /// Current temperature in Celsius
    temperature_2m: f32,
    /// Relative humidity percentage
    relative_humidity_2m: u8,
    /// Apparent (feels like) temperature
    apparent_temperature: f32,
    /// WMO weather interpretation code
    weather_code: u8,
    /// 1 if daytime, 0 if night
    is_day: u8,
}

// ============================================================================
// Public Weather Data Struct
// ============================================================================

/// Processed weather data for display in the widget.
///
/// This struct contains all weather information needed for rendering,
/// extracted and normalized from the Open-Meteo API response.
///
/// # Serialization
///
/// Implements Serialize/Deserialize so the last successful reading can be cached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherData {
    /// Current temperature in Celsius
    pub temperature: f32,
    /// "Feels like" temperature (wind chill / heat index)
    pub feels_like: f32,
    /// Current minimum temperature (not available from Open-Meteo current, set same as temp)
    pub temp_min: f32,
    /// Current maximum temperature (not available from Open-Meteo current, set same as temp)
    pub temp_max: f32,
    /// Humidity percentage (0-100)
    pub humidity: u8,
    /// Capitalized weather description
    pub description: String,
    /// Icon code for weather visualization (OpenWeatherMap-compatible format)
    pub icon: String,
    /// City name returned by API
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResolvedLocation {
    query: String,
    latitude: f64,
    longitude: f64,
    display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WeatherCache {
    location: ResolvedLocation,
    data: WeatherData,
}

impl WeatherCache {
    fn path() -> PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        path.push("cosmic-widget-applet");
        let _ = fs::create_dir_all(&path);
        path.push("weather.json");
        path
    }

    fn load(query: &str) -> Option<Self> {
        let content = fs::read_to_string(Self::path()).ok()?;
        let cache: Self = serde_json::from_str(&content).ok()?;
        (cache.location.query == query).then_some(cache)
    }

    fn save(&self) {
        let Ok(json) = serde_json::to_string(self) else {
            return;
        };
        let _ = fs::write(Self::path(), json);
    }
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
            icon: String::from("01d"), // Clear day as default icon
            location: String::from("Unknown"),
        }
    }
}

// ============================================================================
// WMO Weather Code Mapping
// ============================================================================

/// Convert WMO weather code to description and OpenWeatherMap-compatible icon code.
///
/// WMO Weather interpretation codes (WW):
/// - 0: Clear sky
/// - 1, 2, 3: Mainly clear, partly cloudy, overcast
/// - 45, 48: Fog and depositing rime fog
/// - 51, 53, 55: Drizzle (light, moderate, dense)
/// - 56, 57: Freezing drizzle
/// - 61, 63, 65: Rain (slight, moderate, heavy)
/// - 66, 67: Freezing rain
/// - 71, 73, 75: Snowfall (slight, moderate, heavy)
/// - 77: Snow grains
/// - 80, 81, 82: Rain showers (slight, moderate, violent)
/// - 85, 86: Snow showers
/// - 95: Thunderstorm
/// - 96, 99: Thunderstorm with hail
fn wmo_to_description_and_icon(code: u8, is_day: bool) -> (String, String) {
    let day_suffix = if is_day { "d" } else { "n" };

    let (description, icon_base) = match code {
        0 => ("Clear sky", "01"),
        1 => ("Mainly clear", "02"),
        2 => ("Partly cloudy", "03"),
        3 => ("Overcast", "04"),
        45 | 48 => ("Fog", "50"),
        51 => ("Light drizzle", "09"),
        53 => ("Moderate drizzle", "09"),
        55 => ("Dense drizzle", "09"),
        56 | 57 => ("Freezing drizzle", "09"),
        61 => ("Slight rain", "10"),
        63 => ("Moderate rain", "10"),
        65 => ("Heavy rain", "10"),
        66 | 67 => ("Freezing rain", "10"),
        71 => ("Slight snowfall", "13"),
        73 => ("Moderate snowfall", "13"),
        75 => ("Heavy snowfall", "13"),
        77 => ("Snow grains", "13"),
        80 => ("Slight rain showers", "09"),
        81 => ("Moderate rain showers", "09"),
        82 => ("Violent rain showers", "09"),
        85 => ("Slight snow showers", "13"),
        86 => ("Heavy snow showers", "13"),
        95 => ("Thunderstorm", "11"),
        96 | 99 => ("Thunderstorm with hail", "11"),
        _ => ("Unknown", "01"),
    };

    (
        description.to_string(),
        format!("{}{}", icon_base, day_suffix),
    )
}

// ============================================================================
// Weather Monitor Struct
// ============================================================================

/// Monitors weather conditions via Open-Meteo API.
///
/// Fetches weather data in a background thread to avoid blocking the render loop.
/// Updates are rate-limited to once every 2 minutes to respect API quotas.
///
/// # Threading Model
///
/// - `weather_data`: Shared state with latest weather info
/// - `location`: Shared config, can be updated from settings
/// - `update_sender`: Bounded channel that wakes the worker immediately
/// - The last coordinates and reading are restored from disk on startup
///
/// # Configuration
///
/// Only requires a location to be set. No API key needed!
pub struct WeatherMonitor {
    /// Shared weather data, updated by background thread
    pub weather_data: Arc<Mutex<Option<WeatherData>>>,
    /// Timestamp of last update (for rate limiting)
    pub last_update: Instant,
    /// Location query string (city name or "city,country")
    location: Arc<Mutex<String>>,
    /// Cached coordinates from last geocoding lookup
    cached_location: Arc<Mutex<Option<ResolvedLocation>>>,
    /// Wakes the background worker as soon as an update is requested.
    update_sender: SyncSender<()>,
    /// API key (kept for backward compatibility, but no longer required)
    #[allow(dead_code)]
    api_key: Arc<Mutex<String>>,
}

impl WeatherMonitor {
    /// Create a new weather monitor with background update thread.
    ///
    /// # Arguments
    ///
    /// * `api_key` - Ignored (kept for backward compatibility)
    /// * `location` - Location query (e.g., "London", "New York,US")
    ///
    /// # Initialization
    ///
    /// 1. Sets `last_update` beyond the rate limit to trigger an immediate refresh
    /// 2. Spawns background thread for API requests
    /// 3. Background thread blocks until an update request arrives
    pub fn new(api_key: String, location: String) -> Self {
        // Start beyond the rate limit so the first caller queues a live refresh.
        let last_update = Instant::now() - std::time::Duration::from_secs(121);

        let api_key = Arc::new(Mutex::new(api_key));
        let location = Arc::new(Mutex::new(location));
        let cached_weather = WeatherCache::load(&location.lock().unwrap());
        let weather_data = Arc::new(Mutex::new(
            cached_weather.as_ref().map(|cache| cache.data.clone()),
        ));
        let cached_location = Arc::new(Mutex::new(cached_weather.map(|cache| cache.location)));
        let (update_sender, update_receiver) = sync_channel(1);

        // Spawn background thread for weather updates
        // This avoids blocking the main render loop on network requests
        let location_clone = Arc::clone(&location);
        let weather_data_clone = Arc::clone(&weather_data);
        let cached_location_clone = Arc::clone(&cached_location);

        std::thread::spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(5))
                .build()
            {
                Ok(client) => client,
                Err(error) => {
                    log::error!("Failed to initialize weather HTTP client: {error}");
                    return;
                }
            };

            while update_receiver.recv().is_ok() {
                let location = location_clone.lock().unwrap().clone();

                if !location.is_empty() {
                    log::info!(
                        "Background: Fetching weather data for location: {}",
                        location
                    );

                    // Get coordinates from the persistent cache or geocoding API.
                    let cached = {
                        let guard = cached_location_clone.lock().unwrap();
                        guard.clone()
                    };

                    let resolved = match cached {
                        Some(cached) if cached.query == location => {
                            log::debug!(
                                "Using cached coordinates for {}: ({}, {})",
                                location,
                                cached.latitude,
                                cached.longitude
                            );
                            Some(cached)
                        }
                        _ => {
                            log::info!("Geocoding location: {}", location);
                            match Self::geocode_location(&client, &location) {
                                Ok((lat, lon, name)) => {
                                    log::info!(
                                        "Geocoded {} to ({}, {}) - {}",
                                        location,
                                        lat,
                                        lon,
                                        name
                                    );
                                    let resolved = ResolvedLocation {
                                        query: location.clone(),
                                        latitude: lat,
                                        longitude: lon,
                                        display_name: name,
                                    };
                                    *cached_location_clone.lock().unwrap() = Some(resolved.clone());
                                    Some(resolved)
                                }
                                Err(e) => {
                                    log::error!("Failed to geocode location {}: {}", location, e);
                                    None
                                }
                            }
                        }
                    };

                    if let Some(resolved) = resolved {
                        match Self::fetch_weather_static(
                            &client,
                            resolved.latitude,
                            resolved.longitude,
                            &resolved.display_name,
                        ) {
                            Ok(data) => {
                                log::info!(
                                    "Background: Weather data fetched: {}°C, {} (icon: {})",
                                    data.temperature,
                                    data.description,
                                    data.icon
                                );
                                let is_current_location = location_clone
                                    .lock()
                                    .map(|current| *current == location)
                                    .unwrap_or(false);
                                if is_current_location {
                                    *weather_data_clone.lock().unwrap() = Some(data.clone());
                                    WeatherCache {
                                        location: resolved,
                                        data,
                                    }
                                    .save();
                                }
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
            cached_location,
            update_sender,
        }
    }

    /// Request a weather update if rate limit has elapsed.
    ///
    /// Rate-limited to once every 2 minutes (120 seconds). Open-Meteo allows
    /// up to 10,000 calls/day for non-commercial use. The actual API call
    /// runs in the background thread; this queues a wake-up signal.
    ///
    /// # Skipped When
    ///
    /// - Location is empty or not configured
    /// - Less than 2 minutes since last update
    pub fn update(&mut self) {
        // Only update if we have a location
        {
            let location = self.location.lock().unwrap();

            if location.is_empty() {
                log::trace!("Weather update skipped: location not configured");
                return;
            }
        }

        // Don't update more than once every 2 minutes (API rate limiting)
        let elapsed = self.last_update.elapsed().as_secs();
        if elapsed < 120 {
            log::trace!(
                "Weather update skipped: too soon ({}s since last update, need 120s)",
                elapsed
            );
            return;
        }

        match self.update_sender.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {
                log::info!("Requesting weather update from background thread");
                self.last_update = Instant::now();
            }
            Err(TrySendError::Disconnected(())) => {
                log::error!("Weather background worker is unavailable");
            }
        }
    }

    /// Geocode a location name to coordinates using Open-Meteo Geocoding API.
    fn geocode_location(
        client: &reqwest::blocking::Client,
        location: &str,
    ) -> Result<(f64, f64, String), Box<dyn std::error::Error>> {
        let location = location.trim_matches('"');

        let url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
            urlencoding::encode(location)
        );

        let response: GeocodingResponse = client.get(&url).send()?.json()?;

        let result = response
            .results
            .and_then(|r| r.into_iter().next())
            .ok_or("No location found")?;

        // Build a nice location name
        let location_name = if let Some(country) = &result.country {
            if let Some(admin1) = &result.admin1 {
                format!("{}, {}", result.name, admin1)
            } else {
                format!("{}, {}", result.name, country)
            }
        } else {
            result.name.clone()
        };

        Ok((result.latitude, result.longitude, location_name))
    }

    /// Fetch weather data from Open-Meteo API (blocking).
    ///
    /// This is a static method called from the background thread.
    ///
    /// # API Request
    ///
    /// ```text
    /// GET https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=...
    /// ```
    fn fetch_weather_static(
        client: &reqwest::blocking::Client,
        lat: f64,
        lon: f64,
        location: &str,
    ) -> Result<WeatherData, Box<dyn std::error::Error>> {
        log::debug!("Making API request for coordinates: ({}, {})", lat, lon);

        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,is_day&temperature_unit=celsius",
            lat, lon
        );

        let response: OpenMeteoResponse = client.get(&url).send()?.json()?;

        log::debug!("Weather API response received");

        let is_day = response.current.is_day == 1;
        let (description, icon) =
            wmo_to_description_and_icon(response.current.weather_code, is_day);

        Ok(WeatherData {
            temperature: response.current.temperature_2m,
            feels_like: response.current.apparent_temperature,
            temp_min: response.current.temperature_2m, // Not available in current data
            temp_max: response.current.temperature_2m, // Not available in current data
            humidity: response.current.relative_humidity_2m,
            description,
            icon,
            location: location.to_string(),
        })
    }

    /// Update the API key (kept for backward compatibility, but no longer used).
    pub fn set_api_key(&mut self, api_key: String) {
        *self.api_key.lock().unwrap() = api_key;
    }

    /// Update the location query (called when settings change).
    /// Restores matching cached data or clears the previous location's data.
    pub fn set_location(&mut self, location: String) {
        let old_location = self.location.lock().unwrap().clone();
        if old_location != location {
            let cached_weather = WeatherCache::load(&location);
            *self.location.lock().unwrap() = location;
            *self.cached_location.lock().unwrap() =
                cached_weather.as_ref().map(|cache| cache.location.clone());
            *self.weather_data.lock().unwrap() = cached_weather.map(|cache| cache.data);
            self.last_update = Instant::now() - std::time::Duration::from_secs(121);
        }
    }
}

// ============================================================================
// Weather Icon Drawing
// ============================================================================

/// Draw a weather icon using the Weather Icons font.
///
/// Maps icon codes to Weather Icons Unicode characters
/// and renders them using Pango/Cairo.
///
/// # Arguments
///
/// * `cr` - Cairo context for drawing
/// * `x` - Left edge X coordinate
/// * `y` - Top edge Y coordinate
/// * `size` - Icon size in pixels (width and height)
/// * `icon_code` - Icon code (e.g., "01d", "10n")
///
/// # Icon Code Format
///
/// Uses OpenWeatherMap-compatible codes like "01d" or "10n":
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
    let condition = if icon_code.len() >= 2 {
        &icon_code[0..2]
    } else {
        "01"
    };
    let is_day = icon_code.ends_with('d');

    // Map icon codes to Weather Icons font Unicode characters
    // Reference: https://erikflowers.github.io/weather-icons/
    let icon_char = match condition {
        "01" => {
            if is_day {
                "\u{f00d}"
            } else {
                "\u{f02e}"
            }
        } // wi-day-sunny / wi-night-clear
        "02" => {
            if is_day {
                "\u{f002}"
            } else {
                "\u{f086}"
            }
        } // wi-day-cloudy / wi-night-alt-cloudy
        "03" => {
            if is_day {
                "\u{f013}"
            } else {
                "\u{f031}"
            }
        } // wi-day-sunny-overcast / wi-night-partly-cloudy
        "04" => "\u{f041}", // wi-cloudy (same day/night)
        "09" => {
            if is_day {
                "\u{f009}"
            } else {
                "\u{f029}"
            }
        } // wi-day-showers / wi-night-alt-showers
        "10" => {
            if is_day {
                "\u{f008}"
            } else {
                "\u{f028}"
            }
        } // wi-day-rain / wi-night-alt-rain
        "11" => {
            if is_day {
                "\u{f010}"
            } else {
                "\u{f02d}"
            }
        } // wi-day-thunderstorm / wi-night-alt-thunderstorm
        "13" => {
            if is_day {
                "\u{f00a}"
            } else {
                "\u{f02a}"
            }
        } // wi-day-snow / wi-night-alt-snow
        "50" => {
            if is_day {
                "\u{f003}"
            } else {
                "\u{f04a}"
            }
        } // wi-day-fog / wi-night-fog
        _ => "\u{f041}",    // Default to wi-cloudy
    };

    // Create pango layout for text/icon rendering
    let layout = pangocairo::functions::create_layout(cr);

    // Use the Weather Icons font at slightly smaller than requested size
    // (0.9 factor for visual balance)
    let font_desc =
        pango::FontDescription::from_string(&format!("Weather Icons {}", (size * 0.9) as i32));
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
