// SPDX-License-Identifier: MPL-2.0

//! Native COSMIC settings application for the desktop overlay.

use crate::config::{Config, TemperatureGaugeStyle, WidgetSection};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::widget::canvas;
use cosmic::iced::{
    Alignment, Color, Length, Point, Radians, Rectangle, Size, Subscription, mouse,
};
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar};
use cosmic::{Application, Element};
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

const CONFIG_APP_ID: &str = "com.github.zoliviragh.CosmicWidget";
const PAGE_WIDTH: f32 = 720.0;
const SHORT_INPUT_WIDTH: f32 = 140.0;
const LONG_INPUT_WIDTH: f32 = 280.0;
const TEMPERATURE_STYLE_PREVIEW_HEIGHT: f32 = 104.0;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CachedBatteryDevice {
    name: String,
    kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CachedDiskInfo {
    name: String,
    mount_point: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct WidgetCache {
    disks: Vec<CachedDiskInfo>,
    battery_devices: Vec<CachedBatteryDevice>,
}

impl WidgetCache {
    fn cache_path() -> std::path::PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        path.push("cosmic-widget-applet");
        let _ = std::fs::create_dir_all(&path);
        path.push("widget_cache.json");
        path
    }

    fn load() -> Self {
        std::fs::read_to_string(Self::cache_path())
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let _ = std::fs::write(Self::cache_path(), json);
    }
}

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

    fn page<'a>(&self, content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        let content = widget::container(content)
            .width(Length::Fill)
            .max_width(PAGE_WIDTH)
            .padding([24, 32]);

        widget::container(widget::scrollable(content))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .into()
    }

    fn display_page(&self) -> Element<'_, Message> {
        let clock = widget::settings::section()
            .title("Clock and date")
            .add(
                widget::settings::item::builder("Clock")
                    .description("Show the current time")
                    .toggler(self.config.show_clock, Message::ToggleClock),
            )
            .add(
                widget::settings::item::builder("Date")
                    .description("Show the date below the clock")
                    .toggler(self.config.show_date, Message::ToggleDate),
            )
            .add(
                widget::settings::item::builder("24-hour time")
                    .description("Use 23:15 instead of 11:15 PM")
                    .toggler(self.config.use_24hour_time, Message::Toggle24HourTime),
            );

        let metrics = widget::settings::section()
            .title("System metrics")
            .add(
                widget::settings::item::builder("CPU utilization")
                    .toggler(self.config.show_cpu, Message::ToggleCpu),
            )
            .add(
                widget::settings::item::builder("Memory utilization")
                    .toggler(self.config.show_memory, Message::ToggleMemory),
            )
            .add(
                widget::settings::item::builder("GPU utilization")
                    .toggler(self.config.show_gpu, Message::ToggleGpu),
            )
            .add(
                widget::settings::item::builder("Network activity")
                    .toggler(self.config.show_network, Message::ToggleNetwork),
            )
            .add(
                widget::settings::item::builder("Disk I/O")
                    .toggler(self.config.show_disk, Message::ToggleDisk),
            )
            .add(
                widget::settings::item::builder("Percentage labels")
                    .description("Show exact values beside utilization and storage bars")
                    .toggler(self.config.show_percentages, Message::TogglePercentages),
            );

        let temperatures = widget::settings::section()
            .title("Temperatures")
            .add(
                widget::settings::item::builder("CPU temperature")
                    .toggler(self.config.show_cpu_temp, Message::ToggleCpuTemp),
            )
            .add(
                widget::settings::item::builder("GPU temperature")
                    .toggler(self.config.show_gpu_temp, Message::ToggleGpuTemp),
            );

        let temperature_style = self.temperature_style_selector();

        let sections = widget::settings::section()
            .title("Sections")
            .add(
                widget::settings::item::builder("Storage")
                    .toggler(self.config.show_storage, Message::ToggleStorage),
            )
            .add(
                widget::settings::item::builder("Devices")
                    .toggler(self.config.show_battery, Message::ToggleDevices),
            )
            .add(
                widget::settings::item::builder("Weather")
                    .toggler(self.config.show_weather, Message::ToggleWeather),
            )
            .add(
                widget::settings::item::builder("Notifications")
                    .toggler(self.config.show_notifications, Message::ToggleNotifications),
            )
            .add(
                widget::settings::item::builder("Now Playing")
                    .toggler(self.config.show_media, Message::ToggleMedia),
            );

        self.page(widget::settings::view_column(vec![
            clock.into(),
            metrics.into(),
            temperatures.into(),
            temperature_style,
            sections.into(),
        ]))
    }

    fn temperature_style_selector(&self) -> Element<'_, Message> {
        let options = widget::row::with_capacity(3)
            .spacing(16)
            .width(Length::Fill)
            .push(self.temperature_style_card(TemperatureGaugeStyle::Arc, "Arc"))
            .push(self.temperature_style_card(TemperatureGaugeStyle::Circular, "Circular"))
            .push(self.temperature_style_card(TemperatureGaugeStyle::Text, "Text"));

        widget::column::with_capacity(2)
            .spacing(8)
            .push(widget::text::heading("Gauge style"))
            .push(options)
            .into()
    }

    fn temperature_style_card(
        &self,
        style: TemperatureGaugeStyle,
        label: &'static str,
    ) -> Element<'_, Message> {
        let preview = cosmic::iced::widget::Canvas::new(TemperatureStylePreview { style })
            .width(Length::Fill)
            .height(Length::Fixed(TEMPERATURE_STYLE_PREVIEW_HEIGHT));
        let selected = self.config.temperature_gauge_style == style;
        let button = widget::button::custom_image_button(preview, None::<Message>)
            .class(cosmic::theme::Button::Image)
            .selected(selected)
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fixed(TEMPERATURE_STYLE_PREVIEW_HEIGHT))
            .on_press(Message::SetTemperatureGaugeStyle(style));

        widget::column::with_capacity(2)
            .spacing(6)
            .width(Length::FillPortion(1))
            .push(button)
            .push(widget::container(widget::text::body(label)).center_x(Length::Fill))
            .into()
    }

    fn layout_page(&self) -> Element<'_, Message> {
        let mut order = widget::settings::section().title("Section order");
        let enabled_sections = self
            .config
            .section_order
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, section)| section_enabled(&self.config, *section))
            .collect::<Vec<_>>();
        let last_visible_index = enabled_sections.len().saturating_sub(1);

        for (visible_index, (index, section)) in enabled_sections.into_iter().enumerate() {
            let up = widget::button::icon(widget::icon::from_name("go-up-symbolic"))
                .padding(6)
                .on_press_maybe((visible_index > 0).then_some(Message::MoveSectionUp(index)));
            let down = widget::button::icon(widget::icon::from_name("go-down-symbolic"))
                .padding(6)
                .on_press_maybe(
                    (visible_index < last_visible_index).then_some(Message::MoveSectionDown(index)),
                );
            let controls = widget::row::with_capacity(2)
                .spacing(4)
                .align_y(Alignment::Center)
                .push(up)
                .push(down);

            order = order.add(widget::settings::item::builder(section.label()).control(controls));
        }

        let reset = widget::button::standard("Reset to default")
            .leading_icon(widget::icon::from_name("view-refresh-symbolic"))
            .on_press(Message::ResetPosition);
        let edit = widget::button::suggested(if self.config.widget_movable {
            "Editing"
        } else {
            "Edit"
        })
        .leading_icon(widget::icon::from_name("edit-symbolic"))
        .on_press_maybe((!self.config.widget_movable).then_some(Message::EditPosition));
        let position_controls = widget::row::with_capacity(2)
            .spacing(8)
            .align_y(Alignment::Center)
            .push(reset)
            .push(edit);

        let position = widget::settings::section()
            .title("Position")
            .add(
                widget::settings::item::builder("Overlay position")
                    .description(format!(
                        "{} px from left, {} px from top",
                        self.config.widget_x, self.config.widget_y
                    ))
                    .control(position_controls),
            )
            .add(
                widget::settings::item::builder("Horizontal offset")
                    .description("Pixels from the left edge")
                    .control(
                        widget::text_input("0", &self.x_input)
                            .on_input(Message::UpdateX)
                            .width(Length::Fixed(SHORT_INPUT_WIDTH)),
                    ),
            )
            .add(
                widget::settings::item::builder("Vertical offset")
                    .description("Pixels from the top edge")
                    .control(
                        widget::text_input("0", &self.y_input)
                            .on_input(Message::UpdateY)
                            .width(Length::Fixed(SHORT_INPUT_WIDTH)),
                    ),
            );

        self.page(widget::settings::view_column(vec![
            order.into(),
            position.into(),
        ]))
    }

    fn services_page(&self) -> Element<'_, Message> {
        let devices = widget::settings::section().title("Devices").add(
            widget::settings::item::builder("Solaar compatibility fallback")
                .description("Query unsupported Logitech devices through Solaar")
                .toggler(
                    self.config.enable_solaar_integration,
                    Message::ToggleSolaarIntegration,
                ),
        );

        let weather = widget::settings::section().title("Weather").add(
            widget::settings::item::builder("Location")
                .description("City or city and region")
                .control(
                    widget::text_input("City, region", &self.weather_location_input)
                        .on_input(Message::UpdateWeatherLocation)
                        .width(Length::Fixed(LONG_INPUT_WIDTH)),
                ),
        );

        let notifications = widget::settings::section().title("Notifications").add(
            widget::settings::item::builder("History limit")
                .description("Keep between 1 and 20 notifications")
                .control(
                    widget::text_input("5", &self.max_notifications_input)
                        .on_input(Message::UpdateMaxNotifications)
                        .width(Length::Fixed(SHORT_INPUT_WIDTH)),
                ),
        );

        let media = widget::settings::section().title("Media").add(
            widget::settings::item::builder("Cider API token")
                .description("Optional token for authenticated Cider installations")
                .control(
                    widget::secure_input(
                        "Optional",
                        &self.cider_api_token_input,
                        Some(Message::ToggleCiderTokenVisibility),
                        self.cider_token_hidden,
                    )
                    .on_input(Message::UpdateCiderApiToken)
                    .width(Length::Fixed(LONG_INPUT_WIDTH)),
                ),
        );

        let mut sections: Vec<Element<'_, Message>> = vec![
            devices.into(),
            weather.into(),
            notifications.into(),
            media.into(),
        ];

        if !self.cached_devices.is_empty() {
            let mut devices = widget::settings::section().title("Remembered devices");
            for (index, device) in self.cached_devices.iter().enumerate() {
                let kind = device.kind.as_deref().unwrap_or("Device");
                let remove = widget::button::icon(widget::icon::from_name("user-trash-symbolic"))
                    .padding(6)
                    .on_press(Message::RemoveCachedDevice(index));
                devices = devices.add(
                    widget::settings::item::builder(&device.name)
                        .description(kind)
                        .control(remove),
                );
            }
            sections.push(devices.into());
        }

        self.page(widget::settings::view_column(sections))
    }

    fn behavior_page(&self) -> Element<'_, Message> {
        let general = widget::settings::section()
            .title("General")
            .add(
                widget::settings::item::builder("Start overlay automatically")
                    .description("Start when the panel applet loads")
                    .toggler(self.config.widget_autostart, Message::ToggleWidgetAutostart),
            )
            .add(
                widget::settings::item::builder("Debug logging")
                    .description("Write diagnostics to /tmp/cosmic-widget.log")
                    .toggler(self.config.enable_logging, Message::ToggleLogging),
            );

        self.page(widget::settings::view_column(vec![general.into()]))
    }
}

#[derive(Debug, Clone, Copy)]
struct TemperatureStylePreview {
    style: TemperatureGaugeStyle,
}

impl canvas::Program<Message, cosmic::Theme, cosmic::Renderer> for TemperatureStylePreview {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry<cosmic::Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let cosmic = theme.cosmic();
        let background: Color = theme.current_container().component.base.into();
        let track: Color = theme.current_container().component.on_disabled.into();
        let active: Color = cosmic.accent.base.into();
        let preview = canvas::Path::rounded_rectangle(
            Point::ORIGIN,
            bounds.size(),
            cosmic.corner_radii.radius_m.into(),
        );
        frame.fill(&preview, background);

        if self.style == TemperatureGaugeStyle::Text {
            draw_text_temperature_preview(&mut frame, bounds, track, active);
        } else {
            let radius = (bounds.height * 0.265).min(bounds.width * 0.13).min(27.0);
            let y = bounds.height / 2.0;
            draw_temperature_preview(
                &mut frame,
                self.style,
                Point::new(bounds.width * 0.3, y),
                radius,
                0.42,
                track,
                active,
            );
            draw_temperature_preview(
                &mut frame,
                self.style,
                Point::new(bounds.width * 0.7, y),
                radius,
                0.68,
                track,
                active,
            );
        }

        vec![frame.into_geometry()]
    }
}

fn draw_temperature_preview(
    frame: &mut canvas::Frame<cosmic::Renderer>,
    style: TemperatureGaugeStyle,
    center: Point,
    radius: f32,
    progress: f32,
    track: Color,
    active: Color,
) {
    const WIDTH: f32 = 5.0;
    let (start, sweep) = match style {
        TemperatureGaugeStyle::Arc => (3.0 * PI / 4.0, 3.0 * PI / 2.0),
        TemperatureGaugeStyle::Circular => (-PI / 2.0, 2.0 * PI),
        TemperatureGaugeStyle::Text => return,
    };

    if style == TemperatureGaugeStyle::Circular {
        frame.stroke(
            &canvas::Path::circle(center, radius),
            canvas::Stroke::default()
                .with_color(track)
                .with_width(WIDTH),
        );
    } else {
        frame.stroke(
            &preview_arc(center, radius, start, start + sweep),
            canvas::Stroke::default()
                .with_color(track)
                .with_width(WIDTH)
                .with_line_cap(canvas::LineCap::Round),
        );
    }

    frame.stroke(
        &preview_arc(center, radius, start, start + sweep * progress),
        canvas::Stroke::default()
            .with_color(active)
            .with_width(WIDTH)
            .with_line_cap(canvas::LineCap::Round),
    );
}

fn draw_text_temperature_preview(
    frame: &mut canvas::Frame<cosmic::Renderer>,
    bounds: Rectangle,
    track: Color,
    active: Color,
) {
    for (index, width) in [0.42_f32, 0.68].into_iter().enumerate() {
        let y = 31.0 + index as f32 * 41.0;
        frame.fill(&canvas::Path::circle(Point::new(22.0, y), 5.0), active);
        frame.fill(
            &canvas::Path::rounded_rectangle(
                Point::new(36.0, y - 4.0),
                Size::new((bounds.width - 98.0).max(24.0), 8.0),
                4.0.into(),
            ),
            track,
        );
        frame.fill(
            &canvas::Path::rounded_rectangle(
                Point::new(bounds.width - 49.0, y - 7.0),
                Size::new(33.0 * width + 12.0, 14.0),
                5.0.into(),
            ),
            active,
        );
    }
}

fn preview_arc(center: Point, radius: f32, start: f32, end: f32) -> canvas::Path {
    canvas::Path::new(|builder| {
        builder.arc(canvas::path::Arc {
            center,
            radius,
            start_angle: Radians(start),
            end_angle: Radians(end),
        });
    })
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
