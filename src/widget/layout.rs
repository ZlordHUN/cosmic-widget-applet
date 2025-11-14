//! Widget layout calculations
//! 
//! Handles dynamic height calculation based on enabled components

use crate::config::Config;

/// Calculate the required widget height based on enabled components
pub fn calculate_widget_height(config: &Config) -> u32 {
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
    
    required_height += 20; // Bottom padding
    
    required_height.max(100) // Minimum 100px height
}
