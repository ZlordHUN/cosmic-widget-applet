// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};
use serde::{Deserialize, Serialize};

/// Widget sections that can be reordered
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetSection {
    Utilization,
    Temperatures,
    Storage,
    Battery,
    Weather,
}

impl WidgetSection {
    pub fn label(&self) -> &'static str {
        match self {
            WidgetSection::Utilization => "Utilization",
            WidgetSection::Temperatures => "Temperatures",
            WidgetSection::Storage => "Storage",
            WidgetSection::Battery => "Battery",
            WidgetSection::Weather => "Weather",
        }
    }
}

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Enable CPU monitoring
    pub show_cpu: bool,
    /// Enable memory monitoring
    pub show_memory: bool,
    /// Enable network monitoring
    pub show_network: bool,
    /// Enable disk I/O monitoring
    pub show_disk: bool,
    /// Enable storage/disk usage monitoring
    pub show_storage: bool,
    /// Enable GPU monitoring
    pub show_gpu: bool,
    /// Show CPU temperature
    pub show_cpu_temp: bool,
    /// Show GPU temperature
    pub show_gpu_temp: bool,
    /// Use circular display for temperatures (false = text display)
    pub use_circular_temp_display: bool,
    /// Show weather information
    pub show_weather: bool,
    /// OpenWeatherMap API key
    pub weather_api_key: String,
    /// Weather location (city name or coordinates)
    pub weather_location: String,
    /// Show clock display
    pub show_clock: bool,
    /// Show date display
    pub show_date: bool,
    /// Use 24-hour time format (false = 12-hour with AM/PM)
    pub use_24hour_time: bool,
    /// Update interval in milliseconds
    pub update_interval_ms: u64,
    /// Show percentage values
    pub show_percentages: bool,
    /// Widget X position on screen
    pub widget_x: i32,
    /// Widget Y position on screen
    pub widget_y: i32,
    /// Allow widget to be moved (when settings is open)
    pub widget_movable: bool,
    /// Order of widget sections
    pub section_order: Vec<WidgetSection>,
    /// Auto-start widget when applet loads
    pub widget_autostart: bool,
    /// Enable battery section in widget
    pub show_battery: bool,
    /// Enable Solaar integration for battery data
    pub enable_solaar_integration: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_cpu: true,
            show_memory: true,
            show_network: false,
            show_disk: false,
            show_storage: true,
            show_gpu: false,
            show_cpu_temp: false,
            show_gpu_temp: false,
            use_circular_temp_display: true,
            show_weather: false,
            weather_api_key: String::new(),
            weather_location: String::from("London,UK"),
            show_clock: true,
            show_date: true,
            use_24hour_time: false,
            update_interval_ms: 1000,
            show_percentages: true,
            widget_x: 50,
            widget_y: 50,
            widget_movable: false,
            section_order: vec![
                WidgetSection::Utilization,
                WidgetSection::Temperatures,
                WidgetSection::Storage,
                WidgetSection::Battery,
                WidgetSection::Weather,
            ],
            widget_autostart: true,
            show_battery: false,
            enable_solaar_integration: false,
        }
    }
}
