// SPDX-License-Identifier: MPL-2.0

use super::gauge;
use super::stats::SystemSnapshot;
use crate::battery::BatteryDevice;
use crate::config::{Config, WidgetSection};
use crate::media::{AlbumArt, MediaInfo, PlaybackStatus};
use crate::notifications::Notification;
use crate::storage::DiskInfo;
use crate::weather::WeatherData;
use chrono::{DateTime, Local};
use cosmic::iced::core::image::FilterMethod;
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length, mouse};
use cosmic::{Element, theme, widget};
use std::rc::Rc;

const METRIC_ICON_SIZE: u16 = 18;
const METRIC_LABEL_WIDTH: f32 = 56.0;
const MEDIA_ACTIVE_SECTION_HEIGHT: f32 = 232.0;
const MEDIA_CONTENT_HEIGHT: f32 = 204.0;
const MEDIA_SOURCE_SELECTOR_HEIGHT: f32 = 34.0;
const MEDIA_SOURCE_BUTTON_SIZE: f32 = 34.0;
const MEDIA_SOURCE_DOT_SIZE: f32 = 12.0;
const MEDIA_ARTWORK_SIZE: f32 = 96.0;
const MEDIA_VIDEO_ARTWORK_WIDTH: f32 = 128.0;
const MEDIA_VIDEO_ARTWORK_HEIGHT: f32 = 72.0;
const MEDIA_CONTENT_SPACING: u16 = 12;
const MEDIA_CONTROL_ICON_SIZE: u16 = 20;
const MEDIA_CONTROL_PADDING: u16 = 6;
const MEDIA_CONTROL_SPACING: u16 = 8;
const MEDIA_TIMELINE_FOOTER_GAP: f32 = 4.0;

pub fn widget_view<'a>(
    config: &Config,
    now: DateTime<Local>,
    stats: &'a SystemSnapshot,
    expanded_notification_group: Option<&'a str>,
    expanded_notification: Option<&'a super::NotificationKey>,
    notification_group_progress: f32,
    notification_group_expanded: bool,
    notification_progress: f32,
    dismissing_notifications: &'a [super::DismissingNotification],
    notification_scroll_translation: f32,
    surface_height: u32,
    media_seek_preview: Option<f64>,
    media_timeline_hovered: bool,
) -> Element<'a, super::Message> {
    let spacing = theme::system_preference().cosmic().spacing;
    let now_timestamp = now.timestamp().max(0) as u64;
    let content_width = super::SURFACE_WIDTH as f32 - 2.0 * f32::from(spacing.space_m);
    let mut content = widget::column::with_capacity(config.section_order.len() + 1)
        .width(Length::Fixed(content_width))
        .spacing(spacing.space_s);
    let mut has_content = false;

    if config.show_clock || config.show_date {
        content = content.push(clock_view(config, now, spacing.space_xxs));
        has_content = true;
    }

    for section in &config.section_order {
        let migrated = match section {
            WidgetSection::Utilization if show_utilization(config) => Some(utilization_view(
                config,
                stats,
                spacing.space_xs,
                spacing.space_xs,
            )),
            WidgetSection::Network if config.show_network => {
                Some(network_view(stats, spacing.space_xs, spacing.space_xs))
            }
            WidgetSection::DiskIo if config.show_disk => {
                Some(disk_io_view(stats, spacing.space_xs, spacing.space_xs))
            }
            WidgetSection::Temperatures if show_temperatures(config) => Some(temperature_view(
                config,
                stats,
                spacing.space_s,
                spacing.space_xs,
            )),
            WidgetSection::Storage if config.show_storage => Some(storage_view(
                config,
                stats,
                spacing.space_xs,
                spacing.space_xxs,
            )),
            WidgetSection::Battery if config.show_battery => {
                Some(devices_view(stats, spacing.space_xs, spacing.space_xs))
            }
            WidgetSection::Weather if config.show_weather => Some(weather_view(
                stats,
                spacing.space_xs,
                spacing.space_s,
                spacing.space_xxs,
            )),
            WidgetSection::Notifications if config.show_notifications => Some(notifications_view(
                stats,
                expanded_notification_group,
                expanded_notification,
                notification_group_progress,
                notification_group_expanded,
                notification_progress,
                dismissing_notifications,
                notification_scroll_translation,
                now_timestamp,
                spacing.space_xs,
                spacing.space_xxs,
            )),
            WidgetSection::Media if config.show_media => Some(media_view(
                stats,
                media_seek_preview,
                spacing.space_xs,
                spacing.space_xs,
                spacing.space_xxs,
                media_timeline_hovered,
            )),
            _ => None,
        };

        if let Some(section) = migrated {
            if has_content {
                content = content.push(widget::divider::horizontal::light());
            }
            content = content.push(section);
            has_content = true;
        }
    }

    let overlay: Element<'a, super::Message> = widget::container(content)
        .padding(spacing.space_m)
        .width(Length::Fill)
        .height(Length::Fixed(surface_height as f32))
        .class(theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let mut style = theme::Container::background(cosmic, theme.transparent);
            style.border.radius = cosmic.corner_radii.radius_l.into();
            style
        }))
        .into();

    if config.widget_movable {
        let drag_layer: Element<'a, super::Message> = widget::mouse_area(
            widget::container(widget::space())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_move(super::Message::OverlayPointerMoved)
        .on_press(super::Message::BeginOverlayDrag)
        .on_release(super::Message::EndOverlayDrag)
        .interaction(mouse::Interaction::Grab)
        .into();
        let pin = widget::button::icon(widget::icon::from_name("pin-symbolic"))
            .class(theme::Button::Suggested)
            .tooltip("Pin overlay")
            .on_press(super::Message::PinOverlay);
        let pin_layer: Element<'a, super::Message> = widget::container(pin)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(cosmic::iced::alignment::Horizontal::Right)
            .align_y(cosmic::iced::alignment::Vertical::Top)
            .into();

        return cosmic::iced::widget::Stack::with_children([overlay, drag_layer, pin_layer])
            .width(Length::Fill)
            .height(Length::Fixed(surface_height as f32))
            .into();
    }

    widget::column::with_children([overlay.into()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn clock_view<'a>(
    config: &Config,
    now: DateTime<Local>,
    spacing: u16,
) -> Element<'a, super::Message> {
    let mut clock = widget::column::with_capacity(2).spacing(spacing);

    if config.show_clock {
        let (time, suffix) = format_time_parts(now, config.use_24hour_time);
        let time_row = widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .spacing(spacing)
            .push(
                widget::text::text(time)
                    .size(48)
                    .font(cosmic::font::semibold()),
            )
            .push(widget::text::title4(suffix));
        clock = clock.push(time_row);
    }

    if config.show_date {
        let date_row = widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .spacing(spacing)
            .push(widget::icon::from_name("x-office-calendar-symbolic").size(16))
            .push(widget::text::body(now.format("%A, %-d %B %Y").to_string()));
        clock = clock.push(date_row);
    }

    clock.into()
}

fn utilization_view<'a>(
    config: &Config,
    stats: &SystemSnapshot,
    section_spacing: u16,
    metric_spacing: u16,
) -> Element<'a, super::Message> {
    let mut section = section(
        "utilities-system-monitor-symbolic",
        "Utilization",
        section_spacing,
    );

    if config.show_cpu {
        section = section.push(metric(
            MetricIcon::Cpu,
            "CPU",
            stats.cpu_usage,
            config.show_percentages,
            metric_spacing,
        ));
    }
    if config.show_memory {
        section = section.push(metric(
            MetricIcon::Memory,
            "Memory",
            stats.memory_usage,
            config.show_percentages,
            metric_spacing,
        ));
    }
    if config.show_gpu {
        section = section.push(metric(
            MetricIcon::Gpu,
            "GPU",
            stats.gpu_usage,
            config.show_percentages,
            metric_spacing,
        ));
    }

    section.into()
}

fn network_view<'a>(
    stats: &SystemSnapshot,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, super::Message> {
    section(
        "network-transmit-receive-symbolic",
        "Network",
        section_spacing,
    )
    .push(network_rate_row(
        "network-receive-symbolic",
        "Download",
        stats.network_rx_rate,
        row_spacing,
    ))
    .push(network_rate_row(
        "network-transmit-symbolic",
        "Upload",
        stats.network_tx_rate,
        row_spacing,
    ))
    .into()
}

fn network_rate_row<'a>(
    icon_name: &'static str,
    label: &'static str,
    bytes_per_second: f64,
    spacing: u16,
) -> Element<'a, super::Message> {
    widget::row::with_capacity(4)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(widget::icon::from_name(icon_name).size(METRIC_ICON_SIZE))
        .push(widget::text::body(label))
        .push(widget::space::horizontal())
        .push(widget::text::monotext(format_network_rate(
            bytes_per_second,
        )))
        .into()
}

fn disk_io_view<'a>(
    stats: &SystemSnapshot,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, super::Message> {
    section(
        "drive-harddisk-solidstate-symbolic",
        "Disk I/O",
        section_spacing,
    )
    .push(network_rate_row(
        "document-open-symbolic",
        "Read",
        stats.disk_read_rate,
        row_spacing,
    ))
    .push(network_rate_row(
        "document-save-symbolic",
        "Write",
        stats.disk_write_rate,
        row_spacing,
    ))
    .into()
}

fn temperature_view<'a>(
    config: &Config,
    stats: &SystemSnapshot,
    section_spacing: u16,
    gauge_spacing: u16,
) -> Element<'a, super::Message> {
    if config.temperature_gauge_style == crate::config::TemperatureGaugeStyle::Text {
        let mut temperatures = section_with_icon(
            embedded_symbolic_icon(
                include_bytes!("../../assets/icons/temperature-filled-symbolic.svg"),
                18,
            ),
            "Temperatures",
            section_spacing,
        );
        if config.show_cpu_temp {
            temperatures = temperatures.push(temperature_text_item(
                MetricIcon::Cpu,
                "CPU",
                stats.cpu_temp,
                gauge_spacing,
            ));
        }
        if config.show_gpu_temp {
            temperatures = temperatures.push(temperature_text_item(
                MetricIcon::Gpu,
                "GPU",
                stats.gpu_temp,
                gauge_spacing,
            ));
        }
        return temperatures.into();
    }

    let mut gauges = widget::row::with_capacity(2)
        .spacing(gauge_spacing)
        .align_y(Alignment::Center);

    if config.show_cpu_temp {
        gauges = gauges.push(temperature_item(
            "CPU",
            stats.cpu_temp,
            config.temperature_gauge_style,
        ));
    }
    if config.show_gpu_temp {
        gauges = gauges.push(temperature_item(
            "GPU",
            stats.gpu_temp,
            config.temperature_gauge_style,
        ));
    }

    section_with_icon(
        embedded_symbolic_icon(
            include_bytes!("../../assets/icons/temperature-filled-symbolic.svg"),
            18,
        ),
        "Temperatures",
        section_spacing,
    )
    .push(widget::container(gauges).center_x(Length::Fill))
    .into()
}

fn storage_view<'a>(
    config: &Config,
    stats: &'a SystemSnapshot,
    section_spacing: u16,
    item_spacing: u16,
) -> Element<'a, super::Message> {
    let mut storage = section(
        "drive-harddisk-solidstate-symbolic",
        "Storage",
        section_spacing,
    );

    if stats.disks.is_empty() {
        storage = storage.push(widget::text::caption("No mounted storage found"));
    } else {
        for disk in &stats.disks {
            storage = storage.push(storage_item(disk, config.show_percentages, item_spacing));
        }
    }

    storage.into()
}

fn devices_view<'a>(
    stats: &'a SystemSnapshot,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, super::Message> {
    let mut devices = section(
        "preferences-input-devices-symbolic",
        "Devices",
        section_spacing,
    );

    if stats.devices.is_empty() {
        devices = devices.push(widget::text::caption("No battery devices found"));
    } else {
        for device in &stats.devices {
            devices = devices.push(device_item(device, row_spacing));
        }
    }

    devices.into()
}

fn weather_view<'a>(
    stats: &'a SystemSnapshot,
    section_spacing: u16,
    content_spacing: u16,
    detail_spacing: u16,
) -> Element<'a, super::Message> {
    let mut weather = section("weather-symbolic", "Weather", section_spacing);

    if let Some(data) = &stats.weather {
        weather = weather.push(weather_content(data, content_spacing, detail_spacing));
    } else {
        weather = weather.push(widget::text::caption("Retrieving weather..."));
    }

    weather.into()
}

fn notifications_view<'a>(
    stats: &'a SystemSnapshot,
    expanded_notification_group: Option<&'a str>,
    expanded_notification: Option<&'a super::NotificationKey>,
    notification_group_progress: f32,
    notification_group_expanded: bool,
    notification_progress: f32,
    dismissing_notifications: &'a [super::DismissingNotification],
    notification_scroll_translation: f32,
    now_timestamp: u64,
    section_spacing: u16,
    item_spacing: u16,
) -> Element<'a, super::Message> {
    let clear_all = widget::button::standard("Clear all")
        .height(Length::Fixed(28.0))
        .padding([0, 8])
        .font_size(12)
        .line_height(17)
        .on_press_maybe(
            (!stats.notifications.is_empty()).then_some(super::Message::ClearNotifications),
        );
    let heading = widget::row::with_capacity(4)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(filled_notification_icon())
        .push(widget::text::heading("Notifications"))
        .push(widget::space::horizontal())
        .push(clear_all);
    let mut notifications = widget::column::with_capacity(2)
        .spacing(section_spacing)
        .push(heading);

    if stats.notifications.is_empty() {
        notifications = notifications.push(widget::text::caption("No notifications"));
    } else {
        let mut list = widget::column::with_capacity(stats.notifications.len() * 2);
        let mut has_entries = false;

        for group in notification_groups(&stats.notifications) {
            if group.notifications.len() == 1 {
                let notification = group.notifications[0];
                let expanded =
                    expanded_notification.is_some_and(|selected| selected.matches(notification));
                let dismissal_progress =
                    notification_dismissal_progress(dismissing_notifications, notification);
                let extra_height = if expanded {
                    super::notification_extra_height(notification) as f32
                        * notification_progress.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                list = list.push(notification_list_entry(
                    notification_item(
                        notification,
                        expanded,
                        dismissal_progress.is_some(),
                        now_timestamp,
                        item_spacing,
                    ),
                    super::NOTIFICATION_ITEM_HEIGHT as f32 + extra_height,
                    has_entries,
                    dismissal_progress,
                ));
                has_entries = true;
                continue;
            }

            let group_mounted = expanded_notification_group == Some(group.source);
            list = list.push(notification_list_entry(
                notification_group_item(
                    &group,
                    group_mounted && notification_group_expanded,
                    item_spacing,
                ),
                super::NOTIFICATION_ITEM_HEIGHT as f32,
                has_entries,
                None,
            ));
            has_entries = true;
            if group_mounted {
                let mut group_items = widget::column::with_capacity(group.notifications.len());
                let mut group_height = 0.0;
                for notification in group.notifications {
                    let expanded = expanded_notification
                        .is_some_and(|selected| selected.matches(notification));
                    let dismissal_progress =
                        notification_dismissal_progress(dismissing_notifications, notification);
                    let extra_height = if expanded {
                        super::notification_extra_height(notification) as f32
                            * notification_progress.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let item_height = super::NOTIFICATION_ITEM_HEIGHT as f32 + extra_height;
                    group_height += item_height;
                    group_items = group_items.push(notification_list_entry(
                        notification_item(
                            notification,
                            expanded,
                            dismissal_progress.is_some(),
                            now_timestamp,
                            item_spacing,
                        ),
                        item_height,
                        true,
                        dismissal_progress,
                    ));
                }
                list = list.push(
                    widget::container(group_items)
                        .width(Length::Fill)
                        .height(Length::Fixed(
                            group_height * notification_group_progress.clamp(0.0, 1.0),
                        ))
                        .clip(true),
                );
            }
        }

        let list: Element<'a, super::Message> = widget::container(list.width(Length::Fill))
            .width(Length::Fill)
            .class(notification_panel_class())
            .into();
        notifications = notifications.push(
            widget::scrollable(super::translate::vertical(
                list,
                notification_scroll_translation,
            ))
            .width(Length::Fill)
            .height(Length::Fixed(
                super::notification_viewport_height_with_animation(
                    stats,
                    expanded_notification,
                    expanded_notification_group,
                    notification_progress,
                    notification_group_progress,
                ),
            ))
            .auto_scroll(true)
            .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                cosmic::iced::widget::scrollable::Scrollbar::hidden(),
            ))
            .on_scroll(|viewport| {
                super::Message::NotificationScrolled(viewport.absolute_offset().y)
            }),
        );
    }

    notifications.into()
}

fn notification_panel_class() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let container = theme.current_container();
        let mut background: Color = container.on.into();
        let mut border: Color = container.on.into();

        // A low-opacity foreground tint remains visibly distinct from the
        // surrounding surface without masking the shared compositor blur.
        background.a = if theme.transparent { 0.045 } else { 0.07 };
        border.a = if theme.transparent { 0.10 } else { 0.14 };

        cosmic::iced::widget::container::Style {
            icon_color: Some(container.on.into()),
            text_color: Some(container.on.into()),
            background: Some(Background::Color(background)),
            border: Border {
                color: border,
                width: 1.0,
                radius: cosmic.corner_radii.radius_s.into(),
            },
            ..Default::default()
        }
    })
}

fn notification_list_entry<'a>(
    content: Element<'a, super::Message>,
    height: f32,
    divided: bool,
    dismissal_progress: Option<f32>,
) -> Element<'a, super::Message> {
    let row = widget::row::with_capacity(2)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .push(widget::container(content))
        .push(widget::space::vertical().height(32))
        .padding([6, 8]);
    let mut entry = widget::column::with_capacity(2);

    if divided {
        entry =
            entry.push(widget::container(widget::divider::horizontal::default()).padding([0, 8]));
    }

    let entry: Element<'a, super::Message> = widget::container(entry.push(row))
        .width(Length::Fill)
        .height(Length::Fixed(height.max(0.0)))
        .clip(true)
        .into();

    match dismissal_progress {
        Some(progress) => super::slide::left(entry, progress),
        None => entry,
    }
}

fn notification_dismissal_progress(
    dismissals: &[super::DismissingNotification],
    notification: &Notification,
) -> Option<f32> {
    dismissals
        .iter()
        .find(|dismissal| dismissal.matches(notification))
        .map(|dismissal| dismissal.animation.progress)
}

struct NotificationGroup<'a> {
    source: &'a str,
    notifications: Vec<&'a Notification>,
}

fn notification_groups(notifications: &[Notification]) -> Vec<NotificationGroup<'_>> {
    let mut groups: Vec<NotificationGroup<'_>> = Vec::new();

    for notification in notifications {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.source == super::notification_source(notification))
        {
            group.notifications.push(notification);
        } else {
            groups.push(NotificationGroup {
                source: super::notification_source(notification),
                notifications: vec![notification],
            });
        }
    }

    groups
}

fn notification_group_item<'a>(
    group: &NotificationGroup<'a>,
    expanded: bool,
    spacing: u16,
) -> Element<'a, super::Message> {
    let text = widget::column::with_capacity(2)
        .width(Length::Fill)
        .spacing(0)
        .push(widget::text::caption_heading(group.source))
        .push(widget::text::caption(format!(
            "{} notifications",
            group.notifications.len()
        )));
    let chevron = widget::icon::from_name(if expanded {
        "go-down-symbolic"
    } else {
        "go-next-symbolic"
    })
    .size(16);
    let content = widget::row::with_capacity(3)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(notification_dot(group.source))
        .push(text)
        .push(chevron);

    widget::mouse_area(content)
        .on_press(super::Message::ToggleNotificationGroup {
            source: group.source.to_string(),
        })
        .interaction(cosmic::iced::mouse::Interaction::Pointer)
        .into()
}

fn notification_item<'a>(
    notification: &'a Notification,
    expanded: bool,
    dismissing: bool,
    now_timestamp: u64,
    spacing: u16,
) -> Element<'a, super::Message> {
    let raw_summary = if notification.summary.trim().is_empty() {
        &notification.app_name
    } else {
        &notification.summary
    };
    let raw_detail = if notification.body.trim().is_empty() {
        &notification.app_name
    } else {
        &notification.body
    };
    let summary = if expanded {
        raw_summary.trim().to_string()
    } else {
        compact_single_line(raw_summary, usize::MAX)
    };
    let detail = if expanded {
        raw_detail.trim().to_string()
    } else {
        compact_single_line(raw_detail, usize::MAX)
    };
    let wrapping = if expanded {
        cosmic::iced::widget::text::Wrapping::WordOrGlyph
    } else {
        cosmic::iced::widget::text::Wrapping::None
    };
    let ellipsize = if expanded {
        cosmic::iced::widget::text::Ellipsize::None
    } else {
        cosmic::iced::widget::text::Ellipsize::End(
            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
        )
    };
    let dismiss = widget::button::icon(widget::icon::from_name("window-close-symbolic").size(14))
        .tooltip("Dismiss notification")
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(28.0))
        .padding(5)
        .on_press_maybe(
            (!dismissing).then_some(super::Message::DismissNotification {
                app_name: notification.app_name.clone(),
                timestamp: notification.timestamp,
            }),
        );
    let text = widget::column::with_capacity(2)
        .width(Length::Fill)
        .spacing(0)
        .push(
            widget::text::caption_heading(summary)
                .width(Length::Fill)
                .wrapping(wrapping)
                .ellipsize(ellipsize),
        )
        .push(
            widget::text::caption(detail)
                .width(Length::Fill)
                .wrapping(wrapping)
                .ellipsize(ellipsize),
        );
    let content = widget::row::with_capacity(2)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(notification_dot(&notification.app_name))
        .push(text);
    let content = widget::mouse_area(content)
        .on_press(super::Message::ToggleNotification {
            app_name: notification.app_name.clone(),
            timestamp: notification.timestamp,
        })
        .interaction(cosmic::iced::mouse::Interaction::Pointer);
    let age = widget::container(widget::text::caption(relative_notification_time(
        now_timestamp,
        notification.timestamp,
    )))
    .width(Length::Fill)
    .align_x(cosmic::iced::alignment::Horizontal::Right);
    let metadata = widget::row::with_capacity(2)
        .width(Length::Fixed(88.0))
        .align_y(Alignment::Center)
        .spacing(2)
        .push(age)
        .push(dismiss);

    widget::row::with_capacity(2)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(content)
        .push(metadata)
        .into()
}

fn filled_notification_icon() -> Element<'static, super::Message> {
    embedded_symbolic_icon(
        include_bytes!("../../assets/icons/notification-bell-filled-symbolic.svg"),
        18,
    )
}

fn media_view<'a>(
    stats: &'a SystemSnapshot,
    seek_preview: Option<f64>,
    section_spacing: u16,
    content_spacing: u16,
    detail_spacing: u16,
    timeline_hovered: bool,
) -> Element<'a, super::Message> {
    if let Some((_, info)) = stats
        .media
        .current_player()
        .filter(|(_, info)| info.is_active())
    {
        return section("emblem-music-symbolic", "Now Playing", section_spacing)
            .height(Length::Fixed(MEDIA_ACTIVE_SECTION_HEIGHT))
            .push(media_content(
                info,
                &stats.media,
                seek_preview,
                content_spacing,
                detail_spacing,
                timeline_hovered,
            ))
            .into();
    }

    section("emblem-music-symbolic", "Now Playing", section_spacing)
        .push(widget::text::caption("No media playing"))
        .into()
}

fn media_content<'a>(
    info: &'a MediaInfo,
    player_state: &'a crate::media::MultiPlayerState,
    seek_preview: Option<f64>,
    _content_spacing: u16,
    detail_spacing: u16,
    timeline_hovered: bool,
) -> Element<'a, super::Message> {
    let progress = seek_preview
        .unwrap_or_else(|| info.progress())
        .clamp(0.0, 1.0);
    let position = if seek_preview.is_some() && info.duration > 0 {
        (info.duration as f64 * progress) as u64
    } else {
        info.position
    };
    let previous = widget::button::icon(
        widget::icon::from_name("media-skip-backward-symbolic").size(MEDIA_CONTROL_ICON_SIZE),
    )
    .tooltip("Previous track")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(
        info.can_go_previous
            .then_some(super::Message::PreviousMedia),
    );
    let play_pause_icon = match info.status {
        PlaybackStatus::Playing => "media-playback-pause-symbolic",
        PlaybackStatus::Paused | PlaybackStatus::Stopped => "media-playback-start-symbolic",
    };
    let play_pause = widget::button::icon(
        widget::icon::from_name(play_pause_icon).size(MEDIA_CONTROL_ICON_SIZE),
    )
    .tooltip(match info.status {
        PlaybackStatus::Playing => "Pause",
        PlaybackStatus::Paused | PlaybackStatus::Stopped => "Play",
    })
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe((info.can_play || info.can_pause).then_some(super::Message::PlayPauseMedia));
    let next = widget::button::icon(
        widget::icon::from_name("media-skip-forward-symbolic").size(MEDIA_CONTROL_ICON_SIZE),
    )
    .tooltip("Next track")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(info.can_go_next.then_some(super::Message::NextMedia));
    let controls = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(MEDIA_CONTROL_SPACING)
        .push(previous)
        .push(play_pause)
        .push(next);
    let identity = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(detail_spacing)
        .push(media_player_badge(&info.player_name))
        .push(widget::space::horizontal())
        .push(controls);
    let subtitle = media_subtitle(info);
    let metadata = widget::column::with_capacity(3)
        .width(Length::Fill)
        .spacing(detail_spacing)
        .push(super::marquee::media_title(&info.title))
        .push(super::marquee::media_subtitle(&subtitle))
        .push(identity);
    let details = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(MEDIA_CONTENT_SPACING)
        .push(media_artwork(info.album_art.as_ref()))
        .push(metadata);
    let progress_control: Element<'a, super::Message> = if info.can_seek && info.duration > 0 {
        let slider = widget::slider(0.0..=1.0, progress, super::Message::MediaSeekChanged)
            .step(0.001)
            .height(20)
            .class(theme::style::iced::Slider::Custom {
                active: Rc::new(move |theme| {
                    media_seek_style(
                        theme,
                        cosmic::iced::widget::slider::Status::Active,
                        timeline_hovered,
                    )
                }),
                hovered: Rc::new(|theme| {
                    media_seek_style(theme, cosmic::iced::widget::slider::Status::Hovered, true)
                }),
                dragging: Rc::new(|theme| {
                    media_seek_style(theme, cosmic::iced::widget::slider::Status::Dragged, true)
                }),
            })
            .on_release(super::Message::CommitMediaSeek);

        widget::mouse_area(slider)
            .on_enter(super::Message::MediaTimelineHoverChanged(true))
            .on_exit(super::Message::MediaTimelineHoverChanged(false))
            .into()
    } else {
        widget::progress_bar::linear::Linear::new()
            .girth(6)
            .progress(progress as f32)
            .width(Length::Fill)
            .into()
    };
    let current_time =
        widget::container(widget::text::body(format_media_time(position))).width(Length::Fill);
    let duration = widget::container(widget::text::body(format_media_time(info.duration)))
        .width(Length::Fill)
        .align_x(cosmic::iced::alignment::Horizontal::Right);
    let mut footer = widget::row::with_capacity(3)
        .width(Length::Fill)
        .height(Length::Fixed(MEDIA_SOURCE_SELECTOR_HEIGHT))
        .align_y(Alignment::Center)
        .push(current_time);
    if player_state.player_count() > 1 {
        footer = footer.push(media_source_selector(player_state));
    }
    footer = footer.push(duration);

    widget::column::with_capacity(5)
        .height(Length::Fixed(MEDIA_CONTENT_HEIGHT))
        .spacing(0)
        .push(details)
        .push(widget::space::vertical())
        .push(progress_control)
        .push(widget::space().height(Length::Fixed(MEDIA_TIMELINE_FOOTER_GAP)))
        .push(footer)
        .into()
}

fn media_seek_style(
    theme: &cosmic::Theme,
    status: cosmic::iced::widget::slider::Status,
    show_handle: bool,
) -> cosmic::iced::widget::slider::Style {
    use cosmic::iced::widget::slider::{Catalog as _, HandleShape};

    let mut style = theme.style(&theme::style::iced::Slider::Standard, status);
    if !show_handle {
        style.handle.shape = HandleShape::Circle { radius: 0.0 };
        style.handle.background = Background::Color(Color::TRANSPARENT);
        style.handle.border_width = 0.0;
    }
    style
}

fn media_source_selector<'a>(
    player_state: &'a crate::media::MultiPlayerState,
) -> Element<'a, super::Message> {
    let mut dots = widget::row::with_capacity(player_state.player_count())
        .height(Length::Fixed(MEDIA_SOURCE_SELECTOR_HEIGHT))
        .align_y(Alignment::Center)
        .spacing(2);

    for (index, (player_id, info)) in player_state.players.iter().enumerate() {
        let dot = media_source_dot(index == player_state.current_index);
        let button = widget::button::custom(dot)
            .width(Length::Fixed(MEDIA_SOURCE_BUTTON_SIZE))
            .height(Length::Fixed(MEDIA_SOURCE_BUTTON_SIZE))
            .padding((MEDIA_SOURCE_BUTTON_SIZE - MEDIA_SOURCE_DOT_SIZE) / 2.0)
            .class(theme::Button::Text)
            .on_press(super::Message::SelectMediaPlayer(player_id.clone()));
        dots = dots.push(widget::tooltip(
            button,
            widget::text::caption(format!("Switch to {}", info.player_name)),
            widget::tooltip::Position::Top,
        ));
    }

    dots.into()
}

fn media_source_dot(active: bool) -> Element<'static, super::Message> {
    widget::container(widget::space())
        .width(Length::Fixed(MEDIA_SOURCE_DOT_SIZE))
        .height(Length::Fixed(MEDIA_SOURCE_DOT_SIZE))
        .class(theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let accent: Color = cosmic.accent_color().into();
            let neutral: Color = cosmic.on_bg_color().into();

            cosmic::iced::widget::container::Style {
                background: active.then_some(Background::Color(accent)),
                border: Border {
                    color: Color { a: 0.55, ..neutral },
                    width: if active { 0.0 } else { 1.0 },
                    radius: [MEDIA_SOURCE_DOT_SIZE / 2.0; 4].into(),
                },
                ..Default::default()
            }
        }))
        .into()
}

fn media_artwork(art: Option<&AlbumArt>) -> Element<'static, super::Message> {
    if let Some(art) = art {
        let is_video_art = u64::from(art.source_width) * 4 >= u64::from(art.source_height) * 5;
        let (width, height) = if is_video_art {
            (MEDIA_VIDEO_ARTWORK_WIDTH, MEDIA_VIDEO_ARTWORK_HEIGHT)
        } else {
            (MEDIA_ARTWORK_SIZE, MEDIA_ARTWORK_SIZE)
        };
        return widget::image(art.iced_handle.clone())
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .content_fit(ContentFit::Cover)
            .filter_method(FilterMethod::Linear)
            .border_radius([4.0; 4])
            .into();
    }

    widget::container(widget::icon::from_name("audio-x-generic-symbolic").size(48))
        .center_x(Length::Fixed(MEDIA_ARTWORK_SIZE))
        .center_y(Length::Fixed(MEDIA_ARTWORK_SIZE))
        .class(theme::Container::List)
        .into()
}

fn media_player_badge(player_name: &str) -> Element<'static, super::Message> {
    widget::container(widget::text::body(compact_single_line(player_name, 18)))
        .padding([3, 8])
        .class(theme::Container::Secondary)
        .into()
}

fn media_subtitle(info: &MediaInfo) -> String {
    let subtitle = match (info.artist.trim(), info.album.trim()) {
        ("", "") => info.player_name.clone(),
        (artist, "") => artist.to_string(),
        ("", album) => album.to_string(),
        (artist, album) => format!("{artist} - {album}"),
    };
    compact_single_line(&subtitle, usize::MAX)
}

fn format_media_time(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn notification_dot(app_name: &str) -> Element<'static, super::Message> {
    let band = notification_band(app_name);

    widget::container(
        widget::space()
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0)),
    )
    .class(theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        let color = match band {
            NotificationBand::Accent => cosmic.accent_color(),
            NotificationBand::Success => cosmic.success_color(),
            NotificationBand::Warning => cosmic.warning_color(),
            NotificationBand::Destructive => cosmic.destructive_color(),
        };

        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(color.into())),
            border: Border {
                radius: [4.0; 4].into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }))
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationBand {
    Accent,
    Success,
    Warning,
    Destructive,
}

fn notification_band(app_name: &str) -> NotificationBand {
    match app_name
        .bytes()
        .fold(0_u8, |hash, byte| hash.wrapping_add(byte))
        % 4
    {
        0 => NotificationBand::Accent,
        1 => NotificationBand::Success,
        2 => NotificationBand::Warning,
        _ => NotificationBand::Destructive,
    }
}

fn compact_single_line(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }

    let mut compact = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    compact.push('…');
    compact
}

fn relative_notification_time(now: u64, timestamp: u64) -> String {
    let elapsed = now.saturating_sub(timestamp);
    match elapsed {
        0..=4 => "now".to_string(),
        5..=59 => format!("{elapsed}s ago"),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

fn weather_content<'a>(
    data: &'a WeatherData,
    content_spacing: u16,
    detail_spacing: u16,
) -> Element<'a, super::Message> {
    let temperature = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(detail_spacing)
        .push(widget::text::title4(format_weather_temperature(
            data.temperature,
        )))
        .push(widget::space::horizontal())
        .push(feels_like_badge(data.feels_like));

    let details = widget::column::with_capacity(3)
        .width(Length::Fill)
        .spacing(detail_spacing)
        .push(temperature)
        .push(widget::text::body(&data.description))
        .push(widget::text::caption(&data.location));

    widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(content_spacing)
        .push(weather_condition_icon(&data.icon))
        .push(details)
        .into()
}

fn weather_condition_icon(icon: &str) -> Element<'static, super::Message> {
    let mut handle = widget::icon::from_name(weather_icon_name(icon)).handle();
    handle.symbolic = true;
    widget::icon::icon(handle).size(64).into()
}

fn feels_like_badge(value: f32) -> Element<'static, super::Message> {
    widget::container(widget::text::caption(format!(
        "Feels like {}",
        format_weather_temperature(value)
    )))
    .padding([4, 8])
    .class(theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let warning: Color = cosmic.warning_color().into();
        let background = Color { a: 0.16, ..warning };
        let border = Color { a: 0.7, ..warning };

        cosmic::iced::widget::container::Style {
            text_color: Some(warning),
            background: Some(Background::Color(background)),
            border: Border {
                color: border,
                width: 1.0,
                radius: cosmic.corner_radii.radius_s.into(),
            },
            ..Default::default()
        }
    }))
    .into()
}

fn weather_icon_name(icon: &str) -> &'static str {
    match icon {
        "01d" => "weather-clear-symbolic",
        "01n" => "weather-clear-night-symbolic",
        "02d" => "weather-few-clouds-symbolic",
        "02n" => "weather-few-clouds-night-symbolic",
        "03d" | "03n" | "04d" | "04n" => "weather-overcast-symbolic",
        "09d" | "09n" | "10d" | "10n" => "weather-showers-symbolic",
        "11d" | "11n" => "weather-storm-symbolic",
        "13d" | "13n" => "weather-snow-symbolic",
        "50d" | "50n" => "weather-fog-symbolic",
        _ => "weather-severe-alert-symbolic",
    }
}

fn format_weather_temperature(value: f32) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract().abs() < f32::EPSILON {
        format!("{rounded:.0}\u{b0}C")
    } else {
        format!("{rounded:.1}\u{b0}C")
    }
}

fn section<'a>(
    icon_name: &'static str,
    title: &'a str,
    spacing: u16,
) -> cosmic::widget::Column<'a, super::Message, cosmic::Theme> {
    section_with_icon(
        widget::icon::from_name(icon_name).size(18).into(),
        title,
        spacing,
    )
}

fn section_with_icon<'a>(
    icon: Element<'static, super::Message>,
    title: &'a str,
    spacing: u16,
) -> cosmic::widget::Column<'a, super::Message, cosmic::Theme> {
    let heading = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(icon)
        .push(widget::text::heading(title));

    widget::column::with_capacity(4)
        .spacing(spacing)
        .push(heading)
}

fn metric<'a>(
    icon: MetricIcon,
    label: &'a str,
    value: f32,
    show_percentage: bool,
    spacing: u16,
) -> Element<'a, super::Message> {
    let value = value.clamp(0.0, 100.0);
    let mut metric_row = widget::row::with_capacity(4)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(metric_icon(icon))
        .push(
            widget::container(widget::text::caption_heading(label))
                .width(Length::Fixed(METRIC_LABEL_WIDTH)),
        )
        .push(gauge::indicator_bar(value));

    if show_percentage {
        metric_row = metric_row.push(widget::text::monotext(format!("{value:>5.1}%")));
    }

    metric_row.into()
}

fn storage_item<'a>(
    disk: &'a DiskInfo,
    show_percentage: bool,
    spacing: u16,
) -> Element<'a, super::Message> {
    let percentage = disk.used_percentage.clamp(0.0, 100.0);
    let mut title = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(widget::text::body(&disk.name).width(Length::Fill));

    if show_percentage {
        title = title.push(widget::text::monotext(format!("{percentage:.1}%")));
    }

    let details = if disk.is_loading || disk.total_space == 0 {
        "Loading...".to_string()
    } else {
        let used = disk.total_space.saturating_sub(disk.available_space);
        format!(
            "{} / {}",
            format_storage_bytes(used),
            format_storage_bytes(disk.total_space)
        )
    };

    widget::column::with_capacity(3)
        .spacing(spacing)
        .push(title)
        .push(gauge::indicator_bar(percentage))
        .push(
            widget::row::with_capacity(2)
                .push(widget::space::horizontal())
                .push(widget::text::caption(details)),
        )
        .into()
}

fn device_item<'a>(device: &'a BatteryDevice, spacing: u16) -> Element<'a, super::Message> {
    widget::row::with_capacity(4)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(device_icon(device.kind.as_deref()))
        .push(widget::text::body(&device.name).width(Length::Fill))
        .push(battery_status(device, spacing))
        .into()
}

fn device_icon(kind: Option<&str>) -> Element<'static, super::Message> {
    let kind = kind.unwrap_or_default().to_ascii_lowercase();
    let icon = if kind.contains("mouse") {
        "input-mouse-symbolic"
    } else if kind.contains("keyboard") {
        "input-keyboard-symbolic"
    } else if kind.contains("headset") || kind.contains("headphone") {
        "audio-headset-symbolic"
    } else if kind.contains("controller") || kind.contains("gamepad") {
        "input-gaming-symbolic"
    } else {
        "preferences-input-devices-symbolic"
    };

    widget::icon::from_name(icon).size(METRIC_ICON_SIZE).into()
}

fn battery_status(device: &BatteryDevice, spacing: u16) -> Element<'static, super::Message> {
    let (icon, label, band, opacity) = battery_visuals(device);

    let battery_icon = widget::icon::from_name(icon)
        .icon()
        .size(METRIC_ICON_SIZE)
        .opacity(opacity)
        .class(theme::Svg::custom(move |theme| {
            let cosmic = theme.cosmic();
            let color = match band {
                BatteryBand::Success => cosmic.success_color(),
                BatteryBand::Warning => cosmic.warning_color(),
                BatteryBand::Destructive => cosmic.destructive_color(),
                BatteryBand::Cached => cosmic.accent_color(),
                BatteryBand::Unavailable => cosmic.on_bg_color(),
            };

            cosmic::iced::widget::svg::Style {
                color: Some(color.into()),
            }
        }));

    widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(spacing / 2)
        .push(battery_icon)
        .push(widget::text::monotext(label))
        .into()
}

fn battery_visuals(device: &BatteryDevice) -> (String, String, BatteryBand, f32) {
    if device.is_loading && device.level.is_some() {
        let level = device.level.unwrap_or_default();
        (
            battery_icon_name(level, is_charging(device.status.as_deref())),
            format!("{level}%"),
            BatteryBand::Cached,
            0.8,
        )
    } else if device.is_loading {
        (
            "battery-missing-symbolic".to_string(),
            "...".to_string(),
            BatteryBand::Unavailable,
            0.6,
        )
    } else if !device.is_connected {
        (
            "battery-missing-symbolic".to_string(),
            "N/A".to_string(),
            BatteryBand::Unavailable,
            0.6,
        )
    } else if let Some(level) = device.level {
        (
            battery_icon_name(level, is_charging(device.status.as_deref())),
            format!("{level}%"),
            battery_band(level),
            1.0,
        )
    } else {
        (
            "battery-missing-symbolic".to_string(),
            "N/A".to_string(),
            BatteryBand::Unavailable,
            0.6,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatteryBand {
    Success,
    Warning,
    Destructive,
    Cached,
    Unavailable,
}

fn battery_band(level: u8) -> BatteryBand {
    match level {
        0..=15 => BatteryBand::Destructive,
        16..=30 => BatteryBand::Warning,
        _ => BatteryBand::Success,
    }
}

fn battery_icon_name(level: u8, charging: bool) -> String {
    let bucket = match level.min(100) {
        0..=2 => 0,
        3..=7 => 5,
        8..=15 => 10,
        16..=27 => 20,
        28..=42 => 35,
        43..=57 => 50,
        58..=72 => 65,
        73..=85 => 80,
        86..=95 => 90,
        _ => 100,
    };
    let charging = if charging { "-charging" } else { "" };
    format!("cosmic-applet-battery-level-{bucket}{charging}-symbolic")
}

fn is_charging(status: Option<&str>) -> bool {
    status.is_some_and(|status| {
        let status = status.to_ascii_lowercase();
        status.starts_with("charging") || status.starts_with("recharging")
    })
}

fn format_storage_bytes(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = KB * 1_000.0;
    const GB: f64 = MB * 1_000.0;
    const TB: f64 = GB * 1_000.0;

    let bytes = bytes as f64;
    if bytes >= TB {
        format!("{:.1} TB", bytes / TB)
    } else if bytes >= GB {
        format!("{:.0} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn format_network_rate(bytes_per_second: f64) -> String {
    const KB: f64 = 1_024.0;
    const MB: f64 = KB * 1_024.0;
    const GB: f64 = MB * 1_024.0;

    let rate = if bytes_per_second.is_finite() {
        bytes_per_second.max(0.0)
    } else {
        0.0
    };

    if rate >= GB {
        format!("{:.1} GB/s", rate / GB)
    } else if rate >= MB {
        format!("{:.1} MB/s", rate / MB)
    } else if rate >= KB {
        format!("{:.1} KB/s", rate / KB)
    } else {
        format!("{rate:.0} B/s")
    }
}

#[derive(Clone, Copy)]
enum MetricIcon {
    Cpu,
    Memory,
    Gpu,
}

fn metric_icon(icon: MetricIcon) -> Element<'static, super::Message> {
    let bytes: &'static [u8] = match icon {
        MetricIcon::Cpu => include_bytes!("../../assets/icons/cpu-symbolic.svg"),
        MetricIcon::Memory => include_bytes!("../../assets/icons/memory-symbolic.svg"),
        MetricIcon::Gpu => include_bytes!("../../assets/icons/gpu-symbolic.svg"),
    };
    embedded_symbolic_icon(bytes, METRIC_ICON_SIZE)
}

fn embedded_symbolic_icon(bytes: &'static [u8], size: u16) -> Element<'static, super::Message> {
    let mut handle = widget::icon::from_svg_bytes(bytes);
    handle.symbolic = true;
    widget::icon::icon(handle).size(size).into()
}

fn temperature_item<'a>(
    label: &'a str,
    value: f32,
    style: crate::config::TemperatureGaugeStyle,
) -> Element<'a, super::Message> {
    widget::column::with_capacity(2)
        .align_x(Alignment::Center)
        .spacing(4)
        .push(gauge::temperature_gauge(value, style))
        .push(widget::text::heading(label))
        .into()
}

fn temperature_text_item<'a>(
    icon: MetricIcon,
    label: &'a str,
    value: f32,
    spacing: u16,
) -> Element<'a, super::Message> {
    widget::row::with_capacity(4)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(metric_icon(icon))
        .push(widget::text::body(label))
        .push(widget::space::horizontal())
        .push(widget::text::monotext(format!("{value:.0}°C")))
        .into()
}

fn show_utilization(config: &Config) -> bool {
    config.show_cpu || config.show_memory || config.show_gpu
}

fn show_temperatures(config: &Config) -> bool {
    config.show_cpu_temp || config.show_gpu_temp
}

fn format_time_parts(now: DateTime<Local>, use_24hour_time: bool) -> (String, String) {
    if use_24hour_time {
        (
            now.format("%H:%M").to_string(),
            now.format(":%S").to_string(),
        )
    } else {
        (
            now.format("%-I:%M").to_string(),
            now.format(":%S %p").to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BatteryBand, NotificationBand, battery_band, battery_icon_name, battery_visuals,
        compact_single_line, format_media_time, format_network_rate, format_storage_bytes,
        format_weather_temperature, is_charging, media_subtitle, notification_band,
        relative_notification_time, weather_icon_name,
    };
    use crate::battery::BatteryDevice;
    use crate::media::MediaInfo;

    #[test]
    fn formats_storage_capacities_for_compact_display() {
        assert_eq!(format_storage_bytes(1_900_000_000_000), "1.9 TB");
        assert_eq!(format_storage_bytes(608_000_000_000), "608 GB");
        assert_eq!(format_storage_bytes(950_000_000), "950 MB");
        assert_eq!(format_storage_bytes(999), "999 B");
    }

    #[test]
    fn formats_network_rates_for_compact_display() {
        assert_eq!(format_network_rate(0.0), "0 B/s");
        assert_eq!(format_network_rate(1_536.0), "1.5 KB/s");
        assert_eq!(format_network_rate(12.25 * 1_024.0 * 1_024.0), "12.2 MB/s");
        assert_eq!(format_network_rate(f64::NAN), "0 B/s");
    }

    #[test]
    fn battery_visuals_follow_level_and_charging_state() {
        assert_eq!(battery_band(82), BatteryBand::Success);
        assert_eq!(battery_band(24), BatteryBand::Warning);
        assert_eq!(battery_band(10), BatteryBand::Destructive);
        assert_eq!(
            battery_icon_name(82, false),
            "cosmic-applet-battery-level-80-symbolic"
        );
        assert_eq!(
            battery_icon_name(69, true),
            "cosmic-applet-battery-level-65-charging-symbolic"
        );
        assert!(is_charging(Some("recharging")));
        assert!(!is_charging(Some("discharging")));
    }

    #[test]
    fn cached_battery_readings_use_the_accent_band_until_verified() {
        let device = BatteryDevice {
            name: "Test Headset".to_string(),
            level: Some(68),
            status: Some("charging".to_string()),
            kind: Some("headset".to_string()),
            codename: None,
            is_loading: true,
            is_connected: false,
        };

        let (icon, label, band, opacity) = battery_visuals(&device);

        assert_eq!(icon, "cosmic-applet-battery-level-65-charging-symbolic");
        assert_eq!(label, "68%");
        assert_eq!(band, BatteryBand::Cached);
        assert_eq!(opacity, 0.8);
    }

    #[test]
    fn weather_visuals_use_cosmic_condition_icons_and_compact_units() {
        assert_eq!(weather_icon_name("01n"), "weather-clear-night-symbolic");
        assert_eq!(weather_icon_name("02d"), "weather-few-clouds-symbolic");
        assert_eq!(weather_icon_name("04d"), "weather-overcast-symbolic");
        assert_eq!(weather_icon_name("13d"), "weather-snow-symbolic");
        assert_eq!(format_weather_temperature(3.2), "3.2\u{b0}C");
        assert_eq!(format_weather_temperature(1.0), "1\u{b0}C");
    }

    #[test]
    fn notification_metadata_is_compact_and_stable() {
        assert_eq!(relative_notification_time(1_000, 1_000), "now");
        assert_eq!(relative_notification_time(1_000, 955), "45s ago");
        assert_eq!(relative_notification_time(4_700, 1_000), "1h ago");
        assert_eq!(relative_notification_time(90_000, 1_000), "1d ago");
        assert_eq!(
            compact_single_line("  Backup  finished\nnow  ", 30),
            "Backup finished now"
        );
        assert_eq!(compact_single_line("abcdefghij", 6), "abcde\u{2026}");
        assert_eq!(notification_band("System"), notification_band("System"));
        assert!(matches!(
            notification_band("System"),
            NotificationBand::Accent
                | NotificationBand::Success
                | NotificationBand::Warning
                | NotificationBand::Destructive
        ));
    }

    #[test]
    fn media_metadata_is_prepared_for_iced() {
        let info = MediaInfo {
            player_name: "Firefox".to_string(),
            artist: "Deftones".to_string(),
            album: "Saturday Night Wrist".to_string(),
            ..Default::default()
        };

        assert_eq!(media_subtitle(&info), "Deftones - Saturday Night Wrist");
        assert_eq!(format_media_time(302_000), "5:02");

        let long_album = MediaInfo {
            album: "Final Straw (20th Anniversary Edition)".to_string(),
            ..Default::default()
        };
        assert_eq!(
            media_subtitle(&long_album),
            "Final Straw (20th Anniversary Edition)"
        );
    }
}
