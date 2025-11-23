//! Widget layout calculations
//! 
//! Handles dynamic height calculation based on enabled components

use crate::config::Config;

/// Calculate the required widget height based on enabled components
pub fn calculate_widget_height(config: &Config, disk_count: usize) -> u32 {
    calculate_widget_height_with_batteries(config, disk_count, 0)
}

/// Calculate the required widget height with battery device count
pub fn calculate_widget_height_with_batteries(config: &Config, disk_count: usize, battery_count: usize) -> u32 {
    calculate_widget_height_with_all(config, disk_count, battery_count, 0)
}

/// Calculate the required widget height with all component counts
pub fn calculate_widget_height_with_all(config: &Config, disk_count: usize, battery_count: usize, notification_count: usize) -> u32 {
    let mut required_height = 10; // Base padding
    
    // Clock and date
    if config.show_clock {
        required_height += 70; // Clock height
    }
    if config.show_date {
        required_height += 35; // Date height
    }
    if config.show_clock || config.show_date {
        required_height += 20; // Spacing after clock/date
    }
    
    // Utilization section
    if config.show_cpu || config.show_memory || config.show_gpu {
        required_height += 35; // "Utilization" header
        if config.show_cpu {
            required_height += 30; // CPU bar
        }
        if config.show_memory {
            required_height += 30; // RAM bar
        }
        if config.show_gpu {
            required_height += 30; // GPU bar
        }
    }
    
    // Temperature section
    if config.show_cpu_temp || config.show_gpu_temp {
        required_height += 10; // Spacing before temps
        required_height += 35; // "Temperatures" header
        
        if config.use_circular_temp_display {
            // Circular display: larger height for circles
            required_height += 60; // Circular temp display height
        } else {
            // Text display
            if config.show_cpu_temp {
                required_height += 25; // CPU temp
            }
            if config.show_gpu_temp {
                required_height += 25; // GPU temp
            }
        }
    }
    
    // Network section
    if config.show_network {
        required_height += 50; // Two network lines
    }
    
    // Storage section
    if config.show_storage && disk_count > 0 {
        required_height += 10; // Spacing before header
        required_height += 35; // "Storage" header
        required_height += disk_count as u32 * 45; // Each disk: 20px name + 12px bar + 13px spacing
    }
    
    // Disk section
    if config.show_disk {
        required_height += 50; // Two disk lines
    }
    
    // Weather section
    if config.show_weather {
        required_height += 10; // Spacing before header
        required_height += 35; // Header
        required_height += 70; // Icon and text content
    }

    // Battery section
    if config.show_battery {
        required_height += 10; // Spacing before header
        required_height += 35; // "Battery" header
        if battery_count > 0 {
            // Each device: 28px for name + 38px for battery icon/percentage + spacing
            required_height += battery_count as u32 * 66;
        } else {
            // Default space for "no devices" message
            required_height += 25;
        }
    }
    
    // Notifications section
    if config.show_notifications {
        required_height += 10; // Spacing before header
        required_height += 35; // "Notifications" header
        if notification_count > 0 {
            // Each notification: ~63px (18px app name + 20px summary + 18px body + 5px spacing)
            let displayed_count = notification_count.min(5); // Max 5 notifications
            required_height += displayed_count as u32 * 63;
        } else {
            // "No notifications" message
            required_height += 25;
        }
    }
    
    required_height += 20; // Bottom padding
    
    required_height.max(100) // Minimum 100px height
}
