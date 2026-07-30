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

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

pub const UPDATE_INTERVAL_MS: u64 = 1_000;

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
    /// Aggregate download and upload throughput
    Network,
    /// Aggregate disk read and write throughput
    DiskIo,
    /// CPU and GPU temperature displays (circular or text)
    Temperatures,
    /// Disk space usage for mounted filesystems
    Storage,
    /// Battery levels for supported wireless peripherals
    Battery,
    /// Current weather conditions from Open-Meteo
    Weather,
    /// Desktop notifications with grouping and dismiss controls
    Notifications,
    /// Now playing information from MPRIS, Cider, and Emby
    Media,
}

/// Visual style used by the Iced temperature gauges.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemperatureGaugeStyle {
    /// A three-quarter ring with an opening at the bottom.
    #[default]
    Arc,
    /// A complete 360-degree ring.
    Circular,
    /// Simple CPU and GPU rows with text values.
    Text,
}

impl WidgetSection {
    /// Returns the human-readable label for this section.
    ///
    /// Used in the settings UI for the section reordering list.
    pub fn label(&self) -> &'static str {
        match self {
            WidgetSection::Utilization => "Utilization",
            WidgetSection::Network => "Network",
            WidgetSection::DiskIo => "Disk I/O",
            WidgetSection::Temperatures => "Temperatures",
            WidgetSection::Storage => "Storage",
            WidgetSection::Battery => "Devices",
            WidgetSection::Weather => "Weather",
            WidgetSection::Notifications => "Notifications",
            WidgetSection::Media => "Now Playing",
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
    /// Supports NVIDIA (NVML), AMD, and Intel GPUs without subprocesses.
    pub show_gpu: bool,

    /// Show network transfer rates (upload/download speeds).
    /// Displayed as a reorderable Network section.
    pub show_network: bool,

    /// Show aggregate disk read and write throughput.
    pub show_disk: bool,

    // ========================================================================
    // Temperature Section
    // ========================================================================
    /// Show CPU temperature in the Temperatures section.
    /// Reads from hwmon sensors via sysinfo.
    pub show_cpu_temp: bool,

    /// Show GPU temperature in the Temperatures section.
    /// Uses NVML for NVIDIA and hwmon for AMD/Intel.
    pub show_gpu_temp: bool,

    /// Legacy Cairo temperature display option retained for compatibility.
    pub use_circular_temp_display: bool,

    /// Gauge shape used by the Iced overlay.
    pub temperature_gauge_style: TemperatureGaugeStyle,

    // ========================================================================
    // Storage Section
    // ========================================================================
    /// Show disk space usage for mounted filesystems.
    /// Displays each mounted disk with used/total space and a progress bar.
    pub show_storage: bool,

    // ========================================================================
    // Battery Section
    // ========================================================================
    /// Show battery levels for supported peripherals.
    pub show_battery: bool,

    /// Enable Solaar as a fallback for Logitech devices without a native reader.
    pub enable_solaar_integration: bool,

    // ========================================================================
    // Weather Section
    // ========================================================================
    /// Show weather information from Open-Meteo (no API key required).
    /// Requires a location to be configured.
    pub show_weather: bool,

    /// API key (deprecated - no longer required with Open-Meteo).
    /// Kept for backward compatibility but ignored.
    pub weather_api_key: String,

    /// Location for weather data (city name or "City,Country" format).
    /// Examples: "London", "New York", "Berlin, Germany"
    pub weather_location: String,

    // ========================================================================
    // Notifications Section
    // ========================================================================
    /// Show desktop notifications in the widget.
    /// Synchronizes with the COSMIC notification service over D-Bus.
    pub show_notifications: bool,

    /// Maximum number of notifications to keep in the display.
    /// Oldest notifications are removed when this limit is exceeded.
    pub max_notifications: usize,

    // ========================================================================
    // Media Section
    // ========================================================================
    /// Show now playing information from MPRIS, Cider, and Emby sources.
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

    // ========================================================================
    // Widget Position & Behavior
    // ========================================================================
    /// X coordinate (pixels from left edge) for widget placement.
    pub widget_x: i32,

    /// Y coordinate (pixels from top edge) for widget placement.
    pub widget_y: i32,

    /// Horizontal position restored by the settings application's reset action.
    pub default_widget_x: i32,

    /// Vertical position restored by the settings application's reset action.
    pub default_widget_y: i32,

    /// Whether the per-installation reset position has been captured.
    pub position_defaults_initialized: bool,

    /// Allow the overlay position to be edited by dragging.
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
    /// Write overlay diagnostics to /tmp/cosmic-widget.log.
    pub enable_logging: bool,
}

impl Config {
    pub const ALL_SECTIONS: [WidgetSection; 9] = [
        WidgetSection::Utilization,
        WidgetSection::Network,
        WidgetSection::DiskIo,
        WidgetSection::Temperatures,
        WidgetSection::Storage,
        WidgetSection::Battery,
        WidgetSection::Weather,
        WidgetSection::Notifications,
        WidgetSection::Media,
    ];

    /// Add every current overlay section while retaining the user's existing order.
    pub fn ensure_all_sections(&mut self) -> bool {
        let mut changed = false;

        for (canonical_index, section) in Self::ALL_SECTIONS.iter().copied().enumerate() {
            if self.section_order.contains(&section) {
                continue;
            }

            let insertion_index = Self::ALL_SECTIONS[..canonical_index]
                .iter()
                .rev()
                .find_map(|previous| {
                    self.section_order
                        .iter()
                        .position(|candidate| candidate == previous)
                        .map(|index| index + 1)
                })
                .unwrap_or(0);
            self.section_order.insert(insertion_index, section);
            changed = true;
        }

        changed
    }

    /// Capture the current position as the reset target when upgrading an
    /// existing installation that predates per-installation defaults.
    pub fn ensure_position_defaults(&mut self) -> bool {
        if self.position_defaults_initialized {
            return false;
        }

        self.default_widget_x = self.widget_x;
        self.default_widget_y = self.widget_y;
        self.position_defaults_initialized = true;
        true
    }

    pub fn reset_widget_position(&mut self) {
        self.widget_x = self.default_widget_x;
        self.widget_y = self.default_widget_y;
        self.widget_movable = false;
    }
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
    /// - Widget auto-starts at position (7260, 50)
    /// - 1-second update interval for good balance of responsiveness and efficiency
    fn default() -> Self {
        Self {
            // Utilization: Show basic system stats by default
            show_cpu: true,
            show_memory: true,
            show_gpu: false, // Requires GPU, not always present
            show_network: false,
            show_disk: false,

            // Temperatures: Disabled by default (not all systems have sensors)
            show_cpu_temp: false,
            show_gpu_temp: false,
            use_circular_temp_display: true,
            temperature_gauge_style: TemperatureGaugeStyle::Arc,

            // Storage: Show disk usage by default
            show_storage: true,

            // Devices: Disabled until supported hardware is detected
            show_battery: false,
            enable_solaar_integration: false,

            // Weather: Disabled until a location is configured
            show_weather: false,
            weather_api_key: String::new(),
            weather_location: String::from("London,UK"),

            // Notifications: Disabled by default
            show_notifications: false,
            max_notifications: 5,

            // Media: Disabled until a player is available
            show_media: false,
            cider_api_token: String::new(),

            // Clock: Show by default with 12-hour format
            show_clock: true,
            show_date: true,
            use_24hour_time: false,

            // Display: Show percentages
            show_percentages: true,

            // Position: 50 px from the top and right on a 7680 px-wide display
            widget_x: 7260,
            widget_y: 50,
            default_widget_x: 7260,
            default_widget_y: 50,
            position_defaults_initialized: false,
            widget_movable: false,
            widget_autostart: true,

            // Section order: Logical grouping from most to least common
            section_order: vec![
                WidgetSection::Utilization,
                WidgetSection::Network,
                WidgetSection::DiskIo,
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

#[cfg(test)]
mod tests {
    use super::{Config, TemperatureGaugeStyle, WidgetSection};

    #[test]
    fn arc_is_the_backward_compatible_temperature_style() {
        assert_eq!(
            Config::default().temperature_gauge_style,
            TemperatureGaugeStyle::Arc
        );
    }

    #[test]
    fn default_position_matches_the_installed_layout() {
        let config = Config::default();

        assert_eq!((config.widget_x, config.widget_y), (7260, 50));
        assert_eq!(
            (config.default_widget_x, config.default_widget_y),
            (7260, 50)
        );
    }

    #[test]
    fn captures_and_restores_the_installed_position() {
        let mut config = Config::default();
        config.widget_x = 7260;
        config.widget_y = 50;

        assert!(config.ensure_position_defaults());
        assert!(!config.ensure_position_defaults());

        config.widget_x = 100;
        config.widget_y = 200;
        config.widget_movable = true;
        config.reset_widget_position();

        assert_eq!((config.widget_x, config.widget_y), (7260, 50));
        assert!(!config.widget_movable);
    }

    #[test]
    fn adds_missing_sections_without_reordering_existing_sections() {
        let mut config = Config::default();
        config.section_order = vec![
            WidgetSection::Weather,
            WidgetSection::Utilization,
            WidgetSection::Storage,
        ];

        assert!(config.ensure_all_sections());
        let existing = config
            .section_order
            .iter()
            .copied()
            .filter(|section| {
                matches!(
                    section,
                    WidgetSection::Weather | WidgetSection::Utilization | WidgetSection::Storage
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            existing,
            vec![
                WidgetSection::Weather,
                WidgetSection::Utilization,
                WidgetSection::Storage
            ]
        );
        assert!(
            Config::ALL_SECTIONS
                .iter()
                .all(|section| config.section_order.contains(section))
        );
        assert!(!config.ensure_all_sections());
    }
}
