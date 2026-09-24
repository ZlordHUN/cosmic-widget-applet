// SPDX-License-Identifier: MPL-2.0

use super::{
    NOTIFICATIONS_INTERFACE, NOTIFICATIONS_PATH, NOTIFICATIONS_SERVICE, Notification,
    NotificationBusEvent, NotificationMessageParser, apply_local_dismissal_suppression,
    is_unwanted_battery_discharge, load_cached_notifications, open_notification_monitor_connection,
    persist_cached_notifications, preserve_notification_targets, upsert_notification,
};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::Duration;
use zbus::Message;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{OwnedValue, Value};

fn cache_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cosmic-widget-notification-test-{}-{name}.json",
        std::process::id()
    ))
}

fn notification(summary: &str, timestamp: u64) -> Notification {
    Notification {
        id: None,
        server_owner: None,
        sender_owner: None,
        app_name: "Test".to_string(),
        summary: summary.to_string(),
        body: "Body".to_string(),
        timestamp,
        open_folder: None,
        activation_action: None,
        file_transfer: None,
    }
}

fn transfer_notification(state: super::FileTransferState, progress: u8) -> Notification {
    Notification {
        id: Some(15),
        server_owner: Some(":1.82".to_string()),
        app_name: "COSMIC Files".to_string(),
        file_transfer: Some(super::FileTransfer { state, progress }),
        ..notification("Copying files", 42)
    }
}

#[test]
fn filters_solaar_battery_discharge_notifications_only() {
    let mut discharge = notification("MX Mechanical Mini", 1);
    discharge.app_name = "solaar".into();
    discharge.body = "Battery: empty (discharging)".into();
    assert!(is_unwanted_battery_discharge(&discharge));

    let mut charging = discharge.clone();
    charging.body = "Battery: 80% (charging)".into();
    assert!(!is_unwanted_battery_discharge(&charging));

    let mut other_app = discharge;
    other_app.app_name = "Power Manager".into();
    assert!(!is_unwanted_battery_discharge(&other_app));
}

fn transfer_exchange(state: &str, progress: i32, replaces_id: u32) -> (Message, Message) {
    let mut hints = super::file_transfers::tests::hints("copy", state, progress);
    hints.insert(
        "transient".to_string(),
        matches!(state, "running" | "paused").into(),
    );
    let call = Message::method(NOTIFICATIONS_PATH, "Notify")
        .unwrap()
        .sender(":1.9001")
        .unwrap()
        .destination(NOTIFICATIONS_SERVICE)
        .unwrap()
        .interface(NOTIFICATIONS_INTERFACE)
        .unwrap()
        .build(&(
            "COSMIC Files",
            replaces_id,
            "com.system76.CosmicFiles",
            state,
            "Transfer details",
            Vec::<String>::new(),
            hints,
            0_i32,
        ))
        .unwrap();
    let reply = Message::method_reply(&call)
        .unwrap()
        .sender(":1.82")
        .unwrap()
        .build(&15_u32)
        .unwrap();
    (call, reply)
}

#[test]
fn file_transfer_progress_and_completion_replace_one_row() {
    let mut parser = NotificationMessageParser::default();
    let mut notifications = Vec::new();
    for (state, progress, timestamp) in [
        ("running", 0, 42),
        ("running", 50, 43),
        ("completed", 100, 44),
    ] {
        let (call, reply) = transfer_exchange(
            state,
            progress,
            if notifications.is_empty() { 0 } else { 15 },
        );
        assert!(parser.push_message(&call, timestamp).unwrap().is_none());
        let Some(NotificationBusEvent::Upsert(item)) =
            parser.push_message(&reply, timestamp).unwrap()
        else {
            panic!("expected transfer update");
        };
        upsert_notification(&mut notifications, item, 20);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].id, Some(15));
        assert_eq!(notifications[0].timestamp, 42);
        assert_eq!(
            notifications[0].file_transfer.as_ref().unwrap().progress,
            progress as u8
        );
    }
    assert_eq!(notifications[0].summary, "completed");
    assert!(!notifications[0].file_transfer.as_ref().unwrap().is_active());
}

#[test]
fn transient_transfer_survives_history_and_completes_in_place() {
    let running = transfer_notification(super::FileTransferState::Running, 50);
    let mut history = super::CosmicNotificationHistory {
        server_owner: ":1.82".to_string(),
        notifications: vec![notification("Other notification", 40)],
    };
    super::reconcile_transfer_history(std::slice::from_ref(&running), &mut history);
    assert_eq!(history.notifications[0], running);
    let completed = Notification {
        summary: "Copy complete".to_string(),
        timestamp: 55,
        file_transfer: None,
        ..running.clone()
    };
    history.notifications = vec![completed];
    super::reconcile_transfer_history(&[running], &mut history);
    assert_eq!(history.notifications.len(), 1);
    assert_eq!(history.notifications[0].summary, "Copy complete");
    assert_eq!(history.notifications[0].timestamp, 42);
    assert!(history.notifications[0].file_transfer.is_none());
}

#[test]
fn history_does_not_keep_unrelated_transients_or_old_daemon_transfers() {
    let running = transfer_notification(super::FileTransferState::Running, 50);
    let mut history = super::CosmicNotificationHistory {
        server_owner: ":1.99".to_string(),
        notifications: Vec::new(),
    };
    super::reconcile_transfer_history(
        &[running, notification("Unrelated transient", 42)],
        &mut history,
    );
    assert!(history.notifications.is_empty());
}

#[test]
fn transfer_dismissal_survives_transient_history_gap() {
    let key = (15, ":1.82".to_string());
    let mut transfers = HashSet::from([key.clone()]);
    let mut dismissed = HashSet::from([key.clone()]);
    let mut history = super::CosmicNotificationHistory {
        server_owner: ":1.82".to_string(),
        notifications: Vec::new(),
    };
    super::reconcile_transfer_dismissals(&history, &mut dismissed, &mut transfers);
    apply_local_dismissal_suppression(&mut history.notifications, &mut dismissed);
    assert!(
        transfers.contains(&key),
        "later progress must remain suppressed"
    );
    history.notifications.push(transfer_notification(
        super::FileTransferState::Completed,
        100,
    ));
    super::reconcile_transfer_dismissals(&history, &mut dismissed, &mut transfers);
    apply_local_dismissal_suppression(&mut history.notifications, &mut dismissed);
    assert!(transfers.is_empty());
    assert!(
        history.notifications.is_empty(),
        "completion must remain dismissed"
    );
    assert!(dismissed.contains(&key));
}

#[test]
fn cache_keeps_completed_transfers_but_never_restores_stale_progress() {
    let path = cache_path("transfers");
    let running = transfer_notification(super::FileTransferState::Running, 50);
    let completed = transfer_notification(super::FileTransferState::Completed, 100);
    persist_cached_notifications(&path, "current-boot", &[running, completed.clone()]).unwrap();
    assert_eq!(
        load_cached_notifications(&path, "current-boot", 20).unwrap(),
        vec![completed]
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn sender_disconnect_removes_only_its_unfinished_transfers() {
    let mut running = transfer_notification(super::FileTransferState::Running, 50);
    running.sender_owner = Some(":1.9001".to_string());
    let completed = Notification {
        id: Some(16),
        file_transfer: Some(super::FileTransfer {
            state: super::FileTransferState::Completed,
            progress: 100,
        }),
        ..running.clone()
    };
    let another_sender = Notification {
        id: Some(17),
        sender_owner: Some(":1.9002".to_string()),
        ..running.clone()
    };
    let mut notifications = vec![running, completed.clone(), another_sender.clone()];
    let signal = Message::signal(
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameOwnerChanged",
    )
    .unwrap()
    .sender("org.freedesktop.DBus")
    .unwrap()
    .build(&(":1.9001", ":1.9001", ""))
    .unwrap();
    let mut parser = NotificationMessageParser::default();
    let Some(NotificationBusEvent::SenderGone(sender)) = parser.push_message(&signal, 43).unwrap()
    else {
        panic!("expected sender disconnect");
    };
    super::remove_abandoned_transfers(&mut notifications, &sender);
    assert_eq!(notifications, vec![completed, another_sender]);
}

fn notify_exchange() -> (Message, Message) {
    let notify = Message::method(NOTIFICATIONS_PATH, "Notify")
        .unwrap()
        .sender(":1.8323")
        .unwrap()
        .destination(NOTIFICATIONS_SERVICE)
        .unwrap()
        .interface(NOTIFICATIONS_INTERFACE)
        .unwrap()
        .build(&(
            "COSMIC synchronization probe".to_string(),
            0_u32,
            String::new(),
            "ID tracking probe".to_string(),
            "Capturing the assigned ID.\nWithout text parsing.".to_string(),
            Vec::<String>::new(),
            HashMap::<String, OwnedValue>::new(),
            -1_i32,
        ))
        .unwrap();
    let reply = Message::method_reply(&notify)
        .unwrap()
        .sender(":1.82")
        .unwrap()
        .build(&15_u32)
        .unwrap();
    (notify, reply)
}

fn discord_notify_exchange() -> (Message, Message) {
    let mut hints = HashMap::new();
    hints.insert(
        "desktop-entry".to_string(),
        OwnedValue::try_from(Value::from("com.discordapp.Discord")).unwrap(),
    );
    let notify = Message::method(NOTIFICATIONS_PATH, "Notify")
        .unwrap()
        .sender(":1.9000")
        .unwrap()
        .destination(NOTIFICATIONS_SERVICE)
        .unwrap()
        .interface(NOTIFICATIONS_INTERFACE)
        .unwrap()
        .build(&(
            "Friend (Direct Message)".to_string(),
            0_u32,
            String::new(),
            "Friend".to_string(),
            "New message".to_string(),
            vec!["default".to_string(), "Open".to_string()],
            hints,
            -1_i32,
        ))
        .unwrap();
    let reply = Message::method_reply(&notify)
        .unwrap()
        .sender(":1.82")
        .unwrap()
        .build(&16_u32)
        .unwrap();
    (notify, reply)
}

fn closed_signal() -> Message {
    Message::signal(
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
        "NotificationClosed",
    )
    .unwrap()
    .sender(":1.82")
    .unwrap()
    .build(&(15_u32, 2_u32))
    .unwrap()
}

#[test]
fn restores_notifications_from_the_current_boot() {
    let path = cache_path("restore");
    let expected = vec![notification("Newest", 20), notification("Older", 10)];
    persist_cached_notifications(&path, "boot-id", &expected).unwrap();

    assert_eq!(
        load_cached_notifications(&path, "boot-id", 5).unwrap(),
        expected
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn migrates_the_old_volatile_session_key_and_honors_the_limit() {
    let path = cache_path("migration");
    let cached = vec![notification("One", 3), notification("Two", 2)];
    persist_cached_notifications(&path, "boot-id:unknown::1.82", &cached).unwrap();
    assert_eq!(
        load_cached_notifications(&path, "boot-id", 1).unwrap(),
        vec![cached[0].clone()]
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn ignores_notifications_from_an_old_boot() {
    let path = cache_path("old-boot");
    persist_cached_notifications(&path, "old-boot:unknown::1.12", &[notification("Old", 1)])
        .unwrap();

    assert!(
        load_cached_notifications(&path, "new-boot", 5)
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn correlates_notify_call_with_cosmic_assigned_id() {
    let mut parser = NotificationMessageParser::default();
    let (notify, reply) = notify_exchange();
    assert!(parser.push_message(&notify, 42).unwrap().is_none());
    let Some(NotificationBusEvent::Upsert(notification)) = parser.push_message(&reply, 43).unwrap()
    else {
        panic!("expected a notification event");
    };
    assert_eq!(notification.id, Some(15));
    assert_eq!(notification.server_owner.as_deref(), Some(":1.82"));
    assert_eq!(notification.app_name, "COSMIC synchronization probe");
    assert_eq!(notification.summary, "ID tracking probe");
    assert_eq!(
        notification.body,
        "Capturing the assigned ID.\nWithout text parsing."
    );
    assert_eq!(notification.timestamp, 42);
    assert!(notification.open_folder.is_none());
    assert!(notification.activation_action.is_none());
}

#[test]
fn captures_discord_default_action_from_desktop_entry() {
    let mut parser = NotificationMessageParser::default();
    let (notify, reply) = discord_notify_exchange();
    assert!(parser.push_message(&notify, 42).unwrap().is_none());
    let Some(NotificationBusEvent::Upsert(notification)) = parser.push_message(&reply, 43).unwrap()
    else {
        panic!("expected a notification event");
    };

    assert_eq!(notification.id, Some(16));
    assert_eq!(notification.app_name, "Friend (Direct Message)");
    assert_eq!(notification.activation_action.as_deref(), Some("default"));
}

#[test]
fn cosmic_history_refresh_preserves_resolved_open_folders() {
    let folder = std::path::PathBuf::from("/mnt/Downloads");
    let mut existing = notification("Download completed", 42);
    existing.id = Some(15);
    existing.server_owner = Some(":1.82".to_string());
    existing.open_folder = Some(folder.clone());
    let mut refreshed = existing.clone();
    refreshed.open_folder = None;

    preserve_notification_targets(&[existing], std::slice::from_mut(&mut refreshed));

    assert_eq!(refreshed.open_folder, Some(folder));
}

#[test]
fn cosmic_history_refresh_preserves_notification_activation() {
    let mut existing = notification("Discord message", 42);
    existing.id = Some(15);
    existing.server_owner = Some(":1.82".to_string());
    existing.activation_action = Some("default".to_string());
    let mut refreshed = existing.clone();
    refreshed.activation_action = None;

    preserve_notification_targets(&[existing], std::slice::from_mut(&mut refreshed));

    assert_eq!(refreshed.activation_action.as_deref(), Some("default"));
}

#[test]
fn parses_cosmic_notification_closed_signal() {
    let mut parser = NotificationMessageParser::default();
    let event = parser.push_message(&closed_signal(), 42).unwrap();

    assert!(matches!(
        event,
        Some(NotificationBusEvent::Closed { id: 15, server_owner, .. }) if server_owner == ":1.82"
    ));
}

#[test]
fn discards_pending_notification_after_a_dbus_error() {
    let mut parser = NotificationMessageParser::default();
    let (notify, reply) = notify_exchange();
    let error = Message::method_error(&notify, "org.freedesktop.DBus.Error.Failed")
        .unwrap()
        .sender(":1.82")
        .unwrap()
        .build(&"Notification rejected")
        .unwrap();

    assert!(parser.push_message(&notify, 42).unwrap().is_none());
    assert_eq!(parser.pending.len(), 1);
    assert!(parser.push_message(&error, 43).unwrap().is_none());
    assert!(parser.pending.is_empty());
    assert!(parser.push_message(&reply, 44).unwrap().is_none());
}

#[test]
#[ignore = "creates and closes a notification through the live COSMIC daemon"]
fn captures_a_live_notification_with_native_zbus_monitoring() {
    let monitor = open_notification_monitor_connection().unwrap();
    let messages = MessageIterator::from(monitor);
    let (event_sender, event_receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut parser = NotificationMessageParser::default();
        for message in messages {
            let Ok(message) = message else {
                return;
            };
            if let Ok(Some(NotificationBusEvent::Upsert(notification))) =
                parser.push_message(&message, 42)
            {
                let _ = event_sender.send(notification);
                return;
            }
        }
    });

    let connection = Connection::session().unwrap();
    let proxy = Proxy::new(
        &connection,
        NOTIFICATIONS_SERVICE,
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
    )
    .unwrap();
    let id: u32 = proxy
        .call(
            "Notify",
            &(
                "COSMIC Widget Test",
                0_u32,
                "",
                "Native zbus notification monitor test",
                "This notification should close automatically.",
                Vec::<String>::new(),
                HashMap::<String, OwnedValue>::new(),
                5_000_i32,
            ),
        )
        .unwrap();

    let captured = event_receiver.recv_timeout(Duration::from_secs(3));
    let _: () = proxy.call("CloseNotification", &id).unwrap();
    let captured = captured.expect("native monitor did not capture the notification");
    assert_eq!(captured.id, Some(id));
    assert_eq!(captured.app_name, "COSMIC Widget Test");
    assert_eq!(captured.summary, "Native zbus notification monitor test");
}

#[test]
fn replacement_keeps_the_original_display_timestamp() {
    let mut existing = notification("Old content", 10);
    existing.id = Some(15);
    existing.server_owner = Some(":1.82".to_string());
    let mut notifications = vec![existing];
    let mut replacement = notification("Updated content", 20);
    replacement.id = Some(15);
    replacement.server_owner = Some(":1.82".to_string());

    upsert_notification(&mut notifications, replacement, 5);

    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].summary, "Updated content");
    assert_eq!(notifications[0].timestamp, 10);
}

#[test]
#[ignore = "creates, updates, and closes a transfer notification through the live COSMIC daemon"]
fn live_cosmic_transfer_replaces_one_row_through_completion() {
    struct CloseOnDrop {
        connection: Connection,
        owner: String,
        id: u32,
    }
    impl Drop for CloseOnDrop {
        fn drop(&mut self) {
            if let Ok(proxy) = Proxy::new(
                &self.connection,
                self.owner.as_str(),
                NOTIFICATIONS_PATH,
                NOTIFICATIONS_INTERFACE,
            ) {
                let _: zbus::Result<()> = proxy.call("CloseNotification", &self.id);
            }
        }
    }

    let marker = format!("COSMIC Widget transfer test {}", std::process::id());
    let monitor = open_notification_monitor_connection().unwrap();
    let (event_sender, event_receiver) = mpsc::channel();
    let expected_marker = marker.clone();
    std::thread::spawn(move || {
        let mut parser = NotificationMessageParser::default();
        let mut captured = 0;
        for message in MessageIterator::from(monitor) {
            let Ok(message) = message else { return };
            if let Ok(Some(NotificationBusEvent::Upsert(notification))) =
                parser.push_message(&message, 42 + captured)
                && notification.summary == expected_marker
            {
                if event_sender.send(notification).is_err() {
                    return;
                }
                captured += 1;
                if captured == 3 {
                    return;
                }
            }
        }
    });

    let connection = Connection::session().unwrap();
    let initial_history = super::load_cosmic_notification_history(&connection)
        .unwrap()
        .expect("COSMIC daemon must expose notification history");
    let mut cleanup = CloseOnDrop {
        connection: connection.clone(),
        owner: initial_history.server_owner.clone(),
        id: 0,
    };
    let proxy = Proxy::new(
        &connection,
        initial_history.server_owner.as_str(),
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
    )
    .unwrap();
    let mut rows = Vec::new();
    for (state, progress) in [("running", 0), ("running", 50), ("completed", 100)] {
        let active = state == "running";
        let mut hints = super::file_transfers::tests::hints("copy", state, progress);
        hints.insert("transient".to_string(), active.into());
        hints.insert("suppress-sound".to_string(), true.into());
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    "COSMIC Files",
                    cleanup.id,
                    "com.system76.CosmicFiles",
                    marker.as_str(),
                    "Integration test only; no files are copied.",
                    Vec::<String>::new(),
                    hints,
                    if active { 0_i32 } else { 5_000_i32 },
                ),
            )
            .unwrap();
        let previous_id = std::mem::replace(&mut cleanup.id, id);
        if previous_id != 0 {
            assert_eq!(id, previous_id, "progress must retain the notification ID");
        }
        let captured = event_receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(captured.id, Some(id));
        assert_eq!(
            captured.file_transfer.as_ref().unwrap().progress,
            progress as u8
        );
        assert_eq!(captured.file_transfer.as_ref().unwrap().is_active(), active);
        upsert_notification(&mut rows, captured, 200);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].timestamp, 42);

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut history = loop {
            let history = super::load_cosmic_notification_history(&connection)
                .unwrap()
                .unwrap();
            let saved = history.notifications.iter().any(|row| row.id == Some(id));
            if saved != active || std::time::Instant::now() >= deadline {
                assert_eq!(saved, !active, "only terminal updates belong in history");
                break history;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        super::reconcile_transfer_history(&rows, &mut history);
        let matching = history
            .notifications
            .iter()
            .filter(|row| row.id == Some(id))
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].timestamp, 42);
        assert_eq!(matching[0].file_transfer, rows[0].file_transfer);
    }

    let abandoned_sender = Connection::session().unwrap();
    let mut abandoned = transfer_notification(super::FileTransferState::Running, 50);
    abandoned.sender_owner = abandoned_sender.unique_name().map(ToString::to_string);
    rows.push(abandoned);
    super::prune_disconnected_transfer_senders(&connection, &mut rows);
    assert_eq!(rows.len(), 2, "a connected sender must retain its progress");
    drop(abandoned_sender);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while rows.len() > 1 && std::time::Instant::now() < deadline {
        super::prune_disconnected_transfer_senders(&connection, &mut rows);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        rows.len(),
        1,
        "history refresh must recover a missed disconnect"
    );
    assert_eq!(
        rows[0].file_transfer.as_ref().unwrap().state,
        super::FileTransferState::Completed
    );
}

#[test]
fn suppresses_local_dismissals_until_cosmic_removes_them() {
    let mut dismissed = HashSet::from([(15, ":1.82".to_string()), (99, ":1.12".to_string())]);
    let mut first = notification("Dismissed locally", 20);
    first.id = Some(15);
    first.server_owner = Some(":1.82".to_string());
    let mut second = notification("Still active", 10);
    second.id = Some(16);
    second.server_owner = Some(":1.82".to_string());
    let mut history = vec![first, second.clone()];

    apply_local_dismissal_suppression(&mut history, &mut dismissed);
    assert_eq!(history, vec![second.clone()]);
    assert_eq!(dismissed, HashSet::from([(15, ":1.82".to_string())]));

    let mut next_history = vec![second];
    apply_local_dismissal_suppression(&mut next_history, &mut dismissed);
    assert!(dismissed.is_empty());
}
