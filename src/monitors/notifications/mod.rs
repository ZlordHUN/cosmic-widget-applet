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

mod downloads;
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
            history
                .notifications
                .retain(|notification| !is_unwanted_battery_discharge(notification));
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
                        if is_unwanted_battery_discharge(&notification) {
                            let previous_len = notifs.len();
                            if let Some((id, owner)) = remote_notification_id(&notification) {
                                notifs.retain(|candidate| {
                                    remote_notification_id(candidate) != Some((id, owner.clone()))
                                });
                            }
                            if notifs.len() != previous_len {
                                let _ =
                                    persist_cached_notifications(cache_path, session_key, &notifs);
                            }
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
        self.notifications
            .lock()
            .unwrap()
            .iter()
            .filter(|notification| !is_unwanted_battery_discharge(notification))
            .cloned()
            .collect()
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

/// Solaar reports routine peripheral battery state changes through desktop
/// notifications. Keep those out of the widget while retaining other Solaar
/// notifications (for example, pairing or connection status).
fn is_unwanted_battery_discharge(notification: &Notification) -> bool {
    let body = notification.body.to_ascii_lowercase();
    let summary = notification.summary.to_ascii_lowercase();
    notification.app_name.eq_ignore_ascii_case("solaar")
        && body.contains("discharg")
        && (body.contains("battery") || summary.contains("battery"))
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
            && !is_unwanted_battery_discharge(notification)
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
mod tests;
