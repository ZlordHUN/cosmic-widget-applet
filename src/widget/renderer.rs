// SPDX-License-Identifier: MPL-2.0

//! # Widget Rendering Module
//!
//! This is the core rendering module for the COSMIC Monitor Widget. It uses
//! Cairo for 2D graphics and Pango for text rendering to draw all widget
//! sections onto an ARGB32 image surface.
//!
//! ## Architecture
//!
//! The renderer operates on raw pixel buffers provided by the Wayland compositor.
//! It creates a Cairo ImageSurface wrapping the buffer and draws using Cairo's
//! immediate-mode API.
//!
//! ```text
//! ┌─────────────────────┐
//! │  Wayland Buffer     │  Raw ARGB32 pixel data
//! │  (shared memory)    │
//! └──────────┬──────────┘
//!            │
//! ┌──────────▼──────────┐
//! │  Cairo ImageSurface │  Wraps buffer with unsafe lifetime extension
//! │  (Format::ARgb32)   │
//! └──────────┬──────────┘
//!            │
//! ┌──────────▼──────────┐
//! │  Cairo Context      │  Drawing operations
//! │  + Pango Layout     │  Text rendering
//! └──────────┬──────────┘
//!            │
//!     ┌──────┴──────┬───────────┬───────────┬─────────┐
//!     ▼             ▼           ▼           ▼         ▼
//! DateTime    Utilization   Temperature   Storage   Weather
//!   Section    (CPU/GPU)     (Gauges)     (Disks)   (Icons)
//!     ▼             ▼           ▼           ▼         ▼
//!  Battery    Notifications    Media      Network   (Legacy)
//! ```
//!
//! ## Rendering Pipeline
//!
//! 1. Create Cairo surface from raw buffer (unsafe lifetime extension)
//! 2. Clear background to transparent (ARGB 0,0,0,0)
//! 3. Iterate through configured section order
//! 4. Each section renders at current Y position, returns new Y
//! 5. Flush surface to ensure all operations complete
//! 6. Return click bounds for interactive elements
//!
//! ## Text Rendering Strategy
//!
//! All text is rendered with a black outline for visibility on any background:
//! 1. Create Pango layout with text content
//! 2. Convert layout to Cairo path (`pangocairo::functions::layout_path`)
//! 3. Stroke path with black (outline)
//! 4. Fill path with white or color (text body)
//!
//! ## Interactive Element Bounds
//!
//! The renderer returns bounding boxes for clickable elements:
//! - Notification groups (expand/collapse)
//! - Notification clear buttons (per-notification and per-group)
//! - Clear All button
//! - Media playback controls (prev/play/pause/next)
//!
//! These bounds are used by widget_main.rs to handle click events.

use cairo;
use pango;
use pangocairo;

use super::utilization::{draw_cpu_icon, draw_ram_icon, draw_gpu_icon, draw_progress_bar};
use super::temperature::draw_temp_circle;
use super::weather::draw_weather_icon;
use super::storage::DiskInfo;
use super::battery::BatteryDevice;
use super::notifications::Notification;
use super::media::MediaInfo;
use super::theme::CosmicTheme;
use crate::config::WidgetSection;

// ============================================================================
// Render Parameters Struct
// ============================================================================

/// All parameters needed to render the widget.
///
/// This struct aggregates all data sources and configuration flags needed
/// for a single render pass. It's created fresh each frame with current
/// monitor readings and settings.
///
/// # Data Sources
///
/// - **Utilization**: CPU, memory, GPU percentages from UtilizationMonitor
/// - **Temperature**: CPU/GPU temps from TemperatureMonitor
/// - **Network**: RX/TX rates from NetworkMonitor
/// - **Storage**: Disk info array from StorageMonitor
/// - **Battery**: Device array from BatteryMonitor
/// - **Weather**: Data from WeatherMonitor
/// - **Notifications**: Grouped notifications from NotificationMonitor
/// - **Media**: Playback info from MediaMonitor
/// - **Theme**: COSMIC desktop theme (accent color, dark/light mode)
///
/// # Configuration Flags
///
/// Boolean flags control which sections to render. These come from the
/// user's settings and allow selective display of widget sections.
///
/// # Section Order
///
/// The `section_order` array determines the vertical arrangement of sections.
/// Users can reorder sections in the settings UI.
pub struct RenderParams<'a> {
    /// Surface width in pixels
    pub width: i32,
    /// Surface height in pixels
    pub height: i32,
    
    // Utilization data
    /// CPU usage percentage (0.0 - 100.0)
    pub cpu_usage: f32,
    /// Memory usage percentage (0.0 - 100.0)
    pub memory_usage: f32,
    /// GPU usage percentage (0.0 - 100.0)
    pub gpu_usage: f32,
    
    // Temperature data
    /// CPU temperature in Celsius
    pub cpu_temp: f32,
    /// GPU temperature in Celsius
    pub gpu_temp: f32,
    
    // Network data
    /// Network download rate in bytes per second
    pub network_rx_rate: f64,
    /// Network upload rate in bytes per second
    pub network_tx_rate: f64,
    
    // Section visibility flags
    /// Show CPU utilization bar
    pub show_cpu: bool,
    /// Show memory utilization bar
    pub show_memory: bool,
    /// Show network stats (legacy, not in section order yet)
    pub show_network: bool,
    /// Show disk I/O stats (legacy, not in section order yet)
    pub show_disk: bool,
    /// Show storage/disk usage section
    pub show_storage: bool,
    /// Show GPU utilization bar
    pub show_gpu: bool,
    /// Show CPU temperature
    pub show_cpu_temp: bool,
    /// Show GPU temperature
    pub show_gpu_temp: bool,
    /// Show clock (time)
    pub show_clock: bool,
    /// Show date
    pub show_date: bool,
    /// Show percentage text next to progress bars
    pub show_percentages: bool,
    /// Use 24-hour time format (vs 12-hour with AM/PM)
    pub use_24hour_time: bool,
    /// Use circular gauge display for temperatures
    pub use_circular_temp_display: bool,
    /// Show weather section
    pub show_weather: bool,
    /// Show battery/peripheral section
    pub show_battery: bool,
    /// Show notifications section
    pub show_notifications: bool,
    /// Show media player section
    pub show_media: bool,
    /// Enable Solaar integration for Logitech devices
    pub enable_solaar_integration: bool,
    
    // Weather data
    /// Current temperature from weather API
    pub weather_temp: f32,
    /// Weather description (e.g., "Partly cloudy")
    pub weather_desc: &'a str,
    /// Location name from weather API
    pub weather_location: &'a str,
    /// Weather icon code (e.g., "01d", "10n")
    pub weather_icon: &'a str,
    
    // Complex data references
    /// Array of disk information for storage section
    pub disk_info: &'a [DiskInfo],
    /// Array of battery device information
    pub battery_devices: &'a [BatteryDevice],
    /// Pre-grouped notifications (app_name, notifications)
    pub grouped_notifications: &'a [(String, Vec<Notification>)],
    /// Set of collapsed notification group names
    pub collapsed_groups: &'a std::collections::HashSet<String>,
    /// Current media playback information
    pub media_info: &'a MediaInfo,
    /// Ordered list of sections to render
    pub section_order: &'a [WidgetSection],
    /// Current local time for clock/date display
    pub current_time: chrono::DateTime<chrono::Local>,
    /// COSMIC desktop theme settings (colors, dark/light mode)
    pub theme: &'a CosmicTheme,
}

// ============================================================================
// Type Aliases
// ============================================================================

/// Media button hit-test bounds: (button_name, x_start, y_start, x_end, y_end)
///
/// Used for detecting clicks on media playback controls.
/// Button names: "previous", "play_pause", "next"
pub type MediaButtonBounds = Vec<(String, f64, f64, f64, f64)>;

// ============================================================================
// Main Rendering Functions
// ============================================================================

/// Main rendering function for the complete widget.
///
/// Renders all enabled sections onto the provided pixel buffer and returns
/// bounds for all interactive elements (notifications and media controls).
///
/// # Arguments
///
/// * `canvas` - Mutable ARGB32 pixel buffer (width * height * 4 bytes)
/// * `params` - All render parameters including data and configuration
///
/// # Returns
///
/// Tuple of interactive element bounds:
/// - `notification_section_bounds`: Y range of notification section
/// - `group_bounds`: Vec of (app_name, y_start, y_end) for groups
/// - `clear_button_bounds`: Vec of (id, x1, y1, x2, y2) for X buttons
/// - `clear_all_bounds`: Optional bounds for "Clear All" button
/// - `media_button_bounds`: Vec of media control button bounds
///
/// # Safety
///
/// Uses unsafe to extend the lifetime of the canvas buffer for Cairo.
/// This is safe because:
/// 1. The ImageSurface is dropped before the function returns
/// 2. The canvas buffer outlives all Cairo operations
/// 3. The surface is flushed before returning
pub fn render_widget(canvas: &mut [u8], params: RenderParams) -> (Option<(f64, f64)>, Vec<(String, f64, f64)>, Vec<(String, f64, f64, f64, f64)>, Option<(f64, f64, f64, f64)>, MediaButtonBounds) {
    // Use unsafe to extend the lifetime for Cairo
    // This is safe because the surface doesn't outlive the canvas buffer
    let surface = unsafe {
        let ptr = canvas.as_mut_ptr();
        let len = canvas.len();
        let static_slice: &'static mut [u8] = std::slice::from_raw_parts_mut(ptr, len);
        
        cairo::ImageSurface::create_for_data(
            static_slice,
            cairo::Format::ARgb32,
            params.width,
            params.height,
            params.width * 4,
        )
        .expect("Failed to create cairo surface")
    };

    let mut notification_bounds: Option<(f64, f64)> = None;
    let mut notification_group_bounds: Vec<(String, f64, f64)> = Vec::new();
    let mut notification_clear_bounds: Vec<(String, f64, f64, f64, f64)> = Vec::new();
    let mut clear_all_bounds: Option<(f64, f64, f64, f64)> = None;
    let mut media_button_bounds: MediaButtonBounds = Vec::new();

    {
        let cr = cairo::Context::new(&surface).expect("Failed to create cairo context");

        // Clear background to fully transparent
        cr.save().expect("Failed to save");
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().expect("Failed to clear");
        cr.restore().expect("Failed to restore");

        // Set up Pango for text rendering
        let layout = pangocairo::functions::create_layout(&cr);
        
        // Track vertical position
        let mut y_pos = 10.0;
        
        // Render sections
        if params.show_clock || params.show_date {
            y_pos = render_datetime(&cr, &layout, y_pos, params.show_clock, params.show_date, params.use_24hour_time, &params.current_time);
            y_pos += 20.0; // Spacing after datetime
        } else {
            y_pos = 10.0; // Start at top if no clock/date
        }
        
        // Render sections in the configured order
        for section in params.section_order {
            match section {
                WidgetSection::Utilization => {
                    if params.show_cpu || params.show_memory || params.show_gpu {
                        y_pos = render_utilization(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Temperatures => {
                    if params.show_cpu_temp || params.show_gpu_temp {
                        y_pos += 10.0; // Spacing before temperature section
                        y_pos = render_temperatures(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Storage => {
                    if params.show_storage {
                        y_pos += 10.0; // Spacing before storage section
                        y_pos = render_storage(&cr, &layout, y_pos, params.disk_info, params.show_percentages);
                    }
                }
                WidgetSection::Battery => {
                    if params.show_battery {
                        y_pos += 10.0; // Spacing before battery section
                        y_pos = render_battery_section(
                            &cr,
                            &layout,
                            y_pos,
                            params.battery_devices,
                            params.enable_solaar_integration,
                        );
                    }
                }
                WidgetSection::Weather => {
                    if params.show_weather {
                        y_pos += 10.0; // Spacing before weather section
                        y_pos = render_weather(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Notifications => {
                    if params.show_notifications {
                        y_pos += 10.0; // Spacing before notifications section
                        let (new_y, bounds, groups, clear_bounds, clear_all) = render_notifications(
                            &cr,
                            &layout,
                            y_pos,
                            params.grouped_notifications,
                            params.collapsed_groups,
                            params.theme,
                        );
                        y_pos = new_y;
                        notification_bounds = Some(bounds);
                        notification_group_bounds = groups;
                        notification_clear_bounds = clear_bounds;
                        clear_all_bounds = clear_all;
                    }
                }
                WidgetSection::Media => {
                    if params.show_media {
                        y_pos += 10.0; // Spacing before media section
                        let (new_y, buttons) = render_media(&cr, &layout, y_pos, params.media_info, params.theme);
                        y_pos = new_y;
                        media_button_bounds = buttons;
                    }
                }
            }
        }
        
        // Render network and disk (not yet in reorderable sections)
        if params.show_network {
            y_pos = render_network(&cr, &layout, y_pos, params.network_rx_rate, params.network_tx_rate);
        }
        
        if params.show_disk {
            y_pos = render_disk(&cr, &layout, y_pos);
        }
    }
    
    // Ensure Cairo surface is flushed
    surface.flush();
    
    (notification_bounds, notification_group_bounds, notification_clear_bounds, clear_all_bounds, media_button_bounds)
}

// ============================================================================
// Alternative Rendering Functions (Unused but kept for split-surface architecture)
// ============================================================================

/// Render main widget WITHOUT notifications (for split surface architecture).
///
/// This function was designed for a split-surface approach where notifications
/// would be rendered on a separate surface. Currently unused but kept for
/// potential future use.
///
/// # Note
///
/// This is marked as dead code by the compiler. The current implementation
/// uses a single surface for all rendering.
#[allow(dead_code)]
pub fn render_main_widget(canvas: &mut [u8], params: RenderParams) -> (Vec<(String, f64, f64)>, Vec<(String, f64, f64, f64, f64)>, Option<(f64, f64, f64, f64)>) {
    // Use unsafe to extend the lifetime for Cairo
    let surface = unsafe {
        let ptr = canvas.as_mut_ptr();
        let len = canvas.len();
        let static_slice: &'static mut [u8] = std::slice::from_raw_parts_mut(ptr, len);
        
        cairo::ImageSurface::create_for_data(
            static_slice,
            cairo::Format::ARgb32,
            params.width,
            params.height,
            params.width * 4,
        )
        .expect("Failed to create cairo surface")
    };

    let mut notification_bounds = (Vec::new(), Vec::new(), None);

    {
        let cr = cairo::Context::new(&surface).expect("Failed to create cairo context");

        // Clear background to fully transparent
        cr.save().expect("Failed to save");
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().expect("Failed to clear");
        cr.restore().expect("Failed to restore");

        // Set up Pango for text rendering
        let layout = pangocairo::functions::create_layout(&cr);
        
        // Track vertical position
        let mut y_pos = 10.0;
        
        // Render sections (excluding notifications)
        if params.show_clock || params.show_date {
            y_pos = render_datetime(&cr, &layout, y_pos, params.show_clock, params.show_date, params.use_24hour_time, &params.current_time);
            y_pos += 20.0; // Spacing after datetime
        } else {
            y_pos = 10.0; // Start at top if no clock/date
        }
        
        // Render sections in the configured order (skip notifications)
        for section in params.section_order {
            match section {
                WidgetSection::Utilization => {
                    if params.show_cpu || params.show_memory || params.show_gpu {
                        y_pos = render_utilization(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Temperatures => {
                    if params.show_cpu_temp || params.show_gpu_temp {
                        y_pos += 10.0;
                        y_pos = render_temperatures(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Storage => {
                    if params.show_storage {
                        y_pos += 10.0;
                        y_pos = render_storage(&cr, &layout, y_pos, params.disk_info, params.show_percentages);
                    }
                }
                WidgetSection::Battery => {
                    if params.show_battery {
                        y_pos += 10.0;
                        y_pos = render_battery_section(
                            &cr,
                            &layout,
                            y_pos,
                            params.battery_devices,
                            params.enable_solaar_integration,
                        );
                    }
                }
                WidgetSection::Weather => {
                    if params.show_weather {
                        y_pos += 10.0;
                        y_pos = render_weather(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Notifications => {
                    // Render notifications directly on main surface
                    if params.show_notifications {
                        let (new_y, _bounds, groups, clear_bounds, clear_all) = render_notifications(&cr, &layout, y_pos, params.grouped_notifications, params.collapsed_groups, params.theme);
                        y_pos = new_y;  // Update y_pos so next section knows where to start
                        notification_bounds = (groups, clear_bounds, clear_all);
                    }
                }
                WidgetSection::Media => {
                    if params.show_media {
                        y_pos += 10.0;
                        let (new_y, _buttons) = render_media(&cr, &layout, y_pos, params.media_info, params.theme);
                        y_pos = new_y;
                    }
                }
            }
        }
    }
    
    surface.flush();
    notification_bounds
}

/// Render ONLY notifications on separate surface (for split surface architecture).
///
/// This function was designed for a split-surface approach where notifications
/// would be rendered independently. Currently unused but kept for potential
/// future use.
///
/// # Note
///
/// This is marked as dead code by the compiler. The current implementation
/// uses a single surface for all rendering.
#[allow(dead_code)]
pub fn render_notification_surface(
    canvas: &mut [u8], 
    width: i32,
    height: i32,
    grouped_notifications: &[(String, Vec<Notification>)],
    collapsed_groups: &std::collections::HashSet<String>,
) -> (Vec<(String, f64, f64)>, Vec<(String, f64, f64, f64, f64)>, Option<(f64, f64, f64, f64)>) {
    let surface = unsafe {
        let ptr = canvas.as_mut_ptr();
        let len = canvas.len();
        let static_slice: &'static mut [u8] = std::slice::from_raw_parts_mut(ptr, len);
        
        cairo::ImageSurface::create_for_data(
            static_slice,
            cairo::Format::ARgb32,
            width,
            height,
            width * 4,
        )
        .expect("Failed to create cairo surface")
    };

    let mut notification_group_bounds: Vec<(String, f64, f64)> = Vec::new();
    let mut notification_clear_bounds: Vec<(String, f64, f64, f64, f64)> = Vec::new();
    let mut clear_all_bounds: Option<(f64, f64, f64, f64)> = None;

    {
        let cr = cairo::Context::new(&surface).expect("Failed to create cairo context");

        // Clear background to fully transparent
        cr.save().expect("Failed to save");
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().expect("Failed to clear");
        cr.restore().expect("Failed to restore");

        // Set up Pango for text rendering
        let layout = pangocairo::functions::create_layout(&cr);
        
        // Use default theme for standalone notification surface
        let theme = CosmicTheme::default();
        
        // Render notifications starting from top
        let (_new_y, _bounds, groups, clear_bounds, clear_all) = render_notifications(
            &cr, 
            &layout, 
            10.0,  // Start at top with small padding
            grouped_notifications,
            collapsed_groups,
            &theme,
        );
        
        notification_group_bounds = groups;
        notification_clear_bounds = clear_bounds;
        clear_all_bounds = clear_all;
    }
    
    surface.flush();
    
    (notification_group_bounds, notification_clear_bounds, clear_all_bounds)
}

// ============================================================================
// DateTime Section
// ============================================================================

/// Render date and time display at the top of the widget.
///
/// The clock is rendered with a large font (48pt) for hours and minutes,
/// with seconds in a smaller font (28pt) to the right. For 12-hour format,
/// AM/PM is appended after seconds.
///
/// # Clock Format Examples
///
/// - 24-hour: `14:30:45`
/// - 12-hour: `2:30:45 PM`
///
/// # Date Format
///
/// Full weekday, day, month, year: `Wednesday, 15 January 2025`
///
/// # Visual Layout
///
/// ```text
/// 14:30 :45      ← Clock (large + small seconds)
/// Wednesday, 15 January 2025  ← Date
/// ```
fn render_datetime(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    show_clock: bool,
    show_date: bool,
    use_24hour_time: bool,
    now: &chrono::DateTime<chrono::Local>,
) -> f64 {
    let mut y_pos = y_start;
    
    if show_clock {
        // Draw large time (HH:MM or h:MM based on format)
        let time_str = if use_24hour_time {
            now.format("%H:%M").to_string()
        } else {
            now.format("%-I:%M").to_string()
        };
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 48");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&time_str);
        
        // White text with black outline
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.move_to(10.0, y_pos);
        
        // Draw outline
        cr.set_line_width(3.0);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        
        // Fill with white
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // Get width of the time text to position seconds correctly
        let (time_width, _) = layout.pixel_size();
        
        // Draw seconds (:SS) slightly smaller and raised
        let seconds_str = now.format(":%S").to_string();
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 28");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&seconds_str);
        
        cr.move_to(10.0 + time_width as f64, y_pos + 5.0);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // For 12-hour format, add AM/PM indicator
        if !use_24hour_time {
            let ampm_str = now.format(" %p").to_string();
            let font_desc = pango::FontDescription::from_string("Ubuntu Bold 20");
            layout.set_font_description(Some(&font_desc));
            layout.set_text(&ampm_str);
            
            let (seconds_width, _) = layout.pixel_size();
            cr.move_to(10.0 + time_width as f64 + seconds_width as f64, y_pos + 10.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y_pos += 70.0; // Move down after clock
    }
    
    if show_date {
        // Draw date below with more spacing
        let date_str = now.format("%A, %d %B %Y").to_string();
        let font_desc = pango::FontDescription::from_string("Ubuntu 16");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&date_str);
        
        cr.move_to(10.0, y_pos);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        y_pos += 35.0; // Move down after date
    }
    
    y_pos
}

// ============================================================================
// Section Rendering Functions
// ============================================================================
// Each function renders a specific section of the widget and returns the
// Y position after rendering (for vertical stacking).

/// Render CPU, RAM, and GPU utilization bars.
///
/// Displays each enabled resource with:
/// - Icon (CPU chip, RAM stick, GPU card)
/// - Label text
/// - Progress bar with color-coded fill (green/yellow/red)
/// - Optional percentage text
///
/// # Layout
///
/// ```text
/// Utilization
/// [CPU icon] CPU: [████████░░░░] 75.2%
/// [RAM icon] RAM: [██████░░░░░░] 52.1%
/// [GPU icon] GPU: [██░░░░░░░░░░] 23.5%
/// ```
fn render_utilization(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    let icon_size = 20.0;
    let bar_width = 200.0;
    let bar_height = 12.0;
    
    // Draw section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Utilization");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    y += 35.0;
    
    // Set normal font for items
    let font_desc = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&font_desc));
    cr.set_line_width(2.0);
    
    if params.show_cpu {
        draw_cpu_icon(cr, 10.0, y - 2.0, icon_size);
        
        layout.set_text("CPU:");
        cr.move_to(10.0 + icon_size + 10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        draw_progress_bar(cr, 90.0, y, bar_width, bar_height, params.cpu_usage);
        
        if params.show_percentages {
            let cpu_text = format!("{:.1}%", params.cpu_usage);
            layout.set_text(&cpu_text);
            cr.move_to(300.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 30.0;
    }
    
    if params.show_memory {
        draw_ram_icon(cr, 10.0, y - 2.0, icon_size);
        
        layout.set_text("RAM:");
        cr.move_to(10.0 + icon_size + 10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        draw_progress_bar(cr, 90.0, y, bar_width, bar_height, params.memory_usage);
        
        if params.show_percentages {
            let mem_text = format!("{:.1}%", params.memory_usage);
            layout.set_text(&mem_text);
            cr.move_to(300.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 30.0;
    }
    
    if params.show_gpu {
        draw_gpu_icon(cr, 10.0, y - 2.0, icon_size);
        
        layout.set_text("GPU:");
        cr.move_to(10.0 + icon_size + 10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        draw_progress_bar(cr, 90.0, y, bar_width, bar_height, params.gpu_usage);
        
        if params.show_percentages {
            let gpu_text = format!("{:.1}%", params.gpu_usage);
            layout.set_text(&gpu_text);
            cr.move_to(300.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 30.0;
    }
    
    y
}

/// Render temperature section (CPU and GPU temps).
///
/// Supports two display modes controlled by `use_circular_temp_display`:
/// - **Circular**: Animated gauge rings with color-coded fill
/// - **Text**: Simple text display "CPU: 45.2°C"
///
/// # Layout (Circular Mode)
///
/// ```text
/// Temperatures
///  ╭───╮  ╭───╮
/// │ 45°│ │ 52°│
///  ╰───╯  ╰───╯
///   CPU    GPU
/// ```
fn render_temperatures(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    
    // Draw section header
    let font_desc = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&font_desc));
    layout.set_text("Temperatures");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0;
    
    // Delegate to circular or text renderer based on settings
    if params.use_circular_temp_display {
        y = render_circular_temps(cr, layout, y, params);
    } else {
        y = render_text_temps(cr, layout, y, params);
    }
    
    y
}

/// Render circular temperature gauges side by side.
///
/// Draws hollow ring gauges that fill based on temperature. The ring
/// color changes based on temperature percentage of max (100°C):
/// - Green: < 50%
/// - Yellow: 50-80%
/// - Red: > 80%
fn render_circular_temps(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let y = y_start;
    let circle_radius = 25.0;
    let circle_diameter = circle_radius * 2.0;
    let spacing = 20.0;
    let mut x_offset = 15.0;
    let max_temp = 100.0;
    
    if params.show_cpu_temp {
        draw_temp_circle(cr, x_offset, y, circle_radius, params.cpu_temp, max_temp);
        
        // Temperature value in center
        let temp_text = if params.cpu_temp > 0.0 {
            format!("{:.0}°", params.cpu_temp)
        } else {
            "N/A".to_string()
        };
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 12");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&temp_text);
        let (text_width, text_height) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - text_width as f64 / 2.0,
            y + circle_radius - text_height as f64 / 2.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // "CPU" label below circle
        let label_font = pango::FontDescription::from_string("Ubuntu 10");
        layout.set_font_description(Some(&label_font));
        layout.set_text("CPU");
        let (label_width, _) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - label_width as f64 / 2.0,
            y + circle_diameter + 6.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        x_offset += circle_diameter + spacing;
    }
    
    if params.show_gpu_temp {
        draw_temp_circle(cr, x_offset, y, circle_radius, params.gpu_temp, max_temp);
        
        // Temperature value in center
        let temp_text = if params.gpu_temp > 0.0 {
            format!("{:.0}°", params.gpu_temp)
        } else {
            "N/A".to_string()
        };
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 12");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&temp_text);
        let (text_width, text_height) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - text_width as f64 / 2.0,
            y + circle_radius - text_height as f64 / 2.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // "GPU" label below circle
        let label_font = pango::FontDescription::from_string("Ubuntu 10");
        layout.set_font_description(Some(&label_font));
        layout.set_text("GPU");
        let (label_width, _) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - label_width as f64 / 2.0,
            y + circle_diameter + 6.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
    }
    
    y + circle_diameter + 15.0
}

/// Render text-based temperatures
fn render_text_temps(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    let font_desc = pango::FontDescription::from_string("Ubuntu 14");
    layout.set_font_description(Some(&font_desc));
    
    if params.show_cpu_temp {
        if params.cpu_temp > 0.0 {
            layout.set_text(&format!("  CPU: {:.1}°C", params.cpu_temp));
        } else {
            layout.set_text("  CPU: N/A");
        }
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
    }
    
    if params.show_gpu_temp {
        if params.gpu_temp > 0.0 {
            layout.set_text(&format!("  GPU: {:.1}°C", params.gpu_temp));
        } else {
            layout.set_text("  GPU: N/A");
        }
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
    }
    
    y
}

/// Render network stats
fn render_network(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    rx_rate: f64,
    tx_rate: f64,
) -> f64 {
    let mut y = y_start;
    
    layout.set_text(&format!("Network ↓: {:.1} KB/s", rx_rate / 1024.0));
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    layout.set_text(&format!("Network ↑: {:.1} KB/s", tx_rate / 1024.0));
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    y
}

/// Render disk stats
fn render_disk(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
) -> f64 {
    let mut y = y_start;
    
    layout.set_text("Disk Read: 0.0 KB/s");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    layout.set_text("Disk Write: 0.0 KB/s");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    y
}

/// Temporary battery section placeholder until Solaar integration is implemented
fn render_battery_section(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    devices: &[BatteryDevice],
    enable_solaar_integration: bool,
) -> f64 {
    let mut y = y_start;

    // Section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Battery");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0;

    // Simple text to indicate Solaar integration state
    let font_desc = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&font_desc));

    if !enable_solaar_integration {
        layout.set_text("Solaar integration disabled");
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
        return y;
    }

    if devices.is_empty() {
        layout.set_text("No Solaar devices detected");
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
        return y;
    }

    let icon_size = 24.0;

    for device in devices {
        // Draw device name
        layout.set_text(&device.name);
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 28.0;

        if !device.is_connected {
            // Device is disconnected - show disconnected icon
            draw_disconnected_icon(cr, 10.0, y - 2.0, icon_size);
            
            // Draw "Disconnected" text
            layout.set_text("Disconnected");
            cr.move_to(10.0 + icon_size + 8.0, y - 2.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(0.7, 0.7, 0.7);
            cr.fill().expect("Failed to fill");
            
            y += 38.0;
        } else if device.is_loading {
            // Device is connected but loading - show disconnected icon with "Connecting..." text
            draw_disconnected_icon(cr, 10.0, y - 2.0, icon_size);
            
            // Draw "Connecting..." text
            layout.set_text("Connecting...");
            cr.move_to(10.0 + icon_size + 8.0, y - 2.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(0.7, 0.7, 0.7);
            cr.fill().expect("Failed to fill");
            
            y += 38.0;
        } else if let Some(level) = device.level {
            // Check if device is charging (use lowercase and check for "recharging" or starts with "charging")
            let is_charging = device.status.as_deref()
                .map(|s| {
                    let lower = s.to_lowercase();
                    lower.starts_with("charging") || lower.starts_with("recharging")
                })
                .unwrap_or(false);
            
            // Draw vertical battery icon
            draw_battery_icon(cr, 10.0, y - 2.0, icon_size, level);
            
            // If charging, draw a lightning bolt overlay
            if is_charging {
                draw_charging_indicator(cr, 10.0, y - 2.0, icon_size);
            }

            // Draw percentage text next to battery with charging indicator
            let percentage_text = if is_charging {
                format!("{}% ⚡", level)
            } else {
                format!("{}%", level)
            };
            layout.set_text(&percentage_text);
            cr.move_to(10.0 + icon_size + 8.0, y - 2.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");

            y += 38.0; // Increased spacing between devices
        } else {
            // No battery level available
            layout.set_text("  Battery: N/A");
            cr.move_to(10.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            y += 38.0; // Increased spacing between devices
        }
    }

    y
}

/// Draw a vertical battery icon with fill level
fn draw_battery_icon(cr: &cairo::Context, x: f64, y: f64, size: f64, level: u8) {
    let (r, g, b) = get_battery_color(level);
    let body_height = size;
    let body_width = size * 0.6;
    let terminal_height = size * 0.1;
    let terminal_width = body_width * 0.4;
    
    // Battery terminal (small rectangle on top)
    let terminal_x = x + (body_width - terminal_width) / 2.0;
    cr.rectangle(terminal_x, y, terminal_width, terminal_height);
    cr.set_source_rgb(0.6, 0.6, 0.6);
    cr.fill_preserve().expect("Failed to fill");
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.0);
    cr.stroke().expect("Failed to stroke");
    
    // Battery body (vertical rectangle)
    let body_y = y + terminal_height;
    cr.rectangle(x, body_y, body_width, body_height);
    cr.set_source_rgb(0.2, 0.2, 0.2);
    cr.fill_preserve().expect("Failed to fill");
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
    
    // Fill level indicator inside battery (from bottom up)
    if level > 0 {
        let fill_height = (body_height - 4.0) * (level as f64 / 100.0);
        let fill_y = body_y + body_height - 2.0 - fill_height;
        cr.rectangle(x + 2.0, fill_y, body_width - 4.0, fill_height);
        cr.set_source_rgb(r, g, b);
        cr.fill().expect("Failed to fill");
    }
}

/// Draw a disconnected/loading icon for battery devices
fn draw_disconnected_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw a battery outline in gray with a slash through it
    let body_height = size;
    let body_width = size * 0.6;
    let terminal_height = size * 0.1;
    let terminal_width = body_width * 0.4;
    
    // Battery terminal (gray)
    let terminal_x = x + (body_width - terminal_width) / 2.0;
    cr.rectangle(terminal_x, y, terminal_width, terminal_height);
    cr.set_source_rgb(0.5, 0.5, 0.5);
    cr.fill_preserve().expect("Failed to fill");
    cr.set_source_rgb(0.3, 0.3, 0.3);
    cr.set_line_width(1.0);
    cr.stroke().expect("Failed to stroke");
    
    // Battery body (gray outline, no fill)
    let body_y = y + terminal_height;
    cr.rectangle(x, body_y, body_width, body_height);
    cr.set_source_rgb(0.5, 0.5, 0.5);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
    
    // Draw diagonal slash to indicate disconnected
    cr.move_to(x, body_y);
    cr.line_to(x + body_width, body_y + body_height);
    cr.set_source_rgb(0.8, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a charging indicator (lightning bolt) overlay on battery icon
fn draw_charging_indicator(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    let body_width = size * 0.6;
    let body_height = size;
    let terminal_height = size * 0.1;
    let body_y = y + terminal_height;
    
    // Draw lightning bolt in center of battery
    let bolt_x = x + body_width / 2.0;
    let bolt_y = body_y + body_height * 0.2;
    let bolt_height = body_height * 0.6;
    let bolt_width = body_width * 0.4;
    
    cr.save().expect("Failed to save");
    cr.set_source_rgba(1.0, 1.0, 0.0, 0.9); // Yellow with slight transparency
    cr.set_line_width(2.0);
    
    // Draw lightning bolt shape
    cr.move_to(bolt_x, bolt_y);
    cr.line_to(bolt_x - bolt_width / 3.0, bolt_y + bolt_height / 2.0);
    cr.line_to(bolt_x, bolt_y + bolt_height / 2.0);
    cr.line_to(bolt_x - bolt_width / 3.0, bolt_y + bolt_height);
    cr.stroke().expect("Failed to stroke");
    
    cr.move_to(bolt_x, bolt_y + bolt_height / 2.0);
    cr.line_to(bolt_x + bolt_width / 3.0, bolt_y);
    cr.stroke().expect("Failed to stroke");
    
    cr.restore().expect("Failed to restore");
}

/// Get RGB color based on battery level
fn get_battery_color(level: u8) -> (f64, f64, f64) {
    if level > 60 {
        (0.0, 0.8, 0.0) // Green
    } else if level > 30 {
        (1.0, 0.8, 0.0) // Yellow/Orange
    } else if level > 15 {
        (1.0, 0.5, 0.0) // Orange
    } else {
        (1.0, 0.0, 0.0) // Red
    }
}

/// Render weather section
fn render_weather(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    
    // Section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Weather");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 40.0;  // More space after header to prevent icon overlap
    
    // Draw weather icon (offset from left edge to prevent clipping)
    let icon_size = 40.0;
    draw_weather_icon(cr, 20.0, y, icon_size, params.weather_icon);
    
    // Weather info to the right of icon
    let info_x = 80.0;
    let font_desc = pango::FontDescription::from_string("Ubuntu 14");
    layout.set_font_description(Some(&font_desc));
    
    // Temperature
    if !params.weather_temp.is_nan() {
        layout.set_text(&format!("{:.1}°C", params.weather_temp));
    } else {
        layout.set_text("N/A");
    }
    cr.move_to(info_x, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Description
    layout.set_text(params.weather_desc);
    cr.move_to(info_x, y + 20.0);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Location
    let location_font = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&location_font));
    layout.set_text(params.weather_location);
    cr.move_to(info_x, y + 45.0);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(0.7, 0.7, 0.7);
    cr.fill().expect("Failed to fill");
    
    y + 70.0 // Return updated y position
}

/// Render storage/disk usage section
fn render_storage(cr: &cairo::Context, layout: &pango::Layout, y: f64, disk_info: &[DiskInfo], show_percentages: bool) -> f64 {
    let mut y = y;
    let bar_width = 200.0;
    let bar_height = 12.0;
    
    // Section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Storage");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0; // Spacing after header
    
    // Draw each disk
    let font_desc = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&font_desc));
    cr.set_line_width(2.0);
    
    for disk in disk_info {
        // Draw disk name/mount point
        layout.set_text(&disk.name);
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 20.0; // Space between name and bar
        
        // Draw progress bar (empty if loading, normal if ready)
        let percentage = if disk.is_loading { 0.0 } else { disk.used_percentage };
        draw_progress_bar(cr, 10.0, y, bar_width, bar_height, percentage);
        
        // Draw percentage if enabled
        if show_percentages {
            let percentage_text = if disk.is_loading {
                "Loading...".to_string()
            } else {
                format!("{:.1}%", disk.used_percentage)
            };
            layout.set_text(&percentage_text);
            cr.move_to(220.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 25.0; // Space after bar before next disk
    }
    
    y
}

/// Render notifications section with theme-aware colors.
///
/// Uses the COSMIC theme for panel backgrounds and text colors.
fn render_notifications(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    grouped_notifications: &[(String, Vec<Notification>)],
    collapsed_groups: &std::collections::HashSet<String>,
    theme: &CosmicTheme,
) -> (f64, (f64, f64), Vec<(String, f64, f64)>, Vec<(String, f64, f64, f64, f64)>, Option<(f64, f64, f64, f64)>) {  
    // Returns (new_y_pos, (section_y_start, section_y_end), group_bounds, clear_button_bounds, clear_all_bounds)
    
    let section_start = y_start;
    let mut y_pos = y_start;
    let mut group_bounds = Vec::new();
    let mut clear_button_bounds = Vec::new();
    let mut clear_all_bounds = None;
    
    // Get theme colors
    let (text_r, text_g, text_b) = theme.text_color();
    let (sec_r, sec_g, sec_b) = theme.secondary_text_color();
    let (panel_r, panel_g, panel_b, panel_a) = theme.panel_background();
    let (border_r, border_g, border_b, border_a) = theme.border_color();
    let (accent_r, accent_g, accent_b) = theme.accent_rgb();
    
    // Draw section header
    let font_desc = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&font_desc));
    layout.set_text("Notifications");
    
    // Get header height for vertical alignment
    let (_, header_height) = layout.pixel_size();
    
    cr.move_to(10.0, y_pos);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(text_r, text_g, text_b);
    cr.fill().expect("Failed to fill");
    
    // Draw "Clear All" button aligned vertically with header
    if !grouped_notifications.is_empty() {
        let button_width = 70.0;
        let button_height = 18.0;
        let button_x = 285.0;
        // Vertically center with header text
        let button_y = y_pos + (header_height as f64 - button_height) / 2.0;
        
        // Draw button background
        cr.set_source_rgba(0.8, 0.2, 0.2, 0.7); // Red with transparency
        cr.rectangle(button_x, button_y, button_width, button_height);
        cr.fill().expect("Failed to fill clear all button");
        
        // Draw button border
        cr.set_source_rgb(1.0, 0.3, 0.3); // Lighter red border
        cr.set_line_width(1.0);
        cr.rectangle(button_x, button_y, button_width, button_height);
        cr.stroke().expect("Failed to stroke clear all button");
        
        // Draw button text
        let font_desc_small = pango::FontDescription::from_string("Ubuntu Bold 9");
        layout.set_font_description(Some(&font_desc_small));
        layout.set_text("Clear All");
        
        cr.move_to(button_x + 10.0, button_y + 3.0);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(text_r, text_g, text_b);
        cr.fill().expect("Failed to fill");
        
        clear_all_bounds = Some((button_x, button_y, button_x + button_width, button_y + button_height));
    }
    
    y_pos += 35.0; // More space after header before groups
    
    // Render each notification group
    if grouped_notifications.is_empty() {
        // Show "No notifications" message
        let font_desc = pango::FontDescription::from_string("Ubuntu Italic 11");
        layout.set_font_description(Some(&font_desc));
        layout.set_text("No notifications");
        
        cr.move_to(15.0, y_pos);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(sec_r, sec_g, sec_b);
        cr.fill().expect("Failed to fill");
        
        y_pos += 25.0;
    } else {
        // Render each pre-grouped notification group (already sorted)
        for (app_name, group_notifs) in grouped_notifications.iter() {
            let group_y_start = y_pos;
            let is_collapsed = collapsed_groups.contains(app_name);
            
            // Calculate total height of this group for background
            let mut temp_y = y_pos + 22.0; // Header height
            if !is_collapsed {
                for notification in group_notifs.iter().take(5) {
                    temp_y += 20.0; // Summary line with X button
                    if !notification.body.is_empty() {
                        temp_y += 14.0; // Body
                    }
                    temp_y += 4.0; // Spacing
                }
            }
            let group_height = temp_y - group_y_start;
            
            // Draw semi-transparent background for the group (theme-aware)
            cr.set_source_rgba(panel_r, panel_g, panel_b, panel_a);
            cr.rectangle(10.0, group_y_start - 8.0, 360.0, group_height + 16.0);
            cr.fill().expect("Failed to fill background");
            
            // Draw border around the group (theme-aware)
            cr.set_source_rgba(border_r, border_g, border_b, border_a);
            cr.set_line_width(1.5);
            cr.rectangle(10.0, group_y_start - 8.0, 360.0, group_height + 16.0);
            cr.stroke().expect("Failed to stroke border");
            
            // Draw group header (app name with count and expand/collapse indicator)
            let font_desc_bold = pango::FontDescription::from_string("Ubuntu Bold 11");
            layout.set_font_description(Some(&font_desc_bold));
            
            let indicator = if is_collapsed { "▶" } else { "▼" };
            let header_text = format!("{} {} ({})", indicator, app_name, group_notifs.len());
            layout.set_text(&header_text);
            
            cr.move_to(15.0, y_pos);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            // Use accent color for app name header
            cr.set_source_rgb(accent_r * 1.2, accent_g * 1.2, accent_b * 1.2); // Slightly brighter accent
            cr.fill().expect("Failed to fill");
            
            // Draw X button to clear this group
            let x_button_size = 14.0;
            let x_button_x = 340.0; // Right side of the group
            let x_button_y = y_pos;
            
            // Draw X button background circle
            cr.set_source_rgba(0.8, 0.2, 0.2, 0.6); // Semi-transparent red
            cr.arc(x_button_x, x_button_y + 7.0, x_button_size / 2.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.fill().expect("Failed to fill X button background");
            
            // Draw X button border
            cr.set_source_rgb(1.0, 0.3, 0.3); // Lighter red border
            cr.set_line_width(1.0);
            cr.arc(x_button_x, x_button_y + 7.0, x_button_size / 2.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.stroke().expect("Failed to stroke X button border");
            
            // Draw X symbol
            let x_size = 4.0;
            let x_center_x = x_button_x;
            let x_center_y = y_pos + 7.0;
            
            cr.set_source_rgb(1.0, 1.0, 1.0); // White X
            cr.set_line_width(1.5);
            cr.move_to(x_center_x - x_size, x_center_y - x_size);
            cr.line_to(x_center_x + x_size, x_center_y + x_size);
            cr.stroke().expect("Failed to draw X line 1");
            
            cr.move_to(x_center_x + x_size, x_center_y - x_size);
            cr.line_to(x_center_x - x_size, x_center_y + x_size);
            cr.stroke().expect("Failed to draw X line 2");
            
            // Record X button bounds for click detection (group clear)
            clear_button_bounds.push((
                app_name.clone(),
                x_button_x - x_button_size / 2.0,
                x_button_y,
                x_button_x + x_button_size / 2.0,
                x_button_y + 14.0,
            ));
            
            y_pos += 22.0;
            let group_y_end = y_pos;
            
            // Record group header bounds for click detection
            group_bounds.push((app_name.clone(), group_y_start, group_y_end));
            
            // If not collapsed, show notifications in this group
            if !is_collapsed {
                let font_desc = pango::FontDescription::from_string("Ubuntu 11");
                
                for notification in group_notifs.iter().take(5) {
                    // Summary text (indented)
                    layout.set_font_description(Some(&font_desc));
                    
                    // Truncate summary if too long (leave room for X button)
                    let summary = if notification.summary.len() > 38 {
                        format!("{}...", &notification.summary[..35])
                    } else {
                        notification.summary.clone()
                    };
                    layout.set_text(&summary);
                    
                    cr.move_to(25.0, y_pos); // Indent notifications
                    pangocairo::functions::layout_path(cr, layout);
                    cr.set_source_rgb(0.0, 0.0, 0.0);
                    cr.stroke_preserve().expect("Failed to stroke");
                    cr.set_source_rgb(text_r, text_g, text_b);
                    cr.fill().expect("Failed to fill");
                    
                    // Draw individual dismiss X button for this notification
                    let notif_x_size = 10.0;
                    let notif_x_x = 340.0;
                    let notif_x_y = y_pos + 2.0;
                    
                    // Draw small X button background
                    cr.set_source_rgba(0.6, 0.2, 0.2, 0.5); // Subtle red
                    cr.arc(notif_x_x, notif_x_y + 5.0, notif_x_size / 2.0, 0.0, 2.0 * std::f64::consts::PI);
                    cr.fill().expect("Failed to fill notification X");
                    
                    // Draw X symbol (smaller)
                    let nx_size = 3.0;
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.set_line_width(1.0);
                    cr.move_to(notif_x_x - nx_size, notif_x_y + 5.0 - nx_size);
                    cr.line_to(notif_x_x + nx_size, notif_x_y + 5.0 + nx_size);
                    cr.stroke().expect("Failed to draw notif X line 1");
                    cr.move_to(notif_x_x + nx_size, notif_x_y + 5.0 - nx_size);
                    cr.line_to(notif_x_x - nx_size, notif_x_y + 5.0 + nx_size);
                    cr.stroke().expect("Failed to draw notif X line 2");
                    
                    // Record individual notification X button bounds
                    // Format: "app_name:timestamp" to identify the specific notification
                    let notif_id = format!("{}:{}", app_name, notification.timestamp);
                    clear_button_bounds.push((
                        notif_id,
                        notif_x_x - notif_x_size / 2.0,
                        notif_x_y,
                        notif_x_x + notif_x_size / 2.0,
                        notif_x_y + notif_x_size,
                    ));
                    
                    y_pos += 20.0;
                    
                    // Body text (if present and not too long)
                    if !notification.body.is_empty() {
                        let body = if notification.body.len() > 45 {
                            format!("{}...", &notification.body[..42])
                        } else {
                            notification.body.clone()
                        };
                        
                        let font_desc_small = pango::FontDescription::from_string("Ubuntu 9");
                        layout.set_font_description(Some(&font_desc_small));
                        layout.set_text(&body);
                        
                        cr.move_to(25.0, y_pos); // Indent body text
                        pangocairo::functions::layout_path(cr, layout);
                        cr.set_source_rgb(0.0, 0.0, 0.0);
                        cr.stroke_preserve().expect("Failed to stroke");
                        cr.set_source_rgb(sec_r, sec_g, sec_b); // Secondary color for body
                        cr.fill().expect("Failed to fill");
                        
                        y_pos += 14.0;
                    }
                    
                    y_pos += 4.0; // Small space between notifications in group
                }
            }
            
            y_pos += 8.0; // Space between groups
        }
    }
    
    y_pos += 10.0; // Section padding
    (y_pos, (section_start, y_pos), group_bounds, clear_button_bounds, clear_all_bounds)
}

/// Render media player section with theme-aware colors.
///
/// Uses the COSMIC theme accent color for the progress bar and play button.
/// Returns (y_position, button_bounds) where button_bounds is Vec<(button_name, x_start, y_start, x_end, y_end)>
fn render_media(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    media_info: &MediaInfo,
    theme: &CosmicTheme,
) -> (f64, MediaButtonBounds) {
    use super::media::PlaybackStatus;
    
    let mut y_pos = y_start;
    let mut button_bounds: MediaButtonBounds = Vec::new();
    
    // Get theme colors
    let (text_r, text_g, text_b) = theme.text_color();
    let (sec_r, sec_g, sec_b) = theme.secondary_text_color();
    let (panel_r, panel_g, panel_b, panel_a) = theme.panel_background();
    let (border_r, border_g, border_b, border_a) = theme.border_color();
    let (accent_r, accent_g, accent_b) = theme.accent_rgb();
    
    // Draw section header
    let font_desc = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&font_desc));
    layout.set_text("Now Playing");
    
    cr.move_to(10.0, y_pos);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(text_r, text_g, text_b);
    cr.fill().expect("Failed to fill");
    
    y_pos += 28.0;  // More space after header
    
    // Check if there's an active player
    if !media_info.is_active() {
        let font_desc = pango::FontDescription::from_string("Ubuntu Italic 11");
        layout.set_font_description(Some(&font_desc));
        layout.set_text("No media playing");
        
        cr.move_to(15.0, y_pos);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(sec_r, sec_g, sec_b);
        cr.fill().expect("Failed to fill");
        
        return (y_pos + 25.0, button_bounds);
    }
    
    // Draw background panel (theme-aware)
    let panel_height = 125.0;
    let panel_y = y_pos;
    cr.set_source_rgba(panel_r, panel_g, panel_b, panel_a);
    cr.rectangle(10.0, panel_y, 360.0, panel_height);
    cr.fill().expect("Failed to fill background");
    
    cr.set_source_rgba(border_r, border_g, border_b, border_a);
    cr.set_line_width(1.5);
    cr.rectangle(10.0, panel_y, 360.0, panel_height);
    cr.stroke().expect("Failed to stroke border");
    
    // Content starts inside the panel with padding
    y_pos += 10.0;
    
    // Draw track title (moved up, no play/pause icon here anymore)
    let text_x = 20.0;
    let font_desc_bold = pango::FontDescription::from_string("Ubuntu Bold 12");
    layout.set_font_description(Some(&font_desc_bold));
    
    let title = if media_info.title.len() > 40 {
        format!("{}...", &media_info.title[..37])
    } else {
        media_info.title.clone()
    };
    layout.set_text(&title);
    
    cr.move_to(text_x, y_pos);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(text_r, text_g, text_b);
    cr.fill().expect("Failed to fill");
    
    // Draw artist
    if !media_info.artist.is_empty() {
        y_pos += 18.0;
        
        let font_desc = pango::FontDescription::from_string("Ubuntu 11");
        layout.set_font_description(Some(&font_desc));
        
        let artist = if media_info.artist.len() > 45 {
            format!("{}...", &media_info.artist[..42])
        } else {
            media_info.artist.clone()
        };
        layout.set_text(&artist);
        
        cr.move_to(text_x, y_pos);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(sec_r, sec_g, sec_b);
        cr.fill().expect("Failed to fill");
    }
    
    // Draw album (if present)
    if !media_info.album.is_empty() {
        y_pos += 16.0;
        
        let font_desc_small = pango::FontDescription::from_string("Ubuntu Italic 10");
        layout.set_font_description(Some(&font_desc_small));
        
        let album = if media_info.album.len() > 50 {
            format!("{}...", &media_info.album[..47])
        } else {
            media_info.album.clone()
        };
        layout.set_text(&album);
        
        cr.move_to(text_x, y_pos);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(0.6, 0.6, 0.6);
        cr.fill().expect("Failed to fill");
    }
    
    // Draw progress bar (full width)
    y_pos += 18.0;
    let bar_x = 20.0;
    let bar_width = 330.0;
    let bar_height = 6.0;
    
    // Background bar
    cr.set_source_rgba(0.3, 0.3, 0.3, 0.8);
    cr.rectangle(bar_x, y_pos, bar_width, bar_height);
    cr.fill().expect("Failed to fill progress background");
    
    // Progress fill (using theme accent color)
    let progress = media_info.progress();
    if progress > 0.0 {
        cr.set_source_rgba(accent_r, accent_g, accent_b, 0.9);
        cr.rectangle(bar_x, y_pos, bar_width * progress, bar_height);
        cr.fill().expect("Failed to fill progress");
    }
    
    // Progress bar border
    cr.set_source_rgba(0.5, 0.5, 0.5, 0.8);
    cr.set_line_width(1.0);
    cr.rectangle(bar_x, y_pos, bar_width, bar_height);
    cr.stroke().expect("Failed to stroke progress border");
    
    // Draw time on left and player name on right (below progress bar)
    y_pos += 10.0;
    let font_desc_time = pango::FontDescription::from_string("Ubuntu 9");
    layout.set_font_description(Some(&font_desc_time));
    
    let time_str = format!("{} / {}", media_info.position_str(), media_info.duration_str());
    layout.set_text(&time_str);
    
    cr.move_to(bar_x, y_pos);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(0.7, 0.7, 0.7);
    cr.fill().expect("Failed to fill");
    
    // Draw player name on the right
    layout.set_text(&media_info.player_name);
    let (text_width, _) = layout.pixel_size();
    cr.move_to(bar_x + bar_width - text_width as f64, y_pos);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(0.5, 0.5, 0.5);
    cr.fill().expect("Failed to fill");
    
    // Draw playback controls (Previous, Play/Pause, Next) - centered below progress
    y_pos += 16.0;
    let button_size = 24.0;
    let button_spacing = 20.0;
    let total_controls_width = button_size * 3.0 + button_spacing * 2.0;
    let controls_start_x = (370.0 - total_controls_width) / 2.0;
    
    // Previous button (<<)
    let prev_x = controls_start_x;
    let prev_y = y_pos;
    
    // Draw previous button background (hover effect area)
    cr.set_source_rgba(0.3, 0.3, 0.4, 0.5);
    cr.arc(prev_x + button_size / 2.0, prev_y + button_size / 2.0, button_size / 2.0 + 2.0, 0.0, 2.0 * std::f64::consts::PI);
    cr.fill().expect("Failed to fill");
    
    // Draw previous icon (two triangles pointing left)
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let tri_size = 8.0;
    // First triangle
    cr.move_to(prev_x + button_size / 2.0 - 2.0, prev_y + button_size / 2.0);
    cr.line_to(prev_x + button_size / 2.0 + tri_size - 2.0, prev_y + button_size / 2.0 - tri_size);
    cr.line_to(prev_x + button_size / 2.0 + tri_size - 2.0, prev_y + button_size / 2.0 + tri_size);
    cr.close_path();
    cr.fill().expect("Failed to fill");
    // Second triangle
    cr.move_to(prev_x + button_size / 2.0 - tri_size - 2.0, prev_y + button_size / 2.0);
    cr.line_to(prev_x + button_size / 2.0 - 2.0, prev_y + button_size / 2.0 - tri_size);
    cr.line_to(prev_x + button_size / 2.0 - 2.0, prev_y + button_size / 2.0 + tri_size);
    cr.close_path();
    cr.fill().expect("Failed to fill");
    
    button_bounds.push(("previous".to_string(), prev_x - 2.0, prev_y - 2.0, prev_x + button_size + 2.0, prev_y + button_size + 2.0));
    
    // Play/Pause button
    let play_x = prev_x + button_size + button_spacing;
    let play_y = y_pos;
    
    // Draw play/pause button background (larger, highlighted with accent color)
    cr.set_source_rgba(accent_r, accent_g, accent_b, 0.6);
    cr.arc(play_x + button_size / 2.0, play_y + button_size / 2.0, button_size / 2.0 + 4.0, 0.0, 2.0 * std::f64::consts::PI);
    cr.fill().expect("Failed to fill");
    
    cr.set_source_rgb(1.0, 1.0, 1.0);
    match media_info.status {
        PlaybackStatus::Playing => {
            // Draw pause icon (two vertical bars)
            let bar_width = 4.0;
            let bar_height = 14.0;
            let bar_y = play_y + (button_size - bar_height) / 2.0;
            cr.rectangle(play_x + button_size / 2.0 - bar_width - 2.0, bar_y, bar_width, bar_height);
            cr.fill().expect("Failed to fill");
            cr.rectangle(play_x + button_size / 2.0 + 2.0, bar_y, bar_width, bar_height);
            cr.fill().expect("Failed to fill");
        }
        PlaybackStatus::Paused | PlaybackStatus::Stopped => {
            // Draw play icon (triangle)
            let tri_size = 10.0;
            cr.move_to(play_x + button_size / 2.0 - tri_size / 2.0, play_y + button_size / 2.0 - tri_size);
            cr.line_to(play_x + button_size / 2.0 - tri_size / 2.0, play_y + button_size / 2.0 + tri_size);
            cr.line_to(play_x + button_size / 2.0 + tri_size, play_y + button_size / 2.0);
            cr.close_path();
            cr.fill().expect("Failed to fill");
        }
    }
    
    button_bounds.push(("play_pause".to_string(), play_x - 4.0, play_y - 4.0, play_x + button_size + 4.0, play_y + button_size + 4.0));
    
    // Next button (>>)
    let next_x = play_x + button_size + button_spacing;
    let next_y = y_pos;
    
    // Draw next button background
    cr.set_source_rgba(0.3, 0.3, 0.4, 0.5);
    cr.arc(next_x + button_size / 2.0, next_y + button_size / 2.0, button_size / 2.0 + 2.0, 0.0, 2.0 * std::f64::consts::PI);
    cr.fill().expect("Failed to fill");
    
    // Draw next icon (two triangles pointing right)
    cr.set_source_rgb(1.0, 1.0, 1.0);
    // First triangle
    cr.move_to(next_x + button_size / 2.0 + 2.0, next_y + button_size / 2.0);
    cr.line_to(next_x + button_size / 2.0 - tri_size + 2.0, next_y + button_size / 2.0 - tri_size);
    cr.line_to(next_x + button_size / 2.0 - tri_size + 2.0, next_y + button_size / 2.0 + tri_size);
    cr.close_path();
    cr.fill().expect("Failed to fill");
    // Second triangle
    cr.move_to(next_x + button_size / 2.0 + tri_size + 2.0, next_y + button_size / 2.0);
    cr.line_to(next_x + button_size / 2.0 + 2.0, next_y + button_size / 2.0 - tri_size);
    cr.line_to(next_x + button_size / 2.0 + 2.0, next_y + button_size / 2.0 + tri_size);
    cr.close_path();
    cr.fill().expect("Failed to fill");
    
    button_bounds.push(("next".to_string(), next_x - 2.0, next_y - 2.0, next_x + button_size + 2.0, next_y + button_size + 2.0));
    
    // Return position after the panel with some padding
    (panel_y + panel_height + 15.0, button_bounds)
}
