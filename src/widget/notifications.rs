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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_DIRECTORY: &str = "cosmic-widget-applet";
const CACHE_FILENAME: &str = "notifications.json";

// ============================================================================
// Notification Struct
// ============================================================================

/// A captured desktop notification.
///
/// Contains the essential fields from a D-Bus Notify method call,
/// plus a timestamp for ordering and identification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
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
        if let Err(error) = persist_cached_notifications(&cache_path, &session_key, &cached) {
            log::warn!("Failed to initialize notification cache: {error}");
        }
        let notifications = Arc::new(Mutex::new(cached));

        // Spawn background thread to monitor D-Bus
        // This runs for the lifetime of the application
        let notifications_clone = Arc::clone(&notifications);
        let cache_path_clone = Arc::clone(&cache_path);
        let session_key_clone = Arc::clone(&session_key);
        let max_count = max_notifications;

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

        Self {
            notifications,
            cache_path,
            session_key,
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

        // Use busctl to monitor D-Bus for Notify calls
        // --user: Watch user session bus (not system bus)
        // --match: Filter for only Notify method calls
        let mut child = Command::new("busctl")
            .args(&[
                "monitor",
                "--user",
                "--match",
                "type=method_call,interface=org.freedesktop.Notifications,member=Notify",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // Suppress busctl stderr noise
            .spawn()?;

        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let reader = BufReader::new(stdout);

        // State machine for parsing busctl output
        let mut current_app_name = String::new();
        let mut current_summary = String::new();
        let mut current_body = String::new();
        let mut string_field_index = 0; // Track which STRING field we're at
        let mut in_notify_call = false; // Are we parsing a Notify call?

        // Process busctl output line by line
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();

            // busctl output format: look for Notify method call header
            if trimmed.contains("Member=Notify") {
                // Reset state for new notification
                current_app_name.clear();
                current_summary.clear();
                current_body.clear();
                string_field_index = 0;
                in_notify_call = true;
            } else if in_notify_call && trimmed.starts_with("STRING \"") {
                // Extract string value between quotes
                // Format: STRING "value here"
                if let Some(start) = trimmed.find('"') {
                    if let Some(end) = trimmed.rfind('"') {
                        if start < end {
                            let value = &trimmed[start + 1..end];

                            // Notify STRING parameters in order:
                            // 0: app_name - Application sending the notification
                            // 1: app_icon - Icon name or path (usually empty)
                            // 2: summary - Notification title
                            // 3: body - Notification body text
                            match string_field_index {
                                0 => current_app_name = value.to_string(),
                                2 => current_summary = value.to_string(),
                                3 => {
                                    current_body = value.to_string();
                                    in_notify_call = false; // Done parsing this call

                                    // We have all the data, create notification
                                    if !current_summary.is_empty() {
                                        let timestamp = SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs();

                                        let notification = Notification {
                                            app_name: if current_app_name.is_empty() {
                                                "System".to_string() // Fallback for empty app_name
                                            } else {
                                                current_app_name.clone()
                                            },
                                            summary: current_summary.clone(),
                                            body: current_body.clone(),
                                            timestamp,
                                        };

                                        log::info!(
                                            "Captured notification: {} - {}",
                                            notification.app_name,
                                            notification.summary
                                        );

                                        // Insert at front (newest first) and truncate if needed
                                        let mut notifs = notifications.lock().unwrap();
                                        notifs.insert(0, notification);

                                        if notifs.len() > max_count {
                                            notifs.truncate(max_count);
                                        }
                                        if let Err(error) = persist_cached_notifications(
                                            cache_path,
                                            session_key,
                                            &notifs,
                                        ) {
                                            log::warn!(
                                                "Failed to persist captured notification: {error}"
                                            );
                                        }
                                    }
                                }
                                _ => {} // Ignore other STRING fields (icon, etc.)
                            }
                            string_field_index += 1;
                        }
                    }
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
}

fn notification_cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(CACHE_DIRECTORY)
        .join(CACHE_FILENAME)
}

fn notification_session_key() -> String {
    let session = std::env::var("XDG_SESSION_ID").unwrap_or_else(|_| "unknown".to_string());
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let server_owner = notification_server_owner().unwrap_or_else(|| "unknown".to_string());
    format!("{boot}:{session}:{server_owner}")
}

fn notification_server_owner() -> Option<String> {
    let output = std::process::Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            "s",
            "org.freedesktop.Notifications",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_busctl_owner(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

fn parse_busctl_owner(output: &str) -> Option<String> {
    let (_, owner) = output.split_once('"')?;
    let (owner, _) = owner.split_once('"')?;
    (!owner.is_empty()).then(|| owner.to_string())
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
    if cache.session_key != session_key {
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
        Notification, load_cached_notifications, parse_busctl_owner, persist_cached_notifications,
    };

    fn cache_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cosmic-widget-notification-test-{}-{name}.json",
            std::process::id()
        ))
    }

    fn notification(summary: &str, timestamp: u64) -> Notification {
        Notification {
            app_name: "Test".to_string(),
            summary: summary.to_string(),
            body: "Body".to_string(),
            timestamp,
        }
    }

    #[test]
    fn restores_notifications_from_the_current_session() {
        let path = cache_path("restore");
        let expected = vec![notification("Newest", 20), notification("Older", 10)];
        persist_cached_notifications(&path, "boot:session", &expected).unwrap();

        assert_eq!(
            load_cached_notifications(&path, "boot:session", 5).unwrap(),
            expected
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ignores_notifications_from_an_old_session_and_honors_the_limit() {
        let path = cache_path("session");
        let cached = vec![notification("One", 3), notification("Two", 2)];
        persist_cached_notifications(&path, "old-session", &cached).unwrap();
        assert!(
            load_cached_notifications(&path, "new-session", 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            load_cached_notifications(&path, "old-session", 1).unwrap(),
            vec![cached[0].clone()]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn parses_the_notification_daemon_unique_owner() {
        assert_eq!(
            parse_busctl_owner("s \":1.42\"\n").as_deref(),
            Some(":1.42")
        );
        assert_eq!(parse_busctl_owner(""), None);
    }
}
