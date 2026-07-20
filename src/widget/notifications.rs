// SPDX-License-Identifier: MPL-2.0

//! # Notification Monitoring Module
//!
//! This module captures desktop notifications via D-Bus and displays them
//! in the widget. Uses `busctl` to monitor the `org.freedesktop.Notifications`
//! interface for incoming notification calls.
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
//! │ Desktop App  │───►│ D-Bus       │───►│ busctl        │
//! │ (notify-send)│    │ Notify call │    │ monitor       │
//! └──────────────┘    └─────────────┘    └───────┬───────┘
//!                                                 │
//!                     ┌───────────────┐          │ stdout
//!                     │ Main Thread   │◄─────────┘
//!                     │ (reads list)  │    ┌───────────────┐
//!                     └───────────────┘    │ Background    │
//!                                          │ Thread        │
//!                                          │ (parses)      │
//!                                          └───────────────┘
//! ```
//!
//! ## busctl Output Parsing
//!
//! The `busctl monitor` command outputs D-Bus messages in a text format.
//! We parse STRING fields from Notify method calls:
//!
//! ```text
//! Type=method_call  Member=Notify
//!   STRING "app_name"      # Index 0: Application name
//!   STRING ""              # Index 1: App icon (usually empty)
//!   STRING "Summary text"  # Index 2: Notification title
//!   STRING "Body text"     # Index 3: Notification body
//! ```
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
/// Spawns a background thread running `busctl monitor` to capture incoming
/// notifications. The notification list is shared via Arc<Mutex> for
/// thread-safe access from the main render thread.
///
/// # Threading Model
///
/// - Background thread: Runs `busctl monitor`, parses output, updates list
/// - Main thread: Reads notification list for rendering
/// - Shared state: `notifications` Vec protected by Mutex
///
/// # Resource Usage
///
/// - Spawns one persistent background thread
/// - Spawns one `busctl` child process
/// - Both run for the lifetime of the application
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
    /// 1. Starts `busctl monitor` to watch D-Bus
    /// 2. Parses Notify method calls from stdout
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
            if let Err(e) = Self::monitor_notifications(
                notifications_clone,
                max_count,
                &cache_path_clone,
                &session_key_clone,
            ) {
                log::error!("Notification monitoring error: {}", e);
            }
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

    /// Main D-Bus monitoring loop (runs in background thread).
    ///
    /// Uses `busctl monitor` to watch for Notify method calls on the
    /// user session bus. Parses the text output to extract notification
    /// fields.
    ///
    /// # busctl Command
    ///
    /// ```bash
    /// busctl monitor --user \
    ///   --match "type=method_call,interface=org.freedesktop.Notifications,member=Notify"
    /// ```
    ///
    /// # Parsing Strategy
    ///
    /// 1. Watch for lines containing "Member=Notify" to start new notification
    /// 2. Count STRING fields in order (app_name=0, icon=1, summary=2, body=3)
    /// 3. Extract values between double quotes
    /// 4. After body (field 3), save the notification
    ///
    /// # Error Handling
    ///
    /// Returns error if busctl cannot be spawned. Parsing errors within
    /// the loop are logged but don't stop monitoring.
    fn monitor_notifications(
        notifications: Arc<Mutex<Vec<Notification>>>,
        max_count: usize,
        cache_path: &Path,
        session_key: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        log::info!("Starting notification monitor via busctl");

        // Capture Notify calls, their returned daemon IDs, and close signals.
        let mut child = Command::new("busctl")
            .args(&[
                "monitor",
                "--user",
                "--match",
                "type=method_call,interface=org.freedesktop.Notifications,member=Notify",
                "--match",
                "type=method_return,sender=org.freedesktop.Notifications",
                "--match",
                "type=signal,interface=org.freedesktop.Notifications,member=NotificationClosed",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // Suppress busctl stderr noise
            .spawn()?;

        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let reader = BufReader::new(stdout);

        let mut parser = BusctlNotificationParser::default();

        for line in reader.lines() {
            let line = line?;
            for event in parser.push_line(&line, current_unix_timestamp()) {
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

        Ok(())
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

#[derive(Debug)]
enum NotificationBusEvent {
    Upsert(Notification),
    Closed { id: u32, server_owner: String },
}

#[derive(Debug, Default)]
struct MonitoredBusMessage {
    message_type: String,
    cookie: Option<u64>,
    reply_cookie: Option<u64>,
    sender: Option<String>,
    destination: Option<String>,
    member: Option<String>,
    strings: Vec<String>,
    uint32s: Vec<u32>,
}

#[derive(Debug)]
struct PendingNotification {
    app_name: String,
    summary: String,
    body: String,
    timestamp: u64,
}

#[derive(Debug, Default)]
struct BusctlNotificationParser {
    current: Option<MonitoredBusMessage>,
    pending: HashMap<(String, u64), PendingNotification>,
}

impl BusctlNotificationParser {
    fn push_line(&mut self, line: &str, timestamp: u64) -> Vec<NotificationBusEvent> {
        let trimmed = line.trim();
        let mut events = Vec::new();

        if is_busctl_message_header(trimmed) {
            if let Some(event) = self.finish_message(timestamp) {
                events.push(event);
            }
            let mut message = MonitoredBusMessage::default();
            populate_message_header(&mut message, trimmed);
            self.current = Some(message);
            return events;
        }

        if trimmed.is_empty() {
            if let Some(event) = self.finish_message(timestamp) {
                events.push(event);
            }
            return events;
        }

        let Some(message) = self.current.as_mut() else {
            return events;
        };
        populate_message_header(message, trimmed);
        if let Some(value) = parse_busctl_string(trimmed) {
            message.strings.push(value);
        } else if let Some(value) = parse_busctl_u32(trimmed) {
            message.uint32s.push(value);
        }

        events
    }

    fn finish_message(&mut self, timestamp: u64) -> Option<NotificationBusEvent> {
        let message = self.current.take()?;
        match (message.message_type.as_str(), message.member.as_deref()) {
            ("method_call", Some("Notify")) => {
                let sender = message.sender?;
                let cookie = message.cookie?;
                let summary = message.strings.get(2)?.clone();
                if summary.is_empty() {
                    return None;
                }
                self.pending.insert(
                    (sender, cookie),
                    PendingNotification {
                        app_name: message
                            .strings
                            .first()
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .unwrap_or_else(|| "System".to_string()),
                        summary,
                        body: message.strings.get(3).cloned().unwrap_or_default(),
                        timestamp,
                    },
                );
                None
            }
            ("method_return", _) => {
                let key = (message.destination?, message.reply_cookie?);
                let pending = self.pending.remove(&key)?;
                let id = *message.uint32s.first()?;
                Some(NotificationBusEvent::Upsert(Notification {
                    id: Some(id),
                    server_owner: message.sender,
                    app_name: pending.app_name,
                    summary: pending.summary,
                    body: pending.body,
                    timestamp: pending.timestamp,
                }))
            }
            ("signal", Some("NotificationClosed")) => Some(NotificationBusEvent::Closed {
                id: *message.uint32s.first()?,
                server_owner: message.sender?,
            }),
            _ => None,
        }
    }
}

fn is_busctl_message_header(line: &str) -> bool {
    line.starts_with("Type=") || line.starts_with("‣ Type=")
}

fn populate_message_header(message: &mut MonitoredBusMessage, line: &str) {
    if let Some(value) = busctl_header_value(line, "Type") {
        message.message_type = value.to_string();
    }
    if let Some(value) = busctl_header_value(line, "Cookie").and_then(parse_u64) {
        message.cookie = Some(value);
    }
    if let Some(value) = busctl_header_value(line, "ReplyCookie").and_then(parse_u64) {
        message.reply_cookie = Some(value);
    }
    if let Some(value) = busctl_header_value(line, "Sender") {
        message.sender = Some(value.to_string());
    }
    if let Some(value) = busctl_header_value(line, "Destination") {
        message.destination = Some(value.to_string());
    }
    if let Some(value) = busctl_header_value(line, "Member") {
        message.member = Some(value.to_string());
    }
}

fn busctl_header_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(&prefix))
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn parse_busctl_string(line: &str) -> Option<String> {
    let encoded = line.strip_prefix("STRING ")?.strip_suffix(';')?;
    serde_json::from_str(encoded).ok()
}

fn parse_busctl_u32(line: &str) -> Option<u32> {
    line.strip_prefix("UINT32 ")?
        .strip_suffix(';')?
        .parse()
        .ok()
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
        BusctlNotificationParser, Notification, NotificationBusEvent,
        apply_local_dismissal_suppression, load_cached_notifications,
        persist_cached_notifications, upsert_notification,
    };
    use std::collections::HashSet;

    const NOTIFY_EXCHANGE: &str = r#"‣ Type=method_call  Endian=l  Flags=4  Version=1 Cookie=2
  Sender=:1.8323  Destination=org.freedesktop.Notifications  Path=/org/freedesktop/Notifications  Interface=org.freedesktop.Notifications  Member=Notify
  UniqueName=:1.8323
  MESSAGE "susssasa{sv}i" {
          STRING "COSMIC synchronization probe";
          UINT32 0;
          STRING "";
          STRING "ID tracking probe";
          STRING "Capturing the assigned ID.";
  };

‣ Type=method_return  Endian=l  Flags=0  Version=1 Cookie=71  ReplyCookie=2
  Sender=:1.82  Destination=:1.8323
  UniqueName=:1.82
  MESSAGE "u" {
          UINT32 15;
  };
"#;

    const CLOSED_SIGNAL: &str = r#"‣ Type=signal  Endian=l  Flags=0  Version=1 Cookie=73
  Sender=:1.82  Path=/org/freedesktop/Notifications  Interface=org.freedesktop.Notifications  Member=NotificationClosed
  UniqueName=:1.82
  MESSAGE "uu" {
          UINT32 15;
          UINT32 2;
  };
"#;

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
        let mut parser = BusctlNotificationParser::default();
        let mut events = Vec::new();
        for line in NOTIFY_EXCHANGE.lines().chain([""]) {
            events.extend(parser.push_line(line, 42));
        }

        let NotificationBusEvent::Upsert(notification) = &events[0] else {
            panic!("expected a notification event");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(notification.id, Some(15));
        assert_eq!(notification.server_owner.as_deref(), Some(":1.82"));
        assert_eq!(notification.app_name, "COSMIC synchronization probe");
        assert_eq!(notification.summary, "ID tracking probe");
        assert_eq!(notification.body, "Capturing the assigned ID.");
        assert_eq!(notification.timestamp, 42);
    }

    #[test]
    fn parses_cosmic_notification_closed_signal() {
        let mut parser = BusctlNotificationParser::default();
        let mut events = Vec::new();
        for line in CLOSED_SIGNAL.lines().chain([""]) {
            events.extend(parser.push_line(line, 42));
        }

        assert!(matches!(
            events.as_slice(),
            [NotificationBusEvent::Closed { id: 15, server_owner }] if server_owner == ":1.82"
        ));
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
