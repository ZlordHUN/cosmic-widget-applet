// SPDX-License-Identifier: MPL-2.0

//! Widget Layout Calculations
//!
//! This module calculates the dynamic height of the widget based on which
//! sections are enabled and how much content each section has.
//!
//! # Why Dynamic Height?
//!
//! The widget displays variable amounts of content:
//! - Storage section grows with each mounted disk
//! - Battery section grows with each device (system + Solaar)
//! - Notifications section grows up to a maximum count
//!
//! Rather than allocating a fixed maximum height (which would waste space),
//! we calculate exactly how much vertical space is needed.
//!
//! # Calculation Approach
//!
//! Each section contributes a fixed header height plus per-item heights:
//!
//! ```text
//! Section Height = Header (35px) + (Item Count × Item Height)
//! ```
//!
//! The final height is the sum of all enabled sections plus padding.

use crate::config::Config;

// ============================================================================
// Height Constants (in pixels)
// ============================================================================

// These constants should ideally be shared with renderer.rs, but are
// currently duplicated. Changes here must be mirrored in the renderer.

const BASE_PADDING: u32 = 10;
const BOTTOM_PADDING: u32 = 20;
const SECTION_SPACING: u32 = 10;
const HEADER_HEIGHT: u32 = 35;
const MINIMUM_HEIGHT: u32 = 100;

// ============================================================================
// Public API
// ============================================================================

/// Calculate widget height (legacy API, assumes no batteries).
///
/// Use [`calculate_widget_height_with_all`] for full control.
pub fn calculate_widget_height(config: &Config, disk_count: usize) -> u32 {
    calculate_widget_height_with_batteries(config, disk_count, 0)
}

/// Calculate widget height with battery count (legacy API).
///
/// Use [`calculate_widget_height_with_all`] for full control.
pub fn calculate_widget_height_with_batteries(config: &Config, disk_count: usize, battery_count: usize) -> u32 {
    calculate_widget_height_with_all(config, disk_count, battery_count, 0, 0)
}

/// Calculate the required widget height based on enabled sections and content counts.
///
/// This is the primary height calculation function used by the widget's draw loop.
///
/// # Arguments
///
/// * `config` - Current configuration with enabled/disabled sections
/// * `disk_count` - Number of mounted disks to display
/// * `battery_count` - Number of battery devices (system + Solaar)
/// * `notification_count` - Number of notifications (capped at max_notifications)
/// * `player_count` - Number of media players (for pagination dots)
///
/// # Returns
///
/// Height in pixels, minimum 100px
pub fn calculate_widget_height_with_all(config: &Config, disk_count: usize, battery_count: usize, notification_count: usize, player_count: usize) -> u32 {
    let mut required_height = BASE_PADDING;
    
    // === Clock & Date Section ===
    // Always at the top of the widget
    if config.show_clock {
        required_height += 70; // Large clock text
    }
    if config.show_date {
        required_height += 35; // Date text below clock
    }
    if config.show_clock || config.show_date {
        required_height += 20; // Spacing after clock/date
    }
    
    // === Utilization Section ===
    // CPU, Memory, and GPU usage bars
    if config.show_cpu || config.show_memory || config.show_gpu {
        required_height += HEADER_HEIGHT; // "Utilization" header
        if config.show_cpu {
            required_height += 30; // CPU bar + label
        }
        if config.show_memory {
            required_height += 30; // RAM bar + label
        }
        if config.show_gpu {
            required_height += 30; // GPU bar + label
        }
    }
    
    // === Temperature Section ===
    // CPU and/or GPU temperatures
    if config.show_cpu_temp || config.show_gpu_temp {
        required_height += SECTION_SPACING;
        required_height += HEADER_HEIGHT; // "Temperatures" header
        
        if config.use_circular_temp_display {
            // Circular gauges are larger
            required_height += 60;
        } else {
            // Simple text display
            if config.show_cpu_temp {
                required_height += 25;
            }
            if config.show_gpu_temp {
                required_height += 25;
            }
        }
    }
    
    // === Network Section ===
    // Upload/Download rates (if enabled)
    if config.show_network {
        required_height += 50; // Two lines: RX and TX
    }
    
    // === Storage Section ===
    // Dynamic based on mounted disk count
    if config.show_storage && disk_count > 0 {
        required_height += SECTION_SPACING;
        required_height += HEADER_HEIGHT; // "Storage" header
        // Each disk: name (20px) + bar (12px) + spacing (13px) = 45px
        required_height += disk_count as u32 * 45;
    }
    
    // === Disk I/O Section ===
    // Read/Write rates (if enabled, separate from storage)
    if config.show_disk {
        required_height += 50;
    }
    
    // === Weather Section ===
    // Icon + temperature + description
    if config.show_weather {
        required_height += SECTION_SPACING;
        required_height += HEADER_HEIGHT; // "Weather" header
        required_height += 70; // Icon and text content
    }

    // === Battery Section ===
    // Dynamic based on device count
    if config.show_battery {
        required_height += SECTION_SPACING;
        required_height += HEADER_HEIGHT; // "Battery" header
        if battery_count > 0 {
            // Each device: name (28px) + icon/percentage (38px) = 66px
            required_height += battery_count as u32 * 66;
        } else {
            // "No devices" placeholder
            required_height += 25;
        }
    }
    
    // === Notifications Section ===
    // Dynamic based on notification count (capped at 5)
    if config.show_notifications {
        required_height += SECTION_SPACING;
        required_height += HEADER_HEIGHT; // "Notifications" header
        if notification_count > 0 {
            // Each notification: app (18px) + summary (20px) + body (18px) + spacing (5px) = 61px
            // Plus some extra for grouped headers
            let displayed_count = notification_count.min(5);
            required_height += displayed_count as u32 * 63;
        } else {
            // "No notifications" placeholder
            required_height += 25;
        }
    }
    
    // === Media Player Section ===
    // Now playing from Cider
    if config.show_media {
        required_height += SECTION_SPACING;
        required_height += 28; // "Now Playing" header (smaller)
        required_height += 145; // Panel: title, artist, album, progress, controls
        if player_count > 1 {
            required_height += 36; // Extra space for pagination dots
        }
        required_height += 15; // Bottom padding after panel
    }
    
    // Final padding
    required_height += BOTTOM_PADDING;
    
    // Enforce minimum height
    required_height.max(MINIMUM_HEIGHT)
}
