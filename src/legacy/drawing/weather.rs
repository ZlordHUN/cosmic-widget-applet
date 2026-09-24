// SPDX-License-Identifier: MPL-2.0

// ============================================================================
// Embedded Font Resource
// ============================================================================

/// Weather Icons font embedded directly in the binary.
///
/// This TTF file contains glyphs for weather conditions (sun, clouds, rain, etc.)
/// from the Weather Icons project: https://erikflowers.github.io/weather-icons/
const WEATHER_ICONS_FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/weathericons.ttf"
));

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
