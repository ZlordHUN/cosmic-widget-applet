// SPDX-License-Identifier: MPL-2.0

//! Persistent cache for widget data
//!
//! Stores drive and peripheral information to display cached data
//! while the widget is loading fresh data.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedDiskInfo {
    pub name: String,
    pub mount_point: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedBatteryDevice {
    pub name: String,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WidgetCache {
    pub disks: Vec<CachedDiskInfo>,
    pub battery_devices: Vec<CachedBatteryDevice>,
}

impl WidgetCache {
    fn cache_path() -> PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        path.push("cosmic-monitor-applet");
        fs::create_dir_all(&path).ok();
        path.push("widget_cache.json");
        path
    }

    pub fn load() -> Self {
        let path = Self::cache_path();
        if let Ok(content) = fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let path = Self::cache_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            fs::write(&path, json).ok();
        }
    }

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
