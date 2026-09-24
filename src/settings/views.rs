// SPDX-License-Identifier: MPL-2.0

//! Settings page layouts and controls.

use super::{Message, SettingsApp, section_enabled};
use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

const PAGE_WIDTH: f32 = 720.0;
const SHORT_INPUT_WIDTH: f32 = 140.0;
const LONG_INPUT_WIDTH: f32 = 280.0;

impl SettingsApp {
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

    pub(super) fn display_page(&self) -> Element<'_, Message> {
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
            )
            .add(
                widget::settings::item::builder("AI Usage")
                    .description("Show remaining Codex and Claude usage")
                    .toggler(self.config.show_codex_usage, Message::ToggleCodexUsage),
            );

        self.page(widget::settings::view_column(vec![
            clock.into(),
            metrics.into(),
            temperatures.into(),
            temperature_style,
            sections.into(),
        ]))
    }

    pub(super) fn layout_page(&self) -> Element<'_, Message> {
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

    pub(super) fn services_page(&self) -> Element<'_, Message> {
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

    pub(super) fn behavior_page(&self) -> Element<'_, Message> {
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
