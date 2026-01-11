// SPDX-License-Identifier: MPL-2.0

//! Persistent Cache for Widget Data
//!
//! This module provides a JSON-based caching mechanism for device information
//! discovered by the widget. The cache serves two purposes:
//!
//! 1. **Quick startup**: Display cached data immediately while fresh data loads
//! 2. **Settings integration**: The settings app reads this cache to show device
//!    lists without needing direct hardware access
//!
//! # Cache Location
//!
//! The cache is stored at `~/.cache/cosmic-widget-applet/widget_cache.json`
//!
//! # Data Stored
//!
//! - **Disk information**: Name and mount point of discovered disks
//! - **Battery devices**: Name and type of discovered battery sources
//!
//! # Thread Safety
//!
//! The cache uses simple file I/O without locking. In practice, the widget
//! writes and the settings app reads, so conflicts are rare. If data races
//! occur, the worst case is displaying slightly stale data.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;

// ============================================================================
// Cache Data Structures
// ============================================================================

/// Cached information about a mounted disk.
///
/// This is a simplified version of `DiskInfo` for serialization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedDiskInfo {
    /// Disk device name (e.g., "nvme0n1p2")
    pub name: String,
    /// Mount point path (e.g., "/home")
    pub mount_point: String,
}

/// Cached information about a battery device.
///
/// Includes both system batteries and Solaar-managed Bluetooth devices.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedBatteryDevice {
    /// Device name (e.g., "MX Master 3" or "BAT0")
    pub name: String,
    /// Device type if known (e.g., "Mouse", "Keyboard", None for system battery)
    pub kind: Option<String>,
}

/// Main cache structure containing all cached device information.
///
/// Serialized to JSON and stored in the user's cache directory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WidgetCache {
    /// All discovered mounted disks
    pub disks: Vec<CachedDiskInfo>,
    /// All discovered battery sources
    pub battery_devices: Vec<CachedBatteryDevice>,
}

// ============================================================================
// Cache Operations
// ============================================================================

impl WidgetCache {
    /// Returns the path to the cache file.
    ///
    /// Creates the parent directory if it doesn't exist.
    /// Falls back to `/tmp` if cache directory cannot be determined.
    fn cache_path() -> PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        path.push("cosmic-widget-applet");
        fs::create_dir_all(&path).ok();
        path.push("widget_cache.json");
        path
    }

    /// Load the cache from disk.
    ///
    /// Returns `Default` if the file doesn't exist or cannot be parsed.
    /// This ensures the widget always has a valid cache to work with.
    pub fn load() -> Self {
        let path = Self::cache_path();
        if let Ok(content) = fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Save the cache to disk.
    ///
    /// Uses pretty-printed JSON for easier debugging.
    /// Silently ignores write errors (cache is non-critical).
    pub fn save(&self) {
        let path = Self::cache_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            fs::write(&path, json).ok();
        }
    }

    /// Update cached disk information from fresh data.
    ///
    /// Replaces all cached disks and saves immediately.
    pub fn update_disks(&mut self, disks: &[super::storage::DiskInfo]) {
        self.disks = disks
            .iter()
            .map(|d| CachedDiskInfo {
                name: d.name.clone(),
                mount_point: d.mount_point.clone(),
            })
            .collect();
        self.save();
    }

    /// Update cached battery device information from fresh data.
    ///
    /// Replaces all cached devices and saves immediately.
    pub fn update_battery_devices(&mut self, devices: &[super::battery::BatteryDevice]) {
        self.battery_devices = devices
            .iter()
            .map(|d| CachedBatteryDevice {
                name: d.name.clone(),
                kind: d.kind.clone(),
            })
            .collect();
        self.save();
    }
}
