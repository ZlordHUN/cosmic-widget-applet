// SPDX-License-Identifier: MPL-2.0

//! Storage/Disk monitoring

use sysinfo::Disks;
use std::process::Command;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub used_percentage: f32,
    pub total_space: u64,
    pub available_space: u64,
    pub is_loading: bool, // True if showing cached data while loading
}

pub struct StorageMonitor {
    disks: Disks,
    pub disk_info: Vec<DiskInfo>,
    disk_models: Arc<Mutex<HashMap<String, String>>>,
    is_first_update: bool,
}

impl StorageMonitor {
    pub fn new() -> Self {
        let disk_models = Arc::new(Mutex::new(HashMap::new()));
        
        // Load cached disk info to show immediately
        let cache = super::cache::WidgetCache::load();
        let disk_info: Vec<DiskInfo> = cache
            .disks
            .iter()
            .map(|d| DiskInfo {
                name: d.name.clone(),
                mount_point: d.mount_point.clone(),
                used_percentage: 0.0,
                total_space: 0,
                available_space: 0,
                is_loading: true,
            })
            .collect();
        
        // Spawn background thread to update disk models
        let disk_models_clone = Arc::clone(&disk_models);
        std::thread::spawn(move || {
            loop {
                // Fetch disk models from lsblk
                if let Some(models) = Self::fetch_disk_models() {
                    *disk_models_clone.lock().unwrap() = models;
                }
                
                // Refresh every 10 seconds (disk models don't change often)
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
        
        Self {
            disks: Disks::new_with_refreshed_list(),
            disk_info,
            disk_models,
            is_first_update: true,
        }
    }
    
    /// Get disk model names from lsblk (called from background thread)
    fn fetch_disk_models() -> Option<HashMap<String, String>> {
        let mut models = HashMap::new();
        
        // Run lsblk to get device vendor and model
        if let Ok(output) = Command::new("lsblk")
            .args(&["-ndo", "NAME,VENDOR,MODEL"])
            .output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let device = parts[0].to_string();
                        let vendor_and_model = parts[1..].join(" ");
                        models.insert(device, vendor_and_model.trim().to_string());
                    }
                }
            }
        }
        
        Some(models)
    }

    pub fn update(&mut self) {
        // Only refresh existing disk data, don't rescan for new disks every time
        // refresh_list() causes file descriptor leaks when called frequently
        self.disks.refresh();
        
        self.disk_info.clear();
        
        // Get disk models from cache (updated by background thread)
        let disk_models = self.disk_models.lock().unwrap().clone();
        
        for disk in &self.disks {
            let mount_point = disk.mount_point().to_string_lossy().to_string();
            
            // Skip non-meaningful mount points
            // Only show root, /home, and top-level /mnt or /media mounts
            let is_root = mount_point == "/";
            let is_home = mount_point == "/home";
            let is_top_level_mount = mount_point.starts_with("/mnt/") || mount_point.starts_with("/media/");
            
            // Skip boot partitions, snap mounts, and other system partitions
            if mount_point.starts_with("/boot") 
                || mount_point.starts_with("/snap")
                || mount_point.starts_with("/run")
                || mount_point.starts_with("/sys")
                || mount_point.starts_with("/proc")
                || mount_point.starts_with("/dev")
                || mount_point.starts_with("/tmp")
                || mount_point.starts_with("/var/snap") {
                continue;
            }
            
            // Only include root, /home, or external mounts
            if !is_root && !is_home && !is_top_level_mount {
                continue;
            }
            
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total - available;
            let used_percentage = if total > 0 {
                (used as f32 / total as f32) * 100.0
            } else {
                0.0
            };
            
            // Get the device name (e.g., sda, nvme0n1, sdb)
            let device_name = disk.name().to_string_lossy().to_string();
            
            // Extract the base disk name (without partition number)
            // e.g., /dev/sda1 -> sda, /dev/nvme0n1p1 -> nvme0n1
            let base_device = if let Some(dev) = device_name.strip_prefix("/dev/") {
                // Remove partition numbers
                if dev.contains("nvme") || dev.contains("mmcblk") {
                    // NVMe or MMC devices: nvme0n1p1 -> nvme0n1
                    dev.split('p').next().unwrap_or(dev)
                } else {
                    // Regular devices: sda1 -> sda
                    dev.trim_end_matches(|c: char| c.is_ascii_digit())
                }
            } else {
                &device_name
            };
            
            // Try to get a better label for the disk
            let display_name = if mount_point == "/" {
                // For root, try to get model name or use "System"
                disk_models.get(base_device)
                    .map(|m| m.clone())
                    .unwrap_or_else(|| "System".to_string())
            } else if mount_point == "/home" {
                "Home".to_string()
            } else {
                // For external drives, try to get the model name
                disk_models.get(base_device)
                    .map(|m| m.clone())
                    .unwrap_or_else(|| {
                        // Fallback to mount point name
                        mount_point
                            .split('/')
                            .last()
                            .unwrap_or(&mount_point)
                            .to_string()
                    })
            };
            
            self.disk_info.push(DiskInfo {
                name: display_name,
                mount_point,
                used_percentage,
                total_space: total,
                available_space: available,
                is_loading: false,
            });
        }
        
        // Update cache after first successful update
        if self.is_first_update && !self.disk_info.is_empty() {
            let mut cache = super::cache::WidgetCache::load();
            cache.update_disks(&self.disk_info);
            self.is_first_update = false;
        }
    }
}
