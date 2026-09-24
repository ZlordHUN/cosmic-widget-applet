// SPDX-License-Identifier: MPL-2.0

//! Integration checks for overlay layout and state transitions.

use super::animation::*;
use super::layout::*;
use super::media_state::*;
use super::notification_state::*;
use super::sections::notifications::notification_group_transfer;
use super::stats::SystemSnapshot;
use super::surface::*;
use super::ticks::*;
use crate::config::{Config, WidgetSection};
use crate::monitors::battery::BatteryDevice;
use crate::monitors::media::MultiPlayerState;
use crate::monitors::media::{MediaInfo, PlaybackStatus, PlayerId};
use crate::monitors::notifications::{FileTransfer, FileTransferState, Notification};
use crate::monitors::storage::DiskInfo;
use crate::monitors::weather::WeatherData;
use cosmic::iced::platform_specific::runtime::wayland::CornerRadius;
use std::time::{Duration, Instant};

#[test]
fn frosted_blur_region_excludes_rounded_corners() {
    let corners = CornerRadius {
        top_left: 16,
        top_right: 16,
        bottom_left: 16,
        bottom_right: 16,
    };

    let regions = rounded_surface_regions(100, corners);
    let top = regions.first().unwrap();
    let middle = &regions[16];
    let bottom = &regions[17];

    assert!(top.x > 0.0);
    assert!(top.width < SURFACE_WIDTH as f32);
    assert_eq!((middle.x, middle.y), (0.0, 16.0));
    assert_eq!(middle.width, SURFACE_WIDTH as f32);
    assert_eq!(middle.height, 68.0);
    assert_eq!((bottom.x, bottom.y), (top.x, 99.0));
    assert_eq!(bottom.width, top.width);
}

#[test]
fn ui_ticks_align_just_after_the_next_epoch_boundary() {
    let interval = Duration::from_secs(1);

    assert_eq!(
        delay_until_next_tick(Duration::from_millis(1_250), interval),
        Duration::from_millis(750) + UI_TICK_SETTLE_DELAY,
    );
    assert_eq!(
        delay_until_next_tick(Duration::from_secs(2), interval),
        interval + UI_TICK_SETTLE_DELAY,
    );
}

#[test]
fn overlay_dragging_uses_the_fixed_grab_point() {
    assert_eq!(
        dragged_overlay_position(
            7250,
            50,
            cosmic::iced::Point::new(40.0, 30.0),
            cosmic::iced::Point::new(55.4, 22.2),
        ),
        (7265, 42)
    );
}

#[test]
fn compositor_catch_up_does_not_reverse_the_drag() {
    let origin = cosmic::iced::Point::new(80.0, 40.0);
    let moved = dragged_overlay_position(7250, 50, origin, cosmic::iced::Point::new(90.0, 45.0));
    assert_eq!(moved, (7260, 55));

    assert_eq!(
        dragged_overlay_position(moved.0, moved.1, origin, origin),
        moved
    );
}

#[test]
fn surface_height_tracks_visible_storage_rows() {
    let mut config = Config::default();
    config.show_storage = true;
    config.section_order = vec![WidgetSection::Storage];
    let mut snapshot = SystemSnapshot::default();

    let empty_height = desired_surface_height(&config, &snapshot);
    snapshot.disks = vec![disk(), disk(), disk()];

    assert!(empty_height > BASE_SURFACE_HEIGHT);
    assert_eq!(desired_surface_height(&config, &snapshot), 780);
}

#[test]
fn surface_height_tracks_network_visibility() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_network = true;
    config.section_order = vec![WidgetSection::Network];

    assert_eq!(
        desired_surface_height(&config, &SystemSnapshot::default()),
        BASE_SURFACE_HEIGHT + NETWORK_SECTION_HEIGHT
    );
}

#[test]
fn surface_height_tracks_disk_io_visibility() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_disk = true;
    config.section_order = vec![WidgetSection::DiskIo];

    assert_eq!(
        desired_surface_height(&config, &SystemSnapshot::default()),
        BASE_SURFACE_HEIGHT + DISK_IO_SECTION_HEIGHT
    );
}

#[test]
fn surface_height_tracks_visible_device_rows() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_battery = true;
    config.section_order = vec![WidgetSection::Battery];
    let mut snapshot = SystemSnapshot::default();

    let empty_height = desired_surface_height(&config, &snapshot);
    snapshot.devices = vec![device(), device(), device()];

    assert!(empty_height > BASE_SURFACE_HEIGHT);
    assert_eq!(desired_surface_height(&config, &snapshot), 709);
}

#[test]
fn surface_height_tracks_loaded_weather_content() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_weather = true;
    config.section_order = vec![WidgetSection::Weather];
    let mut snapshot = SystemSnapshot::default();

    let loading_height = desired_surface_height(&config, &snapshot);
    snapshot.weather = Some(weather());

    assert!(loading_height > BASE_SURFACE_HEIGHT);
    assert_eq!(desired_surface_height(&config, &snapshot), 710);
}

#[test]
fn surface_height_groups_notifications_and_caps_the_viewport() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_notifications = true;
    config.section_order = vec![WidgetSection::Notifications];
    let mut snapshot = SystemSnapshot::default();

    let empty_height = desired_surface_height(&config, &snapshot);
    snapshot.notifications = vec![notification(), notification(), notification()];

    assert!(empty_height > BASE_SURFACE_HEIGHT);
    assert_eq!(desired_surface_height(&config, &snapshot), 668);
    assert_eq!(
        desired_surface_height_with_expansion(
            &config,
            &snapshot,
            None,
            Some("Package manager updated")
        ),
        809
    );

    snapshot.notifications = (0..5)
        .map(|index| {
            let mut item = notification();
            item.app_name = format!("App {index}");
            item
        })
        .collect();
    assert_eq!(desired_surface_height(&config, &snapshot), 809);
}

#[test]
fn surface_height_grows_only_after_notification_selection() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_notifications = true;
    config.section_order = vec![WidgetSection::Notifications];
    let mut snapshot = SystemSnapshot::default();
    let mut item = notification();
    item.body = "A complete notification body that wraps across several lines so all of its content remains readable when expanded.".to_string();
    let selected = item.identity();
    snapshot.notifications = vec![item];
    let compact_height = desired_surface_height(&config, &snapshot);

    assert!(
        desired_surface_height_with_expansion(&config, &snapshot, Some(&selected), None)
            > compact_height
    );
}

#[test]
fn notification_expansion_uses_an_eased_reversible_transition() {
    let started = Instant::now();
    let mut animation = ExpansionAnimation::default();

    animation.transition_to(1.0, started);
    animation.advance(started + NOTIFICATION_EXPANSION_DURATION / 2);
    assert!((animation.progress - 0.5).abs() < 0.01);

    animation.transition_to(0.0, started + NOTIFICATION_EXPANSION_DURATION / 2);
    animation.advance(started + NOTIFICATION_EXPANSION_DURATION * 2);
    assert_eq!(animation.progress, 0.0);
    assert!(animation.is_collapsed());
}

#[test]
fn clear_button_growth_reverses_when_notifications_disappear() {
    let started = Instant::now();
    let mut animation = ExpansionAnimation::default();

    transition_clear_button_for_notification_change(&mut animation, false, true, false, started);
    animation.advance(started + NOTIFICATION_EXPANSION_DURATION / 2);
    assert!((animation.progress - 0.5).abs() < 0.01);

    let reversed_at = started + NOTIFICATION_EXPANSION_DURATION / 2;
    transition_clear_button_for_notification_change(
        &mut animation,
        true,
        false,
        false,
        reversed_at,
    );
    animation.advance(reversed_at + NOTIFICATION_EXPANSION_DURATION / 2);

    assert_eq!(animation.progress, 0.0);
    assert!(animation.is_collapsed());
}

#[test]
fn notification_group_expansion_uses_a_slower_stable_transition() {
    let started = Instant::now();
    let mut animation = ExpansionAnimation::with_duration(NOTIFICATION_GROUP_EXPANSION_DURATION);

    animation.transition_to(1.0, started);
    animation.advance(started + NOTIFICATION_EXPANSION_DURATION);
    assert!(animation.progress < 1.0);
    assert!(animation.is_animating());

    animation.advance(started + NOTIFICATION_GROUP_EXPANSION_DURATION);
    assert_eq!(animation.progress, 1.0);
    assert!(!animation.is_animating());
}

#[test]
fn notification_group_expansion_reverses_from_its_current_progress() {
    let started = Instant::now();
    let duration = NOTIFICATION_GROUP_EXPANSION_DURATION;
    let mut animation = ExpansionAnimation::with_duration(duration);

    animation.transition_to(1.0, started);
    animation.advance(started + duration / 2);
    assert!((animation.progress - 0.5).abs() < 0.01);

    let reversed_at = started + duration / 2;
    animation.transition_to(0.0, reversed_at);
    animation.advance(reversed_at + Duration::from_millis(16));
    assert!(animation.progress <= 0.46);

    animation.advance(reversed_at + duration / 2);

    assert_eq!(animation.progress, 0.0);
    assert!(animation.is_collapsed());
}

#[test]
fn notification_group_viewport_grows_with_animation_progress() {
    let mut snapshot = SystemSnapshot::default();
    snapshot.notifications = vec![notification(), notification(), notification()];

    let compact = notification_viewport_height_with_animation(
        &snapshot,
        None,
        Some("Package manager updated"),
        0.0,
        0.0,
    );
    let halfway = notification_viewport_height_with_animation(
        &snapshot,
        None,
        Some("Package manager updated"),
        0.0,
        0.5,
    );
    let expanded = notification_viewport_height_with_animation(
        &snapshot,
        None,
        Some("Package manager updated"),
        0.0,
        1.0,
    );

    assert!(compact < halfway);
    assert!(halfway < expanded);
}

#[test]
fn active_file_transfer_is_visible_in_a_group_with_newer_notifications() {
    let mut latest = notification();
    latest.app_name = "COSMIC Files".to_string();
    let mut transfer = notification();
    transfer.app_name = latest.app_name.clone();
    transfer.summary = "Copying files".to_string();
    transfer.file_transfer = Some(FileTransfer {
        progress: 35,
        state: FileTransferState::Running,
    });
    let grouped = [&latest, &transfer];

    let preview = notification_group_transfer(&grouped).unwrap();
    assert_eq!(preview.summary, "Copying files");
    assert_eq!(preview.file_transfer.as_ref().unwrap().progress, 35);

    transfer.file_transfer.as_mut().unwrap().state = FileTransferState::Paused;
    let grouped = [&latest, &transfer];
    assert_eq!(
        notification_group_transfer(&grouped).unwrap().summary,
        "Copying files"
    );
}

#[test]
fn file_transfer_completion_keeps_the_group_preview_and_restores_compact_height() {
    let mut transfer = notification();
    transfer.app_name = "COSMIC Files".to_string();
    transfer.summary = "Moving files".to_string();
    transfer.file_transfer = Some(FileTransfer {
        progress: 80,
        state: FileTransferState::Running,
    });
    let mut older = notification();
    older.app_name = transfer.app_name.clone();
    let mut snapshot = SystemSnapshot {
        notifications: vec![transfer, older],
        ..Default::default()
    };
    let active_height = notification_viewport_height(&snapshot, None, Some("COSMIC Files"));
    assert_eq!(
        active_height,
        NOTIFICATION_ITEM_HEIGHT * MAX_VISIBLE_NOTIFICATION_ROWS
    );
    assert_eq!(
        notification_viewport_height(&snapshot, None, None),
        NOTIFICATION_ITEM_HEIGHT + FILE_TRANSFER_PROGRESS_EXTRA_HEIGHT
    );

    let transfer = &mut snapshot.notifications[0];
    transfer.summary = "Move complete".to_string();
    transfer.file_transfer = Some(FileTransfer {
        progress: 100,
        state: FileTransferState::Completed,
    });
    let grouped = snapshot.notifications.iter().collect::<Vec<_>>();
    let preview = notification_group_transfer(&grouped).unwrap();

    assert_eq!(preview.summary, "Move complete");
    assert!(!preview.file_transfer.as_ref().unwrap().is_active());
    assert_eq!(
        notification_viewport_height(&snapshot, None, Some("COSMIC Files")),
        NOTIFICATION_ITEM_HEIGHT * 3
    );
    assert_eq!(
        notification_viewport_height(&snapshot, None, None),
        NOTIFICATION_ITEM_HEIGHT
    );
}

#[test]
fn only_active_file_transfer_rows_reserve_progress_height() {
    let mut item = notification();
    assert_eq!(notification_base_height(&item), NOTIFICATION_ITEM_HEIGHT);

    for state in [
        FileTransferState::Running,
        FileTransferState::Paused,
        FileTransferState::Completed,
        FileTransferState::Cancelled,
        FileTransferState::Failed,
    ] {
        item.file_transfer = Some(FileTransfer {
            progress: 50,
            state,
        });
        let expected = NOTIFICATION_ITEM_HEIGHT
            + if matches!(
                state,
                FileTransferState::Running | FileTransferState::Paused
            ) {
                FILE_TRANSFER_PROGRESS_EXTRA_HEIGHT
            } else {
                0
            };
        let snapshot = SystemSnapshot {
            notifications: vec![item.clone()],
            ..Default::default()
        };
        assert_eq!(notification_base_height(&item), expected);
        assert_eq!(
            notification_viewport_height(&snapshot, None, None),
            expected
        );
    }
}

#[test]
fn transfer_group_progress_height_counts_preview_once_and_animates_children() {
    let mut running = notification();
    running.app_name = "COSMIC Files".to_string();
    running.file_transfer = Some(FileTransfer {
        progress: 50,
        state: FileTransferState::Running,
    });
    let mut paused = running.clone();
    paused.file_transfer.as_mut().unwrap().state = FileTransferState::Paused;
    let mut completed = running.clone();
    completed.file_transfer.as_mut().unwrap().state = FileTransferState::Completed;
    let snapshot = SystemSnapshot {
        notifications: vec![running, paused, completed],
        ..Default::default()
    };
    let grouped = snapshot.notifications.iter().collect::<Vec<_>>();
    let base = NOTIFICATION_ITEM_HEIGHT as f32;
    let transfer_base = base + FILE_TRANSFER_PROGRESS_EXTRA_HEIGHT as f32;
    assert_eq!(
        notification_group_base_height(&grouped) as f32,
        transfer_base
    );
    assert_eq!(
        notification_viewport_height_with_animation(
            &snapshot,
            None,
            Some("COSMIC Files"),
            0.0,
            0.0,
        ),
        transfer_base
    );
    assert_eq!(
        notification_viewport_height_with_animation(
            &snapshot,
            None,
            Some("COSMIC Files"),
            0.0,
            0.5,
        ),
        transfer_base + (2.0 * transfer_base + base) * 0.5
    );
    assert_eq!(
        notification_viewport_height_with_animation(
            &snapshot,
            None,
            Some("COSMIC Files"),
            0.0,
            1.0,
        ),
        (NOTIFICATION_ITEM_HEIGHT * MAX_VISIBLE_NOTIFICATION_ROWS) as f32
    );
}

#[test]
fn same_second_file_transfers_expand_and_dismiss_independently() {
    let mut first = notification();
    first.app_name = "COSMIC Files".to_string();
    first.id = Some(51);
    first.server_owner = Some(":1.82".to_string());
    first.file_transfer = Some(FileTransfer {
        progress: 25,
        state: FileTransferState::Running,
    });
    let mut second = first.clone();
    second.id = Some(52);
    second.body = "Another transfer with a long destination path that wraps across several lines when this notification is expanded.".to_string();
    let selected = second.identity();
    let dismissal = DismissingNotification::new(&second, Instant::now());
    let mut snapshot = SystemSnapshot {
        notifications: vec![first.clone(), second.clone()],
        ..Default::default()
    };

    assert_eq!(first.timestamp, second.timestamp);
    assert!(!selected.matches(&first));
    assert!(selected.matches(&second));
    assert_eq!(
        expanded_notification_extra_height(&snapshot, Some(&selected)),
        notification_extra_height(&second)
    );
    assert!(notification_extra_height(&second) > notification_extra_height(&first));
    assert!(!dismissal.matches(&first));
    assert!(dismissal.matches(&second));

    snapshot
        .notifications
        .retain(|notification| !dismissal.matches(notification));
    assert_eq!(snapshot.notifications, vec![first]);
}

#[test]
fn notification_scroll_eases_to_the_new_offset() {
    let started = Instant::now();
    let mut animation = ScrollAnimation::default();

    animation.transition_to(60.0, started);
    assert_eq!(animation.translation(), 60.0);

    animation.advance(started + NOTIFICATION_EXPANSION_DURATION / 2);
    assert!((animation.translation() - 30.0).abs() < 0.5);

    animation.advance(started + NOTIFICATION_EXPANSION_DURATION);
    assert_eq!(animation.translation(), 0.0);
    assert!(!animation.is_animating());
}

#[test]
fn notification_scroll_can_snap_during_group_layout_changes() {
    let started = Instant::now();
    let mut animation = ScrollAnimation::default();
    animation.transition_to(60.0, started);

    animation.snap_to(24.0);

    assert_eq!(animation.translation(), 0.0);
    assert!(!animation.is_animating());
}

#[test]
fn surface_height_tracks_active_media_content() {
    let mut config = Config::default();
    config.show_storage = false;
    config.show_media = true;
    config.section_order = vec![WidgetSection::Media];
    let mut snapshot = SystemSnapshot::default();

    let empty_height = desired_surface_height(&config, &snapshot);
    snapshot.media.players.push((
        PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string()),
        media(),
    ));

    assert_eq!(empty_height, BASE_SURFACE_HEIGHT + EMPTY_MEDIA_HEIGHT);
    assert_eq!(
        desired_surface_height(&config, &snapshot),
        BASE_SURFACE_HEIGHT + MEDIA_SECTION_HEIGHT
    );

    snapshot.media.players.push((
        PlayerId::Mpris("org.mpris.MediaPlayer2.cider".to_string()),
        media(),
    ));
    assert_eq!(
        desired_surface_height(&config, &snapshot),
        BASE_SURFACE_HEIGHT + MEDIA_SECTION_HEIGHT
    );
}

#[test]
fn ai_usage_reserves_space_for_both_providers_and_honors_toggle() {
    let mut config = Config {
        show_storage: false,
        show_media: false,
        show_codex_usage: true,
        section_order: vec![WidgetSection::Media, WidgetSection::CodexUsage],
        ..Default::default()
    };
    let mut snapshot = SystemSnapshot::default();
    let empty = desired_surface_height(&config, &snapshot);
    assert!(empty > BASE_SURFACE_HEIGHT);

    snapshot
        .codex_usage
        .windows
        .push(crate::monitors::ai_usage::UsageWindow {
            limit_id: "codex".into(),
            label: "Weekly".into(),
            remaining_percent: 17.0,
            resets_at: Some(2_000_000_000),
            updated_at: 1_999_999_000,
            is_stale: false,
        });
    let window = snapshot.codex_usage.windows[0].clone();
    snapshot.claude_usage.windows = vec![window.clone(), window.clone()];
    let one_row = desired_surface_height(&config, &snapshot);
    // The usual Codex weekly + Claude five-hour/weekly limits share one row.
    assert_eq!(one_row, empty);

    snapshot.claude_usage.windows.push(window.clone());
    let two_rows = desired_surface_height(&config, &snapshot);
    assert!(two_rows > one_row);
    snapshot.codex_usage.windows.push(window);
    snapshot.claude_usage.windows.pop();
    assert_eq!(desired_surface_height(&config, &snapshot), two_rows);

    snapshot.codex_usage = Default::default();
    assert_eq!(desired_surface_height(&config, &snapshot), one_row);

    config.show_codex_usage = false;
    assert_eq!(
        desired_surface_height(&config, &snapshot),
        BASE_SURFACE_HEIGHT
    );
}

fn active_media_state() -> MultiPlayerState {
    MultiPlayerState {
        players: vec![(PlayerId::Cider, media())],
        current_index: 0,
    }
}

#[test]
fn disappearing_media_slides_for_the_notification_duration_before_releasing_height() {
    let now = Instant::now();
    let previous = active_media_state();
    let snapshot = SystemSnapshot::default();
    let config = Config {
        show_media: true,
        section_order: vec![WidgetSection::Media],
        ..Default::default()
    };
    let mut dismissal = None;
    reconcile_media_dismissal(&mut dismissal, &previous, &snapshot.media, now);
    assert_eq!(
        dismissal
            .as_ref()
            .unwrap()
            .state
            .current_player()
            .unwrap()
            .0,
        PlayerId::Cider
    );
    assert_eq!(dismissal.as_ref().unwrap().animation.progress, 0.0);

    advance_media_dismissal(&mut dismissal, now + NOTIFICATION_EXPANSION_DURATION / 2);
    let outgoing = dismissal.as_ref().unwrap();
    assert!((outgoing.animation.progress - 0.5).abs() < 0.001);
    assert_eq!(
        desired_surface_height_with_animation(
            &config,
            &snapshot,
            None,
            None,
            0.0,
            0.0,
            dismissal.is_some()
        ),
        BASE_SURFACE_HEIGHT + MEDIA_SECTION_HEIGHT
    );

    advance_media_dismissal(&mut dismissal, now + NOTIFICATION_EXPANSION_DURATION);
    assert!(dismissal.is_none());
    assert_eq!(
        desired_surface_height_with_animation(
            &config,
            &snapshot,
            None,
            None,
            0.0,
            0.0,
            dismissal.is_some()
        ),
        BASE_SURFACE_HEIGHT + EMPTY_MEDIA_HEIGHT
    );
}

#[test]
fn media_exit_does_not_restart_on_empty_samples_and_cancels_if_source_returns() {
    let now = Instant::now();
    let previous = active_media_state();
    let empty = MultiPlayerState::default();
    let mut dismissal = None;
    reconcile_media_dismissal(&mut dismissal, &previous, &empty, now);
    advance_media_dismissal(&mut dismissal, now + NOTIFICATION_EXPANSION_DURATION / 2);
    reconcile_media_dismissal(
        &mut dismissal,
        &empty,
        &empty,
        now + NOTIFICATION_EXPANSION_DURATION / 2,
    );
    assert!((dismissal.as_ref().unwrap().animation.progress - 0.5).abs() < 0.001);
    reconcile_media_dismissal(
        &mut dismissal,
        &empty,
        &previous,
        now + NOTIFICATION_EXPANSION_DURATION / 2,
    );
    assert!(dismissal.is_none());
}

#[test]
fn media_exit_preserves_outgoing_source_while_live_selection_falls_back() {
    let now = Instant::now();
    let mut previous = active_media_state();
    let fallback_id = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".into());
    previous.players.push((fallback_id.clone(), media()));
    let current = MultiPlayerState {
        players: vec![previous.players[1].clone()],
        current_index: 0,
    };
    let mut dismissal = None;
    reconcile_media_dismissal(&mut dismissal, &previous, &current, now);
    let outgoing = &dismissal.as_ref().unwrap().state;
    assert_eq!(outgoing.current_player().unwrap().0, PlayerId::Cider);
    assert_eq!(outgoing.player_count(), 2);
    assert_eq!(current.current_player().unwrap().0, fallback_id);
    advance_media_dismissal(&mut dismissal, now + NOTIFICATION_EXPANSION_DURATION);
    assert!(dismissal.is_none());
}

#[test]
fn pausing_track_changes_and_manual_source_selection_do_not_dismiss_media() {
    let previous = active_media_state();
    let mut current = previous.clone();
    current.players[0].1.status = PlaybackStatus::Paused;
    let mut dismissal = None;
    reconcile_media_dismissal(&mut dismissal, &previous, &current, Instant::now());
    assert!(dismissal.is_none());
    current.players[0].1.title = "Next track".into();
    reconcile_media_dismissal(&mut dismissal, &previous, &current, Instant::now());
    assert!(dismissal.is_none());
    current.players.push((
        PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".into()),
        media(),
    ));
    current.current_index = 1;
    reconcile_media_dismissal(&mut dismissal, &previous, &current, Instant::now());
    assert!(dismissal.is_none());
}

#[test]
fn cleared_track_metadata_starts_media_exit_without_a_bus_disconnect() {
    let previous = active_media_state();
    let mut current = previous.clone();
    current.players[0].1.title.clear();
    current.players[0].1.status = PlaybackStatus::Stopped;
    let mut dismissal = None;
    reconcile_media_dismissal(&mut dismissal, &previous, &current, Instant::now());
    assert!(dismissal.is_some());
}

#[test]
fn media_reconciliation_preserves_immediate_user_choices() {
    let firefox = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
    let cider = PlayerId::Cider;
    let mut state = crate::monitors::media::MultiPlayerState {
        players: vec![(firefox, media()), (cider.clone(), media())],
        current_index: 0,
    };
    let mut pending = Some(PendingPlayback {
        player_id: cider,
        status: PlaybackStatus::Paused,
        expires_at: Instant::now() + Duration::from_secs(1),
    });

    state.current_index = 1;
    reconcile_media_state(&mut state, &mut pending, Instant::now());

    assert_eq!(state.current_index, 1);
    assert_eq!(
        state.current_player().unwrap().1.status,
        PlaybackStatus::Paused
    );
}

#[test]
fn media_reconciliation_releases_expired_playback_state() {
    let now = Instant::now();
    let mut state = crate::monitors::media::MultiPlayerState {
        players: vec![(PlayerId::Cider, media())],
        current_index: 0,
    };
    let mut pending = Some(PendingPlayback {
        player_id: PlayerId::Cider,
        status: PlaybackStatus::Paused,
        expires_at: now,
    });

    reconcile_media_state(&mut state, &mut pending, now);

    assert!(pending.is_none());
    assert_eq!(
        state.current_player().unwrap().1.status,
        PlaybackStatus::Playing
    );
}

fn disk() -> DiskInfo {
    DiskInfo {
        name: "Disk".to_string(),
        mount_point: "/".to_string(),
        used_percentage: 50.0,
        total_space: 1_000,
        available_space: 500,
        is_loading: false,
    }
}

fn device() -> BatteryDevice {
    BatteryDevice {
        name: "Device".to_string(),
        level: Some(75),
        status: Some("discharging".to_string()),
        kind: Some("mouse".to_string()),
        codename: None,
        is_loading: false,
        is_connected: true,
    }
}

fn weather() -> WeatherData {
    WeatherData {
        temperature: 3.2,
        feels_like: 1.0,
        temp_min: 3.2,
        temp_max: 3.2,
        humidity: 80,
        description: "Overcast".to_string(),
        icon: "04d".to_string(),
        location: "Milwaukee".to_string(),
    }
}

fn notification() -> Notification {
    Notification {
        id: None,
        server_owner: None,
        sender_owner: None,
        app_name: "System".to_string(),
        summary: "Package manager updated".to_string(),
        body: "System is up to date.".to_string(),
        timestamp: 1_000,
        open_folder: None,
        activation_action: None,
        file_transfer: None,
    }
}

fn media() -> MediaInfo {
    MediaInfo {
        player_name: "Firefox".to_string(),
        title: "Making Minecraft fun again".to_string(),
        artist: "Call Me Kevin".to_string(),
        status: PlaybackStatus::Playing,
        position: 74_000,
        duration: 1_769_000,
        can_play: true,
        can_pause: true,
        can_go_next: true,
        can_go_previous: true,
        can_seek: true,
        ..Default::default()
    }
}
