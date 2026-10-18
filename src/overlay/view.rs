// SPDX-License-Identifier: MPL-2.0

//! Compose the configured sections and overlay drag controls.

use super::Message;
use super::components::section::{CARD_SPACING, section_card};
use super::device_state::DeviceList;
use super::media_state::DismissingMedia;
use super::notification_state::{DismissingNotification, NotificationKey};
use super::sections::{ai_usage, clock, devices, media, notifications, storage, system, weather};
use super::stats::SystemSnapshot;
use super::surface::SURFACE_WIDTH;
use crate::config::{Config, WidgetSection};
use chrono::{DateTime, Local};
use cosmic::iced::{Length, Padding, mouse};
use cosmic::{Element, theme, widget};

pub(super) fn widget_view<'a>(
    config: &Config,
    now: DateTime<Local>,
    stats: &'a SystemSnapshot,
    device_list: &'a DeviceList,
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
    dismissing_media: Option<&'a DismissingMedia>,
    media_seek_preview: Option<f64>,
    media_timeline_hovered: bool,
) -> Element<'a, Message> {
    let spacing = theme::system_preference().cosmic().spacing;
    let now_timestamp = now.timestamp().max(0) as u64;
    // Split the original horizontal inset between the surface and its cards,
    // keeping all existing section content at the same width.
    let outer_inset = spacing.space_m / 2;
    let card_inset = spacing.space_m - outer_inset;
    let content_width = SURFACE_WIDTH as f32 - 2.0 * f32::from(outer_inset);
    let mut content = widget::column::with_capacity(config.section_order.len() + 1)
        .width(Length::Fixed(content_width))
        .spacing(CARD_SPACING);

    if config.show_clock || config.show_date {
        content = content.push(
            widget::container(clock::clock_view(config, now, spacing.space_xxs))
                .padding([0, card_inset]),
        );
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
                device_list,
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
            content = content.push(section_card(section, card_inset));
        }
    }

    let overlay: Element<'a, Message> = widget::container(content)
        .padding(Padding {
            top: f32::from(spacing.space_m),
            right: f32::from(outer_inset),
            bottom: f32::from(outer_inset),
            left: f32::from(outer_inset),
        })
        .width(Length::Fill)
        .height(Length::Shrink)
        .class(theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let mut style = theme::Container::background(cosmic, theme.transparent);
            style.border.radius = cosmic.corner_radii.radius_l.into();
            style
        }))
        .into();
    // Measure the actual cards, including wrapping and animated content. The
    // background never reserves empty space from an estimated section height.
    let overlay: Element<'a, Message> = cosmic::iced::widget::sensor(overlay)
        .on_resize(|size| Message::OverlayMeasured(size.height.ceil().max(1.0) as u32))
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
            .height(Length::Shrink)
            .into();
    }

    widget::column::with_children([overlay.into()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
