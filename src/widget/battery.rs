// SPDX-License-Identifier: MPL-2.0

//! Battery monitoring via Solaar CLI
//!
//! This module provides a minimal wrapper that shells out to the
//! `solaar` command to obtain battery information for Logitech
//! devices. It is intentionally conservative: if Solaar is not
//! installed or returns unexpected output, we simply return an
//! empty list.

use std::process::Command;
use std::time::{Duration, Instant};

/// Representation of a single device's battery state
#[derive(Debug, Clone)]
pub struct BatteryDevice {
    pub name: String,
    /// Battery level in percent (0-100) if available
    pub level: Option<u8>,
    /// Textual status (e.g. "discharging", "charging", "good")
    pub status: Option<String>,
    /// Device kind (e.g. "mouse", "keyboard", "headset")
    pub kind: Option<String>,
    /// True if showing cached data while loading
    pub is_loading: bool,
    /// True if device is currently connected (Device path != None)
    pub is_connected: bool,
}

/// Simple battery monitor that periodically queries Solaar
#[derive(Debug, Clone)]
pub struct BatteryMonitor {
    devices: Vec<BatteryDevice>,
    last_update: Option<Instant>,
    /// Minimum interval between Solaar invocations
    refresh_interval: Duration,
    is_first_update: bool,
}

impl BatteryMonitor {
    /// Create a new monitor with a sensible default refresh interval.
    pub fn new() -> Self {
        // Load cached battery devices to show immediately
        let cache = super::cache::WidgetCache::load();
        let devices: Vec<BatteryDevice> = cache
            .battery_devices
            .iter()
            .map(|d| BatteryDevice {
                name: d.name.clone(),
                level: None,
                status: None,
                kind: d.kind.clone(),
                is_loading: false,
                is_connected: false,
            })
            .collect();
            
        Self {
            devices,
            last_update: None,
            // Solaar does not need rapid polling; once every 30s is fine
            refresh_interval: Duration::from_secs(30),
            is_first_update: true,
        }
    }

    /// Current snapshot of devices (from the last successful update).
    pub fn devices(&self) -> &[BatteryDevice] {
        &self.devices
    }

    /// Try to refresh device information if the refresh interval has elapsed.
    ///
    /// This is intentionally best-effort: on any error, we keep the last
    /// successful snapshot and return without propagating failures.
    pub fn update(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_update {
            if now.duration_since(last) < self.refresh_interval {
                return;
            }
        }

        self.last_update = Some(now);

        match query_solaar() {
            Ok(devices) => {
                self.devices = devices;
                
                // Update cache after first successful update
                if self.is_first_update && !self.devices.is_empty() {
                    let mut cache = super::cache::WidgetCache::load();
                    cache.update_battery_devices(&self.devices);
                    self.is_first_update = false;
                }
            }
            Err(_err) => {
                // On error, keep previous data and do nothing
            }
        }
    }
}

/// Invoke the `solaar` CLI and parse battery information, plus HeadsetControl for headsets
fn query_solaar() -> Result<Vec<BatteryDevice>, String> {
    let mut all_devices = Vec::new();
    
    // Query Solaar for Logitech devices
    // Try JSON output if available (newer Solaar versions)
    if let Ok(output) = Command::new("solaar").arg("show").arg("--json").output() {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                if let Ok(devices) = parse_solaar_json(&text) {
                    all_devices.extend(devices);
                }
            }
        }
    }

    // Fallback: plain-text `solaar show` if JSON didn't give us devices
    if all_devices.is_empty() {
        if let Ok(output) = Command::new("solaar").arg("show").output() {
            if output.status.success() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    all_devices.extend(parse_solaar_text(&text));
                }
            }
        }
    }
    
    // Query HeadsetControl for headset devices
    if let Ok(output) = Command::new("headsetcontrol").arg("-b").arg("-o").arg("json").output() {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                if let Ok(headset_devices) = parse_headsetcontrol_json(&text) {
                    all_devices.extend(headset_devices);
                }
            }
        }
    }
    
    Ok(all_devices)
}

/// Parse a very small subset of Solaar's JSON output.
///
/// We avoid pulling in a full JSON dependency specific to Solaar's
/// schema by using `serde_json::Value` and walking only what we need.
fn parse_solaar_json(text: &str) -> Result<Vec<BatteryDevice>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;

    let mut devices = Vec::new();

    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(dev) = extract_device_from_json(&item) {
                    devices.push(dev);
                }
            }
        }
        serde_json::Value::Object(map) => {
            // Some Solaar versions may return an object keyed by device
            for (_key, item) in map {
                if let Some(dev) = extract_device_from_json(&item) {
                    devices.push(dev);
                }
            }
        }
        _ => {}
    }

    Ok(devices)
}

fn extract_device_from_json(value: &serde_json::Value) -> Option<BatteryDevice> {
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown device")
        .to_string();

    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Heuristic: some structures nest battery info under `battery` or `batteries`.
    let (level, status) = if let Some(batt) = value.get("battery") {
        extract_battery_fields(batt)
    } else if let Some(batts) = value.get("batteries") {
        if let Some(first) = batts.as_array().and_then(|a| a.first()) {
            extract_battery_fields(first)
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    Some(BatteryDevice { name, level, status, kind, is_loading: false, is_connected: true })
}

fn extract_battery_fields(value: &serde_json::Value) -> (Option<u8>, Option<String>) {
    let level = value
        .get("level")
        .and_then(|v| v.as_u64())
        .and_then(|v| u8::try_from(v).ok());

    let status = value
        .get("status")
        .or_else(|| value.get("state"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    (level, status)
}

/// Parse HeadsetControl JSON output
fn parse_headsetcontrol_json(text: &str) -> Result<Vec<BatteryDevice>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    
    let mut devices = Vec::new();
    
    if let Some(device_list) = value.get("devices").and_then(|v| v.as_array()) {
        for device_obj in device_list {
            // Check if device query was successful
            if let Some(status) = device_obj.get("status").and_then(|v| v.as_str()) {
                if status != "success" {
                    continue;
                }
            }
            
            // Extract device name
            let name = device_obj
                .get("device")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Headset")
                .to_string();
            
            // All headsets are kind "headset"
            let kind = Some("headset".to_string());
            
            // Extract battery information
            let (level, battery_status) = if let Some(battery) = device_obj.get("battery") {
                let status = battery.get("status").and_then(|v| v.as_str());
                let level = battery.get("level").and_then(|v| v.as_u64()).and_then(|v| u8::try_from(v).ok());
                
                let is_available = status == Some("BATTERY_AVAILABLE");
                let status_text = if is_available {
                    Some("available".to_string())
                } else {
                    Some("unavailable".to_string())
                };
                
                (level, status_text)
            } else {
                (None, None)
            };
            
            // HeadsetControl doesn't provide device path info, so we assume connected if we got data
            let is_connected = true;
            let is_loading = is_connected && level.is_none();
            
            devices.push(BatteryDevice {
                name,
                level,
                status: battery_status,
                kind,
                is_loading,
                is_connected,
            });
        }
    }
    
    Ok(devices)
}

/// Very small text parser for `solaar show` plain-text output.
///
/// This is intentionally forgiving and only looks for lines like:
///   "  Battery: 90% (discharging)"
/// and preceding indented device lines as the device name.
fn parse_solaar_text(text: &str) -> Vec<BatteryDevice> {
    let mut devices = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_kind: Option<String> = None;
    let mut in_device_section = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Look for device names that start with a number and colon (e.g., "1: G309 LIGHTSPEED")
        // These lines have minimal indentation (just a couple of spaces)
        if line.starts_with("  ") && !line.starts_with("    ") {
            if let Some(colon_pos) = line.find(':') {
                let before_colon = &line[..colon_pos].trim();
                // Check if it's a number (device identifier)
                if before_colon.chars().all(|c| c.is_ascii_digit()) {
                    let after_colon = &line[colon_pos + 1..].trim();
                    current_name = Some(after_colon.to_string());
                    current_kind = None;
                    in_device_section = true;
                    continue;
                }
            }
        }

        // Only process device properties if we're in a device section
        if !in_device_section {
            continue;
        }

        // Look for device kind (e.g., "Kind: mouse")
        // This appears in the detailed device info
        if trimmed.starts_with("Kind:") {
            if let Some(kind_value) = trimmed.strip_prefix("Kind:") {
                current_kind = Some(kind_value.trim().to_string());
            }
        }

        // Look for a battery line under the current device
        // This can appear either in features or at the end of device section
        if trimmed.starts_with("Battery:") {
            if let Some(rest) = trimmed.strip_prefix("Battery:") {
                let (level, status) = parse_battery_line(rest.trim());
                // Only add if we have both a device name and battery level
                if let (Some(name), Some(lvl)) = (current_name.clone(), level) {
                    // Check if we already have this device (avoid duplicates)
                    if !devices.iter().any(|d: &BatteryDevice| d.name == name) {
                        devices.push(BatteryDevice { 
                            name, 
                            level: Some(lvl), 
                            status,
                            kind: current_kind.clone(),
                            is_loading: false,
                            is_connected: true,
                        });
                    }
                }
            }
        }

        // Detect when we're leaving a device section (new receiver or device)
        if !line.starts_with("  ") || (line.starts_with("  ") && !line.starts_with("    ") && line.contains("Receiver")) {
            if !trimmed.is_empty() && !trimmed.starts_with("Has") && !trimmed.starts_with("Notifications") {
                in_device_section = false;
            }
        }
    }

    devices
}

fn parse_battery_line(text: &str) -> (Option<u8>, Option<String>) {
    // Example formats:
    //   "90% (discharging)"
    //   "55%, recharging."
    //   "charged" or "good"

    let mut level: Option<u8> = None;
    let mut status: Option<String> = None;

    // Try to find a percentage
    if let Some(percent_pos) = text.find('%') {
        let (num_part, rest) = text.split_at(percent_pos);
        if let Ok(val) = num_part.trim().parse::<u8>() {
            level = Some(val);
        }
        let rest = rest.trim_start_matches('%').trim();
        if !rest.is_empty() {
            // Trim commas, parentheses, and periods from the status string
            status = Some(rest.trim_matches([',', '(', ')', '.']).trim().to_string());
        }
    } else {
        // No explicit percentage; treat the whole text as status
        if !text.is_empty() {
            status = Some(text.to_string());
        }
    }

    (level, status)
}
