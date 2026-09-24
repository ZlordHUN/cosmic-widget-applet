// SPDX-License-Identifier: MPL-2.0

//! CPU, Memory, and GPU Utilization Monitoring
//!
//! This module provides real-time system resource utilization monitoring for:
//! - **CPU**: Overall CPU usage percentage via sysinfo
//! - **Memory**: Used/total RAM with percentage
//! - **GPU**: Utilization for NVIDIA, AMD, and Intel GPUs
//!
//! # GPU Monitoring
//!
//! GPU utilization is monitored in a background thread to avoid blocking the UI.
//! The detection order is:
//!
//! 1. **NVIDIA**: Queries the NVIDIA Management Library through `nvml-wrapper`
//! 2. **AMD**: Reads `/sys/class/drm/card*/device/gpu_busy_percent`
//! 3. **Intel**: Reads current and maximum GPU frequencies from sysfs
//!
//! # Usage
//!
//! Create a [`UtilizationMonitor`] once, then call [`UtilizationMonitor::update`]
//! each sampling interval. Read its CPU and memory fields and use
//! [`UtilizationMonitor::get_gpu_usage`] for the latest GPU worker reading.
//!
//! # Thread Safety
//!
//! GPU usage is stored in an `Arc<Mutex<f32>>` and updated by a background thread.
//! The `get_gpu_usage()` method safely reads the current value.

use std::sync::{Arc, Mutex};
use sysinfo::System;

// ============================================================================
// GPU Vendor Detection
// ============================================================================

/// Supported GPU vendors for utilization monitoring.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GpuVendor {
    /// NVIDIA GPU (uses NVML)
    Nvidia,
    /// AMD GPU (uses sysfs)
    Amd,
    /// Intel integrated/discrete GPU (uses sysfs)
    Intel,
    /// No supported GPU detected
    None,
}

// ============================================================================
// Main Monitor Structure
// ============================================================================

/// Monitors CPU, Memory, and GPU utilization.
///
/// CPU and Memory are updated synchronously via `update()`.
/// GPU utilization is monitored by a background thread for better accuracy.
pub struct UtilizationMonitor {
    /// sysinfo system instance for CPU/Memory data
    sys: System,

    /// Current CPU usage percentage (0-100)
    pub cpu_usage: f32,

    /// Current memory usage percentage (0-100)
    pub memory_usage: f32,

    /// Total system memory in bytes
    pub memory_total: u64,

    /// Used system memory in bytes
    pub memory_used: u64,

    /// GPU usage percentage, updated by background thread
    pub gpu_usage: Arc<Mutex<f32>>,

    /// Detected GPU vendor (determines monitoring method)
    gpu_vendor: GpuVendor,
}

// ============================================================================
// Implementation
// ============================================================================

impl UtilizationMonitor {
    /// Create a new utilization monitor.
    ///
    /// Automatically detects GPU vendor and spawns a background thread
    /// for GPU monitoring if a supported GPU is found.
    pub fn new() -> Self {
        // Shared GPU usage value for thread-safe access
        let gpu_usage = Arc::new(Mutex::new(0.0f32));

        // Detect which GPU monitoring method to use
        let gpu_vendor = Self::detect_gpu_vendor();

        // Spawn background thread for GPU monitoring (if GPU detected)
        if gpu_vendor != GpuVendor::None {
            let gpu_usage_clone = Arc::clone(&gpu_usage);
            std::thread::spawn(move || {
                loop {
                    // Poll every second for smooth updates
                    std::thread::sleep(std::time::Duration::from_secs(1));

                    let usage = match gpu_vendor {
                        GpuVendor::Nvidia => super::nvidia::utilization(),
                        GpuVendor::Amd => Self::fetch_amd_gpu_usage(),
                        GpuVendor::Intel => Self::fetch_intel_gpu_usage(),
                        GpuVendor::None => None,
                    };

                    if let Some(usage) = usage {
                        *gpu_usage_clone.lock().unwrap() = usage;
                    }
                }
            });
        }

        Self {
            sys: System::new_all(),
            cpu_usage: 0.0,
            memory_usage: 0.0,
            memory_total: 0,
            memory_used: 0,
            gpu_usage,
            gpu_vendor,
        }
    }

    /// Update CPU and memory statistics.
    ///
    /// Should be called at the configured update interval (default: 1 second).
    /// GPU usage is updated by the background thread, not here.
    pub fn update(&mut self) {
        // Refresh CPU usage (requires multiple calls for accurate averaging)
        self.sys.refresh_cpu_all();
        self.cpu_usage = self.sys.global_cpu_usage();

        // Refresh memory statistics
        self.sys.refresh_memory();
        self.memory_used = self.sys.used_memory();
        self.memory_total = self.sys.total_memory();
        self.memory_usage = if self.memory_total > 0 {
            (self.memory_used as f32 / self.memory_total as f32) * 100.0
        } else {
            0.0
        };

        // Note: GPU usage is updated in background thread
    }

    /// Get current GPU usage percentage.
    ///
    /// Thread-safe read from the background-updated value.
    /// Returns 0.0 if no GPU is detected or monitoring failed.
    pub fn get_gpu_usage(&self) -> f32 {
        *self.gpu_usage.lock().unwrap()
    }

    // ========================================================================
    // GPU Vendor Detection
    // ========================================================================

    /// Detect which GPU vendor is present on the system.
    ///
    /// Checks NVML first, then detects AMD or Intel DRM devices through sysfs.
    fn detect_gpu_vendor() -> GpuVendor {
        if super::nvidia::hardware_present() {
            return GpuVendor::Nvidia;
        }

        let mut amd_found = false;
        let mut intel_found = false;
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if !name_str.starts_with("card") || name_str.contains('-') {
                    continue;
                }

                let vendor =
                    std::fs::read_to_string(entry.path().join("device/vendor")).unwrap_or_default();
                match vendor.trim().to_ascii_lowercase().as_str() {
                    "0x1002" => amd_found = true,
                    "0x8086" => intel_found = true,
                    _ => {}
                }
            }
        }

        if amd_found {
            GpuVendor::Amd
        } else if intel_found {
            GpuVendor::Intel
        } else {
            GpuVendor::None
        }
    }

    // ========================================================================
    // GPU Usage Fetching (called from background thread)
    // ========================================================================

    /// Fetch AMD GPU utilization.
    ///
    /// Reads the kernel driver's utilization value from sysfs.
    fn fetch_amd_gpu_usage() -> Option<f32> {
        // Primary method: Read from sysfs (most reliable, no permissions needed)
        // AMD GPUs expose utilization in /sys/class/drm/card*/device/gpu_busy_percent
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.starts_with("card") && !name_str.contains("-") {
                    let busy_path = entry.path().join("device/gpu_busy_percent");
                    if let Ok(content) = std::fs::read_to_string(&busy_path) {
                        if let Ok(usage) = content.trim().parse::<f32>() {
                            return Some(usage);
                        }
                    }
                }
            }
        }

        None
    }

    /// Fetch Intel GPU utilization.
    ///
    /// Calculates from the current/maximum frequency ratio exposed by sysfs.
    fn fetch_intel_gpu_usage() -> Option<f32> {
        // Primary method: Calculate usage from frequency ratio
        // Intel GPUs expose frequency in sysfs
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.starts_with("card") && !name_str.contains("-") {
                    // Try gt0 (most common)
                    let cur_freq_path = entry.path().join("gt/gt0/rps_cur_freq_mhz");
                    let max_freq_path = entry.path().join("gt/gt0/rps_max_freq_mhz");

                    if let (Ok(cur_str), Ok(max_str)) = (
                        std::fs::read_to_string(&cur_freq_path),
                        std::fs::read_to_string(&max_freq_path),
                    ) {
                        if let (Ok(cur_freq), Ok(max_freq)) =
                            (cur_str.trim().parse::<f32>(), max_str.trim().parse::<f32>())
                        {
                            if max_freq > 0.0 {
                                return Some((cur_freq / max_freq) * 100.0);
                            }
                        }
                    }
                }
            }
        }

        None
    }
}
