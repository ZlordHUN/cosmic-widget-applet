// SPDX-License-Identifier: MPL-2.0

//! Widget Monitoring Modules
//!
//! This module organizes all the monitoring and rendering functionality for the
//! COSMIC Monitor desktop widget. Each submodule handles a specific monitoring
//! feature or rendering aspect.
//!
//! # Module Overview
//!
//! ## Monitoring Modules
//! These modules collect system information:
//!
//! - [`utilization`]: CPU, Memory, and GPU usage monitoring via sysinfo/nvidia-smi
//! - [`temperature`]: CPU and GPU temperature readings from hwmon sensors
//! - [`network`]: Network interface bandwidth monitoring
//! - [`storage`]: Disk space usage for mounted filesystems
//! - [`battery`]: System battery and Solaar (Logitech) device battery levels
//! - [`weather`]: OpenWeatherMap API integration for current conditions
//! - [`notifications`]: D-Bus desktop notification monitoring
//! - [`media`]: Cider (Apple Music client) now-playing information
//!
//! ## Rendering Modules
//! These modules handle visual output:
//!
//! - [`renderer`]: Cairo-based drawing of all widget sections
//! - [`layout`]: Dynamic height calculation based on enabled sections
//! - [`theme`]: COSMIC desktop theme integration (accent color, dark/light mode)
//!
//! ## Utility Modules
//!
//! - [`cache`]: JSON-based caching for device discovery (shared with settings app)
//!
//! # Usage
//!
//! The main widget (`widget_main.rs`) creates instances of each monitor and calls
//! their `update()` methods periodically. The collected data is then passed to
//! the renderer for display.

// === Monitoring Module Declarations ===
pub mod utilization;
pub mod temperature;
pub mod network;
pub mod weather;
pub mod storage;
pub mod battery;
pub mod notifications;
pub mod media;

// === Rendering Module Declarations ===
pub mod renderer;
pub mod layout;
pub mod theme;

// === Utility Module Declarations ===
pub mod cache;

// === Public Re-exports ===
// These make the main types available as `widget::TypeName` instead of
// `widget::module::TypeName` for cleaner imports in widget_main.rs

/// CPU, Memory, and GPU usage monitoring
pub use utilization::UtilizationMonitor;

/// CPU and GPU temperature monitoring
pub use temperature::TemperatureMonitor;

/// Network bandwidth monitoring
pub use network::NetworkMonitor;

/// Weather data from OpenWeatherMap
pub use weather::{WeatherMonitor, load_weather_font};

/// Disk space monitoring
pub use storage::StorageMonitor;

/// Battery level monitoring (system + Solaar)
pub use battery::{BatteryMonitor, BatteryDevice};

/// Device discovery cache
pub use cache::WidgetCache;

/// Desktop notification monitoring
pub use notifications::NotificationMonitor;

/// Cider media player integration
pub use media::{MediaMonitor, MediaInfo, PlaybackStatus};

/// COSMIC theme integration
pub use theme::CosmicTheme;
