// SPDX-License-Identifier: MPL-2.0

//! Compose the configured sections and overlay drag controls.

use super::Message;
use super::media_state::DismissingMedia;
use super::notification_state::{DismissingNotification, NotificationKey};
use super::sections::{ai_usage, clock, devices, media, notifications, storage, system, weather};
use super::stats::SystemSnapshot;
use super::surface::SURFACE_WIDTH;
use crate::config::{Config, WidgetSection};
use chrono::{DateTime, Local};
use cosmic::iced::{Length, mouse};
use cosmic::{Element, theme, widget};

pub(super) fn widget_view<'a>(
    config: &Config,
    now: DateTime<Local>,
    stats: &'a SystemSnapshot,
    expanded_notification_group: Option<&'a str>,
    expanded_notification: Option<&'a NotificationKey>,
    hovered_notification: Option<&'a NotificationKey>,
    notification_group_progress: f32,
    notification_group_expanded: bool,
    notification_progress: f32,
    dismissing_notifications: &'a [DismissingNotification],
    clearing_notifications: bool,
    clear_button_visibility: f32,
    notification_scroll_translation: f32,
    surface_height: u32,
    dismissing_media: Option<&'a DismissingMedia>,
    media_seek_preview: Option<f64>,
    media_timeline_hovered: bool,
) -> Element<'a, Message> {
    let spacing = theme::system_preference().cosmic().spacing;
    let now_timestamp = now.timestamp().max(0) as u64;
    let content_width = SURFACE_WIDTH as f32 - 2.0 * f32::from(spacing.space_m);
    let mut content = widget::column::with_capacity(config.section_order.len() + 1)
        .width(Length::Fixed(content_width))
        .spacing(spacing.space_s);
    let mut has_content = false;

    if config.show_clock || config.show_date {
        content = content.push(clock::clock_view(config, now, spacing.space_xxs));
        has_content = true;
    }

    for section in &config.section_order {
        let migrated = match section {
            WidgetSection::Utilization if system::show_utilization(config) => Some(
                system::utilization_view(config, stats, spacing.space_xs, spacing.space_xs),
            ),
            WidgetSection::Network if config.show_network => Some(system::network_view(
                stats,
                spacing.space_xs,
                spacing.space_xs,
            )),
            WidgetSection::DiskIo if config.show_disk => Some(system::disk_io_view(
                stats,
                spacing.space_xs,
                spacing.space_xs,
            )),
            WidgetSection::Temperatures if system::show_temperatures(config) => Some(
                system::temperature_view(config, stats, spacing.space_s, spacing.space_xs),
            ),
            WidgetSection::Storage if config.show_storage => Some(storage::storage_view(
                config,
                stats,
                spacing.space_xs,
                spacing.space_xxs,
            )),
            WidgetSection::Battery if config.show_battery => Some(devices::devices_view(
                stats,
                spacing.space_xs,
                spacing.space_xs,
            )),
            WidgetSection::Weather if config.show_weather => Some(weather::weather_view(
                stats,
                spacing.space_xs,
                spacing.space_s,
                spacing.space_xxs,
            )),
            WidgetSection::Notifications if config.show_notifications => {
                Some(notifications::notifications_view(
                    stats,
                    expanded_notification_group,
                    expanded_notification,
                    hovered_notification,
                    notification_group_progress,
                    notification_group_expanded,
                    notification_progress,
                    dismissing_notifications,
                    clearing_notifications,
                    clear_button_visibility,
                    notification_scroll_translation,
                    now_timestamp,
                    spacing.space_xs,
                    spacing.space_xxs,
                ))
            }
            WidgetSection::Media if config.show_media => Some(media::media_view(
                stats,
                dismissing_media,
                media_seek_preview,
                spacing.space_xs,
                spacing.space_xs,
                spacing.space_xxs,
                media_timeline_hovered,
            )),
            WidgetSection::CodexUsage if config.show_codex_usage => Some(ai_usage::view(
                &stats.codex_usage,
                &stats.claude_usage,
                now,
                config.use_24hour_time,
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

    let overlay: Element<'a, Message> = widget::container(content)
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
        let drag_layer: Element<'a, Message> = widget::mouse_area(
            widget::container(widget::space())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_move(Message::OverlayPointerMoved)
        .on_press(Message::BeginOverlayDrag)
        .on_release(Message::EndOverlayDrag)
        .interaction(mouse::Interaction::Grab)
        .into();
        let pin = widget::button::icon(widget::icon::from_name("pin-symbolic"))
            .class(theme::Button::Suggested)
            .tooltip("Pin overlay")
            .on_press(Message::PinOverlay);
        let pin_layer: Element<'a, Message> = widget::container(pin)
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
