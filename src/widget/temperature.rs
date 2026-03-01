// SPDX-License-Identifier: MPL-2.0

//! # Temperature Monitoring Module
//!
//! This module monitors CPU and GPU temperatures using the `sysinfo` crate's
//! hardware sensor interface. It provides real-time temperature readings and
//! visual gauge rendering.
//!
//! ## Data Sources
//!
//! Temperature data comes from Linux hwmon subsystem via sysinfo:
//! - **CPU**: Looks for sensors labeled "cpu", "package", "core", "tctl", or "tdie"
//! - **GPU**: Looks for sensors labeled "gpu", "nvidia", "amd", "radeon", or "edge"
//!
//! ## Sensor Labels by Vendor
//!
//! - **Intel CPU**: "coretemp" driver, labels like "Package id 0", "Core 0"
//! - **AMD CPU**: "k10temp" driver, labels like "Tctl", "Tdie", "Tccd1"
//! - **NVIDIA GPU**: "nvidia" driver, label "GPU"
//! - **AMD GPU**: "amdgpu" driver, label "edge"
//!
//! ## Visual Representation
//!
//! Temperatures are displayed as circular gauges with:
//! - Hollow ring that fills based on temperature ratio
//! - Color coding: Green (<50%), Yellow (50-80%), Red (>80%)
//! - Black border for visibility on any background

use sysinfo::Components;

// ============================================================================
// Temperature Monitor Struct
// ============================================================================

/// Monitors CPU and GPU temperatures via sysinfo.
///
/// Uses the sysinfo crate to query Linux hwmon sensors. The monitor maintains
/// a list of all hardware components and searches for temperature sensors
/// matching known CPU and GPU patterns.
///
/// # Example Labels Matched
///
/// - CPU: "Package id 0", "Core 0", "Tctl", "Tdie"
/// - GPU: "nvidia", "edge", "radeon"
///
/// # Fields
///
/// - `components`: sysinfo's hardware component list
/// - `cpu_temp`: Last read CPU temperature in Celsius
/// - `gpu_temp`: Last read GPU temperature in Celsius
pub struct TemperatureMonitor {
    /// Hardware component list from sysinfo (includes all sensors)
    components: Components,
    /// Current CPU temperature in Celsius (0.0 if not found)
    pub cpu_temp: f32,
    /// Current GPU temperature in Celsius (0.0 if not found)
    pub gpu_temp: f32,
}

impl TemperatureMonitor {
    /// Create a new temperature monitor.
    ///
    /// Initializes sysinfo's component list with an immediate refresh.
    /// This discovers all available hardware sensors on the system.
    pub fn new() -> Self {
        Self {
            components: Components::new_with_refreshed_list(),
            cpu_temp: 0.0,
            gpu_temp: 0.0,
        }
    }

    /// Update temperature readings from hardware sensors.
    ///
    /// Refreshes sysinfo's component data, then searches for CPU and GPU
    /// temperature sensors by matching against known label patterns.
    ///
    /// # CPU Detection Priority
    ///
    /// Uses a tiered priority system (case-insensitive) to find the most
    /// accurate CPU temperature reading:
    ///
    /// **Tier 1 — Preferred (actual die/package temps):**
    /// - "tdie" - AMD Ryzen actual die temperature
    /// - "tccd" - AMD Ryzen CCD (Core Complex Die) temps — averaged if multiple
    /// - "package" - Intel package temperature
    ///
    /// **Tier 2 — Acceptable fallbacks:**
    /// - "cpu" - Generic CPU label
    /// - "core" - Individual core temperature
    ///
    /// **Tier 3 — Last resort:**
    /// - "tctl" - AMD Ryzen control temperature (inflated for fan curves)
    ///
    /// # GPU Detection Priority
    ///
    /// Matches first sensor containing (case-insensitive):
    /// 1. "gpu" - Generic GPU label
    /// 2. "nvidia" - NVIDIA GPU
    /// 3. "amd" - AMD GPU
    /// 4. "radeon" - AMD Radeon (older naming)
    /// 5. "edge" - AMD RDNA/Vega edge sensor
    pub fn update(&mut self) {
        // Refresh all component data from hwmon
        self.components.refresh();
        
        // ---- CPU Temperature Detection ----
        // Use a tiered approach: prefer actual die temps over control temps.
        // AMD Tctl is intentionally offset above real die temp for fan curves,
        // so we prefer Tdie or Tccd readings when available.
        
        let mut tdie_temp: Option<f32> = None;
        let mut tccd_temps: Vec<f32> = Vec::new();
        let mut package_temp: Option<f32> = None;
        let mut cpu_generic_temp: Option<f32> = None;
        let mut core_temp: Option<f32> = None;
        let mut tctl_temp: Option<f32> = None;
        
        for component in &self.components {
            let label = component.label().to_lowercase();
            let temp = component.temperature();
            
            if label.contains("tdie") {
                tdie_temp = Some(temp);
            } else if label.contains("tccd") {
                tccd_temps.push(temp);
            } else if label.contains("package") {
                package_temp = Some(temp);
            } else if label.contains("cpu") {
                cpu_generic_temp = Some(temp);
            } else if label.contains("core") && core_temp.is_none() {
                core_temp = Some(temp);
            } else if label.contains("tctl") {
                tctl_temp = Some(temp);
            }
        }
        
        // Average Tccd readings if we have multiple CCDs
        let tccd_avg = if !tccd_temps.is_empty() {
            Some(tccd_temps.iter().sum::<f32>() / tccd_temps.len() as f32)
        } else {
            None
        };
        
        // Pick best available: Tdie > Tccd avg > Package > CPU > Core > Tctl
        self.cpu_temp = tdie_temp
            .or(tccd_avg)
            .or(package_temp)
            .or(cpu_generic_temp)
            .or(core_temp)
            .or(tctl_temp)
            .unwrap_or(0.0);
        
        // ---- GPU Temperature Detection ----
        // Search through all components for first matching GPU sensor
        self.gpu_temp = 0.0;
        for component in &self.components {
            let label = component.label().to_lowercase();
            if label.contains("gpu") || label.contains("nvidia") || label.contains("amd") 
                || label.contains("radeon") || label.contains("edge") {
                self.gpu_temp = component.temperature();
                break;
            }
        }
    }
}

// ============================================================================
// Drawing Helper Function
// ============================================================================

/// Draw a circular temperature gauge with color-coded progress ring.
///
/// Renders a hollow circular gauge that fills based on the temperature
/// relative to a maximum value. The ring color changes to indicate
/// thermal status:
///
/// - **Green**: Temperature below 50% of max (cool)
/// - **Yellow**: Temperature 50-80% of max (warm)
/// - **Red**: Temperature above 80% of max (hot)
///
/// # Arguments
///
/// * `cr` - Cairo context for drawing
/// * `x` - Left edge X coordinate
/// * `y` - Top edge Y coordinate
/// * `radius` - Radius of the gauge circle
/// * `temp` - Current temperature in Celsius
/// * `max_temp` - Maximum temperature for full circle (e.g., 100.0)
///
/// # Visual Structure
///
/// ```text
/// ┌─────────────────┐
/// │    ╭─────╮      │  Outer border (black)
/// │   ╱  ███  ╲     │  Background ring (dark gray)
/// │  │  ███   │     │  Progress arc (green/yellow/red)
/// │   ╲      ╱      │  Inner border (black)
/// │    ╰─────╯      │
/// └─────────────────┘
/// ```
pub fn draw_temp_circle(cr: &cairo::Context, x: f64, y: f64, radius: f64, temp: f32, max_temp: f32) {
    // Save Cairo state so line_width and source don't leak to callers
    cr.save().expect("Failed to save");
    
    let center_x = x + radius;
    let center_y = y + radius;
    
    // Determine color based on temperature (similar to progress bar logic)
    let percentage = (temp / max_temp * 100.0).min(100.0);
    let (r, g, b) = if percentage < 50.0 {
        (0.4, 0.9, 0.4) // Green
    } else if percentage < 80.0 {
        (0.9, 0.9, 0.4) // Yellow
    } else {
        (0.9, 0.4, 0.4) // Red
    };
    
    // Draw outer ring (background)
    cr.arc(center_x, center_y, radius, 0.0, 2.0 * std::f64::consts::PI);
    cr.set_source_rgba(0.2, 0.2, 0.2, 0.7);
    cr.set_line_width(8.0);
    cr.stroke().expect("Failed to stroke");
    
    // Draw inner colored ring based on temperature
    let angle = (temp / max_temp).min(1.0) as f64 * 2.0 * std::f64::consts::PI;
    cr.arc(center_x, center_y, radius, -std::f64::consts::PI / 2.0, -std::f64::consts::PI / 2.0 + angle);
    cr.set_source_rgb(r, g, b);
    cr.set_line_width(8.0);
    cr.stroke().expect("Failed to stroke");
    
    // Draw border around the ring
    cr.arc(center_x, center_y, radius + 4.0, 0.0, 2.0 * std::f64::consts::PI);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");
    
    cr.arc(center_x, center_y, radius - 4.0, 0.0, 2.0 * std::f64::consts::PI);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");
    
    // Restore Cairo state (resets line_width, source, etc.)
    cr.restore().expect("Failed to restore");
}
