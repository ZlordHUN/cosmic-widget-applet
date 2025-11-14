// SPDX-License-Identifier: MPL-2.0

//! Widget module organization

pub mod utilization;
pub mod temperature;
pub mod network;
pub mod weather;
pub mod renderer;
pub mod layout;

pub use utilization::UtilizationMonitor;
pub use temperature::TemperatureMonitor;
pub use network::NetworkMonitor;
pub use weather::WeatherMonitor;
