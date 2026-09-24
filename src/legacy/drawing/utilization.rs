// SPDX-License-Identifier: MPL-2.0

// ============================================================================
// Drawing Helper Functions
// ============================================================================
// These functions draw icons using Cairo for the utilization section.

/// Draw a CPU icon (chip with pins).
///
/// Used in the utilization section header.
pub fn draw_cpu_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw chip body
    cr.rectangle(x, y, size, size);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");

    // Draw pins on sides
    let pin_length = size * 0.2;
    let pin_spacing = size / 3.0;

    // Left pins
    for i in 0..3 {
        let py = y + pin_spacing * (i as f64 + 0.5);
        cr.move_to(x, py);
        cr.line_to(x - pin_length, py);
    }

    // Right pins
    for i in 0..3 {
        let py = y + pin_spacing * (i as f64 + 0.5);
        cr.move_to(x + size, py);
        cr.line_to(x + size + pin_length, py);
    }

    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a RAM icon (simple memory chip representation)
pub fn draw_ram_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw memory stick body
    cr.rectangle(x, y + size * 0.2, size, size * 0.8);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");

    // Draw notch at top
    let notch_width = size * 0.3;
    let notch_x = x + (size - notch_width) / 2.0;
    cr.rectangle(notch_x, y, notch_width, size * 0.2);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");

    // Draw chips on the body
    let chip_size = size * 0.15;
    for i in 0..3 {
        let chip_y = y + size * 0.3 + i as f64 * size * 0.22;
        cr.rectangle(x + size * 0.15, chip_y, chip_size, chip_size);
        cr.rectangle(x + size * 0.55, chip_y, chip_size, chip_size);
    }
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a GPU icon (graphics card representation)
pub fn draw_gpu_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw GPU card body
    cr.rectangle(x, y + size * 0.3, size * 1.3, size * 0.7);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");

    // Draw fan (circle)
    cr.arc(
        x + size * 0.65,
        y + size * 0.65,
        size * 0.25,
        0.0,
        2.0 * std::f64::consts::PI,
    );
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");

    // Draw PCIe connector
    for i in 0..3 {
        let connector_x = x + i as f64 * size * 0.15;
        cr.rectangle(connector_x, y, size * 0.1, size * 0.25);
    }
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a horizontal progress bar
pub fn draw_progress_bar(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    percentage: f32,
) {
    // Draw background
    cr.rectangle(x, y, width, height);
    cr.set_source_rgba(0.2, 0.2, 0.2, 0.7);
    cr.fill().expect("Failed to fill");

    // Draw border
    cr.rectangle(x, y, width, height);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(1.0);
    cr.stroke().expect("Failed to stroke");

    // Draw filled portion
    let fill_width = width * (percentage / 100.0).min(1.0) as f64;
    if fill_width > 0.0 {
        cr.rectangle(x + 1.0, y + 1.0, fill_width - 2.0, height - 2.0);

        // Gradient fill based on percentage
        let pattern = cairo::LinearGradient::new(x, y, x + width, y);
        if percentage < 50.0 {
            pattern.add_color_stop_rgb(0.0, 0.4, 0.9, 0.4);
            pattern.add_color_stop_rgb(1.0, 0.4, 0.9, 0.4);
        } else if percentage < 80.0 {
            pattern.add_color_stop_rgb(0.0, 0.9, 0.9, 0.4);
            pattern.add_color_stop_rgb(1.0, 0.9, 0.9, 0.4);
        } else {
            pattern.add_color_stop_rgb(0.0, 0.9, 0.4, 0.4);
            pattern.add_color_stop_rgb(1.0, 0.9, 0.4, 0.4);
        }

        cr.set_source(&pattern).expect("Failed to set source");
        cr.fill().expect("Failed to fill");
    }
}
