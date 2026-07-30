// SPDX-License-Identifier: MPL-2.0

//! # Network Monitoring Module
//!
//! This module tracks network throughput (upload/download speeds) across all
//! network interfaces using the `sysinfo` crate.
//!
//! ## Measurement Approach
//!
//! Network speed is calculated by measuring the change in total bytes
//! transferred over time:
//!
//! ```text
//! Rate (bytes/sec) = (current_bytes - previous_bytes) / elapsed_time
//! ```
//!
//! The module aggregates traffic from ALL network interfaces (eth0, wlan0,
//! docker0, lo, etc.) to give a system-wide throughput view.
//!
//! ## Data Sources
//!
//! - **sysinfo crate**: Reads from `/proc/net/dev` or equivalent
//! - **Byte counters**: Cumulative since boot (wraps at 2^64)
//!
//! ## Display Format
//!
//! Rates are converted to human-readable units in the renderer:
//! - KB/s for speeds < 1 MB/s
//! - MB/s for speeds ≥ 1 MB/s
//!
//! ## Edge Cases Handled
//!
//! - **Counter reset**: Kernel updates or interface restarts reset counters to 0
//! - **First update**: No previous data, so rate starts at 0
//! - **Interface changes**: The interface list is rediscovered every 30 seconds

use std::time::{Duration, Instant};
use sysinfo::Networks;

const INTERFACE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

// ============================================================================
// Network Monitor Struct
// ============================================================================

/// Monitors network throughput across all interfaces.
///
/// Calculates download (RX) and upload (TX) speeds in bytes per second by
/// tracking the change in cumulative byte counters over time.
///
/// # Fields
///
/// - `networks`: sysinfo's network interface list
/// - `network_rx_bytes`: Previous total received bytes (for delta calculation)
/// - `network_tx_bytes`: Previous total transmitted bytes (for delta calculation)
/// - `network_rx_rate`: Current download speed in bytes/second
/// - `network_tx_rate`: Current upload speed in bytes/second
/// - `last_update`: Timestamp of last update (for elapsed time calculation)
///
/// # Rate Calculation
///
/// ```text
/// rx_rate = (current_rx - previous_rx) / seconds_elapsed
/// tx_rate = (current_tx - previous_tx) / seconds_elapsed
/// ```
pub struct NetworkMonitor {
    /// sysinfo's network interface list (refreshed on update)
    networks: Networks,
    /// Previous total received bytes across all interfaces
    network_rx_bytes: u64,
    /// Previous total transmitted bytes across all interfaces
    network_tx_bytes: u64,
    /// Current download rate in bytes per second
    pub network_rx_rate: f64,
    /// Current upload rate in bytes per second
    pub network_tx_rate: f64,
    /// Timestamp of last update for elapsed time calculation
    last_update: Instant,
    /// Timestamp of the last full interface discovery pass
    last_interface_refresh: Instant,
}

impl NetworkMonitor {
    /// Create a new network monitor.
    ///
    /// Initializes sysinfo's network list with immediate discovery of all
    /// interfaces. Initial rates are 0.0 until the first update provides a
    /// delta from the baseline captured here.
    pub fn new() -> Self {
        let networks = Networks::new_with_refreshed_list();
        let (network_rx_bytes, network_tx_bytes) = total_bytes(&networks);
        let now = Instant::now();

        Self {
            networks,
            network_rx_bytes,
            network_tx_bytes,
            network_rx_rate: 0.0,
            network_tx_rate: 0.0,
            last_update: now,
            last_interface_refresh: now,
        }
    }

    /// Update network throughput calculations.
    ///
    /// Refreshes sysinfo's network data, sums bytes across all interfaces,
    /// then calculates the rate based on time elapsed since last update.
    ///
    /// # Algorithm
    ///
    /// 1. Calculate elapsed time since last update
    /// 2. Refresh network interface data
    /// 3. Sum RX and TX bytes across ALL interfaces
    /// 4. Calculate rates: `(new_bytes - old_bytes) / elapsed_seconds`
    /// 5. Store new byte counts for next delta calculation
    ///
    /// # Counter Reset Handling
    ///
    /// If byte counters appear to have decreased (system reboot, interface
    /// restart, or first update), rates are reset to 0 to avoid showing
    /// incorrect negative or astronomical values.
    pub fn update(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();

        if now.duration_since(self.last_interface_refresh) >= INTERFACE_REFRESH_INTERVAL {
            self.networks.refresh_list();
            self.last_interface_refresh = now;
        } else {
            self.networks.refresh();
        }

        let (total_rx, total_tx) = total_bytes(&self.networks);

        self.network_rx_rate = rate_from_totals(self.network_rx_bytes, total_rx, elapsed);
        self.network_tx_rate = rate_from_totals(self.network_tx_bytes, total_tx, elapsed);

        // Store current values for next update's delta calculation
        self.network_rx_bytes = total_rx;
        self.network_tx_bytes = total_tx;
        self.last_update = now;
    }
}

fn total_bytes(networks: &Networks) -> (u64, u64) {
    networks.values().fold((0, 0), |(rx, tx), network| {
        (
            rx.saturating_add(network.total_received()),
            tx.saturating_add(network.total_transmitted()),
        )
    })
}

fn rate_from_totals(previous: u64, current: u64, elapsed_seconds: f64) -> f64 {
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return 0.0;
    }

    current
        .checked_sub(previous)
        .map_or(0.0, |bytes| bytes as f64 / elapsed_seconds)
}

#[cfg(test)]
mod tests {
    use super::rate_from_totals;

    #[test]
    fn calculates_bytes_per_second_from_cumulative_totals() {
        assert_eq!(rate_from_totals(1_000, 2_024, 0.5), 2_048.0);
    }

    #[test]
    fn counter_resets_do_not_create_invalid_rates() {
        assert_eq!(rate_from_totals(2_000, 100, 1.0), 0.0);
    }

    #[test]
    fn invalid_elapsed_time_produces_zero_rate() {
        assert_eq!(rate_from_totals(100, 200, 0.0), 0.0);
        assert_eq!(rate_from_totals(100, 200, f64::NAN), 0.0);
    }
}
