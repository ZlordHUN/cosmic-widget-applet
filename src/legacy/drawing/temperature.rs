// SPDX-License-Identifier: MPL-2.0

// ============================================================================
// Drawing Helper Function
// ============================================================================

/// Draw a circular temperature gauge with color-coded progress ring.
///
/// Renders a hollow circular gauge that fills based on the temperature
/// relative to a maximum value. The ring color changes to indicate
/// thermal status:
///
/// - **Green**: Temperature below 50% of max (cool)
/// - **Yellow**: Temperature 50-80% of max (warm)
/// - **Red**: Temperature above 80% of max (hot)
///
/// # Arguments
///
/// * `cr` - Cairo context for drawing
/// * `x` - Left edge X coordinate
/// * `y` - Top edge Y coordinate
/// * `radius` - Radius of the gauge circle
/// * `temp` - Current temperature in Celsius
/// * `max_temp` - Maximum temperature for full circle (e.g., 100.0)
///
/// # Visual Structure
///
/// ```text
/// ┌─────────────────┐
/// │    ╭─────╮      │  Outer border (black)
/// │   ╱  ███  ╲     │  Background ring (dark gray)
/// │  │  ███   │     │  Progress arc (green/yellow/red)
/// │   ╲      ╱      │  Inner border (black)
/// │    ╰─────╯      │
/// └─────────────────┘
/// ```
pub fn draw_temp_circle(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    radius: f64,
    temp: f32,
    max_temp: f32,
) {
    // Save Cairo state so line_width and source don't leak to callers
    cr.save().expect("Failed to save");

    let center_x = x + radius;
    let center_y = y + radius;

    // Determine color based on temperature (similar to progress bar logic)
    let percentage = (temp / max_temp * 100.0).min(100.0);
    let (r, g, b) = if percentage < 50.0 {
        (0.4, 0.9, 0.4) // Green
    } else if percentage < 80.0 {
        (0.9, 0.9, 0.4) // Yellow
    } else {
        (0.9, 0.4, 0.4) // Red
    };

    // Draw outer ring (background)
    cr.arc(center_x, center_y, radius, 0.0, 2.0 * std::f64::consts::PI);
    cr.set_source_rgba(0.2, 0.2, 0.2, 0.7);
    cr.set_line_width(8.0);
    cr.stroke().expect("Failed to stroke");

    // Draw inner colored ring based on temperature
    let angle = (temp / max_temp).min(1.0) as f64 * 2.0 * std::f64::consts::PI;
    cr.arc(
        center_x,
        center_y,
        radius,
        -std::f64::consts::PI / 2.0,
        -std::f64::consts::PI / 2.0 + angle,
    );
    cr.set_source_rgb(r, g, b);
    cr.set_line_width(8.0);
    cr.stroke().expect("Failed to stroke");

    // Draw border around the ring
    cr.arc(
        center_x,
        center_y,
        radius + 4.0,
        0.0,
        2.0 * std::f64::consts::PI,
    );
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");

    cr.arc(
        center_x,
        center_y,
        radius - 4.0,
        0.0,
        2.0 * std::f64::consts::PI,
    );
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");

    // Restore Cairo state (resets line_width, source, etc.)
    cr.restore().expect("Failed to restore");
}
