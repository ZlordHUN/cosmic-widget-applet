// SPDX-License-Identifier: MPL-2.0

//! # Notification Monitoring Module
//!
//! This module captures desktop notifications via D-Bus and displays them
//! in the widget. Uses a native zbus monitor connection to observe the
//! `org.freedesktop.Notifications` interface.
//!
//! ## D-Bus Interface
//!
//! Monitors the standard FreeDesktop Notifications specification:
//! ```text
//! Interface: org.freedesktop.Notifications
//! Method: Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)
//! ```
//!
//! ## Data Flow
//!
//! ```text
//! ┌──────────────┐    ┌─────────────┐    ┌───────────────┐
//! │ Desktop App  │───►│ D-Bus       │───►│ zbus monitor  │
//! │ (notify-send)│    │ Notify call │    │ monitor       │
//! └──────────────┘    └─────────────┘    └───────┬───────┘
//!                                                 │
//!                     ┌───────────────┐          │ messages
//!                     │ Main Thread   │◄─────────┘
//!                     │ (reads list)  │    ┌───────────────┐
//!                     └───────────────┘    │ Background    │
//!                                          │ Thread        │
//!                                          │ (parses)      │
//!                                          └───────────────┘
//! ```
//!
//! ## Structured Message Decoding
//!
//! Notify calls, their method returns, and close signals are decoded directly
//! from their D-Bus signatures. The call serial and reply serial associate the
//! content with the notification ID assigned by the active daemon.
//!
//! This avoids depending on a command's human-readable output format and
//! preserves escaped, quoted, and multiline notification content exactly.
//!
//! ## Notification Management
//!
//! - New notifications are inserted at the front (newest first)
//! - List is capped at `max_notifications` to prevent unbounded growth
//! - Provides methods to clear all, clear by app, or remove specific notifications

#[path = "notifications/downloads.rs"]
mod downloads;
#[path = "notifications/file_transfers.rs"]
mod file_transfers;

pub use file_transfers::FileTransfer;
// The legacy renderer does not display transfer states, but shares this module.
#[allow(unused_imports)]
pub use file_transfers::FileTransferState;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CACHE_DIRECTORY: &str = "cosmic-widget-applet";
const CACHE_FILENAME: &str = "notifications.json";
const COSMIC_NOTIFICATION_HISTORY_LIMIT: usize = 200;
const NOTIFICATIONS_SERVICE: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
const COSMIC_HISTORY_RECONCILE_INTERVAL: Duration = Duration::from_secs(10);
const COSMIC_HISTORY_EVENT_DEBOUNCE: Duration = Duration::from_secs(1);
const NOTIFICATION_MONITOR_RECONNECT_DELAY: Duration = Duration::from_secs(2);

// ============================================================================
// Notification Struct
// ============================================================================

/// A captured desktop notification.
///
/// Contains the essential fields from a D-Bus Notify method call,
/// plus a timestamp for ordering and identification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// ID assigned by the active notification daemon.
    #[serde(default)]
    pub id: Option<u32>,
    /// Unique D-Bus owner that assigned `id`; IDs must not cross daemon restarts.
    #[serde(default)]
    pub server_owner: Option<String>,
    /// Sender connection used to retire abandoned live transfers.
    #[serde(default)]
    pub sender_owner: Option<String>,
    /// Application that sent the notification (e.g., "Firefox", "System")
    pub app_name: String,
    /// Notification title/headline
    pub summary: String,
    /// Notification body text (may be empty)
    pub body: String,
    /// Unix timestamp when notification was captured (seconds since epoch)
    pub timestamp: u64,
    /// Verified local file or directory that can be revealed for this notification.
    #[serde(default)]
    pub open_folder: Option<PathBuf>,
    /// Validated notification-server action that opens the originating item.
    #[serde(default)]
    pub activation_action: Option<String>,
    /// Explicit COSMIC Files copy/move progress; absent for other notifications.
    #[serde(default)]
    pub file_transfer: Option<FileTransfer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationIdentity {
    Remote { id: u32, server_owner: String },
    Local { app_name: String, timestamp: u64 },
}

impl Notification {
    pub fn identity(&self) -> NotificationIdentity {
        match remote_notification_id(self) {
            Some((id, server_owner)) => NotificationIdentity::Remote { id, server_owner },
            None => NotificationIdentity::Local {
                app_name: self.app_name.clone(),
                timestamp: self.timestamp,
            },
        }
    }
}

impl NotificationIdentity {
    pub fn matches(&self, notification: &Notification) -> bool {
        *self == notification.identity()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct NotificationCache {
    session_key: String,
    notifications: Vec<Notification>,
}

enum NotificationDbusCommand {
    RefreshHistory,
    Close(Vec<(u32, String)>),
    Activate {
        id: u32,
        server_owner: String,
        action: String,
    },
}

// ============================================================================
// Notification Monitor Struct
// ============================================================================

/// Monitors D-Bus for desktop notifications.
///
/// Spawns a background thread with a native zbus monitor connection to capture
/// incoming notifications. The notification list is shared via Arc<Mutex> for
/// thread-safe access from the main render thread.
///
/// # Threading Model
///
/// - Monitor thread: Decodes monitored D-Bus messages and updates the list
/// - Command thread: Reuses one connection for dismissals and history reconciliation
/// - Main thread: Reads notification list for rendering
/// - Shared state: `notifications` Vec protected by Mutex
///
/// # Resource Usage
///
/// - Spawns two persistent background threads
/// - Maintains one monitor connection and one ordinary session-bus connection
#[derive(Clone)]
pub struct NotificationMonitor {
    /// Shared notification list, newest first
    notifications: Arc<Mutex<Vec<Notification>>>,
    cache_path: Arc<PathBuf>,
    session_key: Arc<String>,
    locally_dismissed: Arc<Mutex<HashSet<(u32, String)>>>,
    /// Transient transfers are absent from history until they finish, so their
    /// dismissals must survive history refreshes and subsequent progress ticks.
    dismissed_transfers: Arc<Mutex<HashSet<(u32, String)>>>,
    dbus_commands: Sender<NotificationDbusCommand>,
}

impl NotificationMonitor {
    /// Create a new notification monitor with background D-Bus listener.
    ///
    /// # Arguments
    ///
    /// * `max_notifications` - Maximum notifications to keep (oldest are dropped)
    ///
    /// # Background Thread
    ///
    /// Immediately spawns a background thread that:
    /// 1. Opens a native zbus monitoring connection
    /// 2. Decodes Notify method calls and their replies
    /// 3. Extracts app_name, summary, and body
    /// 4. Updates the shared notification list
    pub fn new(max_notifications: usize) -> Self {
        let cache_path = Arc::new(notification_cache_path());
        let session_key = Arc::new(notification_session_key());
        let cached = load_cached_notifications(&cache_path, &session_key, max_notifications)
            .unwrap_or_else(|error| {
                log::warn!("Failed to restore cached notifications: {error}");
                Vec::new()
            });
        let cosmic_history_connection = match zbus::blocking::Connection::session() {
            Ok(connection) => Some(connection),
            Err(error) => {
                log::warn!("Failed to connect to the session bus for COSMIC history: {error}");
                None
            }
        };
        let (mut cached, retention_limit) = match cosmic_history_connection.as_ref() {
            Some(connection) => match load_cosmic_notification_history(connection) {
                Ok(Some(mut history)) => {
                    log::info!(
                        "Loaded {} notifications from COSMIC",
                        history.notifications.len()
                    );
                    preserve_notification_targets(&cached, &mut history.notifications);
                    (history.notifications, COSMIC_NOTIFICATION_HISTORY_LIMIT)
                }
                Ok(None) => (cached, max_notifications),
                Err(error) => {
                    log::warn!("Failed to retrieve COSMIC notification history: {error}");
                    (cached, max_notifications)
                }
            },
            None => (cached, max_notifications),
        };
        downloads::resolve_open_folders(&mut cached);
        if let Err(error) = persist_cached_notifications(&cache_path, &session_key, &cached) {
            log::warn!("Failed to initialize notification cache: {error}");
        }
        let notifications = Arc::new(Mutex::new(cached));
        let locally_dismissed = Arc::new(Mutex::new(HashSet::new()));
        let dismissed_transfers = Arc::new(Mutex::new(HashSet::new()));
        let (dbus_commands, dbus_command_receiver) = std::sync::mpsc::channel();

        // Spawn background thread to monitor D-Bus
        // This runs for the lifetime of the application
        let notifications_clone = Arc::clone(&notifications);
        let cache_path_clone = Arc::clone(&cache_path);
        let session_key_clone = Arc::clone(&session_key);
        let max_count = retention_limit;
        let dbus_commands_clone = dbus_commands.clone();
        let dismissed_transfers_clone = Arc::clone(&dismissed_transfers);
        let locally_dismissed_monitor = Arc::clone(&locally_dismissed);

        std::thread::spawn(move || {
            Self::monitor_notifications(
                notifications_clone,
                max_count,
                &cache_path_clone,
                &session_key_clone,
                dbus_commands_clone,
                dismissed_transfers_clone,
                locally_dismissed_monitor,
            );
        });

        let notifications_clone = Arc::clone(&notifications);
        let cache_path_clone = Arc::clone(&cache_path);
        let session_key_clone = Arc::clone(&session_key);
        let locally_dismissed_clone = Arc::clone(&locally_dismissed);
        let dismissed_transfers_clone = Arc::clone(&dismissed_transfers);
        std::thread::spawn(move || {
            Self::run_dbus_worker(
                notifications_clone,
                locally_dismissed_clone,
                &cache_path_clone,
                &session_key_clone,
                cosmic_history_connection,
                dbus_command_receiver,
                dismissed_transfers_clone,
            );
        });

        Self {
            notifications,
            cache_path,
            session_key,
            locally_dismissed,
            dismissed_transfers,
            dbus_commands,
        }
    }

    fn run_dbus_worker(
        notifications: Arc<Mutex<Vec<Notification>>>,
        locally_dismissed: Arc<Mutex<HashSet<(u32, String)>>>,
        cache_path: &Path,
        session_key: &str,
        mut connection: Option<zbus::blocking::Connection>,
        commands: Receiver<NotificationDbusCommand>,
        dismissed_transfers: Arc<Mutex<HashSet<(u32, String)>>>,
    ) {
        let mut refresh_due = None;
        let mut next_reconciliation = Instant::now() + COSMIC_HISTORY_RECONCILE_INTERVAL;
        let mut history_supported = true;

        loop {
            let deadline = refresh_due.map_or(next_reconciliation, |due: Instant| {
                due.min(next_reconciliation)
            });
            let timeout = deadline.saturating_duration_since(Instant::now());
            match commands.recv_timeout(timeout) {
                Ok(command) => {
                    Self::handle_dbus_command(command, &mut connection, &mut refresh_due)
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(command) = commands.try_recv() {
                Self::handle_dbus_command(command, &mut connection, &mut refresh_due);
            }

            let now = Instant::now();
            let event_refresh_due = refresh_due.is_some_and(|due| now >= due);
            if !event_refresh_due && now < next_reconciliation {
                continue;
            }
            refresh_due = None;
            next_reconciliation = now + COSMIC_HISTORY_RECONCILE_INTERVAL;

            if !history_supported {
                continue;
            }

            if !ensure_notification_connection(&mut connection) {
                continue;
            }
            let history_connection = connection.as_ref().expect("connection was ensured");
            let existing = notifications.lock().unwrap().clone();
            let mut history = match load_cosmic_notification_history(history_connection) {
                Ok(Some(history)) => history,
                Ok(None) => {
                    history_supported = false;
                    continue;
                }
                Err(error) => {
                    log::debug!("Failed to synchronize COSMIC notification history: {error}");
                    if matches!(error, zbus::Error::InputOutput(_)) {
                        connection = None;
                    }
                    continue;
                }
            };

            reconcile_transfer_history(&existing, &mut history);
            prune_disconnected_transfer_senders(history_connection, &mut history.notifications);
            downloads::resolve_open_folders(&mut history.notifications);

            let mut current = notifications.lock().unwrap();
            // A Notify reply or dismissal may arrive while history is fetched.
            // Retry instead of letting an older snapshot undo that live update.
            if *current != existing {
                refresh_due = Some(Instant::now() + COSMIC_HISTORY_EVENT_DEBOUNCE);
                continue;
            }
            let mut dismissed = locally_dismissed.lock().unwrap();
            reconcile_transfer_dismissals(
                &history,
                &mut dismissed,
                &mut dismissed_transfers.lock().unwrap(),
            );
            apply_local_dismissal_suppression(&mut history.notifications, &mut dismissed);
            if *current == history.notifications {
                continue;
            }
            *current = history.notifications;
            if let Err(error) = persist_cached_notifications(cache_path, session_key, &current) {
                log::warn!("Failed to persist synchronized notifications: {error}");
            }
        }
    }

    /// Main D-Bus monitoring supervisor (runs in a background thread).
    fn monitor_notifications(
        notifications: Arc<Mutex<Vec<Notification>>>,
        max_count: usize,
        cache_path: &Path,
        session_key: &str,
        dbus_commands: Sender<NotificationDbusCommand>,
        dismissed_transfers: Arc<Mutex<HashSet<(u32, String)>>>,
        locally_dismissed: Arc<Mutex<HashSet<(u32, String)>>>,
    ) {
        loop {
            if let Err(error) = Self::monitor_notification_connection(
                &notifications,
                max_count,
                cache_path,
                session_key,
                &dbus_commands,
                &dismissed_transfers,
                &locally_dismissed,
            ) {
                log::warn!("Native notification monitor disconnected: {error}");
            }
            std::thread::sleep(NOTIFICATION_MONITOR_RECONNECT_DELAY);
        }
    }

    fn monitor_notification_connection(
        notifications: &Arc<Mutex<Vec<Notification>>>,
        max_count: usize,
        cache_path: &Path,
        session_key: &str,
        dbus_commands: &Sender<NotificationDbusCommand>,
        dismissed_transfers: &Arc<Mutex<HashSet<(u32, String)>>>,
        locally_dismissed: &Arc<Mutex<HashSet<(u32, String)>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use zbus::blocking::MessageIterator;

        let connection = open_notification_monitor_connection()?;
        let mut parser = NotificationMessageParser::default();
        log::info!("Using native zbus notification monitoring");
        for message in MessageIterator::from(&connection) {
            let message = message?;
            let event = match parser.push_message(&message, current_unix_timestamp()) {
                Ok(event) => event,
                Err(error) => {
                    log::debug!("Failed to decode monitored notification message: {error}");
                    continue;
                }
            };
            if let Some(event) = event {
                let mut notifs = notifications.lock().unwrap();
                match event {
                    NotificationBusEvent::Upsert(mut notification) => {
                        if remote_notification_id(&notification).is_some_and(|id| {
                            locally_dismissed.lock().unwrap().contains(&id)
                                || dismissed_transfers.lock().unwrap().contains(&id)
                        }) {
                            let _ = dbus_commands.send(NotificationDbusCommand::RefreshHistory);
                            continue;
                        }
                        log::debug!(
                            "Captured notification {}: {} - {}",
                            notification.id.unwrap_or_default(),
                            notification.app_name,
                            notification.summary
                        );
                        downloads::resolve_open_folders(std::slice::from_mut(&mut notification));
                        let is_active = notification
                            .file_transfer
                            .as_ref()
                            .is_some_and(FileTransfer::is_active);
                        upsert_notification(&mut notifs, notification, max_count);
                        if is_active {
                            // These rows are intentionally live-only. Avoid a
                            // cache fsync and history query on every progress tick.
                            continue;
                        }
                    }
                    NotificationBusEvent::Closed {
                        id,
                        server_owner,
                        reason,
                    } => {
                        if reason == 2
                            && notifs.iter().any(|notification| {
                                notification.id == Some(id)
                                    && notification.server_owner.as_deref() == Some(&server_owner)
                                    && notification
                                        .file_transfer
                                        .as_ref()
                                        .is_some_and(FileTransfer::is_active)
                            })
                        {
                            dismissed_transfers
                                .lock()
                                .unwrap()
                                .insert((id, server_owner.clone()));
                        }
                        let previous_len = notifs.len();
                        notifs.retain(|notification| {
                            notification.id != Some(id)
                                || notification.server_owner.as_deref() != Some(&server_owner)
                        });
                        if notifs.len() == previous_len {
                            continue;
                        }
                        log::info!("COSMIC closed notification {id}");
                    }
                    NotificationBusEvent::SenderGone(sender) => {
                        let previous_len = notifs.len();
                        remove_abandoned_transfers(&mut notifs, &sender);
                        if notifs.len() == previous_len {
                            continue;
                        }
                    }
                }
                if let Err(error) = persist_cached_notifications(cache_path, session_key, &notifs) {
                    log::warn!("Failed to persist notification update: {error}");
                }
                let _ = dbus_commands.send(NotificationDbusCommand::RefreshHistory);
            }
        }

        Err("notification monitor message stream closed".into())
    }

    /// Get a snapshot of current notifications (newest first).
    ///
    /// Returns a clone of the notification list for safe iteration
    /// without holding the lock.
    pub fn get_notifications(&self) -> Vec<Notification> {
        self.notifications.lock().unwrap().clone()
    }

    /// Clear all notifications.
    ///
    /// Removes all notifications from the list. Does not affect the
    /// underlying D-Bus monitoring (new notifications will still appear).
    pub fn clear(&self) {
        let mut notifs = self.notifications.lock().unwrap();
        let remote = self.suppress_notifications(&notifs);
        self.close_remote_notifications(remote);
        notifs.clear();
        self.persist(&notifs);
        log::info!("Cleared all notifications");
    }

    /// Clear all notifications from a specific application.
    ///
    /// # Arguments
    ///
    /// * `app_name` - Application name to filter (exact match)
    pub fn clear_app(&self, app_name: &str) {
        let mut notifs = self.notifications.lock().unwrap();
        let remote = self.suppress_notifications(
            &notifs
                .iter()
                .filter(|notification| notification.app_name == app_name)
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.close_remote_notifications(remote);
        notifs.retain(|n| n.app_name != app_name);
        self.persist(&notifs);
        log::info!("Cleared notifications for app: {}", app_name);
    }

    /// Remove one notification using its daemon ID or cached local identity.
    ///
    /// Used when the user clicks the X button on a specific notification.
    ///
    /// # Arguments
    ///
    /// * `identity` - Stable identity of the notification to remove
    pub fn remove_notification(&self, identity: &NotificationIdentity) {
        let mut notifs = self.notifications.lock().unwrap();
        let selected: Vec<Notification> = notifs
            .iter()
            .find(|notification| identity.matches(notification))
            .cloned()
            .into_iter()
            .collect();
        let remote = self.suppress_notifications(&selected);
        self.close_remote_notifications(remote);
        notifs.retain(|notification| !identity.matches(notification));
        self.persist(&notifs);
        log::info!("Removed notification: {identity:?}");
    }

    /// Invoke the validated default action for a retained notification.
    pub fn activate_notification(&self, identity: &NotificationIdentity) -> bool {
        let target = self
            .notifications
            .lock()
            .unwrap()
            .iter()
            .find(|notification| identity.matches(notification))
            .and_then(|notification| {
                let (id, server_owner) = remote_notification_id(notification)?;
                let action = notification.activation_action.clone()?;
                Some((id, server_owner, action))
            });
        let Some((id, server_owner, action)) = target else {
            return false;
        };
        self.dbus_commands
            .send(NotificationDbusCommand::Activate {
                id,
                server_owner,
                action,
            })
            .is_ok()
    }

    fn persist(&self, notifications: &[Notification]) {
        if let Err(error) =
            persist_cached_notifications(&self.cache_path, &self.session_key, notifications)
        {
            log::warn!("Failed to persist notifications: {error}");
        }
    }

    fn suppress_notifications(&self, notifications: &[Notification]) -> Vec<(u32, String)> {
        let remote = remote_notification_ids(notifications);
        self.locally_dismissed
            .lock()
            .unwrap()
            .extend(remote.iter().cloned());
        self.dismissed_transfers.lock().unwrap().extend(
            notifications
                .iter()
                .filter(|notification| {
                    notification
                        .file_transfer
                        .as_ref()
                        .is_some_and(FileTransfer::is_active)
                })
                .filter_map(remote_notification_id),
        );
        remote
    }

    fn close_remote_notifications(&self, notifications: Vec<(u32, String)>) {
        if !notifications.is_empty()
            && self
                .dbus_commands
                .send(NotificationDbusCommand::Close(notifications))
                .is_err()
        {
            log::warn!("Notification D-Bus worker is unavailable");
        }
    }

    fn handle_dbus_command(
        command: NotificationDbusCommand,
        connection: &mut Option<zbus::blocking::Connection>,
        refresh_due: &mut Option<Instant>,
    ) {
        match command {
            NotificationDbusCommand::RefreshHistory => {}
            NotificationDbusCommand::Close(notifications) => {
                close_remote_notifications(connection, &notifications);
            }
            NotificationDbusCommand::Activate {
                id,
                server_owner,
                action,
            } => {
                activate_remote_notification(connection, id, &server_owner, &action);
            }
        }
        *refresh_due = Some(Instant::now() + COSMIC_HISTORY_EVENT_DEBOUNCE);
    }
}

fn open_notification_monitor_connection()
-> Result<zbus::blocking::Connection, Box<dyn std::error::Error>> {
    use zbus::MatchRule;
    use zbus::blocking::Connection;
    use zbus::message::Type as MessageType;

    let connection = Connection::session()?;
    let rules = [
        MatchRule::builder()
            .msg_type(MessageType::MethodCall)
            .path(NOTIFICATIONS_PATH)?
            .interface(NOTIFICATIONS_INTERFACE)?
            .member("Notify")?
            .build(),
        MatchRule::builder()
            .msg_type(MessageType::MethodReturn)
            .sender(NOTIFICATIONS_SERVICE)?
            .build(),
        MatchRule::builder()
            .msg_type(MessageType::Error)
            .sender(NOTIFICATIONS_SERVICE)?
            .build(),
        MatchRule::builder()
            .msg_type(MessageType::Signal)
            .path(NOTIFICATIONS_PATH)?
            .interface(NOTIFICATIONS_INTERFACE)?
            .member("NotificationClosed")?
            .build(),
        MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .build(),
    ];
    zbus::blocking::fdo::MonitoringProxy::new(&connection)?.become_monitor(&rules, 0)?;
    Ok(connection)
}

#[derive(Debug)]
enum NotificationBusEvent {
    Upsert(Notification),
    SenderGone(String),
    Closed {
        id: u32,
        server_owner: String,
        reason: u32,
    },
}

#[derive(Debug)]
struct PendingNotification {
    app_name: String,
    summary: String,
    body: String,
    timestamp: u64,
    activation_action: Option<String>,
    file_transfer: Option<FileTransfer>,
}

#[derive(Debug, Default)]
struct NotificationMessageParser {
    pending: HashMap<(String, u32), PendingNotification>,
}

impl NotificationMessageParser {
    fn push_message(
        &mut self,
        message: &zbus::Message,
        timestamp: u64,
    ) -> zbus::Result<Option<NotificationBusEvent>> {
        use zbus::message::Type as MessageType;
        use zbus::zvariant::OwnedValue;

        type NotifyArguments = (
            String,
            u32,
            String,
            String,
            String,
            Vec<String>,
            HashMap<String, OwnedValue>,
            i32,
        );

        let header = message.header();
        match header.message_type() {
            MessageType::MethodCall if header.member().is_some_and(|member| member == "Notify") => {
                let Some(sender) = header.sender().map(ToString::to_string) else {
                    return Ok(None);
                };
                let (app_name, _, _, summary, body, actions, hints, _): NotifyArguments =
                    message.body().deserialize()?;
                if summary.is_empty() {
                    return Ok(None);
                }
                let desktop_entry = notification_string_hint(&hints, "desktop-entry");
                let action_keys = actions
                    .chunks_exact(2)
                    .map(|pair| pair[0].clone())
                    .collect::<Vec<_>>();
                let activation_action =
                    discord_activation_action(&app_name, desktop_entry.as_deref(), &action_keys);
                let file_transfer = file_transfers::from_hints(&app_name, &hints);
                self.pending.insert(
                    (sender, message.primary_header().serial_num().get()),
                    PendingNotification {
                        app_name: (!app_name.is_empty())
                            .then_some(app_name)
                            .unwrap_or_else(|| "System".to_string()),
                        summary,
                        body,
                        timestamp,
                        activation_action,
                        file_transfer,
                    },
                );
                Ok(None)
            }
            MessageType::MethodReturn => {
                let Some(key) = header.destination().zip(header.reply_serial()).map(
                    |(destination, reply_serial)| (destination.to_string(), reply_serial.get()),
                ) else {
                    return Ok(None);
                };
                let Some(pending) = self.pending.remove(&key) else {
                    return Ok(None);
                };
                let id: u32 = message.body().deserialize()?;
                Ok(Some(NotificationBusEvent::Upsert(Notification {
                    id: Some(id),
                    server_owner: header.sender().map(ToString::to_string),
                    sender_owner: Some(key.0),
                    app_name: pending.app_name,
                    summary: pending.summary,
                    body: pending.body,
                    timestamp: pending.timestamp,
                    open_folder: None,
                    activation_action: pending.activation_action,
                    file_transfer: pending.file_transfer,
                })))
            }
            MessageType::Error => {
                if let Some(key) = header.destination().zip(header.reply_serial()).map(
                    |(destination, reply_serial)| (destination.to_string(), reply_serial.get()),
                ) {
                    self.pending.remove(&key);
                }
                Ok(None)
            }
            MessageType::Signal
                if header
                    .member()
                    .is_some_and(|member| member == "NameOwnerChanged") =>
            {
                let (name, old_owner, new_owner): (String, String, String) =
                    message.body().deserialize()?;
                if name.starts_with(':') && old_owner == name && new_owner.is_empty() {
                    self.pending.retain(|(sender, _), _| sender != &name);
                    Ok(Some(NotificationBusEvent::SenderGone(name)))
                } else {
                    Ok(None)
                }
            }
            MessageType::Signal
                if header
                    .member()
                    .is_some_and(|member| member == "NotificationClosed") =>
            {
                let Some(server_owner) = header.sender().map(ToString::to_string) else {
                    return Ok(None);
                };
                let (id, reason): (u32, u32) = message.body().deserialize()?;
                Ok(Some(NotificationBusEvent::Closed {
                    id,
                    server_owner,
                    reason,
                }))
            }
            _ => Ok(None),
        }
    }
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn remove_abandoned_transfers(notifications: &mut Vec<Notification>, sender: &str) {
    notifications.retain(|notification| {
        notification.sender_owner.as_deref() != Some(sender)
            || !notification
                .file_transfer
                .as_ref()
                .is_some_and(FileTransfer::is_active)
    });
}

fn prune_disconnected_transfer_senders(
    connection: &zbus::blocking::Connection,
    notifications: &mut Vec<Notification>,
) {
    let senders: HashSet<_> = notifications
        .iter()
        .filter(|notification| {
            notification
                .file_transfer
                .as_ref()
                .is_some_and(FileTransfer::is_active)
        })
        .filter_map(|notification| notification.sender_owner.clone())
        .collect();
    if senders.is_empty() {
        return;
    }
    let Ok(bus) = zbus::blocking::Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    ) else {
        return;
    };
    for sender in senders {
        // NameOwnerChanged can be missed during monitor reconnection. Only a
        // confirmed disconnect retires progress; bus errors are inconclusive.
        if matches!(bus.call::<_, _, bool>("NameHasOwner", &sender), Ok(false)) {
            remove_abandoned_transfers(notifications, &sender);
        }
    }
}

fn upsert_notification(
    notifications: &mut Vec<Notification>,
    notification: Notification,
    max_count: usize,
) {
    let existing = notification.id.and_then(|id| {
        notifications.iter().position(|candidate| {
            candidate.id == Some(id) && candidate.server_owner == notification.server_owner
        })
    });
    if let Some(index) = existing {
        let timestamp = notifications[index].timestamp;
        let open_folder = notification
            .open_folder
            .clone()
            .or_else(|| notifications[index].open_folder.clone());
        let activation_action = notification
            .activation_action
            .clone()
            .or_else(|| notifications[index].activation_action.clone());
        notifications[index] = Notification {
            timestamp,
            open_folder,
            activation_action,
            ..notification
        };
    } else {
        notifications.insert(0, notification);
        notifications.truncate(max_count);
    }
}

fn preserve_notification_targets(existing: &[Notification], refreshed: &mut [Notification]) {
    for notification in refreshed {
        let previous = existing.iter().find(|candidate| {
            match (
                remote_notification_id(candidate),
                remote_notification_id(notification),
            ) {
                (Some(left), Some(right)) => left == right,
                _ => {
                    candidate.app_name == notification.app_name
                        && candidate.timestamp == notification.timestamp
                }
            }
        });
        if notification.open_folder.is_none() {
            notification.open_folder = previous.and_then(|candidate| candidate.open_folder.clone());
        }
        if notification.activation_action.is_none() {
            notification.activation_action =
                previous.and_then(|candidate| candidate.activation_action.clone());
        }
        if let Some(previous) = previous.filter(|previous| previous.file_transfer.is_some()) {
            notification.timestamp = previous.timestamp;
            // Active transfers are transient and cannot occur in retained
            // history. A matching history row is terminal; use its actual text,
            // even if its final live Notify reply was missed.
            notification.file_transfer = previous
                .file_transfer
                .clone()
                .filter(|transfer| !transfer.is_active());
        }
    }
}

fn reconcile_transfer_history(existing: &[Notification], history: &mut CosmicNotificationHistory) {
    preserve_notification_targets(existing, &mut history.notifications);
    for (index, notification) in existing.iter().enumerate().filter(|(_, notification)| {
        notification.file_transfer.is_some()
            && notification.server_owner.as_deref() == Some(history.server_owner.as_str())
    }) {
        let position = history.notifications.iter().position(|candidate| {
            remote_notification_id(candidate) == remote_notification_id(notification)
        });
        let row = if let Some(position) = position {
            history.notifications.remove(position)
        } else if notification
            .file_transfer
            .as_ref()
            .is_some_and(FileTransfer::is_active)
        {
            notification.clone()
        } else {
            continue;
        };
        history
            .notifications
            .insert(index.min(history.notifications.len()), row);
    }
    history
        .notifications
        .truncate(COSMIC_NOTIFICATION_HISTORY_LIMIT);
}

fn reconcile_transfer_dismissals(
    history: &CosmicNotificationHistory,
    dismissed: &mut HashSet<(u32, String)>,
    dismissed_transfers: &mut HashSet<(u32, String)>,
) {
    let history_ids: HashSet<_> = history
        .notifications
        .iter()
        .filter_map(remote_notification_id)
        .collect();
    dismissed_transfers.retain(|id| {
        if id.1 != history.server_owner {
            false
        } else if history_ids.contains(id) {
            // Once a terminal row reaches history, normal history dismissal
            // tracking owns it. Until then, suppress every live replacement.
            dismissed.insert(id.clone());
            false
        } else {
            true
        }
    });
}

fn notification_string_hint(
    hints: &HashMap<String, zbus::zvariant::OwnedValue>,
    name: &str,
) -> Option<String> {
    hints
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
}

fn discord_activation_action(
    app_name: &str,
    desktop_entry: Option<&str>,
    action_keys: &[String],
) -> Option<String> {
    let discord_app = app_name.eq_ignore_ascii_case("discord")
        || desktop_entry.is_some_and(|entry| {
            let entry = entry
                .trim()
                .trim_end_matches(".desktop")
                .to_ascii_lowercase();
            entry == "discord" || entry == "com.discordapp.discord"
        });
    discord_app
        .then(|| {
            action_keys
                .iter()
                .find(|action| action.as_str() == "default")
                .cloned()
        })
        .flatten()
}

fn remote_notification_id(notification: &Notification) -> Option<(u32, String)> {
    Some((
        notification.id?,
        notification.server_owner.as_ref()?.clone(),
    ))
}

fn remote_notification_ids(notifications: &[Notification]) -> Vec<(u32, String)> {
    notifications
        .iter()
        .filter_map(remote_notification_id)
        .collect()
}

fn apply_local_dismissal_suppression(
    history: &mut Vec<Notification>,
    locally_dismissed: &mut HashSet<(u32, String)>,
) {
    let history_ids = history
        .iter()
        .filter_map(remote_notification_id)
        .collect::<HashSet<_>>();
    locally_dismissed.retain(|id| history_ids.contains(id));
    history.retain(|notification| {
        remote_notification_id(notification).is_none_or(|id| !locally_dismissed.contains(&id))
    });
}

fn ensure_notification_connection(connection: &mut Option<zbus::blocking::Connection>) -> bool {
    if connection.is_none() {
        *connection = match zbus::blocking::Connection::session() {
            Ok(connection) => Some(connection),
            Err(error) => {
                log::debug!("Failed to reconnect notification D-Bus worker: {error}");
                None
            }
        };
    }
    connection.is_some()
}

fn close_remote_notifications(
    connection: &mut Option<zbus::blocking::Connection>,
    notifications: &[(u32, String)],
) {
    for attempt in 0..2 {
        if !ensure_notification_connection(connection) {
            return;
        }
        let active_connection = connection.as_ref().expect("connection was ensured");
        match close_remote_notifications_inner(active_connection, notifications) {
            Ok(()) => return,
            Err(error) if matches!(error, zbus::Error::InputOutput(_)) && attempt == 0 => {
                *connection = None;
            }
            Err(error) => {
                log::warn!("Failed to close COSMIC notification: {error}");
                return;
            }
        }
    }
}

fn close_remote_notifications_inner(
    connection: &zbus::blocking::Connection,
    notifications: &[(u32, String)],
) -> zbus::Result<()> {
    use zbus::blocking::Proxy;
    use zbus::names::OwnedUniqueName;

    let bus = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let current_owner: OwnedUniqueName = bus.call("GetNameOwner", &NOTIFICATIONS_SERVICE)?;
    let notifications_proxy = Proxy::new(
        connection,
        current_owner.as_str(),
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
    )?;

    for (id, server_owner) in notifications {
        if current_owner.as_str() == server_owner {
            let _: () = notifications_proxy.call("CloseNotification", id)?;
        }
    }
    Ok(())
}

fn activate_remote_notification(
    connection: &mut Option<zbus::blocking::Connection>,
    id: u32,
    server_owner: &str,
    action: &str,
) {
    for attempt in 0..2 {
        if !ensure_notification_connection(connection) {
            return;
        }
        let active_connection = connection.as_ref().expect("connection was ensured");
        match activate_remote_notification_inner(active_connection, id, server_owner, action) {
            Ok(()) => return,
            Err(error) if matches!(error, zbus::Error::InputOutput(_)) && attempt == 0 => {
                *connection = None;
            }
            Err(error) => {
                log::warn!("Failed to activate COSMIC notification: {error}");
                return;
            }
        }
    }
}

fn activate_remote_notification_inner(
    connection: &zbus::blocking::Connection,
    id: u32,
    server_owner: &str,
    action: &str,
) -> zbus::Result<()> {
    use zbus::blocking::Proxy;
    use zbus::names::OwnedUniqueName;

    let bus = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let current_owner: OwnedUniqueName = bus.call("GetNameOwner", &NOTIFICATIONS_SERVICE)?;
    if current_owner.as_str() != server_owner {
        return Ok(());
    }
    let notifications_proxy = Proxy::new(
        connection,
        current_owner.as_str(),
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
    )?;
    let _: () = notifications_proxy.call("InvokeNotificationAction", &(id, action))?;
    Ok(())
}

type CosmicNotificationHistoryEntry = (u32, String, String, String, u64);
type CosmicNotificationHistoryEntryV2 = (u32, String, String, String, u64, Vec<String>, String);

struct CosmicNotificationHistory {
    server_owner: String,
    notifications: Vec<Notification>,
}

fn load_cosmic_notification_history(
    connection: &zbus::blocking::Connection,
) -> zbus::Result<Option<CosmicNotificationHistory>> {
    use zbus::blocking::Proxy;
    use zbus::names::OwnedUniqueName;

    let bus = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owner: OwnedUniqueName = bus.call("GetNameOwner", &NOTIFICATIONS_SERVICE)?;
    let proxy = Proxy::new(
        connection,
        owner.as_str(),
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
    )?;
    let v2_entries: Option<Vec<CosmicNotificationHistoryEntryV2>> =
        match proxy.call("GetNotificationHistoryV2", &()) {
            Ok(entries) => Some(entries),
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };

    if let Some(entries) = v2_entries {
        return Ok(Some(CosmicNotificationHistory {
            server_owner: owner.to_string(),
            notifications: entries
                .into_iter()
                .map(
                    |(id, app_name, summary, body, timestamp, actions, desktop_entry)| {
                        let activation_action =
                            discord_activation_action(&app_name, Some(&desktop_entry), &actions);
                        Notification {
                            id: Some(id),
                            server_owner: Some(owner.to_string()),
                            sender_owner: None,
                            app_name,
                            summary,
                            body,
                            timestamp,
                            open_folder: None,
                            activation_action,
                            file_transfer: None,
                        }
                    },
                )
                .collect(),
        }));
    }

    let entries: Vec<CosmicNotificationHistoryEntry> =
        match proxy.call("GetNotificationHistory", &()) {
            Ok(entries) => entries,
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };

    let notifications = entries
        .into_iter()
        .map(|(id, app_name, summary, body, timestamp)| Notification {
            id: Some(id),
            server_owner: Some(owner.to_string()),
            sender_owner: None,
            app_name,
            summary,
            body,
            timestamp,
            open_folder: None,
            activation_action: None,
            file_transfer: None,
        })
        .collect::<Vec<_>>();
    Ok(Some(CosmicNotificationHistory {
        server_owner: owner.to_string(),
        notifications,
    }))
}

fn notification_cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(CACHE_DIRECTORY)
        .join(CACHE_FILENAME)
}

fn notification_session_key() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unknown-boot".to_string())
}

fn load_cached_notifications(
    path: &Path,
    session_key: &str,
    max_notifications: usize,
) -> io::Result<Vec<Notification>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut cache: NotificationCache = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    // Older versions appended the login session and the notification daemon's
    // transient D-Bus owner. Accept those files when their boot ID prefix still
    // matches, then rewrite them with the stable boot-only key at startup.
    let from_current_boot = cache.session_key == session_key
        || cache
            .session_key
            .strip_prefix(session_key)
            .is_some_and(|suffix| suffix.starts_with(':'));
    if !from_current_boot {
        return Ok(Vec::new());
    }
    cache.notifications.retain(|notification| {
        !notification
            .file_transfer
            .as_ref()
            .is_some_and(FileTransfer::is_active)
    });
    cache.notifications.truncate(max_notifications);
    Ok(cache.notifications)
}

fn persist_cached_notifications(
    path: &Path,
    session_key: &str,
    notifications: &[Notification],
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cache = NotificationCache {
        session_key: session_key.to_string(),
        notifications: notifications
            .iter()
            .filter(|notification| {
                !notification
                    .file_transfer
                    .as_ref()
                    .is_some_and(FileTransfer::is_active)
            })
            .cloned()
            .collect(),
    };
    let bytes = serde_json::to_vec(&cache)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::{
        NOTIFICATIONS_INTERFACE, NOTIFICATIONS_PATH, NOTIFICATIONS_SERVICE, Notification,
        NotificationBusEvent, NotificationMessageParser, apply_local_dismissal_suppression,
        load_cached_notifications, open_notification_monitor_connection,
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
        let Some(NotificationBusEvent::SenderGone(sender)) =
            parser.push_message(&signal, 43).unwrap()
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
        let Some(NotificationBusEvent::Upsert(notification)) =
            parser.push_message(&reply, 43).unwrap()
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
        let Some(NotificationBusEvent::Upsert(notification)) =
            parser.push_message(&reply, 43).unwrap()
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
}
