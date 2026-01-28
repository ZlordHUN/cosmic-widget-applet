// SPDX-License-Identifier: MPL-2.0

//! Settings Application for COSMIC Monitor
//!
//! This module implements a standalone settings application that provides a
//! comprehensive GUI for configuring all aspects of the monitoring widget.
//!
//! # Features
//!
//! - **Toggle monitoring sections**: Enable/disable CPU, Memory, GPU, etc.
//! - **Configure display options**: Clock format, percentages, temperatures
//! - **Weather configuration**: API key and location settings
//! - **Notification settings**: Enable and set max notification count
//! - **Media player settings**: Cider API token configuration
//! - **Widget positioning**: Set X/Y coordinates or drag while settings open
//! - **Section reordering**: Change the order of widget sections
//! - **Advanced options**: Debug logging toggle
//!
//! # Architecture
//!
//! The settings app is a separate binary (`cosmic-widget-settings`) that:
//! - Reads/writes the same config as the applet and widget via cosmic-config
//! - Enables "movable mode" while open so users can drag the widget
//! - Can restart the widget to apply changes via "Save & Apply"
//!
//! Changes are saved immediately when toggles change, allowing the widget
//! to pick them up on its next config poll (typically within 1 second).

use crate::config::{Config, WidgetSection};
use crate::fl;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::prelude::*;
use cosmic::widget;
use cosmic::Application;
use cosmic::Element;
use serde::{Deserialize, Serialize};

// ============================================================================
// Widget Cache Structures
// ============================================================================
// The widget caches discovered devices (batteries, disks) to a JSON file.
// The settings app reads this cache to display device information and allow
// users to remove stale entries.

/// Cached battery device information from Solaar or system.
///
/// The widget discovers battery devices at runtime and caches them so the
/// settings app can display them without requiring the same device access.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedBatteryDevice {
    /// Device name (e.g., "MX Master 3" or "BAT0")
    pub name: String,
    /// Device type (e.g., "Mouse", "Keyboard", or None for system batteries)
    pub kind: Option<String>,
}

/// Cache file structure for widget-discovered information.
///
/// This cache allows the settings app to show what devices/disks the widget
/// has found, without needing to probe the system itself.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WidgetCache {
    /// Mounted disks discovered by the storage monitor
    pub disks: Vec<CachedDiskInfo>,
    /// Battery devices from system or Solaar integration
    pub battery_devices: Vec<CachedBatteryDevice>,
}

/// Cached disk information for storage display.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedDiskInfo {
    /// Disk name (e.g., "nvme0n1p1")
    pub name: String,
    /// Mount point path (e.g., "/home")
    pub mount_point: String,
}

impl WidgetCache {
    /// Returns the path to the cache file.
    ///
    /// Located at `~/.cache/cosmic-widget-applet/widget_cache.json`
    fn cache_path() -> std::path::PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        path.push("cosmic-widget-applet");
        std::fs::create_dir_all(&path).ok();
        path.push("widget_cache.json");
        path
    }

    /// Load the cache from disk, returning default if file doesn't exist.
    fn load() -> Self {
        let path = Self::cache_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Save the cache to disk.
    fn save(&self) {
        let path = Self::cache_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            std::fs::write(&path, json).ok();
        }
    }
}

// ============================================================================
// Application Model
// ============================================================================

/// Main application state for the settings window.
///
/// Holds the current configuration, text input states for editable fields,
/// and cached device information from the widget.
pub struct SettingsApp {
    /// COSMIC runtime core for window management and styling
    core: cosmic::app::Core,
    
    /// Current configuration (modified as user changes settings)
    config: Config,
    
    /// Handle to cosmic-config for saving configuration
    config_handler: Option<cosmic_config::Config>,
    
    // Text input states - these hold the current text in input fields,
    // which may be invalid (e.g., non-numeric). Only valid values are
    // written to config.
    
    /// Update interval input (milliseconds)
    interval_input: String,
    /// Widget X position input (pixels)
    x_input: String,
    /// Widget Y position input (pixels)
    y_input: String,
    /// OpenWeatherMap API key input
    weather_api_key_input: String,
    /// Weather location input (city name or coordinates)
    weather_location_input: String,
    /// Maximum notifications count input
    max_notifications_input: String,
    /// Cider REST API token input
    cider_api_token_input: String,
    /// Cached battery devices from widget discovery
    cached_devices: Vec<CachedBatteryDevice>,
}

// ============================================================================
// Message Types
// ============================================================================

/// Messages that drive the settings app state machine.
///
/// Each variant corresponds to a user action (toggle, text input, button click)
/// or system event (config update, window close).
#[derive(Debug, Clone)]
pub enum Message {
    // === Config sync ===
    /// Configuration changed externally - update our view
    UpdateConfig(Config),
    
    // === Utilization toggles ===
    /// Toggle CPU usage monitoring
    ToggleCpu(bool),
    /// Toggle Memory usage monitoring
    ToggleMemory(bool),
    /// Toggle Network monitoring (not yet in reorderable sections)
    ToggleNetwork(bool),
    /// Toggle Disk I/O monitoring (not yet in reorderable sections)
    ToggleDisk(bool),
    /// Toggle Storage space display
    ToggleStorage(bool),
    /// Toggle GPU usage monitoring
    ToggleGpu(bool),
    
    // === Temperature toggles ===
    /// Toggle CPU temperature display
    ToggleCpuTemp(bool),
    /// Toggle GPU temperature display
    ToggleGpuTemp(bool),
    /// Toggle between circular gauge and text temperature display
    ToggleCircularTempDisplay(bool),
    
    // === Clock/Date toggles ===
    /// Toggle clock display
    ToggleClock(bool),
    /// Toggle date display
    ToggleDate(bool),
    /// Toggle between 24-hour and 12-hour time format
    Toggle24HourTime(bool),
    
    // === Display option toggles ===
    /// Toggle percentage values on utilization bars
    TogglePercentages(bool),
    
    // === Battery toggles ===
    /// Toggle battery section visibility
    ToggleBatterySection(bool),
    /// Toggle Solaar integration for Logitech device batteries
    ToggleSolaarIntegration(bool),
    /// Remove a cached battery device by index
    RemoveCachedDevice(usize),
    
    // === Notification settings ===
    /// Toggle notifications section
    ToggleNotifications(bool),
    /// Update max notifications count (text input)
    UpdateMaxNotifications(String),
    
    // === Media player settings ===
    /// Toggle media player section
    ToggleMedia(bool),
    /// Update Cider API token (text input)
    UpdateCiderApiToken(String),
    
    // === Interval and position ===
    /// Update polling interval (text input)
    UpdateInterval(String),
    /// Update widget X position (text input)
    UpdateX(String),
    /// Update widget Y position (text input)
    UpdateY(String),
    
    // === Weather settings ===
    /// Toggle weather display
    ToggleWeather(bool),
    /// Update OpenWeatherMap API key (text input)
    UpdateWeatherApiKey(String),
    /// Update weather location (text input)
    UpdateWeatherLocation(String),
    
    // === Widget behavior ===
    /// Toggle auto-start widget when panel loads
    ToggleWidgetAutostart(bool),
    /// Toggle debug logging to file
    ToggleLogging(bool),
    
    // === Section reordering ===
    /// Move a section up in the order list
    MoveSectionUp(usize),
    /// Move a section down in the order list
    MoveSectionDown(usize),
    
    // === Actions ===
    /// Save config and restart the widget
    SaveAndApply,
    /// Settings window close requested
    CloseRequested,
}

// ============================================================================
// Helper Methods
// ============================================================================

impl SettingsApp {
    /// Persist configuration changes to disk.
    ///
    /// Called after every toggle/input change for immediate persistence.
    /// The widget polls config periodically and will pick up changes.
    fn save_config(&self) {
        if let Some(ref config_handler) = self.config_handler {
            if let Err(err) = self.config.write_entry(config_handler) {
                eprintln!("Failed to save config: {}", err);
            }
        }
    }
}

// ============================================================================
// COSMIC Application Implementation
// ============================================================================

impl Application for SettingsApp {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    /// Settings app ID - distinct from the main applet to allow separate windows.
    const APP_ID: &'static str = "com.github.zoliviragh.CosmicWidget.Settings";

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    /// Handle window close - disable widget movement mode before closing.
    fn on_close_requested(&self, _id: cosmic::iced::window::Id) -> Option<Message> {
        Some(Message::CloseRequested)
    }

    /// Initialize the settings application.
    ///
    /// - Loads current configuration
    /// - Migrates old configs (adds new sections if missing)
    /// - Enables widget movement mode
    /// - Loads cached device information
    fn init(
        core: cosmic::app::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // Load config from the main app's config path (not the settings app path)
        let config_handler = cosmic_config::Config::new(
            "com.github.zoliviragh.CosmicWidget",
            Config::VERSION,
        )
        .ok();

        let mut config = config_handler
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();

        // === Config Migration ===
        // When new sections are added to the app, existing configs won't have them.
        // This ensures users don't lose access to new features.
        
        // Add Battery section if missing (added in v1.x)
        if !config.section_order.iter().any(|s| matches!(s, WidgetSection::Battery)) {
            if let Some(storage_pos) = config.section_order.iter().position(|s| matches!(s, WidgetSection::Storage)) {
                config.section_order.insert(storage_pos + 1, WidgetSection::Battery);
            } else if let Some(weather_pos) = config.section_order.iter().position(|s| matches!(s, WidgetSection::Weather)) {
                config.section_order.insert(weather_pos, WidgetSection::Battery);
            } else {
                config.section_order.push(WidgetSection::Battery);
            }
        }

        // Add Notifications section if missing
        if !config.section_order.iter().any(|s| matches!(s, WidgetSection::Notifications)) {
            config.section_order.push(WidgetSection::Notifications);
        }

        // Add Media section if missing
        if !config.section_order.iter().any(|s| matches!(s, WidgetSection::Media)) {
            config.section_order.push(WidgetSection::Media);
        }

        // Enable widget movement while settings window is open
        // This allows users to drag the widget to reposition it
        config.widget_movable = true;
        if let Some(ref handler) = config_handler {
            let _ = config.write_entry(handler);
        }

        // Initialize text inputs from current config values
        let interval_input = format!("{}", config.update_interval_ms);
        let x_input = format!("{}", config.widget_x);
        let y_input = format!("{}", config.widget_y);
        let weather_api_key_input = config.weather_api_key.clone();
        let weather_location_input = config.weather_location.clone();
        let max_notifications_input = config.max_notifications.to_string();
        let cider_api_token_input = config.cider_api_token.clone();
        
        // Load cached battery devices from widget's cache file
        let cache = WidgetCache::load();
        let cached_devices = cache.battery_devices.clone();

        let app = SettingsApp {
            core,
            config,
            config_handler,
            interval_input,
            x_input,
            y_input,
            weather_api_key_input,
            weather_location_input,
            max_notifications_input,
            cider_api_token_input,
            cached_devices,
        };

        (app, Task::none())
    }

    /// Render the settings UI.
    ///
    /// The UI is organized into sections matching the widget's features:
    /// - Monitoring Options (CPU, Memory, GPU, Network, Disk)
    /// - Storage Display
    /// - Temperature Display
    /// - Widget Display (Clock, Date, Time format)
    /// - Display Options (Percentages)
    /// - Battery (including Solaar and cached devices)
    /// - Weather
    /// - Notifications
    /// - Media Player
    /// - Layout Order (drag-to-reorder sections)
    /// - Widget Position
    /// - Advanced (logging)
    fn view(&self) -> Element<Self::Message> {
        let mut content = widget::column()
            .spacing(12)
            .padding(24)
            // === Header ===
            .push(widget::text::title1(fl!("app-title")))
            .push(widget::divider::horizontal::default())
            
            // === Monitoring Options Section ===
            .push(widget::text::heading(fl!("monitoring-options")))
            .push(widget::settings::item(
                fl!("show-cpu"),
                widget::toggler(self.config.show_cpu).on_toggle(Message::ToggleCpu),
            ))
            .push(widget::settings::item(
                fl!("show-memory"),
                widget::toggler(self.config.show_memory).on_toggle(Message::ToggleMemory),
            ))
            .push(widget::settings::item(
                fl!("show-gpu"),
                widget::toggler(self.config.show_gpu).on_toggle(Message::ToggleGpu),
            ))
            .push(widget::settings::item(
                fl!("show-network"),
                widget::toggler(self.config.show_network).on_toggle(Message::ToggleNetwork),
            ))
            .push(widget::settings::item(
                fl!("show-disk"),
                widget::toggler(self.config.show_disk).on_toggle(Message::ToggleDisk),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Storage Display Section ===
            .push(widget::text::heading(fl!("storage-display")))
            .push(widget::settings::item(
                fl!("show-storage"),
                widget::toggler(self.config.show_storage).on_toggle(Message::ToggleStorage),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Temperature Display Section ===
            .push(widget::text::heading(fl!("temperature-display")))
            .push(widget::settings::item(
                fl!("show-cpu-temp"),
                widget::toggler(self.config.show_cpu_temp).on_toggle(Message::ToggleCpuTemp),
            ))
            .push(widget::settings::item(
                fl!("show-gpu-temp"),
                widget::toggler(self.config.show_gpu_temp).on_toggle(Message::ToggleGpuTemp),
            ))
            .push(widget::settings::item(
                fl!("use-circular-temp-display"),
                widget::toggler(self.config.use_circular_temp_display).on_toggle(Message::ToggleCircularTempDisplay),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Widget Display Section (Clock/Date) ===
            .push(widget::text::heading(fl!("widget-display")))
            .push(widget::settings::item(
                fl!("show-clock"),
                widget::toggler(self.config.show_clock).on_toggle(Message::ToggleClock),
            ))
            .push(widget::settings::item(
                fl!("show-date"),
                widget::toggler(self.config.show_date).on_toggle(Message::ToggleDate),
            ))
            .push(widget::settings::item(
                fl!("use-24hour-time"),
                widget::toggler(self.config.use_24hour_time).on_toggle(Message::Toggle24HourTime),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Display Options Section ===
            .push(widget::text::heading(fl!("display-options")))
            .push(widget::settings::item(
                fl!("show-percentages"),
                widget::toggler(self.config.show_percentages).on_toggle(Message::TogglePercentages),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Battery Section ===
            .push(widget::text::heading("Battery"))
            .push(widget::settings::item(
                "Show battery section",
                widget::toggler(self.config.show_battery)
                    .on_toggle(Message::ToggleBatterySection),
            ))
            .push(widget::settings::item(
                "Enable Solaar integration",
                widget::toggler(self.config.enable_solaar_integration)
                    .on_toggle(Message::ToggleSolaarIntegration),
            ));
        
        // Display cached battery devices with remove buttons
        if !self.cached_devices.is_empty() {
            content = content.push(widget::text::body("Cached Devices:"));
            
            for (index, device) in self.cached_devices.iter().enumerate() {
                let device_kind = device.kind.as_deref().unwrap_or("device");
                let device_label = format!("{} ({})", device.name, device_kind);
                
                content = content.push(
                    widget::row()
                        .spacing(8)
                        .padding([4, 16])
                        .push(widget::text::body(device_label))
                        .push(widget::horizontal_space())
                        .push(
                            widget::button::icon(widget::icon::from_name("user-trash-symbolic"))
                                .on_press(Message::RemoveCachedDevice(index))
                                .padding(4)
                        )
                );
            }
        }
        
        content = content
            // === Update Interval ===
            .push(widget::settings::item(
                fl!("update-interval"),
                widget::text_input("", &self.interval_input).on_input(Message::UpdateInterval),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Weather Display Section ===
            // Uses Open-Meteo API (free, no API key required)
            .push(widget::text::heading(fl!("weather-display")))
            .push(widget::settings::item(
                fl!("show-weather"),
                widget::toggler(self.config.show_weather)
                    .on_toggle(Message::ToggleWeather),
            ))
            .push(widget::settings::item(
                fl!("weather-location"),
                widget::text_input("", &self.weather_location_input)
                    .on_input(Message::UpdateWeatherLocation),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Notifications Section ===
            .push(widget::text::heading("Notifications"))
            .push(widget::settings::item(
                "Show Notifications",
                widget::toggler(self.config.show_notifications)
                    .on_toggle(Message::ToggleNotifications),
            ))
            .push(widget::settings::item(
                "Max Notifications",
                widget::text_input("", &self.max_notifications_input)
                    .on_input(Message::UpdateMaxNotifications),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Media Player Section ===
            .push(widget::text::heading("Media Player"))
            .push(widget::settings::item(
                "Show Media Player",
                widget::toggler(self.config.show_media)
                    .on_toggle(Message::ToggleMedia),
            ))
            .push(widget::settings::item(
                "Cider API Token",
                widget::text_input("Leave empty if auth disabled", &self.cider_api_token_input)
                    .on_input(Message::UpdateCiderApiToken),
            ))
            .push(widget::text::body("Displays currently playing track from Cider (Apple Music client)"))
            .push(widget::divider::horizontal::default())
            
            // === Layout Order Section ===
            .push(widget::text::heading(fl!("layout-order")))
            .push(widget::text::body(fl!("layout-order-description")));
        
        // Render section order list with up/down move buttons
        for (index, section) in self.config.section_order.iter().enumerate() {
            // Up button (disabled if at top)
            let up_button = if index > 0 {
                widget::button::icon(widget::icon::from_name("go-up-symbolic"))
                    .on_press(Message::MoveSectionUp(index))
                    .padding(4)
            } else {
                widget::button::icon(widget::icon::from_name("go-up-symbolic"))
                    .padding(4)
            };
            
            // Down button (disabled if at bottom)
            let down_button = if index < self.config.section_order.len() - 1 {
                widget::button::icon(widget::icon::from_name("go-down-symbolic"))
                    .on_press(Message::MoveSectionDown(index))
                    .padding(4)
            } else {
                widget::button::icon(widget::icon::from_name("go-down-symbolic"))
                    .padding(4)
            };
            
            content = content.push(
                widget::row()
                    .spacing(8)
                    .padding([4, 8])
                    .push(up_button)
                    .push(down_button)
                    .push(widget::text::body(section.label()))
                    .push(widget::horizontal_space())
            );
        }
        
        content = content
            .push(widget::divider::horizontal::default())
            
            // === Widget Position Section ===
            .push(widget::text::heading("Widget Position"))
            .push(widget::settings::item(
                fl!("widget-autostart"),
                widget::toggler(self.config.widget_autostart)
                    .on_toggle(Message::ToggleWidgetAutostart),
            ))
            .push(widget::settings::item(
                "X Position",
                widget::text_input("", &self.x_input).on_input(Message::UpdateX),
            ))
            .push(widget::settings::item(
                "Y Position",
                widget::text_input("", &self.y_input).on_input(Message::UpdateY),
            ))
            .push(widget::divider::horizontal::default())
            
            // === Advanced Section ===
            .push(widget::text::heading("Advanced"))
            .push(widget::settings::item(
                "Enable Debug Logging",
                widget::toggler(self.config.enable_logging)
                    .on_toggle(Message::ToggleLogging),
            ))
            .push(widget::text::body("Writes debug logs to /tmp/cosmic-widget.log"))
            
            // === Save & Apply Button ===
            .push(
                widget::row()
                    .spacing(8)
                    .push(widget::column().width(cosmic::iced::Length::Fill))
                    .push(
                        widget::button::suggested("Save & Apply Settings")
                            .on_press(Message::SaveAndApply)
                    )
                    .push(widget::column().width(cosmic::iced::Length::Fill))
            );

        // Wrap in scrollable container for smaller screens
        let scrollable_content = widget::scrollable(content);

        widget::container(scrollable_content)
            .width(cosmic::iced::Length::Fill)
            .height(cosmic::iced::Length::Fill)
            .into()
    }

    /// Process messages and update application state.
    ///
    /// Most messages simply update a config field and save. Text inputs
    /// validate their content before updating (e.g., interval must be 100-10000ms).
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            // === Config Sync ===
            Message::UpdateConfig(config) => {
                self.config = config;
            }
            
            // === Window Close ===
            Message::CloseRequested => {
                // Disable widget movement when settings closes
                self.config.widget_movable = false;
                self.save_config();
                return cosmic::iced::window::get_latest()
                    .and_then(|id| cosmic::iced::window::close(id));
            }
            
            // === Simple Toggle Messages ===
            // Each toggle updates config and saves immediately
            Message::ToggleCpu(enabled) => {
                self.config.show_cpu = enabled;
                self.save_config();
            }
            Message::ToggleMemory(enabled) => {
                self.config.show_memory = enabled;
                self.save_config();
            }
            Message::ToggleNetwork(enabled) => {
                self.config.show_network = enabled;
                self.save_config();
            }
            Message::ToggleDisk(enabled) => {
                self.config.show_disk = enabled;
                self.save_config();
            }
            Message::ToggleStorage(enabled) => {
                self.config.show_storage = enabled;
                self.save_config();
            }
            Message::ToggleGpu(enabled) => {
                self.config.show_gpu = enabled;
                self.save_config();
            }
            Message::ToggleCpuTemp(enabled) => {
                self.config.show_cpu_temp = enabled;
                self.save_config();
            }
            Message::ToggleGpuTemp(enabled) => {
                self.config.show_gpu_temp = enabled;
                self.save_config();
            }
            Message::ToggleCircularTempDisplay(enabled) => {
                self.config.use_circular_temp_display = enabled;
                self.save_config();
            }
            Message::ToggleClock(enabled) => {
                self.config.show_clock = enabled;
                self.save_config();
            }
            Message::ToggleDate(enabled) => {
                self.config.show_date = enabled;
                self.save_config();
            }
            Message::Toggle24HourTime(enabled) => {
                self.config.use_24hour_time = enabled;
                self.save_config();
            }
            Message::TogglePercentages(enabled) => {
                self.config.show_percentages = enabled;
                self.save_config();
            }
            Message::ToggleBatterySection(enabled) => {
                self.config.show_battery = enabled;
                self.save_config();
            }
            Message::ToggleSolaarIntegration(enabled) => {
                self.config.enable_solaar_integration = enabled;
                self.save_config();
            }
            
            // === Battery Device Cache ===
            Message::RemoveCachedDevice(index) => {
                if index < self.cached_devices.len() {
                    self.cached_devices.remove(index);
                    // Persist to cache file
                    let mut cache = WidgetCache::load();
                    cache.battery_devices = self.cached_devices.clone();
                    cache.save();
                }
            }
            
            // === Notification Settings ===
            Message::ToggleNotifications(enabled) => {
                self.config.show_notifications = enabled;
                self.save_config();
            }
            Message::UpdateMaxNotifications(value) => {
                // Validate: must be 1-20
                if let Ok(max) = value.parse::<usize>() {
                    if max > 0 && max <= 20 {
                        self.config.max_notifications = max;
                        self.save_config();
                    }
                }
            }
            
            // === Media Settings ===
            Message::ToggleMedia(enabled) => {
                self.config.show_media = enabled;
                self.save_config();
            }
            Message::UpdateCiderApiToken(value) => {
                self.cider_api_token_input = value.clone();
                self.config.cider_api_token = value;
                self.save_config();
            }
            
            // === Interval Setting ===
            Message::UpdateInterval(value) => {
                self.interval_input = value.clone();
                // Validate: must be 100-10000ms
                if let Ok(interval) = value.parse::<u64>() {
                    if interval >= 100 && interval <= 10000 {
                        self.config.update_interval_ms = interval;
                        self.save_config();
                    }
                }
            }
            
            // === Position Settings ===
            Message::UpdateX(value) => {
                self.x_input = value.clone();
                if let Ok(x) = value.parse::<i32>() {
                    self.config.widget_x = x;
                    self.save_config();
                }
            }
            Message::UpdateY(value) => {
                self.y_input = value.clone();
                if let Ok(y) = value.parse::<i32>() {
                    self.config.widget_y = y;
                    self.save_config();
                }
            }
            
            // === Weather Settings ===
            Message::ToggleWeather(enabled) => {
                self.config.show_weather = enabled;
                self.save_config();
            }
            Message::ToggleWidgetAutostart(enabled) => {
                self.config.widget_autostart = enabled;
                self.save_config();
            }
            Message::ToggleLogging(enabled) => {
                self.config.enable_logging = enabled;
                self.save_config();
            }
            Message::UpdateWeatherApiKey(value) => {
                self.weather_api_key_input = value.clone();
                self.config.weather_api_key = value;
                self.save_config();
            }
            Message::UpdateWeatherLocation(value) => {
                self.weather_location_input = value.clone();
                self.config.weather_location = value;
                self.save_config();
            }
            
            // === Section Reordering ===
            Message::MoveSectionUp(index) => {
                if index > 0 && index < self.config.section_order.len() {
                    self.config.section_order.swap(index, index - 1);
                    self.save_config();
                }
            }
            Message::MoveSectionDown(index) => {
                if index < self.config.section_order.len() - 1 {
                    self.config.section_order.swap(index, index + 1);
                    self.save_config();
                }
            }
            
            // === Save & Apply Action ===
            Message::SaveAndApply => {
                // Ensure all settings are persisted
                self.save_config();
                
                // Restart widget to apply changes that require restart
                eprintln!("Save & Apply clicked! Restarting widget with current settings.");
                
                // Kill existing widget process
                match std::process::Command::new("pkill")
                    .arg("-f")
                    .arg("cosmic-widget")
                    .status() {
                    Ok(status) => eprintln!("pkill status: {:?}", status),
                    Err(e) => eprintln!("pkill error: {:?}", e),
                }
                
                // Brief delay for process cleanup
                std::thread::sleep(std::time::Duration::from_millis(300));
                
                // Spawn new widget using installed binary (from PATH)
                match std::process::Command::new("cosmic-widget")
                    .spawn() {
                    Ok(child) => eprintln!("Widget spawned with PID: {:?}", child.id()),
                    Err(e) => eprintln!("Spawn error: {:?}", e),
                }
            }
        }
        Task::none()
    }
}
