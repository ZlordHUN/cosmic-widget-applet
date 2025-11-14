// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Enable CPU monitoring
    pub show_cpu: bool,
    /// Enable memory monitoring
    pub show_memory: bool,
    /// Enable network monitoring
    pub show_network: bool,
    /// Enable disk I/O monitoring
    pub show_disk: bool,
    /// Enable GPU monitoring
    pub show_gpu: bool,
    /// Show clock display
    pub show_clock: bool,
    /// Show date display
    pub show_date: bool,
    /// Update interval in milliseconds
    pub update_interval_ms: u64,
    /// Show percentage values
    pub show_percentages: bool,
    /// Widget X position on screen
    pub widget_x: i32,
    /// Widget Y position on screen
    pub widget_y: i32,
    /// Allow widget to be moved (when settings is open)
    pub widget_movable: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_cpu: true,
            show_memory: true,
            show_network: false,
            show_disk: false,
            show_gpu: false,
            show_clock: true,
            show_date: true,
            update_interval_ms: 1000,
            show_percentages: true,
            widget_x: 50,
            widget_y: 50,
            widget_movable: false,
        }
    }
}
