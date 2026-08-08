// SPDX-License-Identifier: MPL-2.0

//! # Battery Monitoring Module (External Devices)
//!
//! This module monitors battery levels for external peripherals like wireless mice,
//! keyboards, headsets, and controllers. The Audeze Maxwell, Razer Wolverine
//! V3 Pro 8K PC, and supported Logitech devices use native Linux interfaces; other
//! proprietary devices use established CLI backends.
//!
//! ## Supported Tools
//!
//! - **Native HID**: Audeze Maxwell, Razer Wolverine V3 Pro 8K PC, Corsair,
//!   HyperX, Logitech, Sony, SteelSeries, and Lenovo battery/charging state
//! - **Linux power_supply**: Logitech devices exposed by the kernel HID++ driver
//! - **Solaar**: Optional fallback for devices the native reader cannot reach
//! - **HeadsetControl**: Optional compatibility fallback for newer headsets
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
//! 4. **External fallbacks**: Refresh only backends serving non-native devices
//! 5. **External discovery**: Recheck inactive backends every five minutes
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
//! - Device disconnected → device is omitted from the visible snapshot

use std::collections::HashSet;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::cache::{CachedBatteryDevice, WidgetCache};

#[path = "battery/headsets.rs"]
mod headsets;
#[path = "battery/logitech.rs"]
mod logitech;
#[path = "battery/controllers/razer_wolverine_v3_pro_8k_pc.rs"]
mod razer_wolverine_v3_pro_8k_pc;

const MAXWELL_DEVICE_NAME: &str = "Audeze Maxwell";
const WOLVERINE_DEVICE_NAME: &str = "Razer Wolverine V3 Pro 8K PC";
const MAXWELL_CONNECTION_SETTLE_DELAY: Duration = Duration::from_secs(2);
const WOLVERINE_CONNECTION_SETTLE_DELAY: Duration = Duration::from_secs(1);
const INITIAL_NATIVE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const NATIVE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const INITIAL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const EXTERNAL_FALLBACK_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const EXTERNAL_DISCOVERY_INTERVAL: Duration = Duration::from_secs(5 * 60);

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
/// - `kind`: Device type - "mouse", "keyboard", "headset", "controller"
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

#[derive(Debug, Default)]
struct ExternalDeviceState {
    solaar_devices: Vec<BatteryDevice>,
    headsetcontrol_devices: Vec<BatteryDevice>,
    solaar_fallback_names: HashSet<String>,
    headsetcontrol_fallback_names: HashSet<String>,
    last_discovery: Option<Instant>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct BatteryCacheSnapshot {
    readings: Vec<CachedBatteryDevice>,
    connected_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExternalProbePlan {
    solaar: bool,
    headsetcontrol: bool,
}

impl ExternalProbePlan {
    #[cfg(test)]
    const DISCOVERY: Self = Self {
        solaar: true,
        headsetcontrol: true,
    };

    fn is_empty(self) -> bool {
        !self.solaar && !self.headsetcontrol
    }
}

// ============================================================================
// Battery Monitor Struct
// ============================================================================

/// Monitors battery levels for external peripherals.
///
/// Native readers handle supported Audeze, Razer, and Logitech devices. Solaar
/// and HeadsetControl are retained for discovery and unsupported-device
/// fallbacks.
///
/// # Threading Model
///
/// - `devices`: Shared state protected by Arc<Mutex>
/// - `update_requested`: Flag to trigger background refresh
/// - Background thread retries unresolved startup readings every second
/// - Native polling returns to a five-second interval after startup resolves
/// - Main thread calls `update()` every 30 seconds to refresh active fallbacks
///
/// # Caching
///
/// Device names, types, and last confirmed readings are cached so the widget
/// can show a clearly marked provisional value while live probes complete.
pub struct BatteryMonitor {
    /// Shared device list, updated by background thread
    devices: Arc<Mutex<Vec<BatteryDevice>>>,
    /// Last time `update()` was called (for rate limiting)
    last_update: Instant,
    /// Minimum interval between requesting external fallback updates
    refresh_interval: Duration,
    /// Flag to signal background thread that an update is needed
    update_requested: Arc<Mutex<bool>>,
    /// Whether the Solaar compatibility fallback may be queried
    solaar_enabled: Arc<AtomicBool>,
}

impl BatteryMonitor {
    /// Create a new battery monitor with background polling thread.
    ///
    /// # Initialization Steps
    ///
    /// 1. Load cached device info from disk (shows instantly)
    /// 2. Set `last_update` to 31 seconds ago to trigger immediate first update
    /// 3. Spawn background thread for tool queries
    /// 4. Background thread performs one external-device discovery pass
    /// 5. Cache updated on first successful query
    ///
    /// # Background Thread Behavior
    ///
    /// - Sleeps for 1 second while cached startup readings are unresolved
    /// - Returns to a 5-second native polling interval after resolution
    /// - Only queries active fallback backends during 30-second updates
    /// - Rechecks inactive external backends every five minutes
    /// - On error, keeps previous device snapshot
    pub fn new() -> Self {
        Self::new_with_solaar(true)
    }

    /// Create a monitor with an explicitly configured Solaar fallback.
    pub fn new_with_solaar(enable_solaar: bool) -> Self {
        // Initialize with 31 seconds ago to force immediate first update
        let last_update = Instant::now() - Duration::from_secs(31);

        // Cache provides provisional values only after live discovery confirms
        // that the corresponding device is connected.
        let mut cache = WidgetCache::load();
        if cache.normalize_battery_devices() {
            cache.save();
        }
        let cached_devices: Vec<BatteryDevice> = cache
            .battery_devices
            .iter()
            .map(|d| BatteryDevice {
                name: d.name.clone(),
                level: d.level,
                status: d.status.clone(),
                kind: d.kind.clone(),
                codename: None,
                is_loading: true, // Mark as loading until real data arrives
                is_connected: false,
            })
            .collect();
        let startup_devices = cache
            .last_connected_devices()
            .into_iter()
            .map(|device| BatteryDevice {
                name: device.name,
                level: device.level,
                status: device.status,
                kind: device.kind,
                codename: None,
                is_loading: true,
                is_connected: true,
            })
            .collect();

        let devices = Arc::new(Mutex::new(startup_devices));
        let update_requested = Arc::new(Mutex::new(true)); // Request initial update immediately
        let solaar_enabled = Arc::new(AtomicBool::new(enable_solaar));

        // Spawn background thread for battery updates
        // This avoids blocking the main render loop on slow CLI tools
        let devices_clone = Arc::clone(&devices);
        let update_requested_clone = Arc::clone(&update_requested);
        let solaar_enabled_clone = Arc::clone(&solaar_enabled);

        std::thread::spawn(move || {
            let initial_probe_started = Instant::now();
            let mut last_cache_snapshot = None;
            let mut native_maxwell_authoritative = false;
            let mut native_maxwell = None;
            let mut native_wolverine = None;
            let mut active_solaar = solaar_enabled_clone.load(Ordering::Relaxed);
            let mut logitech_monitor = logitech::Monitor::new();
            let mut native_logitech = query_native_logitech(&mut logitech_monitor);
            let mut headset_monitor = headsets::Monitor::new();
            let mut native_headsets = query_native_headsets(&mut headset_monitor);
            let mut native_headset_coverage = native_headsets.covered_names.clone();

            if !native_logitech.is_empty() {
                log::info!(
                    "Using native monitoring for {} Logitech device(s)",
                    native_logitech.len()
                );
            }
            if !native_headsets.states.is_empty() {
                log::info!(
                    "Using native monitoring for {} additional headset device(s)",
                    native_headsets.states.len()
                );
            }
            match query_native_maxwell(false) {
                Ok(state) => {
                    if state.is_some() {
                        log::info!("Using native HID monitoring for Audeze Maxwell");
                    }
                    native_maxwell_authoritative = true;
                    native_maxwell = state;
                }
                Err(error) => {
                    log::warn!("Native Audeze Maxwell query failed: {error}");
                }
            }

            match query_native_wolverine(false) {
                Ok(state) => {
                    if state.is_some() {
                        log::info!("Using native HID monitoring for Razer Wolverine V3 Pro 8K PC");
                    }
                    native_wolverine = state;
                }
                Err(error) => {
                    log::warn!("Native Razer Wolverine V3 Pro 8K PC query failed: {error}");
                }
            }

            // Probe both external backends once for devices that are not covered
            // by the native readers. Inactive backends are only rediscovered
            // periodically after this initial pass.
            let mut external_state = ExternalDeviceState::default();
            seed_cached_fallbacks(
                &mut external_state,
                &cached_devices,
                native_maxwell_authoritative,
                &native_logitech,
            );
            if !active_solaar {
                external_state.solaar_fallback_names.clear();
            }
            query_external_backends(
                &mut external_state,
                ExternalProbePlan {
                    solaar: active_solaar,
                    headsetcontrol: true,
                },
            );
            external_state.last_discovery = Some(Instant::now());
            reconcile_external_fallbacks(
                &mut external_state,
                native_maxwell_authoritative,
                &native_logitech,
            );
            reconcile_native_headset_fallbacks(&mut external_state, &native_headset_coverage);

            let mut new_devices = combined_battery_devices(
                &external_state,
                native_maxwell_authoritative,
                native_maxwell.clone(),
                &native_logitech,
                native_wolverine.clone(),
                &native_headsets.states,
                &native_headset_coverage,
            );
            prepare_detected_devices(
                &mut new_devices,
                &cached_devices,
                initial_probe_started.elapsed() < INITIAL_PROBE_TIMEOUT,
            );
            *devices_clone.lock().unwrap() = new_devices.clone();

            persist_battery_readings(&mut last_cache_snapshot, &new_devices);

            // Clear the initial update request flag
            *update_requested_clone.lock().unwrap() = false;

            // Retry unresolved cached devices quickly during startup, then use the
            // normal native polling interval. The timeout prevents a broken HID
            // permission or backend from causing permanent one-second polling.
            loop {
                let poll_interval = {
                    let mut devices = devices_clone.lock().unwrap();
                    let elapsed = initial_probe_started.elapsed();
                    expire_initial_readings(&mut devices, elapsed);
                    native_poll_interval(&devices, elapsed)
                };
                std::thread::sleep(poll_interval);

                let configured_solaar = solaar_enabled_clone.load(Ordering::Relaxed);
                if configured_solaar != active_solaar {
                    active_solaar = configured_solaar;
                    if active_solaar {
                        external_state.last_discovery = None;
                    } else {
                        external_state.solaar_devices.clear();
                        external_state.solaar_fallback_names.clear();
                    }
                    *update_requested_clone.lock().unwrap() = true;
                }

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

                let was_wolverine_connected = native_wolverine
                    .as_ref()
                    .is_some_and(|device| device.is_connected);
                if let Ok(state) = query_native_wolverine(was_wolverine_connected) {
                    native_wolverine = state;
                    merge_native_wolverine(
                        &mut devices_clone.lock().unwrap(),
                        native_wolverine.clone(),
                    );
                }

                native_logitech = query_native_logitech(&mut logitech_monitor);
                merge_native_logitech(&mut devices_clone.lock().unwrap(), &native_logitech);
                let previous_headset_coverage = native_headset_coverage.clone();
                native_headsets = query_native_headsets(&mut headset_monitor);
                native_headset_coverage = native_headsets.covered_names.clone();
                let mut headset_rows_to_replace = previous_headset_coverage;
                for name in &native_headset_coverage {
                    if !headset_rows_to_replace
                        .iter()
                        .any(|existing| existing.eq_ignore_ascii_case(name))
                    {
                        headset_rows_to_replace.push(name.clone());
                    }
                }
                merge_native_headsets(
                    &mut devices_clone.lock().unwrap(),
                    &native_headsets.states,
                    &headset_rows_to_replace,
                );
                prepare_detected_devices(
                    &mut devices_clone.lock().unwrap(),
                    &cached_devices,
                    initial_probe_started.elapsed() < INITIAL_PROBE_TIMEOUT,
                );
                reconcile_external_fallbacks(
                    &mut external_state,
                    native_maxwell_authoritative,
                    &native_logitech,
                );
                reconcile_native_headset_fallbacks(&mut external_state, &native_headset_coverage);

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
                    let discovery_due = external_state
                        .last_discovery
                        .is_none_or(|last| last.elapsed() >= EXTERNAL_DISCOVERY_INTERVAL);
                    let plan = external_probe_plan(
                        &external_state,
                        discovery_due,
                        native_maxwell_authoritative,
                        active_solaar,
                    );
                    if !plan.is_empty() {
                        query_external_backends(&mut external_state, plan);
                        if discovery_due {
                            external_state.last_discovery = Some(Instant::now());
                        }
                    }
                    reconcile_external_fallbacks(
                        &mut external_state,
                        native_maxwell_authoritative,
                        &native_logitech,
                    );
                    reconcile_native_headset_fallbacks(
                        &mut external_state,
                        &native_headset_coverage,
                    );

                    let mut new_devices = combined_battery_devices(
                        &external_state,
                        native_maxwell_authoritative,
                        native_maxwell.clone(),
                        &native_logitech,
                        native_wolverine.clone(),
                        &native_headsets.states,
                        &native_headset_coverage,
                    );
                    prepare_detected_devices(
                        &mut new_devices,
                        &cached_devices,
                        initial_probe_started.elapsed() < INITIAL_PROBE_TIMEOUT,
                    );
                    *devices_clone.lock().unwrap() = new_devices.clone();
                }

                let current_devices = devices_clone.lock().unwrap().clone();
                persist_battery_readings(&mut last_cache_snapshot, &current_devices);
            }
        });

        Self {
            devices,
            last_update,
            refresh_interval: EXTERNAL_FALLBACK_REFRESH_INTERVAL,
            update_requested,
            solaar_enabled,
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
    /// Active fallback queries are expensive because they spawn external
    /// processes, so they are limited to every 30 seconds. Native devices
    /// continue to update on their faster background interval.
    pub fn update(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_update) < self.refresh_interval {
            return;
        }

        self.last_update = now;

        // Request background thread to update (non-blocking)
        *self.update_requested.lock().unwrap() = true;
    }

    /// Enable or disable Solaar discovery without affecting native readers.
    pub fn set_solaar_enabled(&self, enabled: bool) {
        if self.solaar_enabled.swap(enabled, Ordering::Relaxed) != enabled {
            *self.update_requested.lock().unwrap() = true;
        }
    }
}

// ============================================================================
// Native Device Queries
// ============================================================================

fn query_native_maxwell(was_connected: bool) -> Result<Option<BatteryDevice>, String> {
    let mut state = headsets::query_audeze_maxwell()?;
    let is_connected = state.as_ref().is_some_and(|state| state.connected);

    if is_connected != was_connected {
        std::thread::sleep(MAXWELL_CONNECTION_SETTLE_DELAY);
        state = headsets::query_audeze_maxwell()?;
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

fn merge_native_maxwell(devices: &mut Vec<BatteryDevice>, native_maxwell: Option<BatteryDevice>) {
    if let Some(device) = native_maxwell.filter(|device| device.is_connected) {
        replace_device_in_place(devices, device);
    } else {
        devices.retain(|device| !device.name.eq_ignore_ascii_case(MAXWELL_DEVICE_NAME));
    }
}

fn query_native_wolverine(was_connected: bool) -> Result<Option<BatteryDevice>, String> {
    let mut state = razer_wolverine_v3_pro_8k_pc::query()?;
    let is_connected = state.as_ref().is_some_and(|state| state.connected);

    if was_connected && !is_connected && state.is_some() {
        std::thread::sleep(WOLVERINE_CONNECTION_SETTLE_DELAY);
        state = razer_wolverine_v3_pro_8k_pc::query()?;
    }

    Ok(state.map(|state| BatteryDevice {
        name: WOLVERINE_DEVICE_NAME.to_string(),
        level: state.level,
        status: state.connected.then(|| {
            if state.charging {
                "charging".to_string()
            } else {
                "discharging".to_string()
            }
        }),
        kind: Some("controller".to_string()),
        codename: None,
        is_loading: false,
        is_connected: state.connected,
    }))
}

fn merge_native_wolverine(
    devices: &mut Vec<BatteryDevice>,
    native_wolverine: Option<BatteryDevice>,
) {
    if let Some(device) =
        native_wolverine.filter(|device| device.is_connected && device.level.is_some())
    {
        replace_device_in_place(devices, device);
    } else {
        devices.retain(|device| !device.name.eq_ignore_ascii_case(WOLVERINE_DEVICE_NAME));
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

fn query_native_headsets(monitor: &mut headsets::Monitor) -> headsets::Snapshot {
    monitor.query()
}

fn native_headset_device(state: &headsets::BatteryState) -> BatteryDevice {
    BatteryDevice {
        name: state.name.clone(),
        level: state.level,
        status: state.status.clone(),
        kind: Some("headset".to_string()),
        codename: None,
        is_loading: false,
        is_connected: true,
    }
}

fn merge_native_headsets(
    devices: &mut Vec<BatteryDevice>,
    native_states: &[headsets::BatteryState],
    covered_names: &[String],
) {
    devices.retain(|device| {
        !covered_names
            .iter()
            .any(|name| same_native_headset_name(&device.name, name))
    });
    devices.extend(native_states.iter().map(native_headset_device));
}

fn same_native_headset_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
        || (left.to_ascii_lowercase().contains("logitech")
            || right.to_ascii_lowercase().contains("logitech"))
            && logitech::same_device_name(left, right)
}

pub(crate) fn same_battery_device_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right) || logitech::same_device_name(left, right)
}

pub(crate) fn battery_device_name_quality(name: &str) -> usize {
    logitech::device_name_quality(name)
}

pub(crate) fn is_valid_battery_device_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && !name
            .chars()
            .any(|character| character.is_control() || character == '\u{fffd}')
        && name.chars().any(char::is_alphanumeric)
}

fn merge_native_logitech(devices: &mut Vec<BatteryDevice>, native_devices: &[BatteryDevice]) {
    for native in native_devices {
        if !native.is_connected {
            devices.retain(|device| !logitech::same_device_name(&device.name, &native.name));
            continue;
        }

        let first_match = devices
            .iter()
            .position(|device| logitech::same_device_name(&device.name, &native.name));
        let mut replacement = native.clone();

        for existing in devices
            .iter()
            .filter(|device| logitech::same_device_name(&device.name, &native.name))
        {
            if battery_device_name_quality(&existing.name)
                > battery_device_name_quality(&replacement.name)
            {
                replacement.name.clone_from(&existing.name);
            }
        }

        devices.retain(|device| !logitech::same_device_name(&device.name, &native.name));
        let insertion_index = first_match.unwrap_or(devices.len()).min(devices.len());
        devices.insert(insertion_index, replacement);
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

fn prepare_detected_devices(
    devices: &mut Vec<BatteryDevice>,
    cached_devices: &[BatteryDevice],
    use_cached_readings: bool,
) {
    devices.retain(|device| device.is_connected);
    if !use_cached_readings {
        return;
    }

    for device in devices.iter_mut().filter(|device| device.level.is_none()) {
        if let Some(cached) = cached_devices.iter().find(|cached| {
            cached.level.is_some() && same_battery_device_name(&cached.name, &device.name)
        }) {
            device.level = cached.level;
            device.status.clone_from(&cached.status);
            device.is_loading = true;
        }
    }
}

fn persist_battery_readings(
    previous_snapshot: &mut Option<BatteryCacheSnapshot>,
    devices: &[BatteryDevice],
) {
    let mut readings: Vec<_> = devices
        .iter()
        .filter(|device| !device.is_loading)
        .map(|device| CachedBatteryDevice {
            name: device.name.clone(),
            kind: device.kind.clone(),
            level: if device.is_connected {
                device.level
            } else {
                None
            },
            status: if device.is_connected && device.level.is_some() {
                device.status.clone()
            } else {
                None
            },
        })
        .collect();
    readings.sort_by_cached_key(|device| device.name.to_ascii_lowercase());
    let mut connected_names: Vec<_> = devices
        .iter()
        .filter(|device| device.is_connected)
        .map(|device| device.name.clone())
        .collect();
    connected_names.sort_by_key(|name| name.to_ascii_lowercase());
    let snapshot = BatteryCacheSnapshot {
        readings,
        connected_names,
    };
    if previous_snapshot.as_ref() == Some(&snapshot) {
        return;
    }

    let mut cache = WidgetCache::load();
    let readings_changed = cache.merge_battery_devices(devices);
    let connections_changed = cache.update_last_connected_battery_devices(devices);
    if readings_changed || connections_changed {
        cache.save();
    }
    *previous_snapshot = Some(snapshot);
}

fn expire_initial_readings(devices: &mut [BatteryDevice], elapsed: Duration) {
    if elapsed < INITIAL_PROBE_TIMEOUT {
        return;
    }

    for device in devices.iter_mut().filter(|device| device.is_loading) {
        device.level = None;
        device.status = None;
        device.is_loading = false;
    }
}

fn native_poll_interval(devices: &[BatteryDevice], elapsed: Duration) -> Duration {
    if elapsed < INITIAL_PROBE_TIMEOUT && devices.iter().any(|device| device.is_loading) {
        INITIAL_NATIVE_POLL_INTERVAL
    } else {
        NATIVE_POLL_INTERVAL
    }
}

fn seed_cached_fallbacks(
    external: &mut ExternalDeviceState,
    cached_devices: &[BatteryDevice],
    native_maxwell_authoritative: bool,
    native_logitech: &[BatteryDevice],
) {
    for device in cached_devices.iter().filter(|device| device.is_loading) {
        if native_logitech
            .iter()
            .any(|native| logitech::same_device_name(&native.name, &device.name))
        {
            continue;
        }

        let name = device.name.to_ascii_lowercase();
        let kind = device.kind.as_deref().unwrap_or_default();
        if device.name.eq_ignore_ascii_case(MAXWELL_DEVICE_NAME)
            || kind.eq_ignore_ascii_case("headset")
        {
            if !native_maxwell_authoritative
                || !device.name.eq_ignore_ascii_case(MAXWELL_DEVICE_NAME)
            {
                external.headsetcontrol_fallback_names.insert(name);
            }
        } else if [
            "mouse",
            "keyboard",
            "numpad",
            "presenter",
            "trackball",
            "touchpad",
        ]
        .iter()
        .any(|candidate| kind.eq_ignore_ascii_case(candidate))
        {
            external.solaar_fallback_names.insert(name);
        }
    }
}

fn reconcile_external_fallbacks(
    external: &mut ExternalDeviceState,
    native_maxwell_authoritative: bool,
    native_logitech: &[BatteryDevice],
) {
    external.solaar_fallback_names.retain(|name| {
        !native_logitech
            .iter()
            .any(|native| logitech::same_device_name(&native.name, name))
    });
    for device in &external.solaar_devices {
        if !native_logitech
            .iter()
            .any(|native| logitech::same_device_name(&native.name, &device.name))
        {
            external
                .solaar_fallback_names
                .insert(device.name.to_ascii_lowercase());
        }
    }

    if native_maxwell_authoritative {
        external
            .headsetcontrol_fallback_names
            .remove(&MAXWELL_DEVICE_NAME.to_ascii_lowercase());
    }
    for device in &external.headsetcontrol_devices {
        if !native_maxwell_authoritative || !device.name.eq_ignore_ascii_case(MAXWELL_DEVICE_NAME) {
            external
                .headsetcontrol_fallback_names
                .insert(device.name.to_ascii_lowercase());
        }
    }
}

fn reconcile_native_headset_fallbacks(
    external: &mut ExternalDeviceState,
    covered_names: &[String],
) {
    external.headsetcontrol_fallback_names.retain(|name| {
        !covered_names
            .iter()
            .any(|native| same_native_headset_name(native, name))
    });
}

fn external_probe_plan(
    external: &ExternalDeviceState,
    discovery_due: bool,
    native_maxwell_authoritative: bool,
    solaar_enabled: bool,
) -> ExternalProbePlan {
    ExternalProbePlan {
        solaar: solaar_enabled && (discovery_due || !external.solaar_fallback_names.is_empty()),
        headsetcontrol: discovery_due
            || !native_maxwell_authoritative
            || !external.headsetcontrol_fallback_names.is_empty(),
    }
}

fn query_external_backends(external: &mut ExternalDeviceState, plan: ExternalProbePlan) {
    if plan.solaar {
        match query_solaar_devices() {
            Ok(devices) => external.solaar_devices = devices,
            Err(error) => log::debug!("Solaar fallback query failed: {error}"),
        }
    }
    if plan.headsetcontrol {
        match query_headsetcontrol_devices() {
            Ok(devices) => external.headsetcontrol_devices = devices,
            Err(error) => log::debug!("HeadsetControl fallback query failed: {error}"),
        }
    }
}

fn combined_battery_devices(
    external: &ExternalDeviceState,
    native_maxwell_authoritative: bool,
    native_maxwell: Option<BatteryDevice>,
    native_logitech: &[BatteryDevice],
    native_wolverine: Option<BatteryDevice>,
    native_headsets: &[headsets::BatteryState],
    native_headset_coverage: &[String],
) -> Vec<BatteryDevice> {
    let mut devices = Vec::new();
    for device in external
        .solaar_devices
        .iter()
        .chain(&external.headsetcontrol_devices)
        .filter(|device| device.is_connected)
    {
        replace_device_in_place(&mut devices, device.clone());
    }
    if native_maxwell_authoritative {
        merge_native_maxwell(&mut devices, native_maxwell);
    }
    merge_native_logitech(&mut devices, native_logitech);
    merge_native_wolverine(&mut devices, native_wolverine);
    merge_native_headsets(&mut devices, native_headsets, native_headset_coverage);
    devices
}

// ============================================================================
// External Tool Query Functions
// ============================================================================

/// Query Solaar for Logitech devices not covered by the native readers.
fn query_solaar_devices() -> Result<Vec<BatteryDevice>, String> {
    // Prefer structured output when supported by the installed Solaar version.
    if let Ok(output) = Command::new("solaar").arg("show").arg("--json").output() {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                if let Ok(devices) = parse_solaar_json(&text) {
                    if !devices.is_empty() {
                        return Ok(devices);
                    }
                }
            }
        }
    }

    // Older Solaar versions require plain text. A nonzero status may still
    // accompany useful device output when querying one setting fails.
    let output = Command::new("solaar")
        .arg("show")
        .output()
        .map_err(|error| format!("failed to start Solaar: {error}"))?;
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("Solaar returned invalid UTF-8: {error}"))?;
    Ok(parse_solaar_text(&text))
}

/// Query HeadsetControl for gaming headsets without a native reader.
fn query_headsetcontrol_devices() -> Result<Vec<BatteryDevice>, String> {
    let output = Command::new("headsetcontrol")
        .arg("-b")
        .arg("-o")
        .arg("json")
        .output()
        .map_err(|error| format!("failed to start HeadsetControl: {error}"))?;
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("HeadsetControl returned invalid UTF-8: {error}"))?;
    match parse_headsetcontrol_json(&text) {
        Ok(devices) => Ok(devices),
        Err(_) if !output.status.success() => Ok(Vec::new()),
        Err(error) => Err(error),
    }
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
    let name = value.get("name").and_then(|v| v.as_str())?.trim();
    if !is_valid_battery_device_name(name) {
        log::debug!("Ignoring Solaar device with an invalid product name");
        return None;
    }
    let name = name.to_string();

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
    let is_connected = value
        .get("online")
        .or_else(|| value.get("connected"))
        .and_then(|value| value.as_bool())
        .unwrap_or(level.is_some());

    Some(BatteryDevice {
        name,
        level,
        status,
        kind,
        codename: None,
        is_loading: false,
        is_connected,
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
            let Some(name) = device_obj
                .get("device")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|name| is_valid_battery_device_name(name))
                .map(str::to_string)
            else {
                log::debug!("Ignoring HeadsetControl device with an invalid product name");
                continue;
            };

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
                    current_name =
                        is_valid_battery_device_name(after_colon).then(|| after_colon.to_string());
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
        BatteryDevice, ExternalDeviceState, ExternalProbePlan, INITIAL_NATIVE_POLL_INTERVAL,
        INITIAL_PROBE_TIMEOUT, NATIVE_POLL_INTERVAL, WOLVERINE_DEVICE_NAME,
        expire_initial_readings, external_probe_plan, headsets, merge_native_headsets,
        merge_native_logitech, merge_native_maxwell, merge_native_wolverine, native_poll_interval,
        parse_headsetcontrol_json, parse_solaar_json, parse_solaar_text, prepare_detected_devices,
        reconcile_external_fallbacks, reconcile_native_headset_fallbacks,
    };
    use std::time::Duration;

    fn battery_device(name: &str, loading: bool) -> BatteryDevice {
        BatteryDevice {
            name: name.to_string(),
            level: None,
            status: None,
            kind: None,
            codename: None,
            is_loading: loading,
            is_connected: false,
        }
    }

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
    fn solaar_json_rejects_malformed_binary_device_names() {
        let output = r#"[
            {
                "name": "\u0000\u0001\u0005\u0016",
                "battery": {
                    "level": 70,
                    "status": "BatteryStatus.DISCHARGING"
                }
            },
            {
                "name": "MX Mechanical Mini",
                "kind": "keyboard",
                "battery": {"level": 65, "status": "discharging"}
            }
        ]"#;

        let devices = parse_solaar_json(output).unwrap();

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "MX Mechanical Mini");
    }

    #[test]
    fn solaar_text_rejects_symbol_only_device_names() {
        let output = "  1: !@#$%^&*\n        Battery: 70% (discharging)\n";

        assert!(parse_solaar_text(output).is_empty());
    }

    #[test]
    fn startup_polling_is_fast_only_while_a_cached_reading_is_unresolved() {
        let mut devices = vec![battery_device("Audeze Maxwell", true)];

        assert_eq!(
            native_poll_interval(&devices, Duration::from_secs(2)),
            INITIAL_NATIVE_POLL_INTERVAL
        );

        devices[0].is_loading = false;
        assert_eq!(
            native_poll_interval(&devices, Duration::from_secs(2)),
            NATIVE_POLL_INTERVAL
        );
    }

    #[test]
    fn unresolved_connected_readings_become_unavailable_after_the_timeout() {
        let mut devices = vec![BatteryDevice {
            level: Some(80),
            status: Some("charging".to_string()),
            is_loading: true,
            is_connected: true,
            ..battery_device("Audeze Maxwell", true)
        }];

        expire_initial_readings(&mut devices, INITIAL_PROBE_TIMEOUT);

        assert_eq!(devices[0].level, None);
        assert_eq!(devices[0].status, None);
        assert!(!devices[0].is_loading);
        assert!(devices[0].is_connected);
        assert_eq!(
            native_poll_interval(&devices, INITIAL_PROBE_TIMEOUT),
            NATIVE_POLL_INTERVAL
        );
    }

    #[test]
    fn cached_readings_are_applied_only_to_detected_connected_devices() {
        let cached = vec![
            BatteryDevice {
                level: Some(82),
                status: Some("discharging".to_string()),
                ..battery_device("Audeze Maxwell", true)
            },
            BatteryDevice {
                level: Some(100),
                status: Some("discharging".to_string()),
                ..battery_device("G309 LIGHTSPEED", true)
            },
        ];
        let mut detected = vec![
            BatteryDevice {
                is_connected: false,
                ..battery_device("Audeze Maxwell", false)
            },
            BatteryDevice {
                is_connected: true,
                ..battery_device("G309 LIGHTSPEED", false)
            },
        ];

        prepare_detected_devices(&mut detected, &cached, true);

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].name, "G309 LIGHTSPEED");
        assert_eq!(detected[0].level, Some(100));
        assert!(detected[0].is_loading);
        assert!(detected[0].is_connected);
    }

    #[test]
    fn live_battery_readings_take_precedence_over_cached_values() {
        let cached = vec![BatteryDevice {
            level: Some(65),
            ..battery_device("MX Mechanical Mini", true)
        }];
        let mut detected = vec![BatteryDevice {
            level: Some(60),
            is_connected: true,
            ..battery_device("MX Mechanical Mini", false)
        }];

        prepare_detected_devices(&mut detected, &cached, true);

        assert_eq!(detected[0].level, Some(60));
        assert!(!detected[0].is_loading);
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
    fn disconnected_native_maxwell_removes_the_cached_row() {
        let mut devices = vec![BatteryDevice {
            name: "Audeze Maxwell".to_string(),
            level: None,
            status: None,
            kind: Some("headset".to_string()),
            codename: None,
            is_loading: false,
            is_connected: false,
        }];
        let disconnected = devices[0].clone();

        merge_native_maxwell(&mut devices, Some(disconnected));

        assert!(devices.is_empty());
    }

    #[test]
    fn wolverine_without_a_live_battery_reading_is_hidden() {
        let controller = BatteryDevice {
            name: WOLVERINE_DEVICE_NAME.to_string(),
            level: Some(80),
            status: Some("discharging".to_string()),
            kind: Some("controller".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        };

        for unavailable in [
            BatteryDevice {
                level: None,
                is_connected: false,
                ..controller.clone()
            },
            BatteryDevice {
                level: None,
                is_connected: true,
                ..controller.clone()
            },
        ] {
            let mut devices = vec![controller.clone()];
            merge_native_wolverine(&mut devices, Some(unavailable));
            assert!(devices.is_empty());
        }
    }

    #[test]
    fn wolverine_with_a_battery_reading_is_visible() {
        let controller = BatteryDevice {
            name: WOLVERINE_DEVICE_NAME.to_string(),
            level: Some(80),
            status: Some("discharging".to_string()),
            kind: Some("controller".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        };
        let mut devices = Vec::new();

        merge_native_wolverine(&mut devices, Some(controller));

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].level, Some(80));
    }

    #[test]
    fn native_logitech_replaces_matching_solaar_devices_only() {
        let mut devices = vec![
            BatteryDevice {
                name: "Logitech G309 LIGHTSPEED".to_string(),
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
            name: "G309".to_string(),
            level: Some(100),
            ..devices[0].clone()
        };

        merge_native_logitech(&mut devices, &[native]);

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "Logitech G309 LIGHTSPEED");
        assert_eq!(devices[1].name, "Unsupported Logitech device");
        assert_eq!(
            devices
                .iter()
                .find(|device| device.name == "Logitech G309 LIGHTSPEED")
                .and_then(|device| device.level),
            Some(100)
        );
        assert!(
            devices
                .iter()
                .any(|device| device.name == "Unsupported Logitech device")
        );
    }

    #[test]
    fn native_logitech_collapses_all_cached_aliases() {
        let template = BatteryDevice {
            name: String::new(),
            level: Some(50),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: true,
            is_connected: false,
        };
        let mut devices = vec![
            BatteryDevice {
                name: "G309".to_string(),
                ..template.clone()
            },
            BatteryDevice {
                name: "G309 LIGHTSPEED".to_string(),
                ..template.clone()
            },
        ];
        let native = BatteryDevice {
            name: "G309 LIGHTSPEED".to_string(),
            level: Some(100),
            is_loading: false,
            is_connected: true,
            ..template
        };

        merge_native_logitech(&mut devices, &[native]);

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "G309 LIGHTSPEED");
        assert_eq!(devices[0].level, Some(100));
        assert!(!devices[0].is_loading);
        assert!(devices[0].is_connected);
    }

    #[test]
    fn disconnected_native_logitech_removes_the_cached_row() {
        let mut devices = vec![BatteryDevice {
            name: "G309 LIGHTSPEED".to_string(),
            level: Some(100),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: true,
            is_connected: true,
        }];
        let disconnected = BatteryDevice {
            level: None,
            status: None,
            is_loading: false,
            is_connected: false,
            ..devices[0].clone()
        };

        merge_native_logitech(&mut devices, &[disconnected]);

        assert!(devices.is_empty());
    }

    #[test]
    fn native_headset_replaces_the_headsetcontrol_copy() {
        let mut devices = vec![BatteryDevice {
            name: "SteelSeries Arctis Nova 7".to_string(),
            level: Some(25),
            status: Some("discharging".to_string()),
            kind: Some("headset".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        }];
        let native = headsets::BatteryState {
            name: "SteelSeries Arctis Nova 7".to_string(),
            level: Some(75),
            status: Some("charging".to_string()),
        };

        merge_native_headsets(
            &mut devices,
            &[native],
            &["SteelSeries Arctis Nova 7".to_string()],
        );

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].level, Some(75));
        assert_eq!(devices[0].status.as_deref(), Some("charging"));
        assert_eq!(devices[0].kind.as_deref(), Some("headset"));
    }

    #[test]
    fn powered_off_native_headset_removes_stale_rows_and_cli_polling() {
        let name = "SteelSeries Arctis Nova 7".to_string();
        let mut devices = vec![battery_device(&name, false)];
        let mut external = ExternalDeviceState {
            headsetcontrol_devices: devices.clone(),
            ..Default::default()
        };
        external
            .headsetcontrol_fallback_names
            .insert(name.to_ascii_lowercase());

        merge_native_headsets(&mut devices, &[], std::slice::from_ref(&name));
        reconcile_native_headset_fallbacks(&mut external, std::slice::from_ref(&name));

        assert!(devices.is_empty());
        assert!(external.headsetcontrol_fallback_names.is_empty());
    }

    #[test]
    fn native_logitech_headset_replaces_a_shorter_discovered_name() {
        let mut devices = vec![battery_device("G522", false)];
        let native = headsets::BatteryState {
            name: "Logitech G522 LIGHTSPEED".to_string(),
            level: Some(90),
            status: Some("discharging".to_string()),
        };

        merge_native_headsets(
            &mut devices,
            &[native],
            &["Logitech G522 LIGHTSPEED".to_string()],
        );

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Logitech G522 LIGHTSPEED");
        assert_eq!(devices[0].level, Some(90));
    }

    #[test]
    fn native_devices_disable_thirty_second_external_polling() {
        let native_logitech = vec![battery_device("G309 LIGHTSPEED", false)];
        let mut external = ExternalDeviceState {
            solaar_devices: vec![battery_device("G309 LIGHTSPEED", false)],
            headsetcontrol_devices: vec![battery_device("Audeze Maxwell", false)],
            ..Default::default()
        };

        reconcile_external_fallbacks(&mut external, true, &native_logitech);

        assert_eq!(
            external_probe_plan(&external, false, true, true),
            ExternalProbePlan {
                solaar: false,
                headsetcontrol: false,
            }
        );
        assert_eq!(
            external_probe_plan(&external, true, true, true),
            ExternalProbePlan::DISCOVERY
        );
    }

    #[test]
    fn unsupported_devices_keep_only_their_fallback_backend_active() {
        let native_logitech = vec![battery_device("G309 LIGHTSPEED", false)];
        let mut external = ExternalDeviceState {
            solaar_devices: vec![
                battery_device("G309 LIGHTSPEED", false),
                battery_device("Unsupported Logitech device", false),
            ],
            headsetcontrol_devices: vec![battery_device("Audeze Maxwell", false)],
            ..Default::default()
        };

        reconcile_external_fallbacks(&mut external, true, &native_logitech);

        assert_eq!(
            external_probe_plan(&external, false, true, true),
            ExternalProbePlan {
                solaar: true,
                headsetcontrol: false,
            }
        );
    }

    #[test]
    fn native_coverage_retires_a_temporary_solaar_fallback() {
        let mut external = ExternalDeviceState::default();
        external
            .solaar_fallback_names
            .insert("mx mechanical mini".to_string());
        assert!(external_probe_plan(&external, false, true, true).solaar);

        reconcile_external_fallbacks(
            &mut external,
            true,
            &[battery_device("MX Mechanical Mini", false)],
        );

        assert!(!external_probe_plan(&external, false, true, true).solaar);
    }

    #[test]
    fn headsetcontrol_remains_a_fallback_when_native_maxwell_fails() {
        let external = ExternalDeviceState::default();

        assert_eq!(
            external_probe_plan(&external, false, false, true),
            ExternalProbePlan {
                solaar: false,
                headsetcontrol: true,
            }
        );
    }

    #[test]
    fn disabled_solaar_never_enters_the_probe_plan() {
        let mut external = ExternalDeviceState::default();
        external
            .solaar_fallback_names
            .insert("unsupported logitech device".to_string());

        assert!(!external_probe_plan(&external, true, true, false).solaar);
    }
}
