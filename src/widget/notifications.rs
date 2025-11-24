// SPDX-License-Identifier: MPL-2.0

//! Notification monitoring via D-Bus

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Notification {
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub timestamp: u64,
}

pub struct NotificationMonitor {
    notifications: Arc<Mutex<Vec<Notification>>>,
    max_notifications: usize,
}

impl NotificationMonitor {
    pub fn new(max_notifications: usize) -> Self {
        let notifications = Arc::new(Mutex::new(Vec::new()));
        
        // Spawn background thread to monitor D-Bus
        let notifications_clone = Arc::clone(&notifications);
        let max_count = max_notifications;
        
        std::thread::spawn(move || {
            if let Err(e) = Self::monitor_notifications(notifications_clone, max_count) {
                log::error!("Notification monitoring error: {}", e);
            }
        });
        
        Self {
            notifications,
            max_notifications,
        }
    }
    
    fn monitor_notifications(
        notifications: Arc<Mutex<Vec<Notification>>>,
        max_count: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::process::{Command, Stdio};
        use std::io::{BufRead, BufReader};
        
        log::info!("Starting notification monitor via busctl");
        
        // Use busctl to monitor D-Bus for Notify calls
        let mut child = Command::new("busctl")
            .args(&[
                "monitor",
                "--user",
                "--match",
                "type=method_call,interface=org.freedesktop.Notifications,member=Notify",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let reader = BufReader::new(stdout);
        
        let mut current_app_name = String::new();
        let mut current_summary = String::new();
        let mut current_body = String::new();
        let mut string_field_index = 0;
        let mut in_notify_call = false;
        
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            
            // busctl output format: look for Notify method call
            if trimmed.contains("Member=Notify") {
                // Reset for new notification
                current_app_name.clear();
                current_summary.clear();
                current_body.clear();
                string_field_index = 0;
                in_notify_call = true;
            } else if in_notify_call && trimmed.starts_with("STRING \"") {
                // Extract string value between quotes
                if let Some(start) = trimmed.find('"') {
                    if let Some(end) = trimmed.rfind('"') {
                        if start < end {
                            let value = &trimmed[start + 1..end];
                            
                            // Notify STRING parameters in order:
                            // 0: app_name, 1: app_icon (empty), 2: summary, 3: body
                            match string_field_index {
                                0 => current_app_name = value.to_string(),
                                2 => current_summary = value.to_string(),
                                3 => {
                                    current_body = value.to_string();
                                    in_notify_call = false;
                                    
                                    // We have all the data, create notification
                                    if !current_summary.is_empty() {
                                        let timestamp = SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs();
                                        
                                        let notification = Notification {
                                            app_name: if current_app_name.is_empty() { 
                                                "System".to_string() 
                                            } else { 
                                                current_app_name.clone() 
                                            },
                                            summary: current_summary.clone(),
                                            body: current_body.clone(),
                                            timestamp,
                                        };
                                        
                                        log::info!("Captured notification: {} - {}", notification.app_name, notification.summary);
                                        
                                        let mut notifs = notifications.lock().unwrap();
                                        notifs.insert(0, notification);
                                        
                                        if notifs.len() > max_count {
                                            notifs.truncate(max_count);
                                        }
                                    }
                                }
                                _ => {}
                            }
                            string_field_index += 1;
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
    
    pub fn get_notifications(&self) -> Vec<Notification> {
        self.notifications.lock().unwrap().clone()
    }
    
    pub fn clear(&self) {
        let mut notifs = self.notifications.lock().unwrap();
        notifs.clear();
        log::info!("Cleared all notifications");
    }
    
    pub fn clear_app(&self, app_name: &str) {
        let mut notifs = self.notifications.lock().unwrap();
        notifs.retain(|n| n.app_name != app_name);
        log::info!("Cleared notifications for app: {}", app_name);
    }
    
    /// Remove a specific notification by app_name and timestamp
    pub fn remove_notification(&self, app_name: &str, timestamp: u64) {
        let mut notifs = self.notifications.lock().unwrap();
        notifs.retain(|n| !(n.app_name == app_name && n.timestamp == timestamp));
        log::info!("Removed notification: {} at {}", app_name, timestamp);
    }
}
