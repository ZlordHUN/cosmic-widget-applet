// SPDX-License-Identifier: MPL-2.0

//! Settings application for the system monitor

use crate::config::Config;
use crate::fl;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::prelude::*;
use cosmic::widget;
use cosmic::{app, Application, Element};

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
}

/// Messages emitted by the settings app
#[derive(Debug, Clone)]
pub enum Message {
    UpdateConfig(Config),
    ToggleCpu(bool),
    ToggleMemory(bool),
    ToggleNetwork(bool),
    ToggleDisk(bool),
    ToggleGpu(bool),
    ToggleClock(bool),
    ToggleDate(bool),
    TogglePercentages(bool),
    UpdateInterval(String),
    UpdateX(String),
    UpdateY(String),
    ApplyPosition,
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

        // Enable widget movement when settings window is open
        config.widget_movable = true;
        if let Some(ref handler) = config_handler {
            let _ = config.write_entry(handler);
        }

        let interval_input = format!("{}", config.update_interval_ms);
        let x_input = format!("{}", config.widget_x);
        let y_input = format!("{}", config.widget_y);

        let app = SettingsApp {
            core,
            config,
            config_handler,
            interval_input,
            x_input,
            y_input,
        };

        (app, Task::none())
    }

    /// Displays the application's interface.
    fn view(&self) -> Element<'_, Self::Message> {
        let content = widget::column()
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
            .push(widget::text::heading(fl!("widget-display")))
            .push(widget::settings::item(
                fl!("show-clock"),
                widget::toggler(self.config.show_clock).on_toggle(Message::ToggleClock),
            ))
            .push(widget::settings::item(
                fl!("show-date"),
                widget::toggler(self.config.show_date).on_toggle(Message::ToggleDate),
            ))
            .push(widget::divider::horizontal::default())
            .push(widget::text::heading(fl!("display-options")))
            .push(widget::settings::item(
                fl!("show-percentages"),
                widget::toggler(self.config.show_percentages).on_toggle(Message::TogglePercentages),
            ))
            .push(widget::settings::item(
                fl!("update-interval"),
                widget::text_input("", &self.interval_input).on_input(Message::UpdateInterval),
            ))
            .push(widget::divider::horizontal::default())
            .push(widget::text::heading("Widget Position"))
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
                        widget::button::standard("Apply Position")
                            .on_press(Message::ApplyPosition)
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
            Message::ToggleGpu(enabled) => {
                self.config.show_gpu = enabled;
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
            Message::TogglePercentages(enabled) => {
                self.config.show_percentages = enabled;
                self.save_config();
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
            Message::ApplyPosition => {
                // Restart the widget to apply new position
                eprintln!("ApplyPosition clicked! Current position: X={}, Y={}", self.config.widget_x, self.config.widget_y);
                
                match std::process::Command::new("pkill")
                    .arg("-f")
                    .arg("cosmic-monitor-widget")
                    .status() {
                    Ok(status) => eprintln!("pkill status: {:?}", status),
                    Err(e) => eprintln!("pkill error: {:?}", e),
                }
                
                std::thread::sleep(std::time::Duration::from_millis(300));
                
                match std::process::Command::new("/usr/bin/cosmic-monitor-widget")
                    .spawn() {
                    Ok(child) => eprintln!("Widget spawned with PID: {:?}", child.id()),
                    Err(e) => eprintln!("Spawn error: {:?}", e),
                }
            }
        }
        Task::none()
    }
}
