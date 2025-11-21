// SPDX-License-Identifier: MPL-2.0

//! Settings application for the system monitor

use crate::config::{Config, WidgetSection};
use crate::fl;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::prelude::*;
use cosmic::widget;
use cosmic::Application;
use cosmic::Element;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedBatteryDevice {
    pub name: String,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WidgetCache {
    pub disks: Vec<CachedDiskInfo>,
    pub battery_devices: Vec<CachedBatteryDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedDiskInfo {
    pub name: String,
    pub mount_point: String,
}

impl WidgetCache {
    fn cache_path() -> std::path::PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        path.push("cosmic-monitor-applet");
        std::fs::create_dir_all(&path).ok();
        path.push("widget_cache.json");
        path
    }

    fn load() -> Self {
        let path = Self::cache_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self) {
        let path = Self::cache_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            std::fs::write(&path, json).ok();
        }
    }
}

/// The settings model
pub struct SettingsApp {
    /// Application state which is managed by the COSMIC runtime.
    core: cosmic::app::Core,
    /// Configuration data that persists between application runs.
    config: Config,
    /// Helper to save config changes.
    config_handler: Option<cosmic_config::Config>,
    /// Temporary state for the interval text input
    interval_input: String,
    /// Temporary state for X position input
    x_input: String,
    /// Temporary state for Y position input
    y_input: String,
    /// Temporary state for weather API key
    weather_api_key_input: String,
    /// Temporary state for weather location
    weather_location_input: String,
    /// Cached battery devices
    cached_devices: Vec<CachedBatteryDevice>,
}

/// Messages emitted by the settings app
#[derive(Debug, Clone)]
pub enum Message {
    UpdateConfig(Config),
    ToggleCpu(bool),
    ToggleMemory(bool),
    ToggleNetwork(bool),
    ToggleDisk(bool),
    ToggleStorage(bool),
    ToggleGpu(bool),
    ToggleCpuTemp(bool),
    ToggleGpuTemp(bool),
    ToggleCircularTempDisplay(bool),
    ToggleClock(bool),
    ToggleDate(bool),
    Toggle24HourTime(bool),
    TogglePercentages(bool),
    ToggleBatterySection(bool),
    ToggleSolaarIntegration(bool),
    RemoveCachedDevice(usize),
    UpdateInterval(String),
    UpdateX(String),
    UpdateY(String),
    ToggleWeather(bool),
    UpdateWeatherApiKey(String),
    UpdateWeatherLocation(String),
    ToggleWidgetAutostart(bool),
    MoveSectionUp(usize),
    MoveSectionDown(usize),
    SaveAndApply,
    CloseRequested,
}

impl SettingsApp {
    fn save_config(&self) {
        if let Some(ref config_handler) = self.config_handler {
            if let Err(err) = self.config.write_entry(config_handler) {
                eprintln!("Failed to save config: {}", err);
            }
        }
    }
}

/// Create a COSMIC application from the settings model
impl Application for SettingsApp {
    /// The async executor that will be used to run your application's commands.
    type Executor = cosmic::executor::Default;

    /// Data that your application receives to its init method.
    type Flags = ();

    /// Messages which the application and its widgets will emit.
    type Message = Message;

    /// Unique identifier in RDNN (reverse domain name notation) format.
    const APP_ID: &'static str = "com.github.zoliviragh.CosmicMonitor.Settings";

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn on_close_requested(&self, _id: cosmic::iced::window::Id) -> Option<Message> {
        Some(Message::CloseRequested)
    }

    /// Initializes the application with any given flags and startup commands.
    fn init(
        core: cosmic::app::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let config_handler = cosmic_config::Config::new(
            "com.github.zoliviragh.CosmicMonitor",
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

        // Migrate old configs: add Battery to section_order if missing
        if !config.section_order.iter().any(|s| matches!(s, WidgetSection::Battery)) {
            // Find position after Storage or before Weather
            if let Some(storage_pos) = config.section_order.iter().position(|s| matches!(s, WidgetSection::Storage)) {
                config.section_order.insert(storage_pos + 1, WidgetSection::Battery);
            } else if let Some(weather_pos) = config.section_order.iter().position(|s| matches!(s, WidgetSection::Weather)) {
                config.section_order.insert(weather_pos, WidgetSection::Battery);
            } else {
                config.section_order.push(WidgetSection::Battery);
            }
        }

        // Enable widget movement when settings window is open
        config.widget_movable = true;
        if let Some(ref handler) = config_handler {
            let _ = config.write_entry(handler);
        }

        let interval_input = format!("{}", config.update_interval_ms);
        let x_input = format!("{}", config.widget_x);
        let y_input = format!("{}", config.widget_y);
        let weather_api_key_input = config.weather_api_key.clone();
        let weather_location_input = config.weather_location.clone();
        
        // Load cached battery devices
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
            cached_devices,
        };

        (app, Task::none())
    }

    /// Displays the application's interface.
    fn view(&self) -> Element<Self::Message> {
        let mut content = widget::column()
            .spacing(12)
            .padding(24)
            .push(widget::text::title1(fl!("app-title")))
            .push(widget::divider::horizontal::default())
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
            .push(widget::text::heading(fl!("storage-display")))
            .push(widget::settings::item(
                fl!("show-storage"),
                widget::toggler(self.config.show_storage).on_toggle(Message::ToggleStorage),
            ))
            .push(widget::divider::horizontal::default())
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
            .push(widget::text::heading(fl!("display-options")))
            .push(widget::settings::item(
                fl!("show-percentages"),
                widget::toggler(self.config.show_percentages).on_toggle(Message::TogglePercentages),
            ))
            .push(widget::divider::horizontal::default())
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
        
        // Add cached battery devices list
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
            .push(widget::settings::item(
                fl!("update-interval"),
                widget::text_input("", &self.interval_input).on_input(Message::UpdateInterval),
            ))
            .push(widget::divider::horizontal::default())
            .push(widget::text::heading(fl!("weather-display")))
            .push(widget::settings::item(
                fl!("show-weather"),
                widget::toggler(self.config.show_weather)
                    .on_toggle(Message::ToggleWeather),
            ))
            .push(widget::settings::item(
                fl!("weather-api-key"),
                widget::text_input("", &self.weather_api_key_input)
                    .on_input(Message::UpdateWeatherApiKey),
            ))
            .push(widget::settings::item(
                fl!("weather-location"),
                widget::text_input("", &self.weather_location_input)
                    .on_input(Message::UpdateWeatherLocation),
            ))
            .push(widget::divider::horizontal::default())
            .push(widget::text::heading(fl!("layout-order")))
            .push(widget::text::body(fl!("layout-order-description")));
        
        // Add section order list with up/down buttons
        for (index, section) in self.config.section_order.iter().enumerate() {
            let up_button = if index > 0 {
                widget::button::icon(widget::icon::from_name("go-up-symbolic"))
                    .on_press(Message::MoveSectionUp(index))
                    .padding(4)
            } else {
                widget::button::icon(widget::icon::from_name("go-up-symbolic"))
                    .padding(4)
            };
            
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

        let scrollable_content = widget::scrollable(content);

        widget::container(scrollable_content)
            .width(cosmic::iced::Length::Fill)
            .height(cosmic::iced::Length::Fill)
            .into()
    }

    /// Handles messages emitted by the application and its widgets.
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::UpdateConfig(config) => {
                self.config = config;
            }
            Message::CloseRequested => {
                // Disable widget movement when settings window closes
                self.config.widget_movable = false;
                self.save_config();
                return cosmic::iced::window::get_latest()
                    .and_then(|id| cosmic::iced::window::close(id));
            }
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
            Message::RemoveCachedDevice(index) => {
                if index < self.cached_devices.len() {
                    self.cached_devices.remove(index);
                    
                    // Update the cache file
                    let mut cache = WidgetCache::load();
                    cache.battery_devices = self.cached_devices.clone();
                    cache.save();
                }
            }
            Message::UpdateInterval(value) => {
                self.interval_input = value.clone();
                if let Ok(interval) = value.parse::<u64>() {
                    if interval >= 100 && interval <= 10000 {
                        self.config.update_interval_ms = interval;
                        self.save_config();
                    }
                }
            }
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
            Message::ToggleWeather(enabled) => {
                self.config.show_weather = enabled;
                self.save_config();
            }
            Message::ToggleWidgetAutostart(enabled) => {
                self.config.widget_autostart = enabled;
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
            Message::SaveAndApply => {
                // Save all current settings to ensure they're persisted
                self.save_config();
                
                // Restart the widget to apply all settings
                eprintln!("Save & Apply clicked! Restarting widget with current settings.");
                
                match std::process::Command::new("pkill")
                    .arg("-f")
                    .arg("cosmic-monitor-widget")
                    .status() {
                    Ok(status) => eprintln!("pkill status: {:?}", status),
                    Err(e) => eprintln!("pkill error: {:?}", e),
                }
                
                std::thread::sleep(std::time::Duration::from_millis(300));
                
                match std::process::Command::new("./target/release/cosmic-monitor-widget")
                    .spawn() {
                    Ok(child) => eprintln!("Widget spawned with PID: {:?}", child.id()),
                    Err(e) => eprintln!("Spawn error: {:?}", e),
                }
            }
        }
        Task::none()
    }
}
