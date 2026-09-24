// SPDX-License-Identifier: MPL-2.0

//! Persisted device data shared with the running widget.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct CachedBatteryDevice {
    pub(super) name: String,
    pub(super) kind: Option<String>,
    #[serde(default)]
    level: Option<u8>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CachedDiskInfo {
    name: String,
    mount_point: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct WidgetCache {
    disks: Vec<CachedDiskInfo>,
    pub(super) battery_devices: Vec<CachedBatteryDevice>,
}

impl WidgetCache {
    fn cache_path() -> std::path::PathBuf {
        let mut path = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        path.push("cosmic-widget-applet");
        let _ = std::fs::create_dir_all(&path);
        path.push("widget_cache.json");
        path
    }

    pub(super) fn load() -> Self {
        std::fs::read_to_string(Self::cache_path())
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub(super) fn save(&self) {
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let _ = std::fs::write(Self::cache_path(), json);
    }
}
