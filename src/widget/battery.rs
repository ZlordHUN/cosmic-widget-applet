// SPDX-License-Identifier: MPL-2.0

//! # Battery Monitoring Module (External Devices)
//!
//! This module monitors battery levels for external peripherals like wireless mice,
//! keyboards, and headsets. The Audeze Maxwell and supported Logitech devices
//! use native Linux interfaces; other proprietary devices use established CLI
//! backends.
//!
//! ## Supported Tools
//!
//! - **Native HID**: Audeze Maxwell and Logitech Bolt battery/charging state
//! - **Linux power_supply**: Logitech devices exposed by the kernel HID++ driver
//! - **Solaar**: Fallback for unsupported Logitech receivers and devices
//! - **HeadsetControl**: Remaining supported gaming headsets
//!
//! ## Data Flow
//!
//! ```text
//! ┌─────────────────┐    ┌──────────────────┐    ┌───────────────────┐
//! │  Background     │    │                  │    │                   │
//! │  Thread         │───►│  Arc<Mutex>      │───►│  Main Thread      │
//! │  (query tools)  │    │  (shared state)  │    │  (reads devices)  │
//! └─────────────────┘    └──────────────────┘    └───────────────────┘
//! ```
//!
//! ## Architecture
//!
//! The monitor uses a background thread for native and external device queries:
//!
//! 1. **Startup**: Load cached device names for instant display
//! 2. **First update**: Immediately query tools in background thread
//! 3. **Native updates**: Query Maxwell and Logitech devices every 5 seconds
//! 4. **External updates**: Request Solaar/HeadsetControl updates every 30 seconds
//!
//! ## Parsing Strategies
//!
//! - **Solaar JSON**: Preferred, uses `solaar show --json`
//! - **Solaar text**: Fallback, parses `solaar show` plain text output
//! - **HeadsetControl**: Uses `headsetcontrol -b -o json`
//!
//! ## Error Handling
//!
//! All external tool failures are silently ignored to maintain stability:
//! - Tool not installed → empty device list
//! - Parse failure → keep previous snapshot
//! - Device disconnected → device shows as not connected

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "battery/audeze_maxwell.rs"]
mod audeze_maxwell;
#[path = "battery/logitech.rs"]
mod logitech;

const MAXWELL_DEVICE_NAME: &str = "Audeze Maxwell";
const MAXWELL_CONNECTION_SETTLE_DELAY: Duration = Duration::from_secs(2);

// ============================================================================
// Battery Device Struct
// ============================================================================

/// Information about a single peripheral device's battery state.
///
/// Represents battery data from Logitech devices (via Solaar) or gaming
/// headsets (via HeadsetControl).
///
/// # Fields
///
/// - `name`: Device product name (e.g., "G309 LIGHTSPEED", "Arctis Nova 7")
/// - `level`: Battery percentage 0-100, None if unavailable
/// - `status`: Text status like "discharging", "charging", "good"
/// - `kind`: Device type - "mouse", "keyboard", "headset"
/// - `codename`: Short device codename for deduplication (e.g., "MX MCHNCL M")
/// - `is_loading`: True while waiting for first real data (showing cached)
/// - `is_connected`: False if device is paired but powered off/out of range
#[derive(Debug, Clone, PartialEq)]
pub struct BatteryDevice {
    /// Device product name from Solaar/HeadsetControl
    pub name: String,
    /// Battery level in percent (0-100) if available
    pub level: Option<u8>,
    /// Textual status (e.g. "discharging", "charging", "good")
    pub status: Option<String>,
    /// Device kind (e.g. "mouse", "keyboard", "headset")
    pub kind: Option<String>,
    /// Device codename for deduplication (Logitech devices may appear multiple times)
    pub codename: Option<String>,
    /// True if showing cached data while loading real data
    pub is_loading: bool,
    /// True if device is currently connected and responding
    pub is_connected: bool,
}

// ============================================================================
// Battery Monitor Struct
// ============================================================================

/// Monitors battery levels for external peripherals via CLI tools.
///
/// Uses Solaar (Logitech devices) and HeadsetControl (gaming headsets) to
/// query battery status. All queries run in a background thread to avoid
/// blocking the main render loop.
///
/// # Threading Model
///
/// - `devices`: Shared state protected by Arc<Mutex>
/// - `update_requested`: Flag to trigger background refresh
/// - Background thread polls flag every 5 seconds
/// - Main thread calls `update()` every 30 seconds to set flag
///
/// # Caching
///
/// Device names and types are cached to disk so the widget can show
/// meaningful device names immediately on startup, even before Solaar
/// has time to respond.
pub struct BatteryMonitor {
    /// Shared device list, updated by background thread
    devices: Arc<Mutex<Vec<BatteryDevice>>>,
    /// Last time `update()` was called (for rate limiting)
    last_update: Instant,
    /// Minimum interval between requesting Solaar updates (30 seconds)
    refresh_interval: Duration,
    /// Flag to signal background thread that an update is needed
    update_requested: Arc<Mutex<bool>>,
}

impl BatteryMonitor {
    /// Create a new battery monitor with background polling thread.
    ///
    /// # Initialization Steps
    ///
    /// 1. Load cached device info from disk (shows instantly)
    /// 2. Set `last_update` to 31 seconds ago to trigger immediate first update
    /// 3. Spawn background thread for tool queries
    /// 4. Background thread immediately queries Solaar/HeadsetControl
    /// 5. Cache updated on first successful query
    ///
    /// # Background Thread Behavior
    ///
    /// - Sleeps for 5 seconds between checks
    /// - Only queries tools when `update_requested` flag is set
    /// - On error, keeps previous device snapshot
    pub fn new() -> Self {
        // Initialize with 31 seconds ago to force immediate first update
        let last_update = Instant::now() - Duration::from_secs(31);

        // Load cached battery devices to show immediately
        // This provides instant display while real data loads
        let cache = super::cache::WidgetCache::load();
        let cached_devices: Vec<BatteryDevice> = cache
            .battery_devices
            .iter()
            .map(|d| BatteryDevice {
                name: d.name.clone(),
                level: None, // No cached level, will show "loading"
                status: None,
                kind: d.kind.clone(),
                codename: None,
                is_loading: true, // Mark as loading until real data arrives
                is_connected: false,
            })
            .collect();

        let devices = Arc::new(Mutex::new(cached_devices));
        let update_requested = Arc::new(Mutex::new(true)); // Request initial update immediately

        // Spawn background thread for battery updates
        // This avoids blocking the main render loop on slow CLI tools
        let devices_clone = Arc::clone(&devices);
        let update_requested_clone = Arc::clone(&update_requested);

        std::thread::spawn(move || {
            let mut is_first_update = true;
            let mut native_maxwell_authoritative = false;
            let mut native_maxwell = None;
            let mut logitech_monitor = logitech::Monitor::new();
            let mut native_logitech = query_native_logitech(&mut logitech_monitor);

            if !native_logitech.is_empty() {
                log::info!(
                    "Using native monitoring for {} Logitech device(s)",
                    native_logitech.len()
                );
                merge_native_logitech(
                    &mut devices_clone.lock().unwrap(),
                    &native_logitech,
                );
            }

            match query_native_maxwell(false) {
                Ok(state) => {
                    if state.is_some() {
                        log::info!("Using native HID monitoring for Audeze Maxwell");
                    }
                    native_maxwell_authoritative = true;
                    native_maxwell = state;
                    merge_native_maxwell(
                        &mut devices_clone.lock().unwrap(),
                        native_maxwell.clone(),
                    );
                }
                Err(error) => {
                    log::warn!("Native Audeze Maxwell query failed: {error}");
                }
            }

            // Perform immediate first update on startup
            match query_external_devices() {
                Ok(mut new_devices) => {
                    if native_maxwell_authoritative {
                        merge_native_maxwell(&mut new_devices, native_maxwell.clone());
                    }
                    merge_native_logitech(&mut new_devices, &native_logitech);
                    *devices_clone.lock().unwrap() = new_devices.clone();

                    // Update cache after first successful update
                    if is_first_update && !new_devices.is_empty() {
                        let mut cache = super::cache::WidgetCache::load();
                        cache.update_battery_devices(&new_devices);
                        is_first_update = false;
                    }
                }
                Err(_) => {
                    // On error, keep cached data - tool may not be installed
                }
            }

            // Clear the initial update request flag
            *update_requested_clone.lock().unwrap() = false;

            // Main background loop - check for update requests every 5 seconds
            loop {
                std::thread::sleep(Duration::from_secs(5));

                let was_maxwell_connected = native_maxwell
                    .as_ref()
                    .is_some_and(|device| device.is_connected);
                if let Ok(state) = query_native_maxwell(was_maxwell_connected) {
                    native_maxwell_authoritative = true;
                    native_maxwell = state;
                    merge_native_maxwell(
                        &mut devices_clone.lock().unwrap(),
                        native_maxwell.clone(),
                    );
                }

                native_logitech = query_native_logitech(&mut logitech_monitor);
                merge_native_logitech(
                    &mut devices_clone.lock().unwrap(),
                    &native_logitech,
                );

                // Check if update is needed (atomic check-and-clear)
                let requested = {
                    let mut req = update_requested_clone.lock().unwrap();
                    if *req {
                        *req = false;
                        true
                    } else {
                        false
                    }
                };

                if requested {
                    match query_external_devices() {
                        Ok(mut new_devices) => {
                            if native_maxwell_authoritative {
                                merge_native_maxwell(&mut new_devices, native_maxwell.clone());
                            }
                            merge_native_logitech(&mut new_devices, &native_logitech);
                            *devices_clone.lock().unwrap() = new_devices.clone();

                            // Update cache after first successful update
                            if is_first_update && !new_devices.is_empty() {
                                let mut cache = super::cache::WidgetCache::load();
                                cache.update_battery_devices(&new_devices);
                                is_first_update = false;
                            }
                        }
                        Err(_) => {
                            // On error, keep previous data
                        }
                    }
                }
            }
        });

        Self {
            devices,
            last_update,
            refresh_interval: Duration::from_secs(30),
            update_requested,
        }
    }

    /// Get current snapshot of battery devices.
    ///
    /// Returns a clone of the device list from the last successful update.
    /// Thread-safe via internal mutex.
    pub fn devices(&self) -> Vec<BatteryDevice> {
        self.devices.lock().unwrap().clone()
    }

    /// Request a battery update if refresh interval has elapsed.
    ///
    /// This is rate-limited to once per 30 seconds. The actual update runs
    /// in the background thread - this just sets a flag.
    ///
    /// # Rate Limiting
    ///
    /// Battery queries are expensive (spawn external processes), so we
    /// limit updates to every 30 seconds. Battery levels don't change
    /// fast enough to need more frequent polling.
    pub fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_update) < self.refresh_interval {
            return;
        }

        self.last_update = now;

        // Request background thread to update (non-blocking)
        *self.update_requested.lock().unwrap() = true;
    }
}

// ============================================================================
// Native Device Queries
// ============================================================================

fn query_native_maxwell(was_connected: bool) -> Result<Option<BatteryDevice>, String> {
    let mut state = audeze_maxwell::query()?;
    let is_connected = state
        .as_ref()
        .is_some_and(|state| state.connected);

    if is_connected != was_connected {
        std::thread::sleep(MAXWELL_CONNECTION_SETTLE_DELAY);
        state = audeze_maxwell::query()?;
    }

    Ok(state.map(|state| BatteryDevice {
        name: MAXWELL_DEVICE_NAME.to_string(),
        level: state.level,
        status: state.connected.then(|| {
            if state.charging {
                "charging".to_string()
            } else {
                "discharging".to_string()
            }
        }),
        kind: Some("headset".to_string()),
        codename: None,
        is_loading: false,
        is_connected: state.connected,
    }))
}

fn merge_native_maxwell(
    devices: &mut Vec<BatteryDevice>,
    native_maxwell: Option<BatteryDevice>,
) {
    if let Some(device) = native_maxwell {
        replace_device_in_place(devices, device);
    } else {
        devices.retain(|device| !device.name.eq_ignore_ascii_case(MAXWELL_DEVICE_NAME));
    }
}

fn query_native_logitech(monitor: &mut logitech::Monitor) -> Vec<BatteryDevice> {
    monitor
        .query()
        .into_iter()
        .map(|state| BatteryDevice {
            name: state.name,
            level: state.level,
            status: state.status,
            kind: state.kind,
            codename: None,
            is_loading: false,
            is_connected: state.connected,
        })
        .collect()
}

fn merge_native_logitech(devices: &mut Vec<BatteryDevice>, native_devices: &[BatteryDevice]) {
    for native in native_devices {
        replace_device_in_place(devices, native.clone());
    }
}

fn replace_device_in_place(devices: &mut Vec<BatteryDevice>, replacement: BatteryDevice) {
    if let Some(existing) = devices
        .iter_mut()
        .find(|device| device.name.eq_ignore_ascii_case(&replacement.name))
    {
        *existing = replacement;
    } else {
        devices.push(replacement);
    }
}

// ============================================================================
// External Tool Query Functions
// ============================================================================

/// Query Solaar and HeadsetControl for battery information.
///
/// Aggregates devices from multiple sources:
/// 1. Solaar JSON output (preferred for Logitech devices)
/// 2. Solaar text output (fallback)
/// 3. HeadsetControl JSON output (gaming headsets)
///
/// # Returns
///
/// Combined list of all discovered devices, or empty list on failure.
fn query_external_devices() -> Result<Vec<BatteryDevice>, String> {
    let mut all_devices = Vec::new();

    // ========================================================================
    // Solaar Query (Logitech devices)
    // ========================================================================

    // Try JSON output if available (newer Solaar versions)
    // JSON is more reliable and structured than text output
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
    // Older Solaar versions don't support JSON output
    // Note: We don't check exit status here because Solaar may output valid
    // device data to stdout before encountering an error (exit code 1).
    // This happens when there's a Python exception while querying certain
    // device settings - the battery data is still valid.
    if all_devices.is_empty() {
        if let Ok(output) = Command::new("solaar").arg("show").output() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                if !text.is_empty() {
                    all_devices.extend(parse_solaar_text(&text));
                }
            }
        }
    }

    // ========================================================================
    // HeadsetControl Query (gaming headsets)
    // ========================================================================

    // HeadsetControl supports many gaming headset brands
    // -b: battery only, -o json: JSON output format
    if let Ok(output) = Command::new("headsetcontrol")
        .arg("-b")
        .arg("-o")
        .arg("json")
        .output()
    {
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

// ============================================================================
// Solaar JSON Parsing
// ============================================================================

/// Parse Solaar's JSON output format.
///
/// Solaar JSON can be either:
/// - Array of device objects
/// - Object keyed by device ID
///
/// We use `serde_json::Value` for flexible parsing without strict schema.
fn parse_solaar_json(text: &str) -> Result<Vec<BatteryDevice>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;

    let mut devices = Vec::new();

    match value {
        // Array format: [{device1}, {device2}, ...]
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(dev) = extract_device_from_json(&item) {
                    devices.push(dev);
                }
            }
        }
        // Object format: {"id1": {device1}, "id2": {device2}, ...}
        serde_json::Value::Object(map) => {
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

/// Extract a BatteryDevice from a Solaar JSON device object.
///
/// Looks for fields:
/// - `name`: Device product name
/// - `kind`: Device type (mouse, keyboard)
/// - `battery` or `batteries`: Battery level and status
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

    // Heuristic: some structures nest battery info under `battery` or `batteries`
    let (level, status) = if let Some(batt) = value.get("battery") {
        extract_battery_fields(batt)
    } else if let Some(batts) = value.get("batteries") {
        // Multiple batteries - take the first one
        if let Some(first) = batts.as_array().and_then(|a| a.first()) {
            extract_battery_fields(first)
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    Some(BatteryDevice {
        name,
        level,
        status,
        kind,
        codename: None,
        is_loading: false,
        is_connected: true,
    })
}

/// Extract battery level and status from a JSON battery object.
///
/// Looks for:
/// - `level`: Numeric percentage (0-100)
/// - `status` or `state`: Text status like "discharging"
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

// ============================================================================
// HeadsetControl JSON Parsing
// ============================================================================

/// Parse HeadsetControl's JSON output format.
///
/// HeadsetControl output structure:
/// ```json
/// {
///   "devices": [
///     {
///       "status": "success",
///       "device": "Arctis Nova 7",
///       "battery": {"status": "BATTERY_AVAILABLE", "level": 85}
///     }
///   ]
/// }
/// ```
fn parse_headsetcontrol_json(text: &str) -> Result<Vec<BatteryDevice>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;

    let mut devices = Vec::new();

    if let Some(device_list) = value.get("devices").and_then(|v| v.as_array()) {
        for device_obj in device_list {
            // Check if device query was successful
            if let Some(status) = device_obj.get("status").and_then(|v| v.as_str()) {
                if status != "success" {
                    continue; // Skip failed device queries
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
                let level = battery.get("level").and_then(|v| v.as_i64()).and_then(|v| {
                    if v >= 0 && v <= 100 {
                        u8::try_from(v).ok()
                    } else {
                        None // -1 means reading failed, treat as no level
                    }
                });

                // HeadsetControl API 1.4 distinguishes an available battery
                // from one that is actively charging.
                let status_text = match (status, level) {
                    (Some("BATTERY_CHARGING"), Some(_)) => Some("charging".to_string()),
                    (Some("BATTERY_AVAILABLE"), Some(_)) => Some("discharging".to_string()),
                    _ => None,
                };

                (level, status_text)
            } else {
                (None, None)
            };

            // Device is connected if HeadsetControl successfully queried it
            // is_loading should be false - we're not "loading", we just couldn't read the battery
            let is_connected = true;
            let is_loading = false;

            devices.push(BatteryDevice {
                name,
                level,
                status: battery_status,
                kind,
                codename: None,
                is_loading,
                is_connected,
            });
        }
    }

    Ok(devices)
}

// ============================================================================
// Solaar Text Parsing (Fallback)
// ============================================================================

/// Parse `solaar show` plain-text output (fallback for older versions).
///
/// Example output format:
/// ```text
/// Unifying Receiver
///   Device path  : /dev/hidraw0
///   ...
///   1: G309 LIGHTSPEED
///         Device path  : /dev/hidraw1
///         ...
///         Battery: 90% (discharging)
/// ```
///
/// # Parsing Strategy
///
/// 1. Look for device names starting with "N: Device Name" pattern
/// 2. Track current device context
/// 3. Extract "Kind:", "Codename:" and "Battery:" fields within device section
/// 4. Avoid duplicates (same device can appear multiple times with different names)
fn parse_solaar_text(text: &str) -> Vec<BatteryDevice> {
    let mut devices = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_kind: Option<String> = None;
    let mut current_codename: Option<String> = None;
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
                // Check if it's a number (device identifier like "1", "2", etc.)
                if before_colon.chars().all(|c| c.is_ascii_digit()) {
                    let after_colon = &line[colon_pos + 1..].trim();
                    current_name = Some(after_colon.to_string());
                    current_kind = None;
                    current_codename = None;
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
        if trimmed.starts_with("Kind:") {
            if let Some(kind_value) = trimmed.strip_prefix("Kind:") {
                current_kind = Some(kind_value.trim().to_string());
            }
        }

        // Look for codename (e.g., "Codename: MX MCHNCL M")
        // This helps deduplicate devices that appear multiple times with different names
        if trimmed.starts_with("Codename") {
            if let Some(codename_value) = trimmed.split(':').nth(1) {
                current_codename = Some(codename_value.trim().to_string());
            }
        }

        // Look for a battery line under the current device
        // Format: "Battery: 90% (discharging)" or "Battery: unknown (device is offline)."
        if trimmed.starts_with("Battery:") {
            if let Some(rest) = trimmed.strip_prefix("Battery:") {
                let (level, status) = parse_battery_line(rest.trim());
                // Add device if we have a name (even without battery level for offline devices)
                if let Some(name) = current_name.clone() {
                    // Device is connected if it has a battery level
                    let is_connected = level.is_some();

                    // Check for duplicates by name or codename (same device can appear multiple times)
                    // Logitech devices paired to multiple slots show up with different names but same codename
                    let existing_idx = devices.iter().position(|d: &BatteryDevice| {
                        d.name == name
                            || (current_codename.is_some() && current_codename == d.codename)
                    });

                    if let Some(idx) = existing_idx {
                        // If existing device is disconnected but this one is connected, replace it
                        if !devices[idx].is_connected && is_connected {
                            devices[idx] = BatteryDevice {
                                name,
                                level,
                                status,
                                kind: current_kind.clone(),
                                codename: current_codename.clone(),
                                is_loading: false,
                                is_connected,
                            };
                        }
                    } else {
                        // New device, add it
                        devices.push(BatteryDevice {
                            name,
                            level,
                            status,
                            kind: current_kind.clone(),
                            codename: current_codename.clone(),
                            is_loading: false,
                            is_connected,
                        });
                    }
                }
            }
        }

        // Detect when we're leaving a device section (new receiver or device)
        if !line.starts_with("  ")
            || (line.starts_with("  ") && !line.starts_with("    ") && line.contains("Receiver"))
        {
            if !trimmed.is_empty()
                && !trimmed.starts_with("Has")
                && !trimmed.starts_with("Notifications")
            {
                in_device_section = false;
            }
        }
    }

    devices
}

/// Parse a battery line from Solaar text output.
///
/// # Example Formats
///
/// - `"90% (discharging)"` → (Some(90), Some("discharging"))
/// - `"55%, recharging."` → (Some(55), Some("recharging"))
/// - `"charged"` → (None, Some("charged"))
/// - `"good"` → (None, Some("good"))
fn parse_battery_line(text: &str) -> (Option<u8>, Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::{
        BatteryDevice, merge_native_logitech, merge_native_maxwell, parse_headsetcontrol_json,
    };

    fn headsetcontrol_output(status: &str, level: i64) -> String {
        format!(
            r#"{{
                "devices": [{{
                    "status": "success",
                    "device": "Test Headset",
                    "battery": {{"status": "{status}", "level": {level}}}
                }}]
            }}"#
        )
    }

    #[test]
    fn maps_headsetcontrol_charging_status() {
        let devices =
            parse_headsetcontrol_json(&headsetcontrol_output("BATTERY_CHARGING", 98)).unwrap();

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].level, Some(98));
        assert_eq!(devices[0].status.as_deref(), Some("charging"));
    }

    #[test]
    fn maps_headsetcontrol_available_status_to_discharging() {
        let devices =
            parse_headsetcontrol_json(&headsetcontrol_output("BATTERY_AVAILABLE", 73)).unwrap();

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].level, Some(73));
        assert_eq!(devices[0].status.as_deref(), Some("discharging"));
    }

    #[test]
    fn native_maxwell_replaces_the_cli_copy() {
        let mut devices = vec![
            BatteryDevice {
                name: "G309 LIGHTSPEED".to_string(),
                level: Some(100),
                status: Some("discharging".to_string()),
                kind: Some("mouse".to_string()),
                codename: None,
                is_loading: false,
                is_connected: true,
            },
            BatteryDevice {
                name: "Audeze Maxwell".to_string(),
                level: Some(25),
                status: Some("discharging".to_string()),
                kind: Some("headset".to_string()),
                codename: None,
                is_loading: false,
                is_connected: true,
            },
        ];
        let native = BatteryDevice {
            level: Some(96),
            status: Some("charging".to_string()),
            ..devices[1].clone()
        };

        merge_native_maxwell(&mut devices, Some(native));

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].level, Some(96));
        assert_eq!(devices[1].status.as_deref(), Some("charging"));
        assert_eq!(devices[0].name, "G309 LIGHTSPEED");
    }

    #[test]
    fn native_logitech_replaces_matching_solaar_devices_only() {
        let mut devices = vec![
            BatteryDevice {
                name: "G309 LIGHTSPEED".to_string(),
                level: Some(50),
                status: Some("discharging".to_string()),
                kind: Some("mouse".to_string()),
                codename: None,
                is_loading: false,
                is_connected: true,
            },
            BatteryDevice {
                name: "Unsupported Logitech device".to_string(),
                level: Some(75),
                status: Some("discharging".to_string()),
                kind: Some("mouse".to_string()),
                codename: None,
                is_loading: false,
                is_connected: true,
            },
        ];
        let native = BatteryDevice {
            level: Some(100),
            ..devices[0].clone()
        };

        merge_native_logitech(&mut devices, &[native]);

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "G309 LIGHTSPEED");
        assert_eq!(devices[1].name, "Unsupported Logitech device");
        assert_eq!(
            devices
                .iter()
                .find(|device| device.name == "G309 LIGHTSPEED")
                .and_then(|device| device.level),
            Some(100)
        );
        assert!(
            devices
                .iter()
                .any(|device| device.name == "Unsupported Logitech device")
        );
    }
}
