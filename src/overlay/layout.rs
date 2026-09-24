// SPDX-License-Identifier: MPL-2.0

//! Surface and notification viewport dimensions for the current content.

use super::notification_state::{NotificationKey, notification_source};
use super::sections::ai_usage;
use super::stats::SystemSnapshot;
use crate::config::{Config, WidgetSection};
use std::collections::HashSet;

pub(super) const BASE_SURFACE_HEIGHT: u32 = 556;
pub(super) const NETWORK_SECTION_HEIGHT: u32 = 120;
pub(super) const DISK_IO_SECTION_HEIGHT: u32 = 120;
pub(super) const EMPTY_STORAGE_HEIGHT: u32 = 63;
pub(super) const STORAGE_SECTION_HEIGHT: u32 = 38;
pub(super) const STORAGE_ITEM_HEIGHT: u32 = 62;
pub(super) const EMPTY_DEVICES_HEIGHT: u32 = 83;
pub(super) const DEVICES_SECTION_HEIGHT: u32 = 54;
pub(super) const DEVICE_ITEM_HEIGHT: u32 = 33;
pub(super) const EMPTY_WEATHER_HEIGHT: u32 = 83;
pub(super) const WEATHER_SECTION_HEIGHT: u32 = 154;
pub(super) const EMPTY_NOTIFICATIONS_HEIGHT: u32 = 83;
pub(super) const NOTIFICATIONS_SECTION_HEIGHT: u32 = 65;
pub(super) const NOTIFICATION_ITEM_HEIGHT: u32 = 47;
pub(super) const FILE_TRANSFER_PROGRESS_EXTRA_HEIGHT: u32 = 24;
pub(super) const MAX_VISIBLE_NOTIFICATION_ROWS: u32 = 4;
pub(super) const NOTIFICATION_LINE_HEIGHT: u32 = 17;
pub(super) const NOTIFICATION_CHARS_PER_LINE: usize = 32;

pub(super) const EMPTY_MEDIA_HEIGHT: u32 = 107;
pub(super) const MEDIA_SECTION_HEIGHT: u32 = 248;

pub(super) fn desired_surface_height(config: &Config, snapshot: &SystemSnapshot) -> u32 {
    desired_surface_height_with_expansion(config, snapshot, None, None)
}

pub(super) fn desired_surface_height_with_expansion(
    config: &Config,
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_notification_group: Option<&str>,
) -> u32 {
    desired_surface_height_with_animation(
        config,
        snapshot,
        expanded_notification,
        expanded_notification_group,
        if expanded_notification.is_some() {
            1.0
        } else {
            0.0
        },
        if expanded_notification_group.is_some() {
            1.0
        } else {
            0.0
        },
        false,
    )
}

pub(super) fn desired_surface_height_with_animation(
    config: &Config,
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_notification_group: Option<&str>,
    notification_progress: f32,
    group_progress: f32,
    media_dismissing: bool,
) -> u32 {
    let mut height = BASE_SURFACE_HEIGHT as f32;
    let network_visible = config.show_network
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Network));

    if network_visible {
        height += NETWORK_SECTION_HEIGHT as f32;
    }

    let disk_io_visible = config.show_disk
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::DiskIo));

    if disk_io_visible {
        height += DISK_IO_SECTION_HEIGHT as f32;
    }

    let storage_visible = config.show_storage
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Storage));

    if storage_visible {
        let storage_height = if snapshot.disks.is_empty() {
            EMPTY_STORAGE_HEIGHT
        } else {
            STORAGE_SECTION_HEIGHT + STORAGE_ITEM_HEIGHT.saturating_mul(snapshot.disks.len() as u32)
        };
        height += storage_height as f32;
    }

    let devices_visible = config.show_battery
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Battery));

    if devices_visible {
        let devices_height = if snapshot.devices.is_empty() {
            EMPTY_DEVICES_HEIGHT
        } else {
            DEVICES_SECTION_HEIGHT
                + DEVICE_ITEM_HEIGHT.saturating_mul(snapshot.devices.len() as u32)
        };
        height += devices_height as f32;
    }

    let weather_visible = config.show_weather
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Weather));

    if weather_visible {
        let weather_height = if snapshot.weather.is_some() {
            WEATHER_SECTION_HEIGHT
        } else {
            EMPTY_WEATHER_HEIGHT
        };
        height += weather_height as f32;
    }

    let notifications_visible = config.show_notifications
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Notifications));

    if notifications_visible {
        let notifications_height = if snapshot.notifications.is_empty() {
            EMPTY_NOTIFICATIONS_HEIGHT
        } else {
            NOTIFICATIONS_SECTION_HEIGHT
                + notification_viewport_height_with_animation(
                    snapshot,
                    expanded_notification,
                    expanded_notification_group,
                    notification_progress,
                    group_progress,
                )
                .round() as u32
        };
        height += notifications_height as f32;
    }

    let media_visible = config.show_media
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Media));

    if media_visible {
        let media_height = if media_dismissing
            || snapshot
                .media
                .current_player()
                .is_some_and(|(_, info)| info.is_active())
        {
            MEDIA_SECTION_HEIGHT
        } else {
            EMPTY_MEDIA_HEIGHT
        };
        height += media_height as f32;
    }

    if config.show_codex_usage && config.section_order.contains(&WidgetSection::CodexUsage) {
        height += ai_usage::section_height(&snapshot.codex_usage, &snapshot.claude_usage);
    }

    height.round() as u32
}

pub(super) fn notification_group_size(snapshot: &SystemSnapshot, source: &str) -> usize {
    snapshot
        .notifications
        .iter()
        .filter(|notification| notification_source(notification) == source)
        .count()
}

pub(super) fn notification_base_height(
    notification: &crate::monitors::notifications::Notification,
) -> u32 {
    NOTIFICATION_ITEM_HEIGHT
        + if notification
            .file_transfer
            .as_ref()
            .is_some_and(crate::monitors::notifications::FileTransfer::is_active)
        {
            FILE_TRANSFER_PROGRESS_EXTRA_HEIGHT
        } else {
            0
        }
}

pub(super) fn notification_group_base_height(
    notifications: &[&crate::monitors::notifications::Notification],
) -> u32 {
    notifications
        .iter()
        .map(|notification| notification_base_height(notification))
        .max()
        .unwrap_or(NOTIFICATION_ITEM_HEIGHT)
}

fn notification_display_rows(snapshot: &SystemSnapshot, expanded_group: Option<&str>) -> u32 {
    let mut groups: Vec<(&str, u32)> = Vec::new();

    for notification in &snapshot.notifications {
        let source = notification_source(notification);
        if let Some((_, count)) = groups
            .iter_mut()
            .find(|(group_source, _)| *group_source == source)
        {
            *count += 1;
        } else {
            groups.push((source, 1));
        }
    }

    groups
        .into_iter()
        .map(|(source, count)| {
            if count > 1 && expanded_group == Some(source) {
                count + 1
            } else {
                1
            }
        })
        .sum()
}

pub(super) fn notification_viewport_height(
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_group: Option<&str>,
) -> u32 {
    notification_viewport_height_with_animation(
        snapshot,
        expanded_notification,
        expanded_group,
        if expanded_notification.is_some() {
            1.0
        } else {
            0.0
        },
        if expanded_group.is_some() { 1.0 } else { 0.0 },
    )
    .round() as u32
}

pub(super) fn notification_viewport_height_with_animation(
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_group: Option<&str>,
    notification_progress: f32,
    group_progress: f32,
) -> f32 {
    let compact_rows = notification_display_rows(snapshot, None) as f32;
    let active_compact_groups = snapshot
        .notifications
        .iter()
        .filter(|notification| notification_base_height(notification) > NOTIFICATION_ITEM_HEIGHT)
        .map(notification_source)
        .collect::<HashSet<_>>()
        .len() as f32;
    let compact_height = NOTIFICATION_ITEM_HEIGHT as f32 * compact_rows
        + FILE_TRANSFER_PROGRESS_EXTRA_HEIGHT as f32 * active_compact_groups;
    let group_height = expanded_group
        .map(|source| {
            snapshot
                .notifications
                .iter()
                .filter(|notification| notification_source(notification) == source)
                .map(notification_base_height)
                .sum::<u32>() as f32
        })
        .unwrap_or(0.0)
        * group_progress.clamp(0.0, 1.0);
    let selected_group_progress = expanded_notification
        .and_then(|key| {
            snapshot
                .notifications
                .iter()
                .find(|notification| key.matches(notification))
        })
        .map(|notification| {
            let source = notification_source(notification);
            if notification_group_size(snapshot, source) > 1 {
                group_progress.clamp(0.0, 1.0)
            } else {
                1.0
            }
        })
        .unwrap_or(1.0);
    let expanded_height = expanded_notification_extra_height(snapshot, expanded_notification)
        as f32
        * notification_progress.clamp(0.0, 1.0)
        * selected_group_progress;
    let content_height = compact_height + group_height + expanded_height;

    content_height.min((NOTIFICATION_ITEM_HEIGHT * MAX_VISIBLE_NOTIFICATION_ROWS) as f32)
}

pub(super) fn expanded_notification_extra_height(
    snapshot: &SystemSnapshot,
    expanded: Option<&NotificationKey>,
) -> u32 {
    let Some(notification) = expanded.and_then(|key| {
        snapshot
            .notifications
            .iter()
            .find(|notification| key.matches(notification))
    }) else {
        return 0;
    };
    notification_extra_height(notification)
}

pub(super) fn notification_extra_height(
    notification: &crate::monitors::notifications::Notification,
) -> u32 {
    let summary = if notification.summary.trim().is_empty() {
        &notification.app_name
    } else {
        &notification.summary
    };
    let body = if notification.body.trim().is_empty() {
        &notification.app_name
    } else {
        &notification.body
    };
    let lines = estimated_wrapped_lines(summary, NOTIFICATION_CHARS_PER_LINE)
        + estimated_wrapped_lines(body, NOTIFICATION_CHARS_PER_LINE);

    lines
        .saturating_sub(2)
        .saturating_mul(NOTIFICATION_LINE_HEIGHT)
}

fn estimated_wrapped_lines(text: &str, line_width: usize) -> u32 {
    text.lines()
        .map(|line| {
            let characters = line.trim().chars().count().max(1);
            characters.div_ceil(line_width) as u32
        })
        .sum::<u32>()
        .max(1)
}
