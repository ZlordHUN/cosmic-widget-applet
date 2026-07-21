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

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CACHE_DIRECTORY: &str = "cosmic-widget-applet";
const CACHE_FILENAME: &str = "notifications.json";
const COSMIC_NOTIFICATION_HISTORY_LIMIT: usize = 200;
const NOTIFICATIONS_SERVICE: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
const COSMIC_HISTORY_SYNC_INTERVAL: Duration = Duration::from_secs(1);
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
    /// Application that sent the notification (e.g., "Firefox", "System")
    pub app_name: String,
    /// Notification title/headline
    pub summary: String,
    /// Notification body text (may be empty)
    pub body: String,
    /// Unix timestamp when notification was captured (seconds since epoch)
    pub timestamp: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct NotificationCache {
    session_key: String,
    notifications: Vec<Notification>,
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
/// - Background thread: Decodes monitored D-Bus messages and updates the list
/// - Main thread: Reads notification list for rendering
/// - Shared state: `notifications` Vec protected by Mutex
///
/// # Resource Usage
///
/// - Spawns one persistent background thread
/// - Maintains one native session-bus monitor connection
#[derive(Clone)]
pub struct NotificationMonitor {
    /// Shared notification list, newest first
    notifications: Arc<Mutex<Vec<Notification>>>,
    cache_path: Arc<PathBuf>,
    session_key: Arc<String>,
    locally_dismissed: Arc<Mutex<HashSet<(u32, String)>>>,
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
        let (cached, retention_limit) = match load_cosmic_notification_history() {
            Ok(Some(history)) => {
                log::info!("Loaded {} notifications from COSMIC", history.len());
                (history, COSMIC_NOTIFICATION_HISTORY_LIMIT)
            }
            Ok(None) => (cached, max_notifications),
            Err(error) => {
                log::warn!("Failed to retrieve COSMIC notification history: {error}");
                (cached, max_notifications)
            }
        };
        if let Err(error) = persist_cached_notifications(&cache_path, &session_key, &cached) {
            log::warn!("Failed to initialize notification cache: {error}");
        }
        let notifications = Arc::new(Mutex::new(cached));
        let locally_dismissed = Arc::new(Mutex::new(HashSet::new()));

        // Spawn background thread to monitor D-Bus
        // This runs for the lifetime of the application
        let notifications_clone = Arc::clone(&notifications);
        let cache_path_clone = Arc::clone(&cache_path);
        let session_key_clone = Arc::clone(&session_key);
        let max_count = retention_limit;

        std::thread::spawn(move || {
            Self::monitor_notifications(
                notifications_clone,
                max_count,
                &cache_path_clone,
                &session_key_clone,
            );
        });

        let notifications_clone = Arc::clone(&notifications);
        let cache_path_clone = Arc::clone(&cache_path);
        let session_key_clone = Arc::clone(&session_key);
        let locally_dismissed_clone = Arc::clone(&locally_dismissed);
        std::thread::spawn(move || {
            Self::synchronize_cosmic_history(
                notifications_clone,
                locally_dismissed_clone,
                &cache_path_clone,
                &session_key_clone,
            );
        });

        Self {
            notifications,
            cache_path,
            session_key,
            locally_dismissed,
        }
    }

    fn synchronize_cosmic_history(
        notifications: Arc<Mutex<Vec<Notification>>>,
        locally_dismissed: Arc<Mutex<HashSet<(u32, String)>>>,
        cache_path: &Path,
        session_key: &str,
    ) {
        loop {
            std::thread::sleep(COSMIC_HISTORY_SYNC_INTERVAL);
            let mut history = match load_cosmic_notification_history() {
                Ok(Some(history)) => history,
                Ok(None) => return,
                Err(error) => {
                    log::debug!("Failed to synchronize COSMIC notification history: {error}");
                    continue;
                }
            };

            let mut dismissed = locally_dismissed.lock().unwrap();
            apply_local_dismissal_suppression(&mut history, &mut dismissed);
            drop(dismissed);

            let mut current = notifications.lock().unwrap();
            if *current == history {
                continue;
            }
            *current = history;
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
    ) {
        loop {
            if let Err(error) = Self::monitor_notification_connection(
                &notifications,
                max_count,
                cache_path,
                session_key,
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
                    NotificationBusEvent::Upsert(notification) => {
                        log::info!(
                            "Captured notification {}: {} - {}",
                            notification.id.unwrap_or_default(),
                            notification.app_name,
                            notification.summary
                        );
                        upsert_notification(&mut notifs, notification, max_count);
                    }
                    NotificationBusEvent::Closed { id, server_owner } => {
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
                }
                if let Err(error) =
                    persist_cached_notifications(cache_path, session_key, &notifs)
                {
                    log::warn!("Failed to persist notification update: {error}");
                }
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
        let remote = remote_notification_ids(&notifs);
        self.suppress_remote_notifications(&remote);
        close_remote_notifications(remote);
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
        let remote = remote_notification_ids(
            &notifs
                .iter()
                .filter(|notification| notification.app_name == app_name)
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.suppress_remote_notifications(&remote);
        close_remote_notifications(remote);
        notifs.retain(|n| n.app_name != app_name);
        self.persist(&notifs);
        log::info!("Cleared notifications for app: {}", app_name);
    }

    /// Remove a specific notification by app name and timestamp.
    ///
    /// Used when the user clicks the X button on a specific notification.
    ///
    /// # Arguments
    ///
    /// * `app_name` - Application name of the notification
    /// * `timestamp` - Unix timestamp when notification was captured
    pub fn remove_notification(&self, app_name: &str, timestamp: u64) {
        let mut notifs = self.notifications.lock().unwrap();
        let remote: Vec<(u32, String)> = notifs
            .iter()
            .find(|notification| {
                notification.app_name == app_name && notification.timestamp == timestamp
            })
            .and_then(remote_notification_id)
            .into_iter()
            .collect();
        self.suppress_remote_notifications(&remote);
        close_remote_notifications(remote);
        notifs.retain(|n| !(n.app_name == app_name && n.timestamp == timestamp));
        self.persist(&notifs);
        log::info!("Removed notification: {} at {}", app_name, timestamp);
    }

    fn persist(&self, notifications: &[Notification]) {
        if let Err(error) =
            persist_cached_notifications(&self.cache_path, &self.session_key, notifications)
        {
            log::warn!("Failed to persist notifications: {error}");
        }
    }

    fn suppress_remote_notifications(&self, notifications: &[(u32, String)]) {
        self.locally_dismissed
            .lock()
            .unwrap()
            .extend(notifications.iter().cloned());
    }
}

fn open_notification_monitor_connection()
    -> Result<zbus::blocking::Connection, Box<dyn std::error::Error>>
{
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
    ];
    zbus::blocking::fdo::MonitoringProxy::new(&connection)?.become_monitor(&rules, 0)?;
    Ok(connection)
}

#[derive(Debug)]
enum NotificationBusEvent {
    Upsert(Notification),
    Closed { id: u32, server_owner: String },
}

#[derive(Debug)]
struct PendingNotification {
    app_name: String,
    summary: String,
    body: String,
    timestamp: u64,
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
                let (app_name, _, _, summary, body, _, _, _): NotifyArguments =
                    message.body().deserialize()?;
                if summary.is_empty() {
                    return Ok(None);
                }
                self.pending.insert(
                    (sender, message.primary_header().serial_num().get()),
                    PendingNotification {
                        app_name: (!app_name.is_empty())
                            .then_some(app_name)
                            .unwrap_or_else(|| "System".to_string()),
                        summary,
                        body,
                        timestamp,
                    },
                );
                Ok(None)
            }
            MessageType::MethodReturn => {
                let Some(key) = header.destination().zip(header.reply_serial()).map(
                    |(destination, reply_serial)| {
                        (destination.to_string(), reply_serial.get())
                    },
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
                    app_name: pending.app_name,
                    summary: pending.summary,
                    body: pending.body,
                    timestamp: pending.timestamp,
                })))
            }
            MessageType::Error => {
                if let Some(key) = header.destination().zip(header.reply_serial()).map(
                    |(destination, reply_serial)| {
                        (destination.to_string(), reply_serial.get())
                    },
                ) {
                    self.pending.remove(&key);
                }
                Ok(None)
            }
            MessageType::Signal
                if header
                    .member()
                    .is_some_and(|member| member == "NotificationClosed") =>
            {
                let Some(server_owner) = header.sender().map(ToString::to_string) else {
                    return Ok(None);
                };
                let (id, _reason): (u32, u32) = message.body().deserialize()?;
                Ok(Some(NotificationBusEvent::Closed { id, server_owner }))
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
        notifications[index] = Notification {
            timestamp,
            ..notification
        };
    } else {
        notifications.insert(0, notification);
        notifications.truncate(max_count);
    }
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
        remote_notification_id(notification)
            .is_none_or(|id| !locally_dismissed.contains(&id))
    });
}

fn close_remote_notifications(notifications: Vec<(u32, String)>) {
    if notifications.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(error) = close_remote_notifications_inner(&notifications) {
            log::warn!("Failed to close COSMIC notification: {error}");
        }
    });
}

fn close_remote_notifications_inner(
    notifications: &[(u32, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::names::OwnedUniqueName;

    let connection = Connection::session()?;
    let bus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let current_owner: OwnedUniqueName = bus.call("GetNameOwner", &NOTIFICATIONS_SERVICE)?;
    let notifications_proxy = Proxy::new(
        &connection,
        NOTIFICATIONS_SERVICE,
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

type CosmicNotificationHistoryEntry = (u32, String, String, String, u64);

fn load_cosmic_notification_history()
    -> Result<Option<Vec<Notification>>, Box<dyn std::error::Error>>
{
    use zbus::blocking::{Connection, Proxy};
    use zbus::names::OwnedUniqueName;

    let connection = Connection::session()?;
    let bus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owner: OwnedUniqueName = bus.call("GetNameOwner", &NOTIFICATIONS_SERVICE)?;
    let proxy = Proxy::new(
        &connection,
        NOTIFICATIONS_SERVICE,
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_INTERFACE,
    )?;
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
            app_name,
            summary,
            body,
            timestamp,
        })
        .collect::<Vec<_>>();
    Ok(Some(notifications))
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
        notifications: notifications.to_vec(),
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
        NotificationBusEvent, NotificationMessageParser,
        apply_local_dismissal_suppression, load_cached_notifications,
        open_notification_monitor_connection, persist_cached_notifications, upsert_notification,
    };
    use std::collections::{HashMap, HashSet};
    use std::sync::mpsc;
    use std::time::Duration;
    use zbus::blocking::{Connection, MessageIterator, Proxy};
    use zbus::Message;
    use zbus::zvariant::OwnedValue;

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
            app_name: "Test".to_string(),
            summary: summary.to_string(),
            body: "Body".to_string(),
            timestamp,
        }
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
    }

    #[test]
    fn parses_cosmic_notification_closed_signal() {
        let mut parser = NotificationMessageParser::default();
        let event = parser.push_message(&closed_signal(), 42).unwrap();

        assert!(matches!(
            event,
            Some(NotificationBusEvent::Closed { id: 15, server_owner }) if server_owner == ":1.82"
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
    fn suppresses_local_dismissals_until_cosmic_removes_them() {
        let mut dismissed = HashSet::from([
            (15, ":1.82".to_string()),
            (99, ":1.12".to_string()),
        ]);
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
