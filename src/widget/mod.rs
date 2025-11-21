// SPDX-License-Identifier: MPL-2.0

//! Widget module organization

pub mod utilization;
pub mod temperature;
pub mod network;
pub mod weather;
pub mod storage;
pub mod renderer;
pub mod layout;
pub mod battery;
pub mod cache;

pub use utilization::UtilizationMonitor;
pub use temperature::TemperatureMonitor;
pub use network::NetworkMonitor;
pub use weather::WeatherMonitor;
pub use storage::StorageMonitor;
pub use battery::{BatteryMonitor, BatteryDevice};
pub use cache::WidgetCache;
