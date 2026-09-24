// SPDX-License-Identifier: MPL-2.0

//! Native COSMIC settings application for the desktop overlay.

mod cache;
mod temperature_preview;
mod views;

use crate::config::{Config, TemperatureGaugeStyle, WidgetSection};
use cache::{CachedBatteryDevice, WidgetCache};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::Subscription;
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar};
use cosmic::{Application, Element};

/// Initialize translations and start the settings application.
pub fn run() -> cosmic::iced::Result {
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    crate::i18n::init(&requested_languages);

    let settings = cosmic::app::Settings::default().size(cosmic::iced::Size::new(960.0, 720.0));
    cosmic::app::run::<SettingsApp>(settings, ())
}

const CONFIG_APP_ID: &str = "com.github.zoliviragh.CosmicWidget";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Display,
    Layout,
    Services,
    Behavior,
}

impl SettingsPage {
    const ALL: [Self; 4] = [Self::Display, Self::Layout, Self::Services, Self::Behavior];

    const fn label(self) -> &'static str {
        match self {
            Self::Display => "Display",
            Self::Layout => "Layout",
            Self::Services => "Services",
            Self::Behavior => "Behavior",
        }
    }

    const fn icon(self) -> &'static str {
        match self {
            Self::Display => "preferences-appearance-symbolic",
            Self::Layout => "format-indent-more-symbolic",
            Self::Services => "preferences-system-symbolic",
            Self::Behavior => "preferences-startup-applications-symbolic",
        }
    }
}

pub struct SettingsApp {
    core: cosmic::app::Core,
    nav_model: nav_bar::Model,
    config: Config,
    config_handler: Option<cosmic_config::Config>,
    x_input: String,
    y_input: String,
    weather_location_input: String,
    max_notifications_input: String,
    cider_api_token_input: String,
    cider_token_hidden: bool,
    cached_devices: Vec<CachedBatteryDevice>,
}

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
    SetTemperatureGaugeStyle(TemperatureGaugeStyle),
    ToggleClock(bool),
    ToggleDate(bool),
    Toggle24HourTime(bool),
    TogglePercentages(bool),
    ToggleDevices(bool),
    ToggleSolaarIntegration(bool),
    ToggleNotifications(bool),
    ToggleMedia(bool),
    ToggleCodexUsage(bool),
    ToggleWeather(bool),
    ToggleWidgetAutostart(bool),
    ToggleLogging(bool),
    UpdateMaxNotifications(String),
    UpdateCiderApiToken(String),
    ToggleCiderTokenVisibility,
    UpdateX(String),
    UpdateY(String),
    ResetPosition,
    EditPosition,
    UpdateWeatherLocation(String),
    RemoveCachedDevice(usize),
    MoveSectionUp(usize),
    MoveSectionDown(usize),
    CloseRequested,
}

impl SettingsApp {
    fn save_config(&self) {
        let Some(handler) = &self.config_handler else {
            return;
        };
        if let Err(error) = self.config.write_entry(handler) {
            log::error!("Failed to save widget settings: {error}");
        }
    }

    fn sync_inputs(&mut self) {
        self.x_input = self.config.widget_x.to_string();
        self.y_input = self.config.widget_y.to_string();
        self.weather_location_input = self.config.weather_location.clone();
        self.max_notifications_input = self.config.max_notifications.to_string();
        self.cider_api_token_input = self.config.cider_api_token.clone();
    }

    fn active_page(&self) -> SettingsPage {
        self.nav_model
            .active_data::<SettingsPage>()
            .copied()
            .unwrap_or(SettingsPage::Display)
    }

    fn update_title(&mut self) -> Task<cosmic::Action<Message>> {
        let page = self.active_page();
        self.set_header_title(page.label().to_string());
        let title = format!("{} - COSMIC Widget", page.label());
        self.core
            .main_window_id()
            .map_or_else(Task::none, |id| self.set_window_title(title, id))
    }
}

impl Application for SettingsApp {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "com.github.zoliviragh.CosmicWidget.Settings";

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::app::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let config_handler = cosmic_config::Config::new(CONFIG_APP_ID, Config::VERSION).ok();
        let (mut config, incomplete_schema) = config_handler
            .as_ref()
            .map(|handler| match Config::get_entry(handler) {
                Ok(config) => (config, false),
                Err((_errors, config)) => (config, true),
            })
            .unwrap_or_else(|| (Config::default(), false));

        let mut migrated = config.ensure_all_sections();
        migrated |= config.ensure_position_defaults();
        if migrated || incomplete_schema {
            if let Some(handler) = &config_handler {
                let _ = config.write_entry(handler);
            }
        }

        let mut nav_model = nav_bar::Model::default();
        for page in SettingsPage::ALL {
            nav_model
                .insert()
                .text(page.label())
                .icon(widget::icon::from_name(page.icon()))
                .data(page);
        }
        nav_model.activate_position(0);

        let cache = WidgetCache::load();
        let mut app = Self {
            core,
            nav_model,
            x_input: config.widget_x.to_string(),
            y_input: config.widget_y.to_string(),
            weather_location_input: config.weather_location.clone(),
            max_notifications_input: config.max_notifications.to_string(),
            cider_api_token_input: config.cider_api_token.clone(),
            cider_token_hidden: true,
            cached_devices: cache.battery_devices,
            config,
            config_handler,
        };
        let task = app.update_title();

        (app, task)
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav_model)
    }

    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Self::Message>> {
        self.nav_model.activate(id);
        self.update_title()
    }

    fn on_close_requested(&self, _id: cosmic::iced::window::Id) -> Option<Message> {
        Some(Message::CloseRequested)
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        self.core()
            .watch_config::<Config>(CONFIG_APP_ID)
            .map(|update| Message::UpdateConfig(update.config))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        match self.active_page() {
            SettingsPage::Display => self.display_page(),
            SettingsPage::Layout => self.layout_page(),
            SettingsPage::Services => self.services_page(),
            SettingsPage::Behavior => self.behavior_page(),
        }
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::UpdateConfig(mut config) => {
                config.ensure_all_sections();
                config.ensure_position_defaults();
                if config != self.config {
                    self.config = config;
                    self.sync_inputs();
                }
                // Config watcher updates are observations, not local edits. Writing
                // them back can amplify the per-key updates emitted by cosmic-config
                // into a feedback loop (most visibly, position reset oscillation).
                return Task::none();
            }
            Message::ToggleCpu(value) => self.config.show_cpu = value,
            Message::ToggleMemory(value) => self.config.show_memory = value,
            Message::ToggleNetwork(value) => self.config.show_network = value,
            Message::ToggleDisk(value) => self.config.show_disk = value,
            Message::ToggleStorage(value) => self.config.show_storage = value,
            Message::ToggleGpu(value) => self.config.show_gpu = value,
            Message::ToggleCpuTemp(value) => self.config.show_cpu_temp = value,
            Message::ToggleGpuTemp(value) => self.config.show_gpu_temp = value,
            Message::SetTemperatureGaugeStyle(style) => {
                self.config.temperature_gauge_style = style;
            }
            Message::ToggleClock(value) => self.config.show_clock = value,
            Message::ToggleDate(value) => self.config.show_date = value,
            Message::Toggle24HourTime(value) => self.config.use_24hour_time = value,
            Message::TogglePercentages(value) => self.config.show_percentages = value,
            Message::ToggleDevices(value) => self.config.show_battery = value,
            Message::ToggleSolaarIntegration(value) => {
                self.config.enable_solaar_integration = value;
            }
            Message::ToggleNotifications(value) => self.config.show_notifications = value,
            Message::ToggleMedia(value) => self.config.show_media = value,
            Message::ToggleCodexUsage(value) => self.config.show_codex_usage = value,
            Message::ToggleWeather(value) => self.config.show_weather = value,
            Message::ToggleWidgetAutostart(value) => self.config.widget_autostart = value,
            Message::ToggleLogging(value) => self.config.enable_logging = value,
            Message::UpdateMaxNotifications(value) => {
                self.max_notifications_input = value;
                if let Some(limit) = parse_bounded_usize(&self.max_notifications_input, 1, 20) {
                    self.config.max_notifications = limit;
                } else {
                    return Task::none();
                }
            }
            Message::UpdateCiderApiToken(value) => {
                self.cider_api_token_input = value.clone();
                self.config.cider_api_token = value;
            }
            Message::ToggleCiderTokenVisibility => {
                self.cider_token_hidden = !self.cider_token_hidden;
                return Task::none();
            }
            Message::UpdateX(value) => {
                self.x_input = value;
                if let Ok(position) = self.x_input.parse::<i32>() {
                    self.config.widget_x = position;
                } else {
                    return Task::none();
                }
            }
            Message::UpdateY(value) => {
                self.y_input = value;
                if let Ok(position) = self.y_input.parse::<i32>() {
                    self.config.widget_y = position;
                } else {
                    return Task::none();
                }
            }
            Message::ResetPosition => {
                self.config.reset_widget_position();
                self.sync_inputs();
            }
            Message::EditPosition => {
                self.config.widget_movable = true;
            }
            Message::UpdateWeatherLocation(value) => {
                self.weather_location_input = value.clone();
                self.config.weather_location = value;
            }
            Message::RemoveCachedDevice(index) => {
                if index < self.cached_devices.len() {
                    self.cached_devices.remove(index);
                    let mut cache = WidgetCache::load();
                    cache.battery_devices.clone_from(&self.cached_devices);
                    cache.save();
                }
                return Task::none();
            }
            Message::MoveSectionUp(index) => {
                if !move_enabled_section(&mut self.config, index, -1) {
                    return Task::none();
                }
            }
            Message::MoveSectionDown(index) => {
                if !move_enabled_section(&mut self.config, index, 1) {
                    return Task::none();
                }
            }
            Message::CloseRequested => {
                return cosmic::iced::window::latest()
                    .and_then(|id| cosmic::iced::window::close(id));
            }
        }

        self.save_config();
        Task::none()
    }
}

fn section_enabled(config: &Config, section: WidgetSection) -> bool {
    match section {
        WidgetSection::Utilization => config.show_cpu || config.show_memory || config.show_gpu,
        WidgetSection::Network => config.show_network,
        WidgetSection::DiskIo => config.show_disk,
        WidgetSection::Temperatures => config.show_cpu_temp || config.show_gpu_temp,
        WidgetSection::Storage => config.show_storage,
        WidgetSection::Battery => config.show_battery,
        WidgetSection::Weather => config.show_weather,
        WidgetSection::Notifications => config.show_notifications,
        WidgetSection::Media => config.show_media,
        WidgetSection::CodexUsage => config.show_codex_usage,
    }
}

fn move_enabled_section(config: &mut Config, index: usize, direction: i8) -> bool {
    let Some(section) = config.section_order.get(index).copied() else {
        return false;
    };
    if !section_enabled(config, section) {
        return false;
    }

    let adjacent = match direction {
        -1 => config.section_order[..index]
            .iter()
            .rposition(|section| section_enabled(config, *section)),
        1 => config.section_order[index + 1..]
            .iter()
            .position(|section| section_enabled(config, *section))
            .map(|offset| index + 1 + offset),
        _ => None,
    };

    if let Some(adjacent) = adjacent {
        config.section_order.swap(index, adjacent);
        true
    } else {
        false
    }
}

fn parse_bounded_usize(value: &str, minimum: usize, maximum: usize) -> Option<usize> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
}

#[cfg(test)]
mod tests {
    use super::{move_enabled_section, parse_bounded_usize, section_enabled};
    use crate::config::{Config, WidgetSection};

    #[test]
    fn accepts_only_supported_notification_limits() {
        assert_eq!(parse_bounded_usize("1", 1, 20), Some(1));
        assert_eq!(parse_bounded_usize("20", 1, 20), Some(20));
        assert_eq!(parse_bounded_usize("0", 1, 20), None);
        assert_eq!(parse_bounded_usize("21", 1, 20), None);
    }

    #[test]
    fn section_visibility_follows_its_display_controls() {
        let mut config = Config::default();
        config.show_cpu = false;
        config.show_memory = false;
        config.show_gpu = false;
        config.show_cpu_temp = true;

        assert!(!section_enabled(&config, WidgetSection::Utilization));
        assert!(section_enabled(&config, WidgetSection::Temperatures));
        assert!(section_enabled(&config, WidgetSection::Storage));
        assert!(!section_enabled(&config, WidgetSection::Media));
        assert!(section_enabled(&config, WidgetSection::CodexUsage));

        config.show_codex_usage = false;
        assert!(!section_enabled(&config, WidgetSection::CodexUsage));
    }

    #[test]
    fn reordering_skips_disabled_sections() {
        let mut config = Config::default();
        config.show_cpu = true;
        config.show_network = false;
        config.show_disk = false;
        config.show_cpu_temp = true;
        config.section_order = vec![
            WidgetSection::Utilization,
            WidgetSection::Network,
            WidgetSection::DiskIo,
            WidgetSection::Temperatures,
        ];

        assert!(move_enabled_section(&mut config, 3, -1));
        assert_eq!(
            config.section_order,
            vec![
                WidgetSection::Temperatures,
                WidgetSection::Network,
                WidgetSection::DiskIo,
                WidgetSection::Utilization,
            ]
        );
    }
}
