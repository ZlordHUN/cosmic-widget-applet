// SPDX-License-Identifier: MPL-2.0

use crate::monitors::notifications::{FileTransfer, Notification};
use crate::overlay::Message;
use crate::overlay::components::{
    section::{compact_single_line, embedded_symbolic_icon},
    shrink, slide, translate,
};
use crate::overlay::layout;
use crate::overlay::notification_state::{
    DismissingNotification, NotificationKey, notification_source,
};
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Background, Border, Color, Length};
use cosmic::{Element, theme, widget};

const CLEAR_ALL_BUTTON_WIDTH: f32 = 64.0;

pub(in crate::overlay) fn notifications_view<'a>(
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
    now_timestamp: u64,
    section_spacing: u16,
    item_spacing: u16,
) -> Element<'a, Message> {
    let mut heading = widget::row::with_capacity(4)
        .height(Length::Fixed(28.0))
        .align_y(Alignment::Center)
        .spacing(8)
        .push(filled_notification_icon())
        .push(widget::text::heading("Notifications"))
        .push(widget::space::horizontal());
    if !stats.notifications.is_empty()
        || clearing_notifications
        || clear_button_visibility > f32::EPSILON
    {
        let clear_all: Element<'a, Message> = widget::button::standard("Clear all")
            .width(Length::Fixed(CLEAR_ALL_BUTTON_WIDTH))
            .height(Length::Fixed(28.0))
            .padding([0, 8])
            .font_size(12)
            .line_height(17)
            .on_press_maybe((!clearing_notifications).then_some(Message::ClearNotifications))
            .into();
        heading = heading.push(if clear_button_visibility < 1.0 {
            shrink::out(clear_all, 1.0 - clear_button_visibility)
        } else {
            clear_all
        });
    }
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
                let hovered =
                    hovered_notification.is_some_and(|selected| selected.matches(notification));
                let dismissal_progress =
                    notification_dismissal_progress(dismissing_notifications, notification);
                let extra_height = if expanded {
                    layout::notification_extra_height(notification) as f32
                        * notification_progress.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                list = list.push(notification_list_entry(
                    notification_item(
                        notification,
                        expanded,
                        dismissal_progress.is_some(),
                        true,
                        hovered,
                        now_timestamp,
                        item_spacing,
                    ),
                    layout::notification_base_height(notification) as f32 + extra_height,
                    has_entries,
                    dismissal_progress,
                ));
                has_entries = true;
                continue;
            }

            let group_mounted = expanded_notification_group == Some(group.source);
            let dismissal_progress =
                notification_group_dismissal_progress(dismissing_notifications, &group);
            list = list.push(notification_list_entry(
                notification_group_item(
                    &group,
                    group_mounted && notification_group_expanded,
                    item_spacing,
                ),
                layout::notification_group_base_height(&group.notifications) as f32,
                has_entries,
                dismissal_progress,
            ));
            has_entries = true;
            if group_mounted {
                let mut group_items = widget::column::with_capacity(group.notifications.len());
                let mut group_height = 0.0;
                for notification in group.notifications {
                    let expanded = expanded_notification
                        .is_some_and(|selected| selected.matches(notification));
                    let hovered =
                        hovered_notification.is_some_and(|selected| selected.matches(notification));
                    let dismissal_progress =
                        notification_dismissal_progress(dismissing_notifications, notification);
                    let extra_height = if expanded {
                        layout::notification_extra_height(notification) as f32
                            * notification_progress.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let item_height =
                        layout::notification_base_height(notification) as f32 + extra_height;
                    group_height += item_height;
                    group_items = group_items.push(notification_list_entry(
                        notification_item(
                            notification,
                            expanded,
                            dismissal_progress.is_some(),
                            true,
                            hovered,
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

        let list: Element<'a, Message> = widget::container(list.width(Length::Fill))
            .width(Length::Fill)
            .class(notification_panel_class())
            .into();
        notifications = notifications.push(
            widget::scrollable(translate::vertical(list, notification_scroll_translation))
                .width(Length::Fill)
                .height(Length::Fixed(
                    layout::notification_viewport_height_with_animation(
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
                .on_scroll(|viewport| Message::NotificationScrolled(viewport.absolute_offset().y)),
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
    content: Element<'a, Message>,
    height: f32,
    divided: bool,
    dismissal_progress: Option<f32>,
) -> Element<'a, Message> {
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

    let entry: Element<'a, Message> = widget::container(entry.push(row))
        .width(Length::Fill)
        .height(Length::Fixed(height.max(0.0)))
        .clip(true)
        .into();

    match dismissal_progress {
        Some(progress) => slide::left(entry, progress),
        None => entry,
    }
}

fn notification_dismissal_progress(
    dismissals: &[DismissingNotification],
    notification: &Notification,
) -> Option<f32> {
    dismissals
        .iter()
        .find(|dismissal| dismissal.matches(notification))
        .map(|dismissal| dismissal.animation.progress)
}

fn notification_group_dismissal_progress(
    dismissals: &[DismissingNotification],
    group: &NotificationGroup<'_>,
) -> Option<f32> {
    group
        .notifications
        .iter()
        .map(|notification| notification_dismissal_progress(dismissals, notification))
        .try_fold(1.0_f32, |progress, item| {
            item.map(|item_progress| progress.min(item_progress))
        })
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
            .find(|group| group.source == notification_source(notification))
        {
            group.notifications.push(notification);
        } else {
            groups.push(NotificationGroup {
                source: notification_source(notification),
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
) -> Element<'a, Message> {
    let transfer_notification = notification_group_transfer(&group.notifications);
    let title = widget::text::caption_heading(compact_single_line(group.source, usize::MAX))
        .width(Length::Fill)
        .wrapping(cosmic::iced::widget::text::Wrapping::None)
        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
        ));
    let text = widget::column::with_capacity(2)
        .width(Length::Fill)
        .spacing(0)
        .push(title)
        .push(
            widget::text::caption(match transfer_notification {
                Some(notification) => compact_single_line(&notification.summary, usize::MAX),
                None => format!("{} notifications", group.notifications.len()),
            })
            .width(Length::Fill)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            )),
        );
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
    let content = notification_with_transfer_progress(
        content.into(),
        transfer_notification.and_then(active_file_transfer),
        spacing,
    );

    widget::mouse_area(content)
        .on_press(Message::ToggleNotificationGroup {
            source: group.source.to_string(),
        })
        .interaction(cosmic::iced::mouse::Interaction::Pointer)
        .into()
}

fn notification_item<'a>(
    notification: &'a Notification,
    expanded: bool,
    dismissing: bool,
    allow_open_action: bool,
    hovered: bool,
    now_timestamp: u64,
    spacing: u16,
) -> Element<'a, Message> {
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
        .on_press_maybe((!dismissing).then_some(Message::DismissNotification {
            key: notification.identity(),
        }));
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
        .on_press(Message::ToggleNotification {
            key: notification.identity(),
        })
        .interaction(cosmic::iced::mouse::Interaction::Pointer);
    let open_message = notification
        .open_folder
        .as_ref()
        .map(|_| Message::OpenNotificationFolder {
            key: notification.identity(),
        })
        .or_else(|| {
            notification
                .activation_action
                .as_ref()
                .map(|_| Message::ActivateNotification {
                    key: notification.identity(),
                })
        });
    let actionable = allow_open_action && open_message.is_some();
    let age_or_action: Element<'a, Message> = if actionable && hovered {
        widget::container(
            widget::button::standard("Open")
                .height(Length::Fixed(28.0))
                .padding([0, 8])
                .font_size(12)
                .line_height(17)
                .on_press(open_message.expect("actionable notifications have an open message")),
        )
        .width(Length::Fill)
        .align_x(cosmic::iced::alignment::Horizontal::Right)
        .into()
    } else {
        widget::container(widget::text::caption(relative_notification_time(
            now_timestamp,
            notification.timestamp,
        )))
        .width(Length::Fill)
        .align_x(cosmic::iced::alignment::Horizontal::Right)
        .into()
    };
    let metadata = widget::row::with_capacity(2)
        .width(Length::Fixed(88.0))
        .align_y(Alignment::Center)
        .spacing(2)
        .push(age_or_action)
        .push(dismiss);

    let row: Element<'a, Message> = widget::row::with_capacity(2)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(content)
        .push(metadata)
        .into();
    let row = notification_with_transfer_progress(row, active_file_transfer(notification), spacing);

    if actionable {
        widget::mouse_area(row)
            .on_enter(Message::NotificationHoverChanged {
                key: notification.identity(),
                hovered: true,
            })
            .on_exit(Message::NotificationHoverChanged {
                key: notification.identity(),
                hovered: false,
            })
            .into()
    } else {
        row
    }
}

fn active_file_transfer(notification: &Notification) -> Option<&FileTransfer> {
    notification
        .file_transfer
        .as_ref()
        .filter(|transfer| transfer.is_active())
}

pub(in crate::overlay) fn notification_group_transfer<'a>(
    notifications: &[&'a Notification],
) -> Option<&'a Notification> {
    notifications
        .iter()
        .copied()
        .find(|notification| active_file_transfer(notification).is_some())
        .or_else(|| {
            notifications
                .first()
                .copied()
                .filter(|notification| notification.file_transfer.is_some())
        })
}

fn file_transfer_progress(transfer: &FileTransfer) -> Element<'static, Message> {
    let progress = transfer.progress.min(100);
    widget::row::with_capacity(2)
        .width(Length::Fill)
        .height(Length::Fixed(18.0))
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::progress_bar::linear::Linear::new()
                .girth(3)
                .progress(f32::from(progress) / 100.0)
                .width(Length::Fill),
        )
        .push(
            widget::container(widget::text::caption(format!("{progress}%")))
                .width(Length::Fixed(36.0))
                .align_x(cosmic::iced::alignment::Horizontal::Right),
        )
        .into()
}

fn notification_with_transfer_progress<'a>(
    content: Element<'a, Message>,
    transfer: Option<&FileTransfer>,
    spacing: u16,
) -> Element<'a, Message> {
    let Some(transfer) = transfer else {
        return content;
    };
    widget::column::with_capacity(2)
        .width(Length::Fill)
        .spacing(6)
        .push(content)
        .push(
            widget::container(file_transfer_progress(transfer))
                .width(Length::Fill)
                // Align the bar with the caption after the notification dot.
                .padding(cosmic::iced::Padding {
                    left: 8.0 + f32::from(spacing),
                    ..Default::default()
                }),
        )
        .into()
}

fn filled_notification_icon() -> Element<'static, Message> {
    embedded_symbolic_icon(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/notification-bell-filled-symbolic.svg"
        )),
        18,
    )
}

fn notification_dot(app_name: &str) -> Element<'static, Message> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
