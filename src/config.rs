// SPDX-License-Identifier: MPL-2.0

//! Configuration module for COSMIC Monitor Applet
//!
//! This module defines the persistent configuration structure that is shared between
//! the panel applet, the standalone widget, and the settings application. Configuration
//! is stored using COSMIC's cosmic-config system and automatically syncs across all
//! components.
//!
//! # Architecture
//!
//! The configuration is stored at `~/.config/cosmic/com.github.zoliviragh.CosmicWidget/v1/`
//! and uses the CosmicConfigEntry derive macro for automatic serialization and versioning.
//!
//! # Usage
//!
//! ```rust
//! use cosmic::cosmic_config::{Config as CosmicConfig, CosmicConfigEntry};
//! use crate::config::Config;
//!
//! let handler = CosmicConfig::new("com.github.zoliviragh.CosmicWidget", Config::VERSION)?;;
//! let config = Config::get_entry(&handler).unwrap_or_default();
//! ```

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};
use serde::{Deserialize, Serialize};

// ============================================================================
// Widget Section Ordering
// ============================================================================

/// Represents the different sections that can be displayed in the widget.
///
/// Users can reorder these sections via the settings application to customize
/// the widget layout. Each section corresponds to a distinct monitoring feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetSection {
    /// CPU, Memory, GPU usage bars and percentages
    Utilization,
    /// CPU and GPU temperature displays (circular or text)
    Temperatures,
    /// Disk space usage for mounted filesystems
    Storage,
    /// Battery levels for laptops and Bluetooth devices (via Solaar)
    Battery,
    /// Current weather conditions from OpenWeatherMap
    Weather,
    /// Desktop notifications with grouping and dismiss controls
    Notifications,
    /// Now playing information from Cider (Apple Music client)
    Media,
}

impl WidgetSection {
    /// Returns the human-readable label for this section.
    ///
    /// Used in the settings UI for the section reordering list.
    pub fn label(&self) -> &'static str {
        match self {
            WidgetSection::Utilization => "Utilization",
            WidgetSection::Temperatures => "Temperatures",
            WidgetSection::Storage => "Storage",
            WidgetSection::Battery => "Battery",
            WidgetSection::Weather => "Weather",
            WidgetSection::Notifications => "Notifications",
            WidgetSection::Media => "Media Player",
        }
    }
}

// ============================================================================
// Main Configuration Structure
// ============================================================================

/// Main configuration structure for the COSMIC Monitor Applet.
///
/// This struct holds all user-configurable options and is automatically
/// persisted to disk via cosmic-config. Changes are detected by the widget
/// through periodic polling and applied in real-time.
///
/// # Sections
///
/// Configuration is organized into logical groups:
/// - **Monitoring toggles**: Enable/disable specific metrics (CPU, Memory, etc.)
/// - **Display options**: Visual preferences (percentages, 24-hour time, etc.)
/// - **Weather settings**: API key and location for weather data
/// - **Position settings**: Widget placement on screen
/// - **Advanced options**: Logging, API tokens, etc.
#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    // ========================================================================
    // Utilization Section
    // ========================================================================
    
    /// Show CPU usage bar and percentage in the Utilization section.
    /// Uses sysinfo crate to read from /proc/stat.
    pub show_cpu: bool,
    
    /// Show memory (RAM) usage bar and percentage in the Utilization section.
    /// Displays used/total memory from /proc/meminfo.
    pub show_memory: bool,
    
    /// Show GPU usage bar and percentage in the Utilization section.
    /// Supports NVIDIA (nvidia-smi), AMD, and Intel GPUs.
    pub show_gpu: bool,
    
    /// Show network transfer rates (upload/download speeds).
    /// Currently not fully implemented in the reorderable sections.
    pub show_network: bool,
    
    /// Show disk I/O activity.
    /// Currently not fully implemented in the reorderable sections.
    pub show_disk: bool,

    // ========================================================================
    // Temperature Section
    // ========================================================================
    
    /// Show CPU temperature in the Temperatures section.
    /// Reads from hwmon sensors via sysinfo.
    pub show_cpu_temp: bool,
    
    /// Show GPU temperature in the Temperatures section.
    /// Uses nvidia-smi for NVIDIA, hwmon for AMD/Intel.
    pub show_gpu_temp: bool,
    
    /// Use circular gauge display for temperatures instead of text.
    /// When true, shows a visual arc gauge; when false, shows "XX°C" text.
    pub use_circular_temp_display: bool,

    // ========================================================================
    // Storage Section
    // ========================================================================
    
    /// Show disk space usage for mounted filesystems.
    /// Displays each mounted disk with used/total space and a progress bar.
    pub show_storage: bool,

    // ========================================================================
    // Battery Section
    // ========================================================================
    
    /// Show battery section for laptop and Bluetooth device batteries.
    /// Requires enable_solaar_integration for Bluetooth devices.
    pub show_battery: bool,
    
    /// Enable Solaar integration for Logitech device battery monitoring.
    /// Solaar must be installed and running. Communicates via D-Bus.
    pub enable_solaar_integration: bool,

    // ========================================================================
    // Weather Section
    // ========================================================================
    
    /// Show weather information from OpenWeatherMap.
    /// Requires a valid API key and location to be configured.
    pub show_weather: bool,
    
    /// OpenWeatherMap API key for fetching weather data.
    /// Get a free key at https://openweathermap.org/api
    pub weather_api_key: String,
    
    /// Location for weather data (city name, "City,Country" format, or coordinates).
    /// Examples: "London,UK", "New York,US", "48.8566,2.3522"
    pub weather_location: String,

    // ========================================================================
    // Notifications Section
    // ========================================================================
    
    /// Show desktop notifications in the widget.
    /// Monitors D-Bus org.freedesktop.Notifications for new notifications.
    pub show_notifications: bool,
    
    /// Maximum number of notifications to keep in the display.
    /// Oldest notifications are removed when this limit is exceeded.
    pub max_notifications: usize,

    // ========================================================================
    // Media Section
    // ========================================================================
    
    /// Show now playing information from Cider (Apple Music client).
    /// Requires Cider to be running with its REST API enabled.
    pub show_media: bool,
    
    /// Cider REST API authentication token.
    /// Leave empty if Cider's "Authorized Requests Only" setting is disabled.
    /// Find this in Cider Settings → Connectivity → Remote Token.
    pub cider_api_token: String,

    // ========================================================================
    // Clock & Date Display
    // ========================================================================
    
    /// Show digital clock at the top of the widget.
    pub show_clock: bool,
    
    /// Show current date below the clock.
    pub show_date: bool,
    
    /// Use 24-hour time format (14:30) instead of 12-hour (2:30 PM).
    pub use_24hour_time: bool,

    // ========================================================================
    // Display Preferences
    // ========================================================================
    
    /// Show percentage values on utilization bars.
    /// When true, displays "XX%" next to each bar.
    pub show_percentages: bool,
    
    /// How often to update system statistics, in milliseconds.
    /// Lower values = more responsive but higher CPU usage.
    /// Recommended range: 500-2000ms.
    pub update_interval_ms: u64,

    // ========================================================================
    // Widget Position & Behavior
    // ========================================================================
    
    /// X coordinate (pixels from left edge) for widget placement.
    /// Can be adjusted by dragging when widget_movable is true.
    pub widget_x: i32,
    
    /// Y coordinate (pixels from top edge) for widget placement.
    /// Can be adjusted by dragging when widget_movable is true.
    pub widget_y: i32,
    
    /// Allow the widget to be repositioned by dragging.
    /// Automatically enabled when the settings window is open.
    pub widget_movable: bool,
    
    /// Order of sections in the widget from top to bottom.
    /// Users can reorder via the settings application.
    pub section_order: Vec<WidgetSection>,
    
    /// Automatically start the widget when the panel applet loads.
    /// If false, the widget must be manually shown via the applet menu.
    pub widget_autostart: bool,

    // ========================================================================
    // Advanced Settings
    // ========================================================================
    
    /// Enable debug logging to /tmp/cosmic-widget.log.
    /// Useful for troubleshooting issues. Disabled by default for performance.
    pub enable_logging: bool,
}

// ============================================================================
// Default Configuration
// ============================================================================

impl Default for Config {
    /// Returns the default configuration for new installations.
    ///
    /// Defaults are chosen to provide a useful out-of-box experience:
    /// - Basic system monitoring (CPU, Memory, Storage) enabled
    /// - Advanced features (GPU, Weather, Media) disabled until configured
    /// - Widget auto-starts at position (50, 50)
    /// - 1-second update interval for good balance of responsiveness and efficiency
    fn default() -> Self {
        Self {
            // Utilization: Show basic system stats by default
            show_cpu: true,
            show_memory: true,
            show_gpu: false,        // Requires GPU, not always present
            show_network: false,    // Not yet in reorderable sections
            show_disk: false,       // Not yet in reorderable sections
            
            // Temperatures: Disabled by default (not all systems have sensors)
            show_cpu_temp: false,
            show_gpu_temp: false,
            use_circular_temp_display: true,
            
            // Storage: Show disk usage by default
            show_storage: true,
            
            // Battery: Disabled (laptop/Solaar specific)
            show_battery: false,
            enable_solaar_integration: false,
            
            // Weather: Disabled (requires API key)
            show_weather: false,
            weather_api_key: String::new(),
            weather_location: String::from("London,UK"),
            
            // Notifications: Disabled by default
            show_notifications: false,
            max_notifications: 5,
            
            // Media: Disabled (requires Cider)
            show_media: false,
            cider_api_token: String::new(),
            
            // Clock: Show by default with 12-hour format
            show_clock: true,
            show_date: true,
            use_24hour_time: false,
            
            // Display: Show percentages, update every second
            show_percentages: true,
            update_interval_ms: 1000,
            
            // Position: Top-left area, auto-start enabled
            widget_x: 50,
            widget_y: 50,
            widget_movable: false,
            widget_autostart: true,
            
            // Section order: Logical grouping from most to least common
            section_order: vec![
                WidgetSection::Utilization,
                WidgetSection::Temperatures,
                WidgetSection::Storage,
                WidgetSection::Battery,
                WidgetSection::Weather,
                WidgetSection::Notifications,
                WidgetSection::Media,
            ],
            
            // Advanced: Logging off by default
            enable_logging: false,
        }
    }
}
